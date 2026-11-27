#!/usr/bin/env python3
"""Read-only readiness artifact inventory and capacity admission checks.

Nothing in this module deletes, moves, truncates or classifies evidence as safe
to delete. Retention and consumer review are required before reclamation.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys


GIB = 1024 ** 3
MAX_MANIFEST_BYTES = 8 * 1024 * 1024
MAX_TOTAL_MANIFEST_HASH_BYTES = 64 * 1024 * 1024
QCOW2_HEADER_V2_BYTES = 72
QCOW2_HEADER_V3_BYTES = 104
MAX_QCOW2_BACKING_NAME_BYTES = 1023
MAX_TOTAL_QCOW2_METADATA_READ_BYTES = 16 * 1024 * 1024


def is_manifest_name(path):
    name = Path(path).name
    return name == "SHA256SUMS" or name.startswith("SHA256SUMS-") or name == "build-manifest.json"


def capacity(path, required_bytes=0, reserve_bytes=8 * GIB, minimum_free_inodes=4096):
    if min(required_bytes, reserve_bytes, minimum_free_inodes) < 0:
        raise ValueError("capacity requirements must be nonnegative")
    fs = os.statvfs(path)
    free = fs.f_bavail * fs.f_frsize
    reasons = []
    if free < required_bytes + reserve_bytes:
        reasons.append("insufficient_free_bytes")
    if fs.f_files and fs.f_favail < minimum_free_inodes:
        reasons.append("insufficient_free_inodes")
    return {
        "path": str(path), "available_bytes": free,
        "available_inodes": fs.f_favail, "required_bytes": required_bytes,
        "reserve_bytes": reserve_bytes, "minimum_free_inodes": minimum_free_inodes,
        "passed": not reasons, "reasons": reasons,
    }


def require_capacity(path, **requirements):
    result = capacity(path, **requirements)
    if not result["passed"]:
        raise RuntimeError("storage admission refused: " + json.dumps(result, sort_keys=True))
    return result


def fingerprint(st):
    return (st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns, st.st_ctime_ns)


def file_hash(path, expected, dir_fd=None):
    # No symlink following, even if a file is replaced after lstat. Refuse
    # special files and changed files rather than hashing an inconsistent view.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dir_fd)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or fingerprint(before) != fingerprint(expected):
            raise RuntimeError("file changed before hashing")
        digest = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
        if fingerprint(os.fstat(stream.fileno())) != fingerprint(before):
            raise RuntimeError("file changed while hashing")
    return digest.hexdigest()


def qcow2_backing_name(path, expected, dir_fd=None):
    """Read only the bounded qcow2 header fields that name a backing image."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dir_fd)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or fingerprint(before) != fingerprint(expected):
            raise RuntimeError("qcow2 image changed before header inspection")
        header = os.pread(stream.fileno(), QCOW2_HEADER_V3_BYTES, 0)
        bytes_read = len(header)
        result = {"image_format": None, "backing_file": None, "metadata_error": None}
        if len(header) < 4 or header[:4] != b"QFI\xfb":
            if fingerprint(os.fstat(stream.fileno())) != fingerprint(before):
                raise RuntimeError("image changed during format detection")
            return result, bytes_read
        result["image_format"] = "qcow2"
        if len(header) < QCOW2_HEADER_V2_BYTES:
            result["metadata_error"] = "truncated_header"
        else:
            version = int.from_bytes(header[4:8], "big")
            if version not in (2, 3):
                result["metadata_error"] = f"unsupported_version_{version}"
            elif version == 3 and len(header) < QCOW2_HEADER_V3_BYTES:
                result["metadata_error"] = "truncated_v3_header"
            else:
                backing_offset = int.from_bytes(header[8:16], "big")
                backing_size = int.from_bytes(header[16:20], "big")
                header_size = QCOW2_HEADER_V2_BYTES if version == 2 else QCOW2_HEADER_V3_BYTES
                if version == 3:
                    header_size = int.from_bytes(header[100:104], "big")
                    if (header_size < QCOW2_HEADER_V3_BYTES or header_size % 8 != 0 or
                            header_size > before.st_size):
                        result["metadata_error"] = "invalid_v3_header_length"
                if backing_offset == 0 and backing_size == 0:
                    pass
                elif (result["metadata_error"] or backing_offset < header_size or backing_size == 0 or
                      backing_size > MAX_QCOW2_BACKING_NAME_BYTES or
                      backing_offset > before.st_size or
                      backing_size > before.st_size - backing_offset):
                    if result["metadata_error"] is None:
                        result["metadata_error"] = "invalid_backing_file_bounds"
                else:
                    raw_name = os.pread(stream.fileno(), backing_size, backing_offset)
                    bytes_read += len(raw_name)
                    if len(raw_name) != backing_size:
                        result["metadata_error"] = "truncated_backing_file_name"
                    elif b"\0" in raw_name:
                        result["metadata_error"] = "nul_in_backing_file_name"
                    else:
                        result["backing_file"] = os.fsdecode(raw_name)
        if fingerprint(os.fstat(stream.fileno())) != fingerprint(before):
            raise RuntimeError("qcow2 image changed during header inspection")
    return result, bytes_read


def declared_consumers(root, files):
    """Map explicit SHA256SUMS/build-manifest artifact references to files.

    This is intentionally a declared-reference graph, not a claim that every
    real consumer is known. Manifest reads stay beneath a pinned root and
    reject symlinks and files that changed since the inventory walk.
    """
    rows = {item["path"]: item for item in files}
    consumers = {}
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for manifest_path, row in rows.items():
            name = Path(manifest_path).name
            is_sums = name == "SHA256SUMS" or name.startswith("SHA256SUMS-")
            is_build = name == "build-manifest.json"
            if row["kind"] != "file" or not (is_sums or is_build):
                continue
            parts = Path(manifest_path).parts
            directory_fd = os.dup(root_fd)
            try:
                for part in parts[:-1]:
                    next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                      dir_fd=directory_fd)
                    os.close(directory_fd)
                    directory_fd = next_fd
                fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=directory_fd)
                with os.fdopen(fd, "rb") as stream:
                    st = os.fstat(stream.fileno())
                    before = (st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns,
                              st.st_ctime_ns)
                    if before != (row["device"], row["inode"], row["apparent_bytes"],
                                  row["mtime_ns"], row["ctime_ns"]):
                        continue
                    raw = stream.read(8 * 1024 * 1024 + 1)
                    after = os.fstat(stream.fileno())
                    if before != (after.st_dev, after.st_ino, after.st_size,
                                  after.st_mtime_ns, after.st_ctime_ns):
                        continue
                if len(raw) > 8 * 1024 * 1024:
                    continue
                captured_hash = row.get("sha256")
                if captured_hash is None or hashlib.sha256(raw).hexdigest() != captured_hash:
                    continue
                references = []
                if is_sums:
                    for line in raw.decode("utf-8", "replace").splitlines():
                        match = re.fullmatch(r"[0-9a-fA-F]{64} [ *](.+)", line)
                        if match:
                            references.append(match.group(1))
                else:
                    try:
                        data = json.loads(raw)
                        artifacts = data.get("artifacts", {})
                        if isinstance(artifacts, dict):
                            references.extend(k for k, v in artifacts.items()
                                              if isinstance(k, str) and isinstance(v, str))
                    except (UnicodeDecodeError, json.JSONDecodeError):
                        continue
                parent = Path(manifest_path).parent
                for reference in references:
                    candidate = Path(reference)
                    if candidate.is_absolute() or ".." in candidate.parts or not candidate.parts:
                        continue
                    target = str(parent / candidate)
                    target_row = rows.get(target)
                    if target_row and target_row["kind"] == "file":
                        consumers.setdefault(target, set()).add(manifest_path)
            except (OSError, RuntimeError):
                continue
            finally:
                os.close(directory_fd)
    finally:
        os.close(root_fd)
    return consumers


def canonicalize_inventory_allocation(report):
    """Recompute unique allocation totals from captured path metadata rows.

    This is also useful to correct group attribution in an existing inventory
    without re-reading the filesystem. An inode is charged to its bytewise
    smallest relative path, making cross-group hardlink attribution stable.
    """
    canonical = {}
    for item in report["files"]:
        identity = (item["device"], item["inode"])
        previous = canonical.get(identity)
        if previous is None or os.fsencode(item["path"]) < os.fsencode(previous["path"]):
            canonical[identity] = item
        item["charged_bytes"] = 0
    for summary in report["groups"].values():
        summary["allocated_bytes"] = 0
        summary["apparent_bytes"] = 0

    allocated = apparent = 0
    for item in canonical.values():
        item["charged_bytes"] = item["allocated_bytes"]
        group = item["path"].split(os.sep, 1)[0] if item["path"] else "."
        summary = report["groups"][group]
        summary["allocated_bytes"] += item["allocated_bytes"]
        summary["apparent_bytes"] += item["apparent_bytes"]
        allocated += item["allocated_bytes"]
        apparent += item["apparent_bytes"]
    report["allocated_bytes"] = allocated
    report["apparent_bytes"] = apparent
    report["version"] = 2
    report["allocation_attribution"] = "bytewise-smallest-relative-path-per-device-inode"
    return report


def inventory(root, hash_max_bytes=0):
    """Measure allocated bytes, count hardlinks once, never traverse symlinks.

    This is a best-effort live inventory, not an atomic snapshot or a complete
    dependency graph. Every unreviewed file is protected by default. Report
    errors explicitly; never turn unreadable directories into empty totals.
    """
    if hash_max_bytes < 0:
        raise ValueError("hash_max_bytes must be nonnegative")
    root = Path(os.path.abspath(root))
    root_st = root.lstat()
    if not stat.S_ISDIR(root_st.st_mode):
        raise ValueError("inventory root must be a real directory")
    files, errors, groups = [], [], {}
    seen = set()
    total = apparent = 0
    manifest_hash_bytes = 0
    unhashed_manifests = 0
    qcow2_metadata_bytes = 0
    qcow2_metadata_skipped_budget = 0
    qcow2_metadata_error_count = 0

    def record(relative, st, parent_fd=None, name=None):
        nonlocal total, apparent, manifest_hash_bytes, unhashed_manifests
        nonlocal qcow2_metadata_bytes, qcow2_metadata_skipped_budget, qcow2_metadata_error_count
        group = relative.parts[0] if relative.parts else "."
        identity = (st.st_dev, st.st_ino)
        unique = identity not in seen
        seen.add(identity)
        charged = st.st_blocks * 512 if unique else 0
        size = st.st_size if unique else 0
        total += charged
        apparent += size
        summary = groups.setdefault(group, {"allocated_bytes": 0, "apparent_bytes": 0,
                                           "entries": 0})
        summary["allocated_bytes"] += charged
        summary["apparent_bytes"] += size
        summary["entries"] += 1
        kind = ("directory" if stat.S_ISDIR(st.st_mode) else
                "file" if stat.S_ISREG(st.st_mode) else
                "symlink" if stat.S_ISLNK(st.st_mode) else "special")
        item = {
            "path": str(relative), "kind": kind, "device": st.st_dev,
            "inode": st.st_ino, "uid": st.st_uid, "gid": st.st_gid,
            "mode": oct(stat.S_IMODE(st.st_mode)), "mtime_ns": st.st_mtime_ns,
            "ctime_ns": st.st_ctime_ns,
            "apparent_bytes": st.st_size, "allocated_bytes": st.st_blocks * 512,
            "charged_bytes": charged, "hardlink_count": st.st_nlink,
            "retention_class": "protected_pending_review",
            "purpose": "readiness artifact; owner review required",
            "consumers": None, "sha256": None,
        }
        files.append(item)
        explicitly_bounded = kind == "file" and 0 < hash_max_bytes >= st.st_size
        is_manifest = kind == "file" and is_manifest_name(relative)
        manifest_bounded = (is_manifest and st.st_size <= MAX_MANIFEST_BYTES and
                            manifest_hash_bytes + st.st_size <= MAX_TOTAL_MANIFEST_HASH_BYTES)
        if explicitly_bounded or manifest_bounded:
            item["sha256"] = file_hash(name, st, dir_fd=parent_fd)
            if manifest_bounded:
                manifest_hash_bytes += st.st_size
        elif is_manifest:
            unhashed_manifests += 1
        if kind == "file" and Path(name or relative.name).suffix.lower() == ".qcow2":
            if qcow2_metadata_bytes + QCOW2_HEADER_V3_BYTES + MAX_QCOW2_BACKING_NAME_BYTES > MAX_TOTAL_QCOW2_METADATA_READ_BYTES:
                qcow2_metadata_skipped_budget += 1
            else:
                metadata, bytes_read = qcow2_backing_name(name, st, dir_fd=parent_fd)
                item.update(metadata)
                qcow2_metadata_bytes += bytes_read
                if metadata["metadata_error"]:
                    qcow2_metadata_error_count += 1
        return kind

    # Open every component relative to a pinned root descriptor. An ancestor
    # replaced by a symlink cannot redirect subsequent reads outside this root.
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        if (os.fstat(root_fd).st_dev, os.fstat(root_fd).st_ino) != (root_st.st_dev, root_st.st_ino):
            raise RuntimeError("inventory root changed before opening")
        record(Path(), root_st)
        stack = [(Path(), (root_st.st_dev, root_st.st_ino))]
        while stack:
            relative, expected = stack.pop()
            directory_fd = os.dup(root_fd)
            try:
                for part in relative.parts:
                    next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                      dir_fd=directory_fd)
                    os.close(directory_fd)
                    directory_fd = next_fd
                opened = os.fstat(directory_fd)
                if (opened.st_dev, opened.st_ino) != expected:
                    raise RuntimeError("directory identity changed before traversal")
                with os.scandir(directory_fd) as entries:
                    names = sorted(entry.name for entry in entries)
                for name in names:
                    child = relative / name
                    try:
                        st = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
                        kind = record(child, st, directory_fd, name)
                        if kind == "directory":
                            if st.st_dev != root_st.st_dev:
                                raise RuntimeError("nested filesystem skipped")
                            stack.append((child, (st.st_dev, st.st_ino)))
                    except (OSError, RuntimeError) as error:
                        errors.append({"path": str(child), "error": str(error)})
            except (OSError, RuntimeError) as error:
                errors.append({"path": str(relative), "error": str(error)})
            finally:
                os.close(directory_fd)
    finally:
        os.close(root_fd)

    report = {
        "version": 2, "root": str(root),
        "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "complete": not errors, "snapshot_atomic": False,
        "allocated_bytes": total, "apparent_bytes": apparent,
        "entry_count": len(files), "groups": groups, "files": files, "errors": errors,
        "capacity": capacity(root), "reclamation_authorized": False,
        "limits": ["live metadata walk; files may change after observation",
                   "consumers and retention require review before reclamation",
                   "hardlinks charged once; shared CoW extents are not deduplicated",
                   "hardlinked inode bytes are assigned to the bytewise canonical path's top-level group",
                   "SHA-256 omitted unless a regular file is within the requested hash bound",
                   "qcow2 backing metadata is bounded to v2/v3 header fields; this is not a full image-integrity check"],
    }
    references = declared_consumers(root, files)
    rows = {item["path"]: item for item in files}
    backing_edges = []
    for item in files:
        item["backing_target"] = None
        item["backing_target_state"] = None
        item["dependent_overlays"] = []
    for item in files:
        if item.get("image_format") != "qcow2" or not item.get("backing_file"):
            continue
        backing = Path(item["backing_file"])
        candidate = backing if backing.is_absolute() else root / Path(item["path"]).parent / backing
        candidate = Path(os.path.abspath(candidate))
        try:
            relative_target = str(candidate.relative_to(root))
        except ValueError:
            item["backing_target"] = str(candidate)
            item["backing_target_state"] = "outside_inventory_root"
            backing_edges.append({"overlay": item["path"], "backing_file": item["backing_file"],
                                  "target": str(candidate), "state": "outside_inventory_root"})
            continue
        target = rows.get(relative_target)
        item["backing_target"] = relative_target
        if target and target["kind"] == "file":
            item["backing_target_state"] = "present_in_inventory"
            target["dependent_overlays"].append(item["path"])
            backing_edges.append({"overlay": item["path"], "backing_file": item["backing_file"],
                                  "target": relative_target, "state": "present_in_inventory"})
        else:
            item["backing_target_state"] = "not_present_in_inventory"
            backing_edges.append({"overlay": item["path"], "backing_file": item["backing_file"],
                                  "target": relative_target, "state": "not_present_in_inventory"})
    for item in files:
        item["declared_consumers"] = sorted(references.get(item["path"], ()))
        item["consumer_scope"] = "explicit_manifests_only"
    report["consumer_graph"] = "declared SHA256SUMS and build-manifest artifact references; incomplete by design"
    report["manifest_integrity"] = {
        "per_manifest_limit_bytes": MAX_MANIFEST_BYTES,
        "total_hash_budget_bytes": MAX_TOTAL_MANIFEST_HASH_BYTES,
        "hashed_bytes": manifest_hash_bytes,
        "unhashed_manifest_count": unhashed_manifests,
        "content_revalidated_before_reference_use": True,
    }
    report["qcow2_backing_graph"] = {
        "edges": backing_edges,
        "edge_count": len(backing_edges),
        "metadata_bytes_read": qcow2_metadata_bytes,
        "metadata_read_budget_bytes": MAX_TOTAL_QCOW2_METADATA_READ_BYTES,
        "metadata_skipped_budget_count": qcow2_metadata_skipped_budget,
        "metadata_error_count": qcow2_metadata_error_count,
        "scope": "qcow2 v2/v3 backing_file header fields only; does not parse other image formats, external data files, or cryptographic image integrity",
    }
    return canonicalize_inventory_allocation(report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path)
    parser.add_argument("--preflight", action="store_true")
    parser.add_argument("--required-gib", type=int, default=0)
    parser.add_argument("--reserve-gib", type=int, default=8)
    parser.add_argument("--minimum-free-inodes", type=int, default=4096)
    parser.add_argument("--hash-max-bytes", type=int, default=0)
    args = parser.parse_args()
    try:
        report = (capacity(args.path, args.required_gib * GIB, args.reserve_gib * GIB,
                           args.minimum_free_inodes) if args.preflight else
                  inventory(args.path, args.hash_max_bytes))
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(2, f"{error}\n")
    json.dump(report, sys.stdout, sort_keys=True, indent=2)
    print()
    return 0 if report.get("complete", report.get("passed")) else 1


if __name__ == "__main__":
    sys.exit(main())
