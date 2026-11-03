"""Create deterministic gzip-compressed source archives for release evidence."""

import gzip
import hashlib
import io
from pathlib import Path
import subprocess
import tarfile
from typing import Iterable, Mapping

SOURCE_TREE_EXCLUDES = frozenset(
    {
        "PRODUCTION_READINESS_AUDIT.md",
        ".claude/settings.local.json",
    }
)


def collect_source_entries(root: Path) -> tuple[list[dict[str, object]], str]:
    """Collect the release inputs and compute their canonical tree digest."""
    root = Path(root).resolve()
    raw_paths = subprocess.check_output(
        ["git", "ls-files", "-co", "--exclude-standard", "-z"], cwd=root
    ).split(b"\0")
    digest = hashlib.sha256()
    entries: list[dict[str, object]] = []
    for raw_path in sorted(path for path in raw_paths if path):
        name = raw_path.decode()
        if name in SOURCE_TREE_EXCLUDES:
            continue
        source = root / name
        if source.is_symlink():
            raise RuntimeError(f"symlink source input is not allowed: {name}")
        if not source.is_file():
            continue
        contents = source.read_bytes()
        encoded_name = source.relative_to(root).as_posix().encode()
        digest.update(len(encoded_name).to_bytes(8, "big"))
        digest.update(encoded_name)
        digest.update(len(contents).to_bytes(8, "big"))
        digest.update(contents)
        entries.append(
            {
                "path": source.relative_to(root).as_posix(),
                "sha256": hashlib.sha256(contents).hexdigest(),
                "size": len(contents),
            }
        )
    return entries, digest.hexdigest()


def write_source_archive(
    root: Path, entries: Iterable[Mapping[str, object]], destination: Path
) -> None:
    """Write entries with normalized metadata and a zero-time gzip header."""
    root = Path(root)
    destination = Path(destination)
    with destination.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for entry in entries:
                    name = str(entry["path"])
                    source = root / name
                    contents = source.read_bytes()
                    actual_hash = hashlib.sha256(contents).hexdigest()
                    expected_hash = str(entry["sha256"])
                    if actual_hash != expected_hash:
                        raise RuntimeError(
                            f"source changed while packaging: {name} "
                            f"({actual_hash} != {expected_hash})"
                        )
                    info = archive.gettarinfo(str(source), arcname=name)
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 0
                    archive.addfile(info, io.BytesIO(contents))
