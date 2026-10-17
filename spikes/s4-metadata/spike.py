#!/usr/bin/env python3
"""Measure metadata round trips through macOS ditto and mounted targets.

All writes stay inside fresh temporary directories or owned disk images.
Exit status describes harness completion; measurement verdicts are in JSON.
"""

from __future__ import annotations

import argparse
import contextlib
import errno
from enum import Enum
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import unicodedata


NANOSECONDS = 1_000_000_000
PROBE_TIME_NS = 1_700_000_000 * NANOSECONDS
CORPUS_TIME_NS = PROBE_TIME_NS + 123_456_789
SPARSE_SIZE = 16 * 1024 * 1024
DEVICE_BLOCK_BYTES = 512
COMMAND_TIMEOUT_SECONDS = 120
APPLEDOUBLE_MAGIC = b"\x00\x05\x16\x07"
APPLEDOUBLE_VERSION = 0x00020000
APPLEDOUBLE_HEADER_BYTES = 26
APPLEDOUBLE_ENTRY_BYTES = 12
FINDER_INFO = "com.apple.FinderInfo"
RESOURCE_FORK = "com.apple.ResourceFork"
FINDER_TAGS = "com.apple.metadata:_kMDItemUserTags"
CUSTOM_ATTRIBUTE = "ch.lkmc.foldover.spike.s4"
FINDER_INFO_BYTES = 32
FINDER_FLAGS_OFFSET = 8
FINDER_BUNDLE_FLAG = 0x2000
ACL_RULE = "everyone allow read,readattr,readextattr,readsecurity"
UNSUPPORTED_ERRORS = {errno.ENOTSUP, errno.EOPNOTSUPP, errno.EINVAL}
REQUIRED_TARGETS = ["apfs", "exfat", "samba-fruit", "samba-no-fruit", "sftp"]


class CorpusOwner(Enum):
    OWNED_IMAGE = "owned-image"
    EXISTING_MOUNT = "existing-mount"


def command(*args: str | Path) -> bytes:
    result = subprocess.run(
        [str(arg) for arg in args], capture_output=True,
        timeout=COMMAND_TIMEOUT_SECONDS, check=False,
    )
    if result.returncode:
        error = result.stderr.decode(errors="replace").strip()
        if args[0] == "/usr/bin/xattr":
            number = re.search(r"\[Errno (\d+)\]", error)
            if number:
                raise OSError(int(number[1]), error)
        raise RuntimeError(
            f"{args[0]} exited {result.returncode}: "
            f"{error}"
        )
    return result.stdout


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# Python's os xattr functions are Linux-only. The corpus owns these names;
# the production driver needs native byte APIs rather than line-delimited names.
def xattr_names(path: Path) -> list[str]:
    if sys.platform == "darwin":
        return command("/usr/bin/xattr", path).decode().splitlines()
    return os.listxattr(path, follow_symlinks=False)


def xattr_get(path: Path, name: str) -> bytes:
    if sys.platform == "darwin":
        return bytes.fromhex(command("/usr/bin/xattr", "-px", name, path).decode())
    return os.getxattr(path, name, follow_symlinks=False)


def xattr_set(path: Path, name: str, value: bytes) -> None:
    if sys.platform == "darwin":
        command("/usr/bin/xattr", "-wx", name, value.hex(), path)
        return
    os.setxattr(path, name, value, follow_symlinks=False)


def probe(operation) -> dict:
    try:
        return {"state": "observed", "value": operation()}
    except OSError as error:
        state = "unsupported" if error.errno in UNSUPPORTED_ERRORS else "unavailable"
        return {"state": state, "value": None, "error": str(error)}
    except (RuntimeError, subprocess.TimeoutExpired) as error:
        return {"state": "unavailable", "value": None, "error": str(error)}


def observed(capabilities: dict, name: str):
    result = capabilities.get(name, {})
    return result.get("value") if result.get("state") == "observed" else None


def unsupported(capabilities: dict, name: str) -> bool:
    result = capabilities.get(name, {})
    return result.get("state") == "unsupported" or observed(capabilities, name) is False


def sparse_file(path: Path) -> None:
    with path.open("wb") as stream:
        stream.write(b"begin")
        stream.seek(SPARSE_SIZE - len(b"end"))
        stream.write(b"end")


def is_sparse(info: os.stat_result) -> bool:
    return info.st_size > 0 and info.st_blocks * DEVICE_BLOCK_BYTES < info.st_size


def acl(path: Path) -> list[str]:
    if sys.platform != "darwin" or path.is_symlink():
        return []
    # Omit ls's path-bearing header; retain the ordered ACL entries.
    return [line.strip() for line in command("/bin/ls", "-lde", path).decode().splitlines()[1:]]


def attribute_values() -> dict[str, bytes]:
    finder = bytearray(FINDER_INFO_BYTES)
    struct.pack_into(">H", finder, FINDER_FLAGS_OFFSET, FINDER_BUNDLE_FLAG)
    return {
        FINDER_INFO: bytes(finder),
        RESOURCE_FORK: b"Foldover resource fork\x00\xff",
        FINDER_TAGS: plistlib.dumps(["Foldover\n6"], fmt=plistlib.FMT_BINARY),
        CUSTOM_ATTRIBUTE: b"metadata must survive\x00\xff",
    }


def capabilities_at(root: Path, owner: CorpusOwner, cleanup_errors: list) -> dict:
    results = {}
    with corpus_directory(root, "foldover-s4-probe-", owner, cleanup_errors) as directory:
        item = directory / "probe"
        item.write_bytes(b"old")

        (directory / "Case").write_bytes(b"case")
        results["case_sensitive"] = probe(lambda: not (directory / "case").exists())
        decomposed = directory / "re\u0301sume\u0301"
        decomposed.write_bytes(b"unicode")
        results["normalizes_names"] = probe(
            lambda: decomposed.name not in os.listdir(directory)
        )

        def colon_name():
            path = directory / "colon:name"
            path.write_bytes(b"colon")
            return path.name in os.listdir(directory)

        results["colon_names"] = probe(colon_name)

        def permissions():
            item.chmod(0o640)
            return stat.S_IMODE(item.stat().st_mode) == 0o640

        results["permissions"] = probe(permissions)

        def links():
            linked = directory / "hard-link"
            os.link(item, linked)
            return item.stat().st_ino == linked.stat().st_ino

        results["hard_links"] = probe(links)

        def symbolic_link():
            linked = directory / "symbolic-link"
            linked.symlink_to(item.name)
            return linked.is_symlink() and os.readlink(linked) == item.name

        results["symlinks"] = probe(symbolic_link)

        def sparse():
            path = directory / "sparse"
            sparse_file(path)
            return is_sparse(path.stat())

        results["sparse"] = probe(sparse)

        def flags():
            os.chflags(item, stat.UF_HIDDEN)
            preserved = bool(item.stat().st_flags & stat.UF_HIDDEN)
            os.chflags(item, 0)
            return preserved

        results["flags"] = probe(flags)

        def access_control():
            command("/bin/chmod", "+a", ACL_RULE, item)
            return bool(acl(item))

        results["acl"] = probe(access_control)

        for name, value in attribute_values().items():
            def xattr(name=name, value=value):
                xattr_set(item, name, value)
                return xattr_get(item, name) == value

            results[f"xattr:{name}"] = probe(xattr)

        def timestamps():
            observations = []
            for offset in [0, 1, 1_000, 1_000_000, 10_000_000, 100_000_000,
                           NANOSECONDS, 2 * NANOSECONDS]:
                requested = PROBE_TIME_NS + offset
                os.utime(item, ns=(requested, requested))
                observations.append({"requested_ns": requested, "stored_ns": item.stat().st_mtime_ns})
            return observations

        results["mtime_observations"] = probe(timestamps)

        def replacement():
            replacement = directory / "replacement"
            replacement.write_bytes(b"new")
            os.replace(replacement, item)
            return item.read_bytes() == b"new" and not replacement.exists()

        # This proves rename-over succeeds, not its behavior during a crash.
        results["rename_over"] = probe(replacement)

    return results


def mtime_tolerance(capabilities: dict) -> int | None:
    observations = observed(capabilities, "mtime_observations")
    if not observations:
        return None
    stored = sorted({item["stored_ns"] for item in observations})
    steps = [later - earlier for earlier, later in zip(stored, stored[1:])]
    return min(steps) if steps else None


def apple_double(path: Path) -> bool:
    if not path.name.startswith("._") or not os.path.lexists(path.with_name(path.name[2:])):
        return False
    if not stat.S_ISREG(path.lstat().st_mode):
        return False
    with path.open("rb") as stream:
        header = stream.read(APPLEDOUBLE_HEADER_BYTES)
        if len(header) != APPLEDOUBLE_HEADER_BYTES or header[:4] != APPLEDOUBLE_MAGIC:
            return False
        if int.from_bytes(header[4:8], "big") != APPLEDOUBLE_VERSION:
            return False
        count = int.from_bytes(header[24:26], "big")
        if not count:
            return False
        table = stream.read(count * APPLEDOUBLE_ENTRY_BYTES)
        if len(table) != count * APPLEDOUBLE_ENTRY_BYTES:
            return False

    table_end = APPLEDOUBLE_HEADER_BYTES + len(table)
    size, identities, ranges = path.lstat().st_size, set(), []
    for identity, offset, length in struct.iter_unpack(">III", table):
        if identity in identities or offset < table_end or offset + length > size:
            return False
        identities.add(identity)
        if length:
            ranges.append((offset, offset + length))
    ranges.sort()
    return all(left[1] <= right[0] for left, right in zip(ranges, ranges[1:]))


def snapshot(root: Path) -> dict:
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise RuntimeError(f"Snapshot root is not a directory: {root}")
    entries, identities, sidecars = {}, {}, []
    paths = [root]
    for directory, directories, files in os.walk(root, followlinks=False):
        paths.extend(Path(directory) / name for name in sorted(directories + files))
    for path in paths:
        relative = path.relative_to(root).as_posix()
        info = path.lstat()
        if apple_double(path):
            sidecars.append(relative)
            continue

        kind = "symlink" if stat.S_ISLNK(info.st_mode) else (
            "directory" if stat.S_ISDIR(info.st_mode) else "file"
        )
        entry = {"kind": kind, "mtime_ns": info.st_mtime_ns}
        if kind == "symlink":
            entry["link_target"] = os.readlink(path)
            entries[relative] = entry
            continue

        entry.update({
            "mode": stat.S_IMODE(info.st_mode),
            "flags": getattr(info, "st_flags", 0),
            "acl": acl(path),
            "xattrs": {
                name: digest(xattr_get(path, name))
                for name in sorted(xattr_names(path))
            },
        })
        if kind == "file":
            entry.update({"sha256": digest(path.read_bytes()), "size": info.st_size,
                          "sparse": is_sparse(info)})
            identities.setdefault((info.st_dev, info.st_ino), []).append(relative)
        entries[relative] = entry

    return {"entries": entries, "sidecars": sorted(sidecars), "hard_links": sorted(
        sorted(paths) for paths in identities.values() if len(paths) > 1
    )}


def case_snapshot(path: Path) -> dict:
    if stat.S_ISDIR(path.lstat().st_mode):
        return snapshot(path)
    captured = snapshot(path.parent)
    names = {path.name, f"._{path.name}"}
    captured["entries"] = {name: entry for name, entry in captured["entries"].items() if name in names}
    captured["hard_links"] = []
    captured["sidecars"] = [name for name in captured["sidecars"] if name in names]
    return captured


def name_collisions(paths: list[str], capabilities: dict) -> list[list[str]]:
    groups = {}
    for path in paths:
        key = unicodedata.normalize("NFC", path)
        if observed(capabilities, "case_sensitive") is False:
            key = key.casefold()
        groups.setdefault(key, []).append(path)
    return sorted(sorted(group) for group in groups.values() if len(group) > 1)


def differences(before: dict, after: dict, capabilities: dict) -> list[dict]:
    changes = []
    for path in sorted(before["entries"].keys() | after["entries"].keys()):
        original, stored = before["entries"].get(path), after["entries"].get(path)
        if original is None or stored is None:
            changes.append({"path": path, "field": "presence", "predicted": False})
            continue
        if original["kind"] != stored["kind"]:
            changes.append({"path": path, "field": "kind", "before": original["kind"],
                            "after": stored["kind"], "predicted": False})
            continue
        for field in sorted(original.keys() | stored.keys()):
            left, right = original.get(field), stored.get(field)
            if left == right:
                continue
            if field == "xattrs":
                for name in sorted(left.keys() | right.keys()):
                    if left.get(name) == right.get(name):
                        continue
                    changes.append({"path": path, "field": f"xattr:{name}",
                                    "predicted": name not in right and
                                    unsupported(capabilities, f"xattr:{name}")})
                continue

            predicted = unsupported(capabilities, {
                "mode": "permissions", "sparse": "sparse", "acl": "acl", "flags": "flags",
            }.get(field, "unmodeled"))
            if field == "mtime_ns":
                tolerance = mtime_tolerance(capabilities)
                predicted = tolerance is not None and abs(left - right) < tolerance
            changes.append({"path": path, "field": field, "before": left, "after": right,
                            "predicted": predicted})
    if before["hard_links"] != after["hard_links"]:
        changes.append({"path": ".", "field": "hard_links", "predicted":
                        unsupported(capabilities, "hard_links")})
    return changes


def create_corpus(root: Path) -> list[dict]:
    cases = []

    def add(name, path, requirements=()):
        cases.append({"name": name, "path": path, "requirements": requirements})

    payload = root / "payload.bin"
    payload.write_bytes(bytes(range(256)) * 16)
    payload.chmod(0o640)
    add("content-and-mode", payload.name)

    metadata = root / "metadata.bin"
    metadata.write_bytes(b"metadata data fork")
    for name, value in attribute_values().items():
        xattr_set(metadata, name, value)
    command("/bin/chmod", "+a", ACL_RULE, metadata)
    os.chflags(metadata, stat.UF_HIDDEN)
    add("tags-fork-finderinfo-acl-flags", metadata.name)

    (root / "link").symlink_to(payload.name)
    add("relative-symlink", "link", ("symlinks",))
    hard_links = root / "hard-links"
    hard_links.mkdir()
    (hard_links / "first").write_bytes(b"one inode, two names")
    os.link(hard_links / "first", hard_links / "second")
    add("hard-link-group", hard_links.name, ("hard_links",))

    sparse_file(root / "sparse.bin")
    add("sparse-file", "sparse.bin")
    (root / "colon:name").write_bytes(b"colon in a POSIX name")
    add("colon-name", "colon:name", ("colon_names",))

    for form in ("NFC", "NFD"):
        directory = root / form
        directory.mkdir()
        (directory / unicodedata.normalize(form, "résumé.txt")).write_bytes(form.encode())
        add(f"unicode-{form}", form)

    case_pair = root / "case-pair"
    case_pair.mkdir()
    (case_pair / "Name.txt").write_bytes(b"upper")
    (case_pair / "name.txt").write_bytes(b"lower")
    if (case_pair / "Name.txt").read_bytes() != b"upper":
        raise RuntimeError("The source image must be case-sensitive")
    add("case-collision", case_pair.name)

    bundle = root / "Example.bundle"
    (bundle / "Contents").mkdir(parents=True)
    (bundle / "Contents" / "Info.plist").write_bytes(plistlib.dumps({
        "CFBundleIdentifier": "ch.lkmc.foldover.spike.s4.fixture",
        "CFBundlePackageType": "BNDL",
    }))
    xattr_set(bundle, FINDER_INFO, attribute_values()[FINDER_INFO])
    add("bundle-tree", bundle.name)

    # Fractional timestamps expose coarse targets without relying on clock speed.
    for path in sorted(root.rglob("*"), key=lambda path: len(path.parts), reverse=True):
        os.utime(path, ns=(CORPUS_TIME_NS, CORPUS_TIME_NS), follow_symlinks=False)
    return cases


@contextlib.contextmanager
def disk_image(workspace: Path, label: str, filesystem: str):
    image, mount = workspace / f"{label}.dmg", workspace / label
    # hdiutil accepts APFS, not diskutil's APFSX personality; newfs_apfs -e selects case sensitivity.
    formatting = ["-fs", "APFS", "-fsargs", "-e"] if filesystem == "APFSX" else ["-fs", filesystem]
    # macOS's exFAT formatter limits volume labels to 11 UTF-16 units.
    command("hdiutil", "create", "-size", "256m", *formatting,
            "-volname", f"folds4{label}", image)
    mount.mkdir()
    command("hdiutil", "attach", "-noverify", "-mountpoint", mount, image)
    try:
        yield mount
    finally:
        detach_image(mount)


def detach_image(mount: Path) -> None:
    try:
        command("hdiutil", "detach", mount)
    except RuntimeError:
        # Finder/indexers can keep an owned image busy after all corpus handles close.
        command("hdiutil", "detach", "-force", mount)


@contextlib.contextmanager
def corpus_directory(parent: Path, prefix: str, owner: CorpusOwner, cleanup_errors: list):
    directory = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    try:
        yield directory
    finally:
        try:
            if sys.platform == "darwin":
                command("/bin/rm", "-rf", "--", directory)
            else:
                shutil.rmtree(directory)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            remaining, inventory_errors = [], []
            for base, directories, files in os.walk(
                directory, followlinks=False, onerror=lambda error: inventory_errors.append(str(error))
            ):
                remaining.extend(
                    os.fsencode((Path(base) / name).relative_to(directory)).hex()
                    for name in directories + files
                )
            cleanup_errors.append({
                "error": str(error), "remaining_name_bytes_hex": remaining,
                "inventory_errors": inventory_errors,
                "disposition": "deferred-to-owned-image-detach" if owner == CorpusOwner.OWNED_IMAGE
                else "cleanup-failed",
            })
            if owner != CorpusOwner.OWNED_IMAGE:
                raise
            # The failure remains measurement data. Detach and image deletion still must succeed.


def measure_target(source: Path, cases: list[dict], target: Path, workspace: Path, owner: CorpusOwner) -> dict:
    probe_cleanup_errors = []
    capabilities = capabilities_at(target, owner, probe_cleanup_errors)
    results = []
    for case in cases:
        origin = source / case["path"]
        source_tree = case_snapshot(origin)

        collisions = name_collisions(list(source_tree["entries"]), capabilities)
        blocked = [name for name in case["requirements"] if unsupported(capabilities, name)]
        if blocked or collisions:
            results.append({"name": case["name"], "state": "blocked-before-copy",
                            "unsupported": blocked, "collisions": collisions})
            continue

        result = {"name": case["name"], "state": "measured", "cleanup_errors": []}
        with corpus_directory(target, "foldover-s4-copy-", owner, result["cleanup_errors"]) as out, \
                corpus_directory(workspace, "foldover-s4-return-", CorpusOwner.OWNED_IMAGE, result["cleanup_errors"]) as back:
            destination, returned = out / case["path"], back / case["path"]
            try:
                command("/usr/bin/ditto", "--rsrc", "--extattr", "--acl", origin, destination)
                outward = case_snapshot(destination)
                command("/usr/bin/ditto", "--rsrc", "--extattr", "--acl", destination, returned)
                round_trip = case_snapshot(returned)
                result.update({
                    "outward_changes": differences(source_tree, outward, capabilities),
                    "round_trip_changes": differences(source_tree, round_trip, capabilities),
                    "outward_sidecars": outward["sidecars"],
                    "returned_sidecars": round_trip["sidecars"],
                })
                result["no_unpredicted_loss"] = not any(
                    not change["predicted"] for change in
                    result["outward_changes"] + result["round_trip_changes"]
                )
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                result.update({"state": "copy-failed", "error": str(error),
                               "no_unpredicted_loss": False})
        if result["cleanup_errors"]:
            result["no_unpredicted_loss"] = False
        results.append(result)

    return {"capabilities": capabilities, "cases": results, "probe_cleanup_errors": probe_cleanup_errors,
            "no_unpredicted_loss": not probe_cleanup_errors and all(
        case.get("no_unpredicted_loss", case["state"] == "blocked-before-copy") for case in results
    )}


@contextlib.contextmanager
def measurement_report(output: Path, report: dict):
    report["harness_completed"] = False
    report["harness_error"] = None
    try:
        yield
    except Exception as error:
        report["harness_error"] = f"{type(error).__name__}: {error}"
        raise
    else:
        report["harness_completed"] = True
    finally:
        targets = report["targets"]
        report["unavailable_targets"] = [name for name in REQUIRED_TARGETS if name not in targets]
        report["tested_targets_passed"] = report["harness_completed"] and bool(targets) and all(
            result["no_unpredicted_loss"] for result in targets.values()
        )
        report["full_s4_passed"] = False
        output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--mounted-target", action="append", default=[], metavar="LABEL=PATH",
                        help="Probe an existing mount using only fresh temporary directories")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("The measured corpus needs macOS; portable logic tests run on Linux")

    mounted = {}
    for value in args.mounted_target:
        label, separator, path = value.partition("=")
        if not separator or not label or label in mounted or label in {"apfs", "exfat", "sftp"}:
            parser.error("Each mounted target needs a unique LABEL=PATH")
        target = Path(path).resolve()
        if not target.is_dir() or not os.path.ismount(target):
            parser.error(f"{target} is not an existing mount point")
        mounted[label] = target

    report = {"system": platform.platform(), "machine": platform.machine(),
              "copier": "macOS ditto --rsrc --extattr --acl", "targets": {},
              "limitations": ["Disk images are not physical disks or NAS devices",
                              "SFTP extension negotiation and hard links need a separate transport probe",
                              "Rename-over success does not prove crash atomicity"]}
    with measurement_report(args.output, report), \
            tempfile.TemporaryDirectory(prefix="foldover-s4-") as workspace:
        workspace = Path(workspace)
        with disk_image(workspace, "source", "APFSX") as source_volume:
            source = source_volume / "corpus"
            source.mkdir()
            cases = create_corpus(source)
            report["source"] = snapshot(source)
            for label, filesystem in [("apfs", "APFS"), ("exfat", "ExFAT")]:
                with disk_image(workspace, label, filesystem) as target:
                    report["targets"][label] = measure_target(
                        source, cases, target, source_volume, CorpusOwner.OWNED_IMAGE
                    )
            for label, target in mounted.items():
                report["targets"][label] = measure_target(
                    source, cases, target, source_volume, CorpusOwner.EXISTING_MOUNT
                )

    print(json.dumps({name: report[name] for name in
                      ("tested_targets_passed", "full_s4_passed", "unavailable_targets")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
