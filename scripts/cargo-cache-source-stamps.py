"""Preserve freshness of unchanged vendored GPUI in CI compiler caches.

Capture BEFORE Cargo; cache the snapshot only with successful compiler outputs.
Restore a package only when its complete file tree, bytes, and modes match.
Never follow links/reparse points or modify files outside the two pinned roots.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import time


PACKAGES = (
    "vendor/gpui-pre/gpui-pre-0.3.8",
    "vendor/gpui-pre/gpui-pre-windows-0.3.8",
)
STATE_NAME = "fintwind-gpui-source-stamps.json"
MAX_STATE_BYTES = 8 * 1024 * 1024
MAX_ENTRIES = 20_000
MAX_PACKAGE_BYTES = 128 * 1024 * 1024


class UnsupportedTree(ValueError):
    pass


def regular_metadata(path):
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise UnsupportedTree("link-or-reparse-point")
    return metadata


def scan(workspace, relative):
    root = workspace
    for part in relative.split("/"):
        root /= part
        if not stat.S_ISDIR(regular_metadata(root).st_mode):
            raise UnsupportedTree("package-root-not-directory")
    entries = []
    total_bytes = 0

    def visit(path, name):
        nonlocal total_bytes
        if len(entries) >= MAX_ENTRIES:
            raise UnsupportedTree("package-entry-limit")
        metadata = regular_metadata(path)
        entry = {"path": name, "mtime_ns": metadata.st_mtime_ns,
                 "mode": stat.S_IMODE(metadata.st_mode)}
        if stat.S_ISDIR(metadata.st_mode):
            entry["kind"] = "directory"
            entries.append(entry)
            for child in sorted(path.iterdir(), key=lambda value: value.name):
                visit(child, f"{name}/{child.name}" if name else child.name)
            after = regular_metadata(path)
            if (after.st_mtime_ns, after.st_ino, after.st_mode) != (
                metadata.st_mtime_ns, metadata.st_ino, metadata.st_mode,
            ):
                raise UnsupportedTree("directory-changed-during-read")
        elif stat.S_ISREG(metadata.st_mode):
            total_bytes += metadata.st_size
            if total_bytes > MAX_PACKAGE_BYTES:
                raise UnsupportedTree("package-byte-limit")
            digest = hashlib.sha256()
            with path.open("rb") as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(chunk)
            after = regular_metadata(path)
            if (after.st_size, after.st_mtime_ns, after.st_ino, after.st_mode) != (
                metadata.st_size, metadata.st_mtime_ns, metadata.st_ino, metadata.st_mode,
            ):
                raise UnsupportedTree("source-changed-during-read")
            entry.update(kind="file", size=metadata.st_size, sha256=digest.hexdigest())
            entries.append(entry)
        else:
            raise UnsupportedTree("non-regular-input")

    visit(root, "")
    return entries


def validate_state(data, profile):
    if (not isinstance(data, dict) or type(data.get("schema")) is not int
            or data["schema"] != 1 or data.get("profile") != profile):
        raise ValueError("schema-or-profile-mismatch")
    packages = data.get("packages")
    if not isinstance(packages, list) or len(packages) > len(PACKAGES):
        raise ValueError("invalid-package-list")
    result = {}
    latest = time.time_ns() + 60_000_000_000
    for package in packages:
        if not isinstance(package, dict) or package.get("path") not in PACKAGES:
            raise ValueError("invalid-package-root")
        if package["path"] in result:
            raise ValueError("duplicate-package-root")
        entries = package.get("entries")
        if not isinstance(entries, list) or not 1 <= len(entries) <= MAX_ENTRIES:
            raise ValueError("invalid-entry-list")
        indexed = {}
        casefolded = set()
        for entry in entries:
            if not isinstance(entry, dict):
                raise ValueError("invalid-entry")
            name = entry.get("path")
            if (not isinstance(name, str) or len(name) > 1024
                    or "\\" in name or ":" in name or "\0" in name):
                raise ValueError("invalid-entry-path")
            if name and any(part in ("", ".", "..") for part in name.split("/")):
                raise ValueError("invalid-entry-path")
            if name.casefold() in casefolded:
                raise ValueError("duplicate-entry")
            casefolded.add(name.casefold())
            stamp = entry.get("mtime_ns")
            if type(stamp) is not int or not 0 < stamp <= latest:
                raise ValueError("invalid-timestamp")
            mode = entry.get("mode")
            if type(mode) is not int or not 0 <= mode <= 0o7777:
                raise ValueError("invalid-mode")
            kind = entry.get("kind")
            if kind not in ("file", "directory"):
                raise ValueError("invalid-entry-kind")
            if kind == "file":
                if type(entry.get("size")) is not int or entry["size"] < 0:
                    raise ValueError("invalid-file-size")
                if not isinstance(entry.get("sha256"), str) or not re.fullmatch("[a-f0-9]{64}", entry["sha256"]):
                    raise ValueError("invalid-file-hash")
            indexed[name] = entry
        if indexed.get("", {}).get("kind") != "directory":
            raise ValueError("missing-root-directory")
        for name in indexed:
            if name and indexed.get(name.rpartition("/")[0], {}).get("kind") != "directory":
                raise ValueError("missing-parent-directory")
        result[package["path"]] = indexed
    return result


def compatible(current, saved):
    if {entry["path"] for entry in current} != set(saved):
        return False
    for entry in current:
        before = saved[entry["path"]]
        for key in ("kind", "mode", "size", "sha256"):
            if entry.get(key) != before.get(key):
                return False
    return True


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    # Replace, rather than partially overwriting a snapshot that might be cached.
    name = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=path.name + "-", delete=False) as output:
            name = output.name
            json.dump(data, output, indent=2)
            output.write("\n")
        os.replace(name, path)
        name = None
    finally:
        if name:
            Path(name).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("restore", "capture"))
    parser.add_argument("--profile", choices=("debug", "release"), required=True)
    parser.add_argument("--state", type=Path, help="Override snapshot output for read-only capture measurements")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    started = time.monotonic()
    workspace = Path.cwd().resolve()
    target = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    state = args.state or target / args.profile / ".fingerprint" / STATE_NAME
    saved = {}
    missing_reason = "snapshot-missing"
    if args.operation == "restore":
        try:
            if regular_metadata(state).st_size > MAX_STATE_BYTES:
                raise ValueError("snapshot-byte-limit")
            saved = validate_state(json.loads(state.read_text(encoding="utf-8")), args.profile)
            missing_reason = "package-not-recorded"
        except FileNotFoundError:
            pass
        except (OSError, ValueError, UnicodeError):
            missing_reason = "snapshot-invalid"
    packages = []
    captured = []
    for relative in PACKAGES:
        item = {"path": relative, "status": "skipped"}
        packages.append(item)
        if args.operation == "restore" and relative not in saved:
            item["reason"] = missing_reason
            continue
        try:
            current = scan(workspace, relative)
        except (OSError, UnsupportedTree) as error:
            item["reason"] = str(error)
            continue
        if args.operation == "restore":
            if not compatible(current, saved[relative]):
                item["reason"] = "package-changed"
                continue
            root = workspace / relative
            # Verify the complete tree before touching anything. Restore leaf
            # files first, then directories deepest-first to retain watch paths.
            ordered = sorted(current, key=lambda entry: (
                entry["kind"] == "directory", -entry["path"].count("/"), -len(entry["path"]),
            ))
            for entry in ordered:
                path = root / entry["path"]
                stamp = saved[relative][entry["path"]]["mtime_ns"]
                if stamp != entry["mtime_ns"]:
                    # Windows Python cannot set follow_symlinks=False here.
                    # The private CI checkout was scanned without following any
                    # reparse point; recheck the leaf before changing metadata.
                    metadata = regular_metadata(path)
                    if (metadata.st_mtime_ns != entry["mtime_ns"]
                            or stat.S_IMODE(metadata.st_mode) != entry["mode"]
                            or (entry["kind"] == "file" and metadata.st_size != entry["size"])):
                        raise OSError(f"Source changed during timestamp restoration: {path}")
                    os.utime(path, ns=(metadata.st_atime_ns, stamp))
            item["status"] = "restored"
        else:
            captured.append({"path": relative, "entries": current})
            item["status"] = "captured"
        item["files"] = sum(entry["kind"] == "file" for entry in current)
        item["directories"] = sum(entry["kind"] == "directory" for entry in current)
    if args.operation == "capture":
        write_json(state, {"schema": 1, "profile": args.profile, "packages": captured})
    report = {"operation": args.operation, "profile": args.profile, "state": str(state),
              "packages": packages, "seconds": round(time.monotonic() - started, 3)}
    if args.report:
        write_json(args.report, report)
    for package in packages:
        print(f"[INFO] {package['status']}: {package['path']} "
              f"{package.get('reason', '')}")


if __name__ == "__main__":
    main()
