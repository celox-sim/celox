#!/usr/bin/env python3
"""Verify exact-byte RV32I evidence; optionally extract to a NEW external directory.

This is an integrity verifier, not a theorem checker. Saved report JSON does not
mint process-local proof handles or authorize composition. A trusted checkout of
this script and the manifest is required: hashes do not authenticate themselves.
Only the Python standard library is needed. No network or tool execution occurs.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tarfile

HEX = re.compile(r"[0-9a-f]{64}\Z")
BAD_NAME = re.compile(
    r"(^|/)(target|node_modules|__pycache__|\.cache|\.git|\.aws|\.ssh|\.env)(/|$)"
    r"|\.(exe|dll|so|a|o|pyc|pyo|pem|p12|key)$|credentials|id_rsa|id_ed25519", re.I
)
SECRET = re.compile(
    rb"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----"
    rb"|(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}|AKIA[0-9A-Z]{16}"
)
MAGIC = (b"\x7fELF", b"MZ", b"\xca\xfe\xba\xbe", b"\xcf\xfa\xed\xfe",
         b"\xfe\xed\xfa\xcf", b"!<arch>\n")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    with regular_open(path) as stream:
        h = hashlib.sha256()
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(data)
    return h.hexdigest()


def regular_open(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError(f"Not a regular file: {path}")
    return os.fdopen(fd, "rb")


def pairs_unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_json(data):
    return json.loads(data, object_pairs_hook=pairs_unique)


def safe_path(name):
    require(isinstance(name, str) and name, "Empty/non-string path")
    require(not name.startswith("/") and "\\" not in name and ":" not in name,
            f"Unsafe path: {name!r}")
    require(all(part not in ("", ".", "..") for part in name.split("/")),
            f"Traversal or noncanonical path: {name!r}")
    require(all(ord(c) >= 32 and ord(c) != 127 for c in name), "Control byte in path")
    return name


def local_name(name):
    safe_path(name)
    require("/" not in name, f"Metadata filename must be a basename: {name}")
    return name


def screen(name, data):
    require(not BAD_NAME.search(name), f"Forbidden filename: {name}")
    require(not data.startswith(MAGIC) and b"\0" not in data,
            f"Binary payload: {name}")
    data.decode("utf-8")
    require(not SECRET.search(data), f"Credential signature: {name}")


def artifact_maps(value, document, pointer=()):
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "artifacts":
                require(isinstance(child, dict), f"Non-map artifacts in {document}")
                require(all(isinstance(k, str) and isinstance(v, str) and HEX.fullmatch(v)
                            for k, v in child.items()), f"Invalid artifact digest in {document}")
                yield {"document": document, "json_path": list(pointer + (key,)),
                       "entries": len(child)}, child
            yield from artifact_maps(child, document, pointer + (key,))
    elif isinstance(value, list):
        for i, child in enumerate(value):
            yield from artifact_maps(child, document, pointer + (i,))


def load_bundle(evidence):
    with regular_open(evidence / "manifest.json") as f:
        manifest = parse_json(f.read())
    require(manifest["schema"] == "rv32i-evidence-bundle-v1", "Unsupported manifest schema")
    require(manifest.get("saved_reports_create_proof_handles") is False,
            "Manifest must preserve the proof-handle trust boundary")
    archives = manifest["archives"]
    require(len({x["id"] for x in archives}) == len(archives), "Duplicate archive ID")
    require(len({x["file"] for x in archives}) == len(archives), "Duplicate archive file")
    relocations = {x["archive_id"]: x for x in manifest["relocation_map"]}
    require(len(relocations) == len(manifest["relocation_map"]), "Duplicate relocation ID")
    require(set(relocations) == {x["id"] for x in archives}, "Incomplete relocation map")
    inventory = {}
    per_archive = {}
    for archive in archives:
        local_name(archive["file"])
        local_name(archive["member_manifest"])
        require(HEX.fullmatch(archive["sha256"]) and HEX.fullmatch(archive["member_manifest_sha256"]),
                "Invalid archive or manifest SHA256")
        with regular_open(evidence / archive["member_manifest"]) as stream:
            data = stream.read()
        require(digest(data) == archive["member_manifest_sha256"], "Member manifest hash mismatch")
        members = parse_json(data)
        require(members["schema"] == "rv32i-evidence-members-v1", "Unsupported members schema")
        require(len(members["members"]) == archive["member_count"], "Declared member count mismatch")
        relocation = relocations[archive["id"]]
        prefix = safe_path(relocation["archive_prefix"])
        require(Path(relocation["original_root"]).is_absolute(), "Original root must be absolute")
        require(relocation["scope"] in ("complete-tree", "selected-files"), "Unknown source scope")
        expected = {}
        for member in members["members"]:
            name = safe_path(member["path"])
            require(name.startswith(prefix + "/"), f"Member outside relocation root: {name}")
            require(name not in inventory, f"Duplicate member: {name}")
            require(type(member["size"]) is int and member["size"] >= 0, "Invalid member size")
            require(isinstance(member["sha256"], str) and HEX.fullmatch(member["sha256"]), "Invalid member hash")
            inventory[name] = expected[name] = member
        require(sum(x["size"] for x in expected.values()) == archive["uncompressed_bytes"],
                "Uncompressed byte total mismatch")
        if relocation["scope"] == "selected-files":
            selected = [safe_path(x) for x in relocation["selected_files"]]
            require(set(expected) == {prefix + "/" + x for x in selected}, "Selected source set mismatch")
        per_archive[archive["id"]] = expected
    require(len(inventory) == manifest["total_member_count"], "Bundle member total mismatch")
    require(sum(x["size"] for x in inventory.values()) == manifest["total_uncompressed_bytes"],
            "Bundle byte total mismatch")
    return manifest, inventory, per_archive, relocations


def walk_archive(path, archive, expected):
    """Only regular entries; never call tarfile.extract/all on untrusted headers."""
    with regular_open(path) as stream:
        require(os.fstat(stream.fileno()).st_size == archive["size"], f"Archive size mismatch: {path.name}")
        h = hashlib.sha256()
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(data)
        require(h.hexdigest() == archive["sha256"], f"Archive SHA256 mismatch: {path.name}")
        stream.seek(0)
        seen = set()
        with tarfile.open(fileobj=stream, mode="r|xz") as tar:
            for info in tar:
                name = safe_path(info.name)
                require(info.isreg() and not info.issparse(), f"Nonregular/sparse archive entry: {name}")
                require(name in expected and name not in seen, f"Unexpected/duplicate member: {name}")
                require(info.size == expected[name]["size"], f"Member size mismatch: {name}")
                require(info.uid == info.gid == info.mtime == 0 and info.mode == 0o644
                        and info.uname == info.gname == "", f"Noncanonical metadata: {name}")
                require(not info.linkname, f"Link target on regular member: {name}")
                require(set(info.pax_headers) <= {"path"}, f"Unexpected extended metadata: {name}")
                with tar.extractfile(info) as item:
                    data = item.read(info.size + 1)
                require(len(data) == info.size and digest(data) == expected[name]["sha256"],
                        f"Member hash mismatch: {name}")
                screen(name, data)
                seen.add(name)
                yield name, data
        require(seen == set(expected), f"Missing members in {path.name}")


def source_inventory(root):
    require(root.is_dir() and not root.is_symlink(), f"Invalid original source root: {root}")
    found = set()
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in dirs:
            p = Path(directory) / name
            require(not p.is_symlink(), f"Symlink in original source: {p}")
        for name in files:
            p = Path(directory) / name
            require(stat.S_ISREG(p.lstat().st_mode), f"Nonregular source: {p}")
            found.add(p.relative_to(root).as_posix())
    return found


def original_checks(per_archive, relocations):
    checked = 0
    for ident, expected in per_archive.items():
        mapping = relocations[ident]
        root = Path(mapping["original_root"])
        prefix = mapping["archive_prefix"] + "/"
        wanted = {name[len(prefix):] for name in expected}
        actual = source_inventory(root) if mapping["scope"] == "complete-tree" else set(mapping["selected_files"])
        require(actual == wanted, f"Original file set differs: {ident}")
        for name, member in expected.items():
            p = root / name[len(prefix):]
            require(p.lstat().st_size == member["size"] and file_digest(p) == member["sha256"],
                    f"Original member mismatch: {p}")
            checked += 1
    return checked


def mapped_artifact_path(document, recorded, relocations):
    if recorded.startswith("/"):
        for mapping in sorted(relocations.values(), key=lambda x: len(x["original_root"]), reverse=True):
            prefix = mapping["original_root"].rstrip("/") + "/"
            if recorded.startswith(prefix):
                return safe_path(mapping["archive_prefix"] + "/" + recorded[len(prefix):])
        raise ValueError(f"Artifact path has no relocation: {recorded}")
    return safe_path(str(PurePosixPath(document).parent / safe_path(recorded)))


def extraction_destination(raw, repo):
    dest = Path(os.path.abspath(raw))
    require(not dest.exists() and not dest.is_symlink(), "Extraction destination must not exist")
    require(dest.parent.is_dir(), "Extraction parent directory must already exist")
    for ancestor in (dest.parent, *dest.parent.parents):
        require(not ancestor.is_symlink(), f"Symlink extraction parent: {ancestor}")
    require(dest != repo and repo not in dest.parents, "Extract outside the repository")
    return dest


def write_safe(root_fd, name, data):
    parts = safe_path(name).split("/")
    fd = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            try:
                os.mkdir(part, mode=0o700, dir_fd=fd)
            except FileExistsError:
                pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        output = os.open(parts[-1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o644, dir_fd=fd)
        with os.fdopen(output, "wb") as stream:
            stream.write(data)
    finally:
        os.close(fd)


def verify_bundle(evidence, extract=None, check_originals=False):
    manifest, inventory, per_archive, relocations = load_bundle(evidence)
    dest = extraction_destination(extract, Path(__file__).resolve().parents[2]) if extract else None
    discovered = []
    map_results = []
    for archive in manifest["archives"]:
        for name, data in walk_archive(evidence / archive["file"], archive, per_archive[archive["id"]]):
            if name.endswith(".json"):
                for descriptor, hashes in artifact_maps(parse_json(data), name):
                    discovered.append(descriptor)
                    for recorded, expected_hash in hashes.items():
                        member = mapped_artifact_path(name, recorded, relocations)
                        require(member in inventory and inventory[member]["sha256"] == expected_hash,
                                f"Recorded artifact mismatch: {name}: {recorded}")
                    map_results.append({**descriptor, "verified": True})
    require(sorted(discovered, key=lambda x: x["document"]) == manifest["recorded_artifact_maps"],
            "Recorded artifact-map inventory differs")
    originals = original_checks(per_archive, relocations) if check_originals else None
    # Extraction starts only after every archive, member, and recorded artifact map passes.
    # A second verified pass prevents archives changed between verification and extraction
    # from being silently trusted. A failed second pass may leave a partial NEW directory.
    if dest:
        dest.mkdir(mode=0o700)
        fd = os.open(dest, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            for archive in manifest["archives"]:
                for name, data in walk_archive(evidence / archive["file"], archive, per_archive[archive["id"]]):
                    write_safe(fd, name, data)
        finally:
            os.close(fd)
    return {"status": "verified", "archives_verified": len(manifest["archives"]),
            "members_verified": len(inventory), "uncompressed_bytes_verified": manifest["total_uncompressed_bytes"],
            "archive_bytes_verified": sum(x["size"] for x in manifest["archives"]),
            "recorded_artifact_maps": sorted(map_results, key=lambda x: x["document"]),
            "recorded_artifact_references_verified": sum(x["entries"] for x in map_results),
            "original_files_verified": originals, "extracted_to": str(dest) if dest else None,
            "saved_reports_create_proof_handles": False,
            "scope": "Byte integrity and recorded artifact hash maps; no proof rerun or composition authorization"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extract", metavar="NEW_EXTERNAL_DIRECTORY", help="After verification, safely extract original relative run-directory names")
    parser.add_argument("--check-originals", action="store_true", help="Also compare every byte and full-tree file set at historical original roots (only available on the original machine)")
    args = parser.parse_args()
    try:
        result = verify_bundle(Path(__file__).resolve().parent / "evidence", args.extract, args.check_originals)
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError, EOFError) as error:
        print(f"Evidence verification FAILED: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
