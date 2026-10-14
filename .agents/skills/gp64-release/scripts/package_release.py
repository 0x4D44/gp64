#!/usr/bin/env python3
"""Build deterministic local gp64 release archives and licence bundles."""

import argparse
import gzip
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit
import zipfile
import zlib
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath


class PackagingError(Exception):
    """An input or release invariant prevented packaging."""


@dataclass(frozen=True)
class HostTarget:
    platform_name: str
    target: str
    binary_name: str
    archive_suffix: str


@dataclass(frozen=True)
class Checkout:
    root: Path
    tag: str
    revision: str
    version: str
    commit_time: int
    metadata: dict


SUPPORTED_HOSTS = {
    "windows-x86_64": HostTarget(
        platform_name="windows-x86_64",
        target="x86_64-pc-windows-msvc",
        binary_name="gp64.exe",
        archive_suffix="windows-x86_64",
    ),
    "macos-arm64": HostTarget(
        platform_name="macos-arm64",
        target="aarch64-apple-darwin",
        binary_name="gp64",
        archive_suffix="macos-arm64",
    ),
}

TAG_RE = re.compile(
    r"^v(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$"
)
REVISION_RE = re.compile(r"^(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})$")
PACKAGE_NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.+-]*$")
PUBLIC_SCP_ORIGIN_RE = re.compile(r"^git@github\.com:0x4D44/gp64\.git$", re.IGNORECASE)
LEGAL_NAME_MARKERS = (
    "license",
    "licence",
    "notice",
    "copyright",
    "copying",
    "authors",
    "credits",
    "patents",
)


def detect_host(system=None, machine=None):
    system = sys.platform if system is None else system
    machine = platform.machine() if machine is None else machine
    normalized_machine = machine.casefold().replace(" ", "")
    if system == "win32" and normalized_machine in ("amd64", "x86_64"):
        return SUPPORTED_HOSTS["windows-x86_64"]
    if system == "darwin" and normalized_machine in ("arm64", "aarch64"):
        return SUPPORTED_HOSTS["macos-arm64"]
    raise PackagingError("supported packaging hosts are Windows x86-64 and macOS Apple Silicon")


def verify_binary_architecture(binary_path, platform_name):
    path = Path(binary_path)
    if path.is_symlink() or not path.is_file():
        raise PackagingError("prebuilt binary must be a regular file, not a symlink")
    try:
        data = path.read_bytes()
    except OSError as exc:
        raise PackagingError("prebuilt binary could not be read") from exc
    verify_binary_bytes(data, platform_name)


def verify_binary_bytes(data, platform_name):
    if not data:
        raise PackagingError("prebuilt binary is empty")

    if platform_name == "windows-x86_64":
        if len(data) < 70 or data[:2] != b"MZ":
            raise PackagingError("prebuilt Windows binary is not a PE executable")
        pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
        if pe_offset > len(data) - 6 or data[pe_offset : pe_offset + 4] != b"PE\0\0":
            raise PackagingError("prebuilt Windows binary has an invalid PE header")
        machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
        if machine != 0x8664:
            raise PackagingError("prebuilt Windows binary is not x86-64")
        return

    if platform_name == "macos-arm64":
        if len(data) < 8 or data[:4] not in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            raise PackagingError("prebuilt macOS binary is not a thin 64-bit Mach-O executable")
        endian = "<" if data[:4] == b"\xcf\xfa\xed\xfe" else ">"
        cputype = struct.unpack_from(endian + "I", data, 4)[0]
        if cputype != 0x0100000C:
            raise PackagingError("prebuilt macOS binary is not Apple Silicon arm64")
        return

    raise PackagingError("unsupported binary platform")


def _run(args, cwd, description, input_data=None):
    try:
        result = subprocess.run(
            args,
            cwd=str(cwd) if cwd is not None else None,
            input=input_data,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
    except FileNotFoundError as exc:
        raise PackagingError("required tool is unavailable: " + str(args[0])) from exc
    except subprocess.CalledProcessError as exc:
        raise PackagingError("{} failed (exit {})".format(description, exc.returncode)) from exc
    return result.stdout


def _text_command(args, cwd, description):
    output = _run(args, cwd, description)
    try:
        return output.decode("utf-8").strip()
    except UnicodeDecodeError as exc:
        raise PackagingError("{} returned non-text output".format(description)) from exc


def _is_inside(path, parent):
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def validate_output_directory(output_dir, repo_root):
    output = Path(output_dir).expanduser().resolve()
    repo = Path(repo_root).resolve()
    if output == repo or _is_inside(output, repo):
        raise PackagingError("release output directory must be outside the source checkout")
    if output.exists() and not output.is_dir():
        raise PackagingError("release output path is not a directory")
    return output


def safe_relative_path(value, label="archive path"):
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise PackagingError("invalid {}".format(label))
    if ":" in value:
        raise PackagingError("invalid {}".format(label))
    raw_parts = value.split("/")
    if any(part in ("", ".", "..") for part in raw_parts):
        raise PackagingError("unsafe {}".format(label))
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise PackagingError("unsafe {}".format(label))
    return path


def ensure_outputs_available(output_dir, names):
    output = Path(output_dir)
    for name in names:
        relative = safe_relative_path(name, "output filename")
        candidate = output.joinpath(*relative.parts)
        if candidate.exists() or candidate.is_symlink():
            raise PackagingError("refusing to overwrite an existing release output")


def _git(root, args, description):
    return _text_command(["git"] + args, root, description)


def _workspace_version(root, metadata):
    workspace_ids = set(metadata.get("workspace_members", []))
    candidates = []
    expected_root_manifest = (root / "Cargo.toml").resolve()
    for package in metadata.get("packages", []):
        if package.get("name") != "gp64" or package.get("id") not in workspace_ids:
            continue
        manifest = Path(package.get("manifest_path", "")).resolve()
        candidates.append((manifest, package))
    if not candidates:
        raise PackagingError("cargo metadata did not identify a workspace package named gp64")
    candidates.sort(key=lambda item: (item[0] != expected_root_manifest, str(item[0])))
    version = candidates[0][1].get("version")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise PackagingError("cargo metadata returned an invalid gp64 workspace version")
    return version


def _is_official_public_origin(value):
    if not isinstance(value, str) or not value or any(ord(char) < 32 for char in value):
        return False
    if PUBLIC_SCP_ORIGIN_RE.fullmatch(value):
        return True
    try:
        parts = urlsplit(value)
        if parts.hostname is None or parts.hostname.casefold() != "github.com":
            return False
        if parts.path != "/0x4D44/gp64.git" or parts.query or parts.fragment:
            return False
        if parts.scheme == "https":
            return parts.username is None and parts.password is None and parts.port is None
        if parts.scheme == "ssh":
            return (
                parts.username == "git"
                and parts.password is None
                and parts.port is None
            )
    except ValueError:
        return False
    return False


def validate_checkout(repo_root, tag, revision):
    root = Path(repo_root).resolve(strict=True)
    if not root.is_dir():
        raise PackagingError("source checkout is not a directory")
    try:
        origin = _text_command(["git", "remote", "get-url", "origin"], root, "git origin lookup")
    except PackagingError as exc:
        raise PackagingError("source checkout origin must be the official public repository") from exc
    if not _is_official_public_origin(origin):
        raise PackagingError("source checkout origin must be the official public repository")
    if not REVISION_RE.fullmatch(revision):
        raise PackagingError("revision must be a full 40- or 64-character commit ID")
    tag_match = TAG_RE.fullmatch(tag)
    if not tag_match:
        raise PackagingError("release tag must use v<workspace-version> form")

    dirty = _git(root, ["status", "--porcelain=v1", "--untracked-files=all"], "git status")
    if dirty:
        raise PackagingError("source checkout is not clean")

    head = _git(root, ["rev-parse", "HEAD"], "git HEAD lookup").lower()
    provided_revision = revision.lower()
    if head != provided_revision:
        raise PackagingError("source checkout HEAD does not match the supplied revision")
    try:
        tag_revision = _git(root, ["rev-parse", "--verify", "refs/tags/{}^{{commit}}".format(tag)], "git tag lookup").lower()
    except PackagingError as exc:
        raise PackagingError("release tag does not resolve to a commit") from exc
    if tag_revision != provided_revision:
        raise PackagingError("release tag does not point to the supplied revision")

    try:
        no_deps = json.loads(
            _run(
                ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
                root,
                "cargo metadata --no-deps",
            )
        )
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise PackagingError("cargo metadata --no-deps returned invalid JSON") from exc
    version = _workspace_version(root, no_deps)
    if tag_match.group("version") != version:
        raise PackagingError("release tag version does not match gp64 workspace version")

    commit_time_text = _git(root, ["show", "-s", "--format=%ct", provided_revision], "git commit time lookup")
    try:
        commit_time = int(commit_time_text)
    except ValueError as exc:
        raise PackagingError("git returned an invalid source commit time") from exc
    return Checkout(root, tag, provided_revision, version, commit_time, no_deps)


def load_full_metadata(root):
    try:
        metadata = json.loads(
            _run(
                ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
                root,
                "full cargo metadata",
            )
        )
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise PackagingError("full cargo metadata returned invalid JSON") from exc
    if metadata.get("workspace_root") != str(Path(root).resolve()):
        raise PackagingError("cargo metadata resolved a different workspace")
    packages = metadata.get("packages")
    if not isinstance(packages, list) or not packages:
        raise PackagingError("full cargo metadata did not include locked packages")
    return metadata


def _package_key(package):
    name = package.get("name")
    version = package.get("version")
    if not isinstance(name, str) or not PACKAGE_NAME_RE.fullmatch(name):
        raise PackagingError("cargo metadata contains an unsafe package name")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise PackagingError("cargo metadata contains an unsafe package version")
    return name, version


def _package_root(package):
    manifest_value = package.get("manifest_path")
    if not isinstance(manifest_value, str):
        raise PackagingError("cargo metadata package is missing its manifest path")
    manifest = Path(manifest_value).resolve(strict=True)
    if not manifest.is_file():
        raise PackagingError("cargo metadata package manifest is unavailable")
    return manifest.parent


def _confined_file(path, package_root):
    candidate = Path(path)
    resolved = candidate.resolve(strict=True)
    root = Path(package_root).resolve(strict=True)
    if not _is_inside(resolved, root) or not resolved.is_file():
        raise PackagingError("dependency legal text resolves outside its crate")
    relative = resolved.relative_to(root).as_posix()
    safe_relative_path(relative, "dependency legal text path")
    return resolved, relative


def _license_file_candidates(package, package_root, excluded_roots=()):
    candidates = {}
    configured = package.get("license_file")
    if configured:
        configured_path = Path(configured)
        if not configured_path.is_absolute():
            configured_path = package_root / configured_path
        resolved, relative = _confined_file(configured_path, package_root)
        if resolved.is_symlink():
            raise PackagingError("dependency license_file must be a regular file")
        candidates[relative] = resolved

    excluded = {Path(path).resolve() for path in excluded_roots}
    ignored_directories = {".agents", ".git", ".scratch", "target"}
    def walk_error(_error):
        raise PackagingError("dependency crate contents could not be scanned for legal text")

    for current, directories, filenames in os.walk(package_root, followlinks=False, onerror=walk_error):
        current_path = Path(current)
        kept_directories = []
        for directory in sorted(directories):
            lowered_directory = directory.casefold()
            candidate_directory = current_path / directory
            if lowered_directory in ignored_directories or candidate_directory.is_symlink():
                continue
            if candidate_directory.resolve() in excluded:
                continue
            kept_directories.append(directory)
        directories[:] = kept_directories
        for filename in sorted(filenames):
            candidate = current_path / filename
            lowered = filename.casefold()
            if not any(marker in lowered for marker in LEGAL_NAME_MARKERS):
                continue
            resolved, relative = _confined_file(candidate, package_root)
            candidates[relative] = resolved
    return [(relative, candidates[relative]) for relative in sorted(candidates)]


def _fallback_file(package_name, package_version, entry, asset_root):
    files = entry.get("files")
    if not isinstance(files, list) or not files:
        raise PackagingError("verified licence fallback has no full text files")
    results = []
    for item in files:
        if not isinstance(item, dict):
            raise PackagingError("verified licence fallback has an invalid file entry")
        relative = safe_relative_path(item.get("path"), "fallback licence path")
        expected = item.get("sha256")
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
            raise PackagingError("verified licence fallback has an invalid SHA-256")
        source = Path(asset_root).joinpath(*relative.parts)
        if source.is_symlink() or not source.is_file():
            raise PackagingError("verified licence fallback text is missing")
        content = source.read_bytes()
        if hashlib.sha256(content).hexdigest() != expected:
            raise PackagingError("verified licence fallback SHA-256 does not match")
        source_url = _public_https_url(item.get("source_url"), "fallback source URL")
        upstream_path = safe_relative_path(item.get("upstream_path"), "upstream licence path").as_posix()
        archive_path = "dependency-notices/{}-{}/supplement/{}".format(
            package_name, package_version, relative.as_posix()
        )
        results.append(
            {
                "archive_path": safe_relative_path(archive_path).as_posix(),
                "bytes": content,
                "origin": "verified-fallback",
                "source_url": source_url,
                "upstream_path": upstream_path,
            }
        )
    return results


def load_fallback_manifest(asset_root):
    root = Path(asset_root).resolve()
    manifest_path = root / "manifest.json"
    if not manifest_path.is_file() or manifest_path.is_symlink():
        return {}
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PackagingError("dependency licence fallback manifest is invalid") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1 or not isinstance(manifest.get("packages"), list):
        raise PackagingError("dependency licence fallback manifest has an unsupported schema")
    entries = {}
    for entry in manifest["packages"]:
        if not isinstance(entry, dict):
            raise PackagingError("dependency licence fallback manifest has an invalid package entry")
        key = _package_key(entry)
        if key in entries:
            raise PackagingError("dependency licence fallback manifest contains a duplicate package")
        if not all(isinstance(entry.get(field), str) and entry.get(field) for field in ("repository", "commit")):
            raise PackagingError("dependency licence fallback manifest is missing source provenance")
        entry["repository"] = _public_https_url(entry["repository"], "fallback repository URL")
        if not re.fullmatch(r"[0-9a-fA-F]{40,64}", entry["commit"]):
            raise PackagingError("dependency licence fallback manifest has an invalid upstream commit")
        package_path = entry.get("path_in_vcs")
        entry["path_in_vcs"] = (
            safe_relative_path(package_path, "upstream licence path").as_posix()
            if isinstance(package_path, str) and package_path
            else None
        )
        entry["provenance_kind"] = entry.get("provenance_kind", "pinned-upstream-text")
        entry["selected_license"] = entry.get("selected_license", entry.get("license", "unspecified"))
        if not isinstance(entry["provenance_kind"], str) or not entry["provenance_kind"]:
            raise PackagingError("dependency licence fallback provenance kind is invalid")
        if not isinstance(entry["selected_license"], str) or not entry["selected_license"]:
            raise PackagingError("dependency licence fallback selected licence is invalid")
        if entry.get("documentation_url") is not None:
            entry["documentation_url"] = _public_https_url(
                entry["documentation_url"], "fallback documentation URL"
            )
        _fallback_file(key[0], key[1], entry, root)
        entries[key] = entry
    return entries


def _public_https_url(value, label):
    if not isinstance(value, str):
        raise PackagingError("{} is missing".format(label))
    parts = urlsplit(value)
    if parts.scheme != "https" or not parts.netloc or parts.username or parts.password or parts.query or parts.fragment:
        raise PackagingError("{} must be a credential-free public HTTPS URL".format(label))
    if any(ord(char) < 32 for char in value):
        raise PackagingError("{} contains control characters".format(label))
    return value


def collect_package_license_files(package, fallback_entries, asset_root, excluded_roots=()):
    name, version = _package_key(package)
    package_root = _package_root(package)
    candidates = _license_file_candidates(package, package_root, excluded_roots)
    if candidates:
        results = []
        for relative, path in candidates:
            archive_path = "dependency-notices/{}-{}/{}".format(name, version, relative)
            results.append(
                {
                    "archive_path": safe_relative_path(archive_path).as_posix(),
                    "bytes": path.read_bytes(),
                    "origin": "crate",
                    "source_url": None,
                    "upstream_path": relative,
                }
            )
        return results

    source = package.get("source")
    if source is None:
        return []
    fallback = fallback_entries.get((name, version))
    if fallback is not None:
        return _fallback_file(name, version, fallback, asset_root)
    raise PackagingError("missing actual licence/notice text for {}@{}".format(name, version))


def _source_kind(package):
    source = package.get("source")
    if source is None:
        return "workspace"
    if source.startswith("registry+"):
        return "registry"
    if source.startswith("git+"):
        return "git"
    return "external"


def collect_dependency_documents(metadata, asset_root):
    fallback_entries = load_fallback_manifest(asset_root)
    documents = []
    packages = []
    blocked = []
    workspace_roots = [
        _package_root(package)
        for package in metadata["packages"]
        if package.get("source") is None
    ]
    ordered = sorted(metadata["packages"], key=lambda package: (package.get("name", ""), package.get("version", "")))
    for package in ordered:
        name, version = _package_key(package)
        package_root = _package_root(package)
        excluded_roots = [
            other_root
            for other_root in workspace_roots
            if other_root != package_root and _is_inside(other_root, package_root)
        ]
        try:
            package_documents = collect_package_license_files(
                package, fallback_entries, asset_root, excluded_roots
            )
        except PackagingError as exc:
            if str(exc).startswith("missing actual licence/notice text"):
                blocked.append((name, version))
                continue
            raise
        listed_files = []
        for item in package_documents:
            digest = hashlib.sha256(item["bytes"]).hexdigest()
            documents.append(item)
            listed_files.append({"path": item["archive_path"], "sha256": digest})
        record = {
            "name": name,
            "version": version,
            "license": package.get("license"),
            "source": _source_kind(package),
            "files": listed_files,
        }
        fallback = fallback_entries.get((name, version))
        if fallback and any(item["origin"] == "verified-fallback" for item in package_documents):
            fallback_record = {
                "repository": fallback["repository"],
                "commit": fallback["commit"].lower(),
                "provenance_kind": fallback["provenance_kind"],
                "selected_license": fallback["selected_license"],
                "files": [
                    {
                        "path": item.get("upstream_path"),
                        "source_url": item.get("source_url"),
                        "sha256": hashlib.sha256(item["bytes"]).hexdigest(),
                    }
                    for item in package_documents
                ],
            }
            if fallback.get("path_in_vcs") is not None:
                fallback_record["path_in_vcs"] = fallback["path_in_vcs"]
            if fallback.get("documentation_url") is not None:
                fallback_record["documentation_url"] = fallback["documentation_url"]
            record["fallback"] = fallback_record
        packages.append(record)
    if blocked:
        names = ", ".join("{}@{}".format(name, version) for name, version in blocked)
        raise PackagingError("blocked crates have no actual licence/notice text: " + names)
    return documents, {"schema_version": 1, "packages": packages}


def _json_bytes(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(",", ":")) + "\n").encode("utf-8")


def _write_bytes(root, relative, content, mode=0o644):
    rel = safe_relative_path(relative)
    destination = Path(root).joinpath(*rel.parts)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise PackagingError("release staging path already exists")
    with destination.open("xb") as output:
        output.write(content)
    try:
        destination.chmod(mode)
    except OSError:
        pass
    return destination


def _copy_repo_file(repo, staging, relative):
    rel = safe_relative_path(relative, "repository legal document path")
    source = Path(repo).joinpath(*rel.parts)
    if source.is_symlink() or not source.is_file():
        raise PackagingError("required repository legal document is unavailable")
    return _write_bytes(staging, rel.as_posix(), source.read_bytes())


def _copy_public_documents(repo, staging):
    for relative in ("LICENSE", "NOTICE", "README.md", "THIRD_PARTY_NOTICES.md"):
        _copy_repo_file(repo, staging, relative)

    license_root = Path(repo) / "licenses"
    required_public_licenses = (
        "licenses/vga-0.2.9-LICENSE-MIT.txt",
        "licenses/vga-0.2.9-LICENSE-APACHE.txt",
    )
    if not license_root.is_dir():
        raise PackagingError("public licence tree is unavailable")
    for source in sorted(license_root.rglob("*")):
        if source.is_symlink():
            raise PackagingError("public licence tree contains a symlink")
        if source.is_dir():
            continue
        if not source.is_file():
            raise PackagingError("public licence tree contains a non-regular file")
        relative = source.relative_to(Path(repo)).as_posix()
        _copy_repo_file(repo, staging, relative)
    if any(not (Path(repo) / relative).is_file() for relative in required_public_licenses):
        raise PackagingError("required public font licence texts are unavailable")

    crates_root = Path(repo) / "crates"
    notices = sorted(crates_root.glob("*/NOTICE.md")) if crates_root.is_dir() else []
    if not notices:
        raise PackagingError("workspace crate notices are unavailable")
    for source in notices:
        if source.is_symlink() or not source.is_file():
            raise PackagingError("crate notice is not a regular file")
        _copy_repo_file(repo, staging, source.relative_to(Path(repo)).as_posix())
    host_dirs_license = "crates/gp64-types/HOST_DIRS_LICENSE.txt"
    if not (Path(repo) / host_dirs_license).is_file():
        raise PackagingError("gp64-types host-directory MIT text is unavailable")
    _copy_repo_file(repo, staging, host_dirs_license)


def _write_dependency_documents(staging, documents, manifest, prefix=""):
    for item in documents:
        relative = item["archive_path"]
        if prefix:
            relative = prefix.rstrip("/") + "/" + relative
        _write_bytes(staging, relative, item["bytes"])

    manifest_relative = "dependency-notices/MANIFEST.json"
    if prefix:
        manifest_relative = prefix.rstrip("/") + "/" + manifest_relative
        manifest = _manifest_with_prefix(manifest, prefix.rstrip("/") + "/")
    _write_bytes(staging, manifest_relative, _json_bytes(manifest))


def _manifest_with_prefix(manifest, prefix):
    copied = json.loads(json.dumps(manifest))
    for package in copied.get("packages", []):
        for item in package.get("files", []):
            item["path"] = prefix + item["path"]
    return copied


def zip_timestamp(timestamp):
    value = datetime.fromtimestamp(timestamp, timezone.utc)
    year = min(2107, max(1980, value.year))
    return (year, value.month, value.day, value.hour, value.minute, value.second - value.second % 2)


def write_deterministic_zip(source_dir, destination, archive_root, source_time):
    root = Path(source_dir)
    prefix = safe_relative_path(archive_root, "archive root").as_posix()
    timestamp = zip_timestamp(source_time)
    target = Path(destination)
    with zipfile.ZipFile(target, mode="x", compression=zipfile.ZIP_STORED, strict_timestamps=False) as archive:
        entries = sorted((path for path in root.rglob("*") if path.is_file()), key=lambda path: path.relative_to(root).as_posix())
        for path in entries:
            if path.is_symlink():
                raise PackagingError("binary package staging contains a symlink")
            relative = safe_relative_path(path.relative_to(root).as_posix()).as_posix()
            info = zipfile.ZipInfo(prefix + "/" + relative, date_time=timestamp)
            info.create_system = 3
            info.compress_type = zipfile.ZIP_STORED
            info.flag_bits |= 0x800
            info.external_attr = (stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)) << 16
            info.extra = b""
            info.comment = b""
            archive.writestr(info, path.read_bytes())


def _tree_manifest(directory):
    root = Path(directory)
    files = []
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        if path.is_dir() and not path.is_symlink():
            continue
        relative = safe_relative_path(path.relative_to(root).as_posix(), "vendor file path").as_posix()
        if path.is_symlink():
            target = os.readlink(path)
            _validate_symlink_target(relative, target)
            content = target.encode("utf-8")
            kind = "symlink"
        elif path.is_file():
            content = path.read_bytes()
            kind = "file"
        else:
            raise PackagingError("vendor tree contains an unsupported file type")
        files.append({"path": relative, "kind": kind, "sha256": hashlib.sha256(content).hexdigest()})
    payload = _json_bytes({"schema_version": 1, "files": files})
    return files, payload, hashlib.sha256(payload).hexdigest()


def _validate_symlink_target(member_path, target):
    if not target or "\\" in target or "\x00" in target or ":" in target:
        raise PackagingError("source archive has an unsafe symlink")
    target_path = PurePosixPath(target)
    if target_path.is_absolute():
        raise PackagingError("source archive has an absolute symlink")
    depth = len(PurePosixPath(member_path).parent.parts)
    for part in target_path.parts:
        if part in ("", "."):
            continue
        if part == "..":
            depth -= 1
            if depth < 0:
                raise PackagingError("source archive symlink escapes the source tree")
        else:
            depth += 1


def extract_git_archive(archive_file, destination):
    root = Path(destination)
    root.mkdir(parents=True, exist_ok=False)
    try:
        with tarfile.open(fileobj=archive_file, mode="r|") as archive:
            for member in archive:
                name = member.name.rstrip("/")
                if not name:
                    continue
                relative = safe_relative_path(name, "git archive member")
                output = root.joinpath(*relative.parts)
                parent = output.parent
                parent.mkdir(parents=True, exist_ok=True)
                cursor = root
                for part in relative.parts[:-1]:
                    cursor = cursor / part
                    if cursor.is_symlink() or (cursor.exists() and not cursor.is_dir()):
                        raise PackagingError("git archive member traverses a non-directory")
                if member.isdir():
                    output.mkdir(parents=True, exist_ok=True)
                    continue
                if member.isfile():
                    source = archive.extractfile(member)
                    if source is None:
                        raise PackagingError("git archive file could not be read")
                    with output.open("xb") as target:
                        shutil.copyfileobj(source, target)
                    output.chmod(member.mode & 0o777)
                    continue
                if member.issym():
                    _validate_symlink_target(relative.as_posix(), member.linkname)
                    if output.exists() or output.is_symlink():
                        raise PackagingError("git archive contains duplicate paths")
                    os.symlink(member.linkname, output)
                    continue
                raise PackagingError("git archive contains an unsupported member type")
    except (tarfile.TarError, OSError) as exc:
        if isinstance(exc, PackagingError):
            raise
        raise PackagingError("git archive could not be safely extracted") from exc


def write_deterministic_tar_gz(source_dir, destination, source_time, archive_root=None):
    root = Path(source_dir)
    target = Path(destination)
    prefix = safe_relative_path(archive_root, "archive root").as_posix() if archive_root else ""
    with target.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                entries = sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix())
                for path in entries:
                    relative = safe_relative_path(path.relative_to(root).as_posix(), "source archive path").as_posix()
                    name = prefix + "/" + relative if prefix else relative
                    info = tarfile.TarInfo(name)
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = source_time
                    info.pax_headers = {}
                    if path.is_symlink():
                        link_target = os.readlink(path)
                        _validate_symlink_target(relative, link_target)
                        info.type = tarfile.SYMTYPE
                        info.linkname = link_target
                        info.mode = 0o777
                        info.size = 0
                        archive.addfile(info)
                    elif path.is_dir():
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o755
                        info.size = 0
                        archive.addfile(info)
                    elif path.is_file():
                        info.type = tarfile.REGTYPE
                        info.mode = 0o755 if stat.S_IMODE(path.stat().st_mode) & 0o111 else 0o644
                        info.size = path.stat().st_size
                        with path.open("rb") as source:
                            archive.addfile(info, source)
                    else:
                        raise PackagingError("source tree contains an unsupported file type")


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_checksum_file(archive_path, sidecar_path):
    name = Path(archive_path).name
    content = "{}  {}\n".format(sha256_file(archive_path), name).encode("ascii")
    with Path(sidecar_path).open("xb") as sidecar:
        sidecar.write(content)


def buildinfo_document(checkout, host, rustc, archive_path):
    return {
        "schema_version": 1,
        "tag": checkout.tag,
        "version": checkout.version,
        "revision": checkout.revision,
        "target": host.target,
        "rustc": rustc,
        "checksum": sha256_file(archive_path),
        "unsigned": True,
    }


def _stage_release_documents(root, staging, binary_path, host, version, source_time, documents, dependency_manifest):
    _copy_public_documents(root, staging)
    for item in documents:
        _write_bytes(staging, item["archive_path"], item["bytes"])
    _write_bytes(staging, "dependency-notices/MANIFEST.json", _json_bytes(dependency_manifest))
    binary_mode = 0o755 if host.platform_name == "macos-arm64" else 0o644
    _write_bytes(staging, host.binary_name, Path(binary_path).read_bytes(), binary_mode)


def _install_staged_outputs(staging, output_dir, names):
    output = Path(output_dir)
    output.mkdir(parents=True, exist_ok=True)
    if output.is_symlink() or not output.is_dir():
        raise PackagingError("release output directory is not a regular directory")
    ensure_outputs_available(output, names)
    created = []
    try:
        for name in names:
            source = Path(staging) / name
            target = output / name
            os.link(source, target)
            created.append(target)
    except OSError as exc:
        for target in created:
            try:
                target.unlink()
            except OSError:
                pass
        raise PackagingError("release outputs could not be installed without overwriting") from exc


def _rustc_version():
    version = _text_command(["rustc", "--version"], None, "rustc version lookup").splitlines()
    if not version or not version[0].startswith("rustc "):
        raise PackagingError("rustc returned an invalid version string")
    return version[0]


def package_binary(repo_root, binary, tag, revision, output_dir):
    host = detect_host()
    checkout = validate_checkout(repo_root, tag, revision)
    output = validate_output_directory(output_dir, checkout.root)
    extension = ".zip" if host.platform_name == "windows-x86_64" else ".tar.gz"
    filename = "gp64-{}-{}{}".format(checkout.tag, host.archive_suffix, extension)
    sha_name = filename + ".sha256"
    info_name = filename + ".buildinfo.json"
    names = [filename, sha_name, info_name]
    ensure_outputs_available(output, names)

    verify_binary_architecture(binary, host.platform_name)
    metadata = load_full_metadata(checkout.root)
    fallback_root = Path(__file__).resolve().parents[1] / "assets" / "dependency-licenses"
    documents, dependency_manifest = collect_dependency_documents(metadata, fallback_root)
    rustc = _rustc_version()

    parent = output.parent
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gp64-release-stage-", dir=str(parent)) as temp:
        stage = Path(temp)
        package_tree = stage / "package"
        package_tree.mkdir()
        _stage_release_documents(
            checkout.root,
            package_tree,
            binary,
            host,
            checkout.version,
            checkout.commit_time,
            documents,
            dependency_manifest,
        )
        archive_path = stage / filename
        archive_root = "gp64-{}-{}".format(checkout.tag, host.archive_suffix)
        if host.platform_name == "windows-x86_64":
            write_deterministic_zip(package_tree, archive_path, archive_root, checkout.commit_time)
        else:
            write_deterministic_tar_gz(package_tree, archive_path, checkout.commit_time, archive_root)
        write_checksum_file(archive_path, stage / sha_name)
        buildinfo = buildinfo_document(checkout, host, rustc, archive_path)
        (stage / info_name).write_bytes(_json_bytes(buildinfo))
        _install_staged_outputs(stage, output, names)
    return [filename, sha_name, info_name]


def _check_source_vendor_inputs(source_root):
    cargo_directory = source_root / ".cargo"
    config = source_root / ".cargo" / "config.toml"
    if cargo_directory.is_symlink() or (cargo_directory.exists() and not cargo_directory.is_dir()):
        raise PackagingError("source revision has an unsafe .cargo path")
    if config.exists() or config.is_symlink():
        raise PackagingError("source revision already contains .cargo/config.toml; offline vendor config needs review")
    if (source_root / "vendor").exists() or (source_root / "vendor").is_symlink():
        raise PackagingError("source revision already contains vendor; refusing to replace it")


def _write_cargo_vendor_config(source_root):
    config = source_root / ".cargo" / "config.toml"
    if config.parent.is_symlink() or (config.parent.exists() and not config.parent.is_dir()):
        raise PackagingError("source archive has an unsafe .cargo path")
    if config.exists() or config.is_symlink():
        raise PackagingError("source revision already contains .cargo/config.toml; offline vendor config needs review")
    _write_bytes(
        source_root,
        ".cargo/config.toml",
        b'[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
        b'[source.vendored-sources]\ndirectory = "vendor"\n\n'
        b'[net]\noffline = true\n',
    )


def _copy_source_legal_docs(source_root, documents, dependency_manifest):
    prefix = "licenses"
    for item in documents:
        destination = prefix + "/" + item["archive_path"]
        _write_bytes(source_root, destination, item["bytes"])
    manifest = _manifest_with_prefix(dependency_manifest, prefix + "/")
    _write_bytes(source_root, "licenses/dependency-notices/MANIFEST.json", _json_bytes(manifest))


def _git_archive_to_tree(checkout, destination):
    with tempfile.TemporaryFile() as archive_data:
        try:
            subprocess.run(
                ["git", "archive", "--format=tar", checkout.revision],
                cwd=str(checkout.root),
                stdout=archive_data,
                stderr=subprocess.PIPE,
                check=True,
            )
        except FileNotFoundError as exc:
            raise PackagingError("required tool is unavailable: git") from exc
        except subprocess.CalledProcessError as exc:
            raise PackagingError("git archive failed (exit {})".format(exc.returncode)) from exc
        archive_data.seek(0)
        extract_git_archive(archive_data, destination)


def package_source(repo_root, tag, revision, output_dir):
    detect_host()
    checkout = validate_checkout(repo_root, tag, revision)
    output = validate_output_directory(output_dir, checkout.root)
    filename = "gp64-{}-source.tar.gz".format(checkout.tag)
    sha_name = filename + ".sha256"
    names = [filename, sha_name]
    ensure_outputs_available(output, names)

    metadata = load_full_metadata(checkout.root)
    fallback_root = Path(__file__).resolve().parents[1] / "assets" / "dependency-licenses"
    documents, dependency_manifest = collect_dependency_documents(metadata, fallback_root)

    parent = output.parent
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gp64-source-stage-", dir=str(parent)) as temp:
        stage = Path(temp)
        source_tree = stage / "source"
        _git_archive_to_tree(checkout, source_tree)
        _check_source_vendor_inputs(source_tree)
        _run(
            ["cargo", "vendor", "--locked", "--offline", "--versioned-dirs", "vendor"],
            source_tree,
            "cargo vendor",
        )
        vendor_root = source_tree / "vendor"
        if vendor_root.is_symlink() or not vendor_root.is_dir():
            raise PackagingError("cargo vendor did not create the expected vendor tree")
        files, vendor_manifest, vendor_hash = _tree_manifest(vendor_root)
        _write_cargo_vendor_config(source_tree)
        _copy_source_legal_docs(source_tree, documents, dependency_manifest)

        lockfile = source_tree / "Cargo.lock"
        if not lockfile.is_file():
            raise PackagingError("source archive is missing Cargo.lock")
        source_manifest = {
            "schema_version": 1,
            "tag": checkout.tag,
            "version": checkout.version,
            "source_commit": checkout.revision,
            "cargo_lock_sha256": sha256_file(lockfile),
            "vendor_file_count": len(files),
            "vendor_manifest_sha256": vendor_hash,
        }
        _write_bytes(source_tree, "VENDOR_MANIFEST.json", vendor_manifest)
        _write_bytes(source_tree, "SOURCE_MANIFEST.json", _json_bytes(source_manifest))

        archive_path = stage / filename
        write_deterministic_tar_gz(source_tree, archive_path, checkout.commit_time)
        write_checksum_file(archive_path, stage / sha_name)
        _install_staged_outputs(stage, output, names)
    return names


def _expected_tag_revision(tag, revision):
    match = TAG_RE.fullmatch(tag) if isinstance(tag, str) else None
    if not match:
        raise PackagingError("release tag must use v<workspace-version> form")
    if not isinstance(revision, str) or not REVISION_RE.fullmatch(revision):
        raise PackagingError("revision must be a full 40- or 64-character commit ID")
    return match.group("version"), revision.lower()


def _require_regular_file(path, description):
    candidate = Path(path)
    if candidate.is_symlink() or not candidate.is_file():
        raise PackagingError("{} must be a regular file".format(description))
    return candidate


def _verify_checksum_sidecar(archive_path):
    archive = _require_regular_file(archive_path, "release archive")
    sidecar = _require_regular_file(str(archive) + ".sha256", "checksum sidecar")
    try:
        text = sidecar.read_text(encoding="ascii")
    except (OSError, UnicodeDecodeError) as exc:
        raise PackagingError("checksum sidecar is not readable ASCII") from exc
    expected = "{}  {}\n".format(sha256_file(archive), archive.name)
    if text != expected:
        raise PackagingError("release archive checksum sidecar does not match")
    return archive


def _dependency_manifest_paths(raw, prefix, file_digests):
    try:
        manifest = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PackagingError("dependency notice manifest is invalid JSON") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise PackagingError("dependency notice manifest has an unsupported schema")
    packages = manifest.get("packages")
    if not isinstance(packages, list):
        raise PackagingError("dependency notice manifest is missing its package list")
    listed = set()
    package_keys = set()
    for package in packages:
        if not isinstance(package, dict):
            raise PackagingError("dependency notice manifest has an invalid package entry")
        key = _package_key(package)
        if key in package_keys:
            raise PackagingError("dependency notice manifest contains a duplicate package")
        package_keys.add(key)
        source = package.get("source")
        package_files = package.get("files")
        if source not in ("workspace", "registry", "git", "external") or not isinstance(package_files, list):
            raise PackagingError("dependency notice manifest has invalid package provenance")
        if source != "workspace" and not package_files:
            raise PackagingError("dependency notice manifest omits legal text for an external package")
        for item in package_files:
            if not isinstance(item, dict):
                raise PackagingError("dependency notice manifest has an invalid file entry")
            relative = safe_relative_path(item.get("path"), "dependency notice path").as_posix()
            prefix_path = prefix.rstrip("/")
            expected_prefix = (prefix_path + "/" if prefix_path else "") + "dependency-notices/"
            if not relative.startswith(expected_prefix):
                raise PackagingError("dependency notice path is outside its legal-document directory")
            digest = item.get("sha256")
            if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise PackagingError("dependency notice manifest contains an invalid SHA-256")
            if relative in listed:
                raise PackagingError("dependency notice manifest lists a file more than once")
            if file_digests.get(relative) != digest:
                raise PackagingError("dependency notice text is missing or its SHA-256 does not match")
            listed.add(relative)
    prefix_path = (prefix.rstrip("/") + "/" if prefix.rstrip("/") else "") + "dependency-notices/"
    actual = {
        path for path in file_digests
        if path.startswith(prefix_path) and path != prefix_path + "MANIFEST.json"
    }
    if actual != listed:
        raise PackagingError("dependency notice files do not match their manifest")
    return manifest


def _verify_required_legal_paths(files_by_path):
    required = (
        "LICENSE",
        "NOTICE",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
        "licenses/vga-0.2.9-LICENSE-MIT.txt",
        "licenses/vga-0.2.9-LICENSE-APACHE.txt",
        "crates/gp64-types/HOST_DIRS_LICENSE.txt",
    )
    missing = [path for path in required if path not in files_by_path]
    if missing:
        raise PackagingError("release archive is missing required legal or project documents")
    if not any(re.fullmatch(r"crates/[^/]+/NOTICE\.md", path) for path in files_by_path):
        raise PackagingError("release archive is missing per-crate notices")


def _zip_file_payloads(archive_path, archive_root):
    files = {}
    file_digests = {}
    seen_casefolded = set()
    try:
        with zipfile.ZipFile(archive_path, mode="r") as archive:
            bad_member = archive.testzip()
            if bad_member is not None:
                raise PackagingError("binary release ZIP has a corrupt member")
            for info in archive.infolist():
                raw_name = info.filename.rstrip("/") if info.is_dir() else info.filename
                relative = safe_relative_path(raw_name, "binary archive member").as_posix()
                if info.is_dir():
                    raise PackagingError("binary release ZIP contains unexpected directory members")
                prefix = archive_root + "/"
                if not relative.startswith(prefix):
                    raise PackagingError("binary release ZIP has an unexpected archive root")
                member_path = relative[len(prefix) :]
                member_path = safe_relative_path(member_path, "binary archive member").as_posix()
                if member_path.casefold() in seen_casefolded:
                    raise PackagingError("binary release ZIP contains duplicate paths")
                seen_casefolded.add(member_path.casefold())
                mode = info.external_attr >> 16
                file_type = stat.S_IFMT(mode)
                if file_type not in (0, stat.S_IFREG):
                    raise PackagingError("binary release ZIP contains a non-regular file")
                files[member_path] = archive.read(info)
    except PackagingError:
        raise
    except (OSError, RuntimeError, zipfile.BadZipFile, zlib.error) as exc:
        raise PackagingError("binary release ZIP could not be read safely") from exc
    for path, content in files.items():
        file_digests[path] = hashlib.sha256(content).hexdigest()
    return files, file_digests


def verify_binary_archive(archive_path, tag, revision, platform_name):
    version, expected_revision = _expected_tag_revision(tag, revision)
    host = SUPPORTED_HOSTS.get(platform_name)
    if host is None:
        raise PackagingError("unsupported binary platform")
    archive = Path(archive_path)
    extension = ".zip" if platform_name == "windows-x86_64" else ".tar.gz"
    expected_name = "gp64-{}-{}{}".format(tag, host.archive_suffix, extension)
    if archive.name != expected_name:
        raise PackagingError("binary archive basename does not match the requested release")
    archive = _verify_checksum_sidecar(archive)
    info_path = archive.with_name(archive.name + ".buildinfo.json")
    _require_regular_file(info_path, "buildinfo sidecar")
    try:
        buildinfo = json.loads(info_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PackagingError("binary buildinfo sidecar is invalid JSON") from exc
    if (
        not isinstance(buildinfo, dict)
        or buildinfo.get("schema_version") != 1
        or buildinfo.get("tag") != tag
        or buildinfo.get("version") != version
        or str(buildinfo.get("revision", "")).lower() != expected_revision
        or buildinfo.get("target") != host.target
        or buildinfo.get("unsigned") is not True
        or not isinstance(buildinfo.get("rustc"), str)
        or not buildinfo.get("rustc", "").startswith("rustc ")
        or buildinfo.get("checksum") != sha256_file(archive)
    ):
        raise PackagingError("binary buildinfo does not match the requested release")

    archive_root = "gp64-{}-{}".format(tag, host.archive_suffix)
    if platform_name == "windows-x86_64":
        files, file_digests = _zip_file_payloads(archive, archive_root)
        file_paths = set(files)
        binary_bytes = files.get(host.binary_name)
        dependency_manifest = files.get("dependency-notices/MANIFEST.json", b"")
        _verify_required_legal_paths(file_paths)
        _dependency_manifest_paths(dependency_manifest, "", file_digests)
    else:
        try:
            with tarfile.open(archive, mode="r:gz") as binary_archive:
                members, kinds = _tar_member_paths(binary_archive, archive_root)
                file_paths = {path for path, kind in kinds.items() if kind == "file"}
                _verify_required_legal_paths(file_paths)
                file_digests = {
                    path: _tar_file_sha256(binary_archive, members[path])
                    for path in file_paths
                    if path.startswith("dependency-notices/")
                }
                dependency_manifest = _tar_read_file(
                    binary_archive, members, "dependency-notices/MANIFEST.json"
                ) or b""
                _dependency_manifest_paths(dependency_manifest, "", file_digests)
                binary_member = members.get(host.binary_name)
                if binary_member is not None and binary_member.isfile() and not (binary_member.mode & 0o111):
                    raise PackagingError("macOS archive executable is not executable")
                binary_bytes = _tar_read_file(
                    binary_archive, members, host.binary_name, limit=512 * 1024 * 1024
                )
        except PackagingError:
            raise
        except (OSError, tarfile.TarError) as exc:
            raise PackagingError("binary macOS tarball could not be read safely") from exc
    if binary_bytes is None:
        raise PackagingError("binary release archive is missing its executable")
    verify_binary_bytes(binary_bytes, platform_name)
    return {
        "archive": archive.name,
        "revision": expected_revision,
        "target": host.target,
        "checksum": sha256_file(archive),
    }


def _tar_member_paths(archive, archive_root=None):
    members = {}
    kinds = {}
    seen_casefolded = set()
    try:
        for member in archive:
            raw_name = member.name.rstrip("/") if member.isdir() else member.name
            relative = safe_relative_path(raw_name, "source archive member").as_posix()
            if archive_root is not None:
                prefix = safe_relative_path(archive_root, "archive root").as_posix() + "/"
                if not relative.startswith(prefix):
                    raise PackagingError("archive has an unexpected root directory")
                relative = safe_relative_path(relative[len(prefix) :], "source archive member").as_posix()
            if relative in members:
                raise PackagingError("source archive contains duplicate paths")
            if relative.casefold() in seen_casefolded:
                raise PackagingError("source archive contains paths that collide on case-insensitive filesystems")
            seen_casefolded.add(relative.casefold())
            if member.isfile():
                kind = "file"
            elif member.isdir():
                kind = "directory"
            elif member.issym():
                kind = "symlink"
                _validate_symlink_target(relative, member.linkname)
            else:
                raise PackagingError("source archive contains an unsupported member type")
            members[relative] = member
            kinds[relative] = kind
    except PackagingError:
        raise
    except (OSError, tarfile.TarError) as exc:
        raise PackagingError("source archive could not be read safely") from exc

    for relative in members:
        path = PurePosixPath(relative)
        for index in range(1, len(path.parts)):
            parent = PurePosixPath(*path.parts[:index]).as_posix()
            if parent in members and kinds[parent] != "directory":
                raise PackagingError("source archive member traverses a non-directory")
    return members, kinds


def _tar_read_file(archive, members, path, limit=32 * 1024 * 1024):
    member = members.get(path)
    if member is None or not member.isfile():
        return None
    source = archive.extractfile(member)
    if source is None:
        raise PackagingError("source archive file could not be read")
    data = source.read(limit + 1)
    if len(data) > limit:
        raise PackagingError("source archive metadata member exceeds the verification limit")
    return data


def _tar_file_sha256(archive, member):
    source = archive.extractfile(member)
    if source is None:
        raise PackagingError("source archive file could not be read")
    digest = hashlib.sha256()
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def _verify_source_dependency_manifest(archive, members, file_digests):
    manifest_path = "licenses/dependency-notices/MANIFEST.json"
    raw = _tar_read_file(archive, members, manifest_path)
    if raw is None:
        raise PackagingError("source archive is missing its dependency notice manifest")
    _dependency_manifest_paths(raw, "licenses", file_digests)


def verify_source_archive(archive_path, tag, revision):
    version, expected_revision = _expected_tag_revision(tag, revision)
    archive = Path(archive_path)
    expected_name = "gp64-{}-source.tar.gz".format(tag)
    if archive.name != expected_name:
        raise PackagingError("source archive basename does not match the requested release")
    archive = _verify_checksum_sidecar(archive)
    try:
        with tarfile.open(archive, mode="r:gz") as source_archive:
            members, kinds = _tar_member_paths(source_archive)
            file_paths = {path for path, kind in kinds.items() if kind == "file"}
            file_digests = {}
            symlinks = {}
            for path, member in members.items():
                if kinds[path] == "symlink":
                    symlinks[path] = member.linkname

            required = (
                "Cargo.toml",
                "Cargo.lock",
                ".cargo/config.toml",
                "SOURCE_MANIFEST.json",
                "VENDOR_MANIFEST.json",
                "LICENSE",
                "NOTICE",
                "README.md",
                "THIRD_PARTY_NOTICES.md",
            )
            if any(path not in file_paths for path in required):
                raise PackagingError("source archive is missing a required source or legal document")
            if "crates/gp64/Cargo.toml" not in file_paths or "crates/gp64/src/main.rs" not in file_paths:
                raise PackagingError("source archive is missing required application source")
            _verify_required_legal_paths(file_paths)
            for path in file_paths:
                if path.startswith("vendor/") or path.startswith("licenses/dependency-notices/"):
                    file_digests[path] = _tar_file_sha256(source_archive, members[path])
            config = _tar_read_file(source_archive, members, ".cargo/config.toml")
            if b'replace-with = "vendored-sources"' not in config or b"offline = true" not in config:
                raise PackagingError("source archive does not configure offline vendored Cargo dependencies")

            try:
                source_manifest_bytes = _tar_read_file(source_archive, members, "SOURCE_MANIFEST.json")
                cargo_lock = _tar_read_file(source_archive, members, "Cargo.lock")
                vendor_manifest_bytes = _tar_read_file(source_archive, members, "VENDOR_MANIFEST.json")
                source_manifest = json.loads(source_manifest_bytes.decode("utf-8"))
                vendor_manifest = json.loads(vendor_manifest_bytes.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise PackagingError("source archive manifests are invalid JSON") from exc
            if (
                not isinstance(source_manifest, dict)
                or source_manifest.get("schema_version") != 1
                or source_manifest.get("tag") != tag
                or source_manifest.get("version") != version
                or str(source_manifest.get("source_commit", "")).lower() != expected_revision
                or source_manifest.get("cargo_lock_sha256") != hashlib.sha256(cargo_lock).hexdigest()
                or source_manifest.get("vendor_manifest_sha256") != hashlib.sha256(vendor_manifest_bytes).hexdigest()
                or not isinstance(vendor_manifest, dict)
                or vendor_manifest.get("schema_version") != 1
                or not isinstance(vendor_manifest.get("files"), list)
            ):
                raise PackagingError("source manifest does not match the requested tag or revision")

            expected_vendor = {}
            for item in vendor_manifest["files"]:
                if not isinstance(item, dict):
                    raise PackagingError("vendor manifest contains an invalid file entry")
                relative = safe_relative_path(item.get("path"), "vendor manifest path").as_posix()
                if relative in expected_vendor:
                    raise PackagingError("vendor manifest contains duplicate paths")
                kind = item.get("kind")
                digest = item.get("sha256")
                if kind not in ("file", "symlink") or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                    raise PackagingError("vendor manifest contains invalid file metadata")
                expected_vendor[relative] = (kind, digest)
            actual_vendor = {
                path[len("vendor/") :]: kinds[path]
                for path in members
                if path.startswith("vendor/") and kinds[path] in ("file", "symlink")
            }
            if actual_vendor != {path: kind for path, (kind, _) in expected_vendor.items()}:
                raise PackagingError("vendored source files do not match their manifest")
            if source_manifest.get("vendor_file_count") != len(expected_vendor):
                raise PackagingError("source manifest vendor file count does not match")
            for relative, (kind, expected_digest) in expected_vendor.items():
                path = "vendor/" + relative
                if kind == "file":
                    actual_digest = file_digests.get(path)
                else:
                    target = symlinks.get(path)
                    actual_digest = hashlib.sha256(target.encode("utf-8")).hexdigest() if target is not None else None
                if actual_digest != expected_digest:
                    raise PackagingError("vendored source content does not match its manifest")

            _verify_source_dependency_manifest(source_archive, members, file_digests)
    except PackagingError:
        raise
    except (OSError, tarfile.TarError) as exc:
        raise PackagingError("source archive could not be read safely") from exc
    return {"archive": archive.name, "revision": expected_revision, "checksum": sha256_file(archive)}


def _arguments(argv=None):
    parser = argparse.ArgumentParser(description="Package or verify gp64 release assets locally.")
    subparsers = parser.add_subparsers(dest="mode", required=True)
    for mode in ("binary", "source"):
        child = subparsers.add_parser(mode)
        child.add_argument("--repo", required=True, help="clean gp64 checkout at the pinned tag")
        child.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
        child.add_argument("--revision", required=True, help="full commit ID that the tag and checkout resolve to")
        child.add_argument("--output-dir", required=True, help="new or existing directory outside the checkout")
        if mode == "binary":
            child.add_argument("--binary", required=True, help="prebuilt native gp64 executable")
    binary_verify = subparsers.add_parser("verify-binary")
    binary_verify.add_argument("--archive", required=True, help="binary release archive to verify")
    binary_verify.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
    binary_verify.add_argument("--revision", required=True, help="expected full source commit ID")
    binary_verify.add_argument(
        "--platform",
        required=True,
        choices=sorted(SUPPORTED_HOSTS),
        help="platform metadata expected in the asset",
    )
    source_verify = subparsers.add_parser("verify-source")
    source_verify.add_argument("--archive", required=True, help="source tarball to verify")
    source_verify.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
    source_verify.add_argument("--revision", required=True, help="expected full source commit ID")
    return parser.parse_args(argv)


def main(argv=None):
    args = _arguments(argv)
    try:
        if args.mode == "binary":
            files = package_binary(args.repo, args.binary, args.tag, args.revision, args.output_dir)
        elif args.mode == "source":
            files = package_source(args.repo, args.tag, args.revision, args.output_dir)
        elif args.mode == "verify-binary":
            result = verify_binary_archive(args.archive, args.tag, args.revision, args.platform)
            print(json.dumps(dict(result, status="verified"), sort_keys=True))
            return 0
        else:
            result = verify_source_archive(args.archive, args.tag, args.revision)
            print(json.dumps(dict(result, status="verified"), sort_keys=True))
            return 0
    except PackagingError as exc:
        print("package_release: {}".format(exc), file=sys.stderr)
        return 2
    print(json.dumps({"status": "created", "files": files}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
