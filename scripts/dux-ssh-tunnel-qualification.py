#!/usr/bin/env python3
"""Exercise the restricted Dux tunnel authorized_keys policy against local sshd.

This starts only a temporary loopback sshd and uses keys/configuration under a
private temporary directory. It never reads or changes /etc/dux or system sshd.
Requires root, sshd, ssh, ssh-keygen, and Python 3.
"""
from __future__ import annotations

import json
import argparse
import ipaddress
import os
from pathlib import Path
import shutil
import shlex
import socket
import subprocess
import tempfile
import threading
import time


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def tcp_listener_addresses(port: int) -> list[ipaddress.IPv4Address | ipaddress.IPv6Address]:
    """Return the kernel's local addresses for LISTEN sockets on port."""
    addresses: list[ipaddress.IPv4Address | ipaddress.IPv6Address] = []
    for table, ipv6 in ((Path("/proc/net/tcp"), False), (Path("/proc/net/tcp6"), True)):
        try:
            lines = table.read_text().splitlines()[1:]
        except OSError:
            continue
        for line in lines:
            fields = line.split()
            if len(fields) < 4 or fields[3] != "0A":
                continue
            raw_address, raw_port = fields[1].split(":")
            if int(raw_port, 16) != port:
                continue
            raw = bytes.fromhex(raw_address)
            if ipv6:
                raw = b"".join(raw[offset : offset + 4][::-1] for offset in range(0, 16, 4))
            else:
                raw = raw[::-1]
            address = ipaddress.ip_address(raw)
            addresses.append(address)
    return addresses


def require_tools() -> tuple[str, str, str]:
    if os.geteuid() != 0:
        raise SystemExit("run as root so the temporary sshd can authenticate the root test account")
    found = tuple(shutil.which(name) for name in ("sshd", "ssh", "ssh-keygen"))
    if any(item is None for item in found):
        raise SystemExit("sshd, ssh, and ssh-keygen are required")
    return found  # type: ignore[return-value]


def run_checked(args: list[str], **kwargs) -> subprocess.CompletedProcess:
    """Run a command and preserve captured diagnostics when it fails."""
    kwargs.pop("check", None)
    result = subprocess.run(args, **kwargs)
    if result.returncode:
        stdout = result.stdout or ""
        stderr = result.stderr or ""
        raise RuntimeError(
            f"command {shlex.join(args)} exited {result.returncode}"
            f"\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )
    return result


def main(gateway_ports: str) -> None:
    sshd, ssh, ssh_keygen = require_tools()
    checks: dict[str, str] = {}
    with tempfile.TemporaryDirectory(prefix="dux-ssh-tunnel-qual-") as temporary:
        root = Path(temporary)
        host_key = root / "host_ed25519"
        client_key = root / "client_ed25519"
        wrong_host_key = root / "wrong_host_ed25519"
        known_hosts = root / "known_hosts"
        authorized_keys = root / "authorized_keys"
        config = root / "sshd_config"
        log = root / "sshd.log"
        run_checked(
            [ssh_keygen, "-q", "-t", "ed25519", "-N", "", "-f", str(host_key)],
        )
        run_checked(
            [ssh_keygen, "-q", "-t", "ed25519", "-N", "", "-f", str(client_key)],
        )
        run_checked(
            [ssh_keygen, "-q", "-t", "ed25519", "-N", "", "-f", str(wrong_host_key)],
        )

        selected_ports: list[int] = []
        while len(selected_ports) < 5:
            port = free_port()
            if port not in selected_ports:
                selected_ports.append(port)
        (
            daemon_port,
            permitted_port,
            denied_port,
            local_forward_port,
            endpoint_target_port,
        ) = selected_ports

        public_key = Path(f"{client_key}.pub").read_text().strip()
        authorized_keys.write_text(
            "restrict,port-forwarding,"
            f'permitlisten="localhost:{permitted_port}",'
            'permitopen="dux-tunnel-disabled.invalid:1",'
            'command="true" '
            f"{public_key}\n"
        )
        authorized_keys.chmod(0o600)
        config.write_text(
            "\n".join(
                (
                    f"Port {daemon_port}",
                    "ListenAddress 127.0.0.1",
                    f"HostKey {host_key}",
                    f"PidFile {root / 'sshd.pid'}",
                    f"AuthorizedKeysFile {authorized_keys}",
                    "PermitRootLogin yes",
                    "PubkeyAuthentication yes",
                    "PasswordAuthentication no",
                    "KbdInteractiveAuthentication no",
                    "UsePAM no",
                    "StrictModes no",
                    "AllowTcpForwarding yes",
                    f"GatewayPorts {gateway_ports}",
                    "LogLevel VERBOSE",
                )
            )
            + "\n"
        )
        run_checked([sshd, "-t", "-f", str(config)], capture_output=True, text=True)

        with log.open("w") as log_file:
            daemon = subprocess.Popen(
                [sshd, "-D", "-e", "-f", str(config)],
                stdout=subprocess.DEVNULL,
                stderr=log_file,
            )
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if daemon.poll() is not None:
                    raise RuntimeError(f"temporary sshd exited with status {daemon.returncode}")
                try:
                    with socket.create_connection(("127.0.0.1", daemon_port), timeout=0.1):
                        break
                except OSError:
                    time.sleep(0.05)
            else:
                raise TimeoutError("temporary sshd did not start")

            common = [
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                f"UserKnownHostsFile={known_hosts}",
                "-o",
                "ConnectTimeout=3",
                "-i",
                str(client_key),
                "-p",
                str(daemon_port),
                "root@127.0.0.1",
            ]

            # Unknown and mismatched host keys must fail without prompting or
            # silently enrolling a key. The verified pin then permits contact.
            known_hosts.write_text("")
            known_hosts.chmod(0o600)
            unknown = subprocess.run(
                [ssh, "-T", *common, "true"], capture_output=True, text=True, timeout=5
            )
            if unknown.returncode == 0:
                raise AssertionError("unknown endpoint host key was accepted")
            checks["unknown_host_key_rejected"] = "passed"

            wrong_public = Path(f"{wrong_host_key}.pub").read_text().strip().split()
            known_hosts.write_text(f"[127.0.0.1]:{daemon_port} {' '.join(wrong_public[:2])}\n")
            mismatch = subprocess.run(
                [ssh, "-T", *common, "true"], capture_output=True, text=True, timeout=5
            )
            if mismatch.returncode == 0:
                raise AssertionError("mismatched endpoint host key was accepted")
            checks["mismatched_host_key_rejected"] = "passed"

            host_public = Path(f"{host_key}.pub").read_text().strip().split()
            known_hosts.write_text(f"[127.0.0.1]:{daemon_port} {' '.join(host_public[:2])}\n")
            checks["verified_host_key_installed"] = "passed"

            # The forced command suppresses arbitrary requested shell commands.
            command = run_checked(
                [ssh, "-T", *common, "printf unexpected-shell-output"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if command.stdout:
                raise AssertionError("forced command allowed caller shell output")
            checks["forced_command"] = "passed"

            # The explicitly permitted loopback listener carries a real byte
            # exchange through the reverse SSH channel.
            echo = socket.socket()
            echo.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            echo.bind(("127.0.0.1", endpoint_target_port))
            echo.listen(1)
            echo.settimeout(8)

            def echo_once() -> None:
                try:
                    connection, _ = echo.accept()
                    with connection:
                        connection.sendall(connection.recv(128))
                except OSError:
                    pass

            echo_thread = threading.Thread(target=echo_once, daemon=True)
            echo_thread.start()
            forward = subprocess.Popen(
                [
                    ssh,
                    "-N",
                    "-T",
                    "-o",
                    "ExitOnForwardFailure=yes",
                    "-o",
                    "ServerAliveInterval=1",
                    "-o",
                    "ServerAliveCountMax=2",
                    *common[:-1],
                    "-R",
                    f"localhost:{permitted_port}:localhost:{endpoint_target_port}",
                    common[-1],
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 5
                received = None
                while time.monotonic() < deadline:
                    if forward.poll() is not None:
                        raise RuntimeError("allowed reverse forward exited unexpectedly")
                    try:
                        with socket.create_connection(("127.0.0.1", permitted_port), timeout=0.2) as client:
                            client.settimeout(1)
                            client.sendall(b"authorized-loopback\n")
                            received = client.recv(128)
                            break
                    except OSError:
                        time.sleep(0.05)
                if received != b"authorized-loopback\n":
                    raise AssertionError(f"allowed reverse forward returned {received!r}")
                checks["allowed_loopback_reverse_forward"] = "passed"
                listeners = tcp_listener_addresses(permitted_port)
                if not listeners or any(not address.is_loopback for address in listeners):
                    raise AssertionError(f"remote listener escaped loopback: {listeners!r}")
                checks["remote_listener_is_loopback"] = "passed"
            finally:
                forward.terminate()
                try:
                    forward.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    forward.kill()
                    forward.wait(timeout=3)
                echo.close()
                echo_thread.join(timeout=1)

            # A key restricted to the generated port must fail when asked to
            # create a second remote listener.
            denied = subprocess.run(
                [
                    ssh,
                    "-N",
                    "-T",
                    "-o",
                    "ExitOnForwardFailure=yes",
                    *common[:-1],
                    "-R",
                    f"localhost:{denied_port}:localhost:{endpoint_target_port}",
                    common[-1],
                ],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if denied.returncode == 0:
                raise AssertionError("unauthorized remote listener port was accepted")
            checks["denied_remote_listener"] = "passed"

            # A separate loopback listener makes an accidentally accepted
            # direct-tcpip/local forward observable without contacting anything
            # outside this temporary test process.
            endpoint_listener = socket.socket()
            endpoint_listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            endpoint_listener.bind(("127.0.0.1", endpoint_target_port))
            endpoint_listener.listen(1)
            endpoint_listener.settimeout(0.4)
            local_forward = subprocess.Popen(
                [
                    ssh,
                    "-N",
                    "-T",
                    *common[:-1],
                    "-L",
                    f"127.0.0.1:{local_forward_port}:127.0.0.1:{endpoint_target_port}",
                    common[-1],
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if local_forward.poll() is not None:
                        raise RuntimeError("SSH exited before local-forward restriction check")
                    try:
                        with socket.create_connection(("127.0.0.1", local_forward_port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise TimeoutError("local-forward listener did not start")
                try:
                    connection, _ = endpoint_listener.accept()
                except TimeoutError:
                    checks["denied_local_forward"] = "passed"
                else:
                    connection.close()
                    raise AssertionError("unauthorized local forward reached its target")
            finally:
                local_forward.terminate()
                try:
                    local_forward.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    local_forward.kill()
                    local_forward.wait(timeout=3)
                endpoint_listener.close()
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=3)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=3)

    print(json.dumps({"checks": checks, "status": "passed"}, sort_keys=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gateway-ports",
        choices=("no", "clientspecified"),
        default="no",
        help="temporary sshd GatewayPorts mode to verify against the key restrictions",
    )
    args = parser.parse_args()
    main(args.gateway_ports)
