#!/usr/bin/env python3
"""Build and verify deterministic gp64 release archives: executable plus licence texts."""

import argparse
import gzip
import hashlib
import io
import json
import os
import platform
import re
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
    archive_extension: str


@dataclass(frozen=True)
class Checkout:
    root: Path
    tag: str
    revision: str
    version: str
    commit_time: int


SUPPORTED_HOSTS = {
    "windows-x86_64": HostTarget(
        platform_name="windows-x86_64",
        target="x86_64-pc-windows-msvc",
        binary_name="gp64.exe",
        archive_suffix="windows-x86_64",
        archive_extension=".zip",
    ),
    "macos-arm64": HostTarget(
        platform_name="macos-arm64",
        target="aarch64-apple-darwin",
        binary_name="gp64",
        archive_suffix="macos-arm64",
        archive_extension=".tar.gz",
    ),
}

LICENSE_NAME = "LICENSE"
THIRD_PARTY_NAME = "THIRD_PARTY_LICENSES.txt"
# Combined licence file: these lead, every other document follows in path order.
LEADING_DOCUMENTS = ("NOTICE", "THIRD_PARTY_NOTICES.md")
GPL_MARKER = b"GNU GENERAL PUBLIC LICENSE"

TAG_RE = re.compile(
    r"^v(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$"
)
REVISION_RE = re.compile(r"^(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})$")
PACKAGE_NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.+-]*$")
PUBLIC_SCP_ORIGIN_RE = re.compile(r"^git@github\.com:0x4D44/gp64(?:\.git)?$", re.IGNORECASE)
PUBLIC_ORIGIN_PATHS = ("/0x4D44/gp64", "/0x4D44/gp64.git")
LEGAL_NAME_MARKERS = (
    "license",
    "licence",
    "notice",
    "copyright",
    "copying",
    "authors",
    "credits",
    "patents",
    "attribution",
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
    return data


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
        if parts.path not in PUBLIC_ORIGIN_PATHS or parts.query or parts.fragment:
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
    return Checkout(root, tag, provided_revision, version, commit_time)


def load_target_metadata(root, target):
    """Locked metadata limited to the packages Cargo resolves for one target."""
    try:
        metadata = json.loads(
            _run(
                [
                    "cargo", "metadata", "--locked", "--offline",
                    "--filter-platform", target, "--format-version", "1",
                ],
                root,
                "cargo metadata for {}".format(target),
            )
        )
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise PackagingError("cargo metadata returned invalid JSON") from exc
    if metadata.get("workspace_root") != str(Path(root).resolve()):
        raise PackagingError("cargo metadata resolved a different workspace")
    packages = metadata.get("packages")
    if not isinstance(packages, list) or not packages:
        raise PackagingError("cargo metadata did not include locked packages")
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


def legal_text(content):
    """Return LF-normalised UTF-8 text, or None for binary content."""
    if b"\x00" in content:
        return None
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError:
        text = content.decode("latin-1")
    return text.replace("\r\n", "\n").encode("utf-8")


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
    ignored_directories = {".agents", ".claude", ".git", ".scratch", "target"}
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
        # A checkout with core.autocrlf=true rewrites committed LF texts as CRLF.
        lf_content = content.replace(b"\r\n", b"\n")
        if expected not in (hashlib.sha256(content).hexdigest(), hashlib.sha256(lf_content).hexdigest()):
            raise PackagingError("verified licence fallback SHA-256 does not match")
        source_url = _public_https_url(item.get("source_url"), "fallback source URL")
        upstream_path = safe_relative_path(item.get("upstream_path"), "upstream licence path").as_posix()
        archive_path = "dependency-notices/{}-{}/supplement/{}".format(
            package_name, package_version, relative.as_posix()
        )
        results.append(
            {
                "archive_path": safe_relative_path(archive_path).as_posix(),
                "package": package_name,
                "version": package_version,
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
    candidates = []
    for relative, path in _license_file_candidates(package, package_root, excluded_roots):
        content = path.read_bytes()
        # Names such as libwinapi_oemlicense.a match the markers but are not texts.
        if legal_text(content) is not None:
            candidates.append((relative, content))
    if candidates:
        results = []
        for relative, content in candidates:
            archive_path = "dependency-notices/{}-{}/{}".format(name, version, relative)
            results.append(
                {
                    "archive_path": safe_relative_path(archive_path).as_posix(),
                    "package": name,
                    "version": version,
                    "bytes": content,
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


def _read_repo_file(repo, relative):
    rel = safe_relative_path(relative, "repository legal document path")
    source = Path(repo).joinpath(*rel.parts)
    if source.is_symlink() or not source.is_file():
        raise PackagingError("required repository legal document is unavailable")
    return source.read_bytes()


def repository_legal_documents(repo):
    """The repository's own notices and the licence texts they reference, by path."""
    repo = Path(repo)
    documents = {relative: _read_repo_file(repo, relative) for relative in LEADING_DOCUMENTS}

    license_root = repo / "licenses"
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
        relative = source.relative_to(repo).as_posix()
        documents[relative] = _read_repo_file(repo, relative)
    if any(relative not in documents for relative in required_public_licenses):
        raise PackagingError("required public font licence texts are unavailable")

    crates_root = repo / "crates"
    notices = sorted(crates_root.glob("*/NOTICE.md")) if crates_root.is_dir() else []
    if not notices:
        raise PackagingError("workspace crate notices are unavailable")
    for source in notices:
        if source.is_symlink() or not source.is_file():
            raise PackagingError("crate notice is not a regular file")
        relative = source.relative_to(repo).as_posix()
        documents[relative] = _read_repo_file(repo, relative)
    host_dirs_license = "crates/gp64-types/HOST_DIRS_LICENSE.txt"
    documents[host_dirs_license] = _read_repo_file(repo, host_dirs_license)
    return documents


def _section(title, body):
    rule = "=" * 78
    return "{0}\n{1}\n{0}\n\n{2}\n\n".format(rule, title, body.rstrip())


def _required_text(content, name):
    text = legal_text(content)
    if text is None:
        raise PackagingError("legal document is not text: " + name)
    return text.decode("utf-8")


def third_party_licenses(repo_documents, dependency_documents):
    """One plain-text file: the repository notices by path, then each distinct
    dependency licence text once, headed by every crate file it covers."""
    missing = [name for name in LEADING_DOCUMENTS if name not in repo_documents]
    if missing:
        raise PackagingError("combined licence file is missing a leading notice")
    order = list(LEADING_DOCUMENTS) + sorted(name for name in repo_documents if name not in LEADING_DOCUMENTS)
    sections = [_section(name, _required_text(repo_documents[name], name)) for name in order]

    # Texts that differ only in whitespace or line wrapping are the same licence.
    groups = {}
    for item in dependency_documents:
        text = _required_text(item["bytes"], item["archive_path"])
        key = " ".join(text.split())
        source = item["upstream_path"]
        if item.get("source_url"):
            source += " from " + item["source_url"]
        groups.setdefault(key, (text, set()))[1].add(
            "{} {}: {}".format(item["package"], item["version"], source)
        )
    ordered = sorted(groups.values(), key=lambda group: sorted(group[1])[0])
    for index, (text, users) in enumerate(ordered, 1):
        title = "Dependency licence text {} of {}".format(index, len(ordered))
        body = "Applies to:\n{}\n\n{}".format("\n".join("  " + user for user in sorted(users)), text)
        sections.append(_section(title, body))
    return "".join(sections).encode("utf-8")


def linked_packages(metadata, root_name="gp64"):
    """Packages compiled into the root binary: its normal dependencies, transitively.
    Development and build-script dependencies never reach the shipped executable."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata.get("resolve", {}).get("nodes", [])}
    roots = [
        package["id"]
        for package in metadata["packages"]
        if package.get("name") == root_name and package.get("source") is None
    ]
    if len(roots) != 1 or roots[0] not in nodes:
        raise PackagingError("cargo metadata did not resolve the {} package".format(root_name))
    linked, pending = set(), [roots[0]]
    while pending:
        package_id = pending.pop()
        if package_id in linked:
            continue
        linked.add(package_id)
        for dependency in nodes[package_id].get("deps", []):
            if any(kind.get("kind") is None for kind in dependency.get("dep_kinds", [])):
                pending.append(dependency["pkg"])
    return {"packages": [packages[package_id] for package_id in sorted(linked)]}


def zip_timestamp(timestamp):
    value = datetime.fromtimestamp(timestamp, timezone.utc)
    year = min(2107, max(1980, value.year))
    return (year, value.month, value.day, value.hour, value.minute, value.second - value.second % 2)


def write_release_zip(members, destination, archive_root, source_time):
    """members: (name, bytes, mode) in archive order."""
    prefix = safe_relative_path(archive_root, "archive root").as_posix()
    timestamp = zip_timestamp(source_time)
    with zipfile.ZipFile(Path(destination), mode="x", compression=zipfile.ZIP_DEFLATED, strict_timestamps=False) as archive:
        for name, content, mode in members:
            relative = safe_relative_path(name).as_posix()
            info = zipfile.ZipInfo(prefix + "/" + relative, date_time=timestamp)
            info.create_system = 3
            info.compress_type = zipfile.ZIP_DEFLATED
            info.flag_bits |= 0x800
            info.external_attr = (stat.S_IFREG | mode) << 16
            info.extra = b""
            info.comment = b""
            archive.writestr(info, content, compresslevel=9)


def write_release_tar_gz(members, destination, archive_root, source_time):
    """members: (name, bytes, mode) in archive order."""
    prefix = safe_relative_path(archive_root, "archive root").as_posix()
    with Path(destination).open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for name, content, mode in members:
                    info = tarfile.TarInfo(prefix + "/" + safe_relative_path(name).as_posix())
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = source_time
                    info.pax_headers = {}
                    info.type = tarfile.REGTYPE
                    info.mode = mode
                    info.size = len(content)
                    archive.addfile(info, io.BytesIO(content))


def release_archive_name(tag, host):
    return "gp64-{}-{}{}".format(tag, host.archive_suffix, host.archive_extension)


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


def package_binary(repo_root, binary, tag, revision, output_dir):
    host = detect_host()
    checkout = validate_checkout(repo_root, tag, revision)
    output = validate_output_directory(output_dir, checkout.root)
    filename = release_archive_name(checkout.tag, host)
    ensure_outputs_available(output, [filename])

    binary_bytes = verify_binary_architecture(binary, host.platform_name)
    metadata = load_target_metadata(checkout.root, host.target)
    fallback_root = Path(__file__).resolve().parents[1] / "assets" / "dependency-licenses"
    documents, _ = collect_dependency_documents(linked_packages(metadata), fallback_root)
    licences = third_party_licenses(repository_legal_documents(checkout.root), documents)
    members = [
        (host.binary_name, binary_bytes, 0o755),
        (LICENSE_NAME, legal_text(_read_repo_file(checkout.root, LICENSE_NAME)), 0o644),
        (THIRD_PARTY_NAME, licences, 0o644),
    ]

    parent = output.parent
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gp64-release-stage-", dir=str(parent)) as temp:
        stage = Path(temp)
        archive_root = "gp64-{}-{}".format(checkout.tag, host.archive_suffix)
        if host.archive_extension == ".zip":
            write_release_zip(members, stage / filename, archive_root, checkout.commit_time)
        else:
            write_release_tar_gz(members, stage / filename, archive_root, checkout.commit_time)
        _install_staged_outputs(stage, output, [filename])
    return [filename]


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _archive_member_name(raw_name, archive_root):
    relative = safe_relative_path(raw_name, "release archive member").as_posix()
    prefix = archive_root + "/"
    if not relative.startswith(prefix):
        raise PackagingError("release archive has an unexpected root directory")
    return safe_relative_path(relative[len(prefix) :], "release archive member").as_posix()


def _zip_members(archive_path, archive_root):
    members = {}
    try:
        with zipfile.ZipFile(archive_path, mode="r") as archive:
            if archive.testzip() is not None:
                raise PackagingError("release ZIP has a corrupt member")
            for info in archive.infolist():
                if info.is_dir():
                    raise PackagingError("release ZIP contains unexpected directory members")
                name = _archive_member_name(info.filename, archive_root)
                if stat.S_IFMT(info.external_attr >> 16) not in (0, stat.S_IFREG):
                    raise PackagingError("release ZIP contains a non-regular file")
                if name in members:
                    raise PackagingError("release archive contains duplicate paths")
                members[name] = (archive.read(info), stat.S_IMODE(info.external_attr >> 16))
    except PackagingError:
        raise
    except (OSError, RuntimeError, zipfile.BadZipFile, zlib.error) as exc:
        raise PackagingError("release ZIP could not be read safely") from exc
    return members


def _tar_members(archive_path, archive_root):
    members = {}
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            for member in archive:
                if not member.isfile():
                    raise PackagingError("release tarball contains a non-regular member")
                name = _archive_member_name(member.name, archive_root)
                if name in members:
                    raise PackagingError("release archive contains duplicate paths")
                source = archive.extractfile(member)
                if source is None:
                    raise PackagingError("release tarball member could not be read")
                members[name] = (source.read(), stat.S_IMODE(member.mode))
    except PackagingError:
        raise
    except (OSError, EOFError, zlib.error, tarfile.TarError) as exc:
        raise PackagingError("release tarball could not be read safely") from exc
    return members


def verify_binary_archive(archive_path, tag, platform_name):
    if not isinstance(tag, str) or not TAG_RE.fullmatch(tag):
        raise PackagingError("release tag must use v<workspace-version> form")
    host = SUPPORTED_HOSTS.get(platform_name)
    if host is None:
        raise PackagingError("unsupported binary platform")
    archive = Path(archive_path)
    if archive.is_symlink() or not archive.is_file():
        raise PackagingError("release archive must be a regular file")
    if archive.name != release_archive_name(tag, host):
        raise PackagingError("release archive basename does not match the requested release")

    archive_root = "gp64-{}-{}".format(tag, host.archive_suffix)
    if host.archive_extension == ".zip":
        members = _zip_members(archive, archive_root)
    else:
        members = _tar_members(archive, archive_root)
    expected = {host.binary_name, LICENSE_NAME, THIRD_PARTY_NAME}
    if set(members) != expected:
        raise PackagingError(
            "release archive must contain exactly {}".format(", ".join(sorted(expected)))
        )

    binary_bytes, binary_mode = members[host.binary_name]
    verify_binary_bytes(binary_bytes, platform_name)
    if host.platform_name == "macos-arm64" and not binary_mode & 0o111:
        raise PackagingError("macOS archive executable is not executable")
    if GPL_MARKER not in members[LICENSE_NAME][0][:512]:
        raise PackagingError("release archive LICENSE is not the GPL text")
    licences = members[THIRD_PARTY_NAME][0]
    try:
        licences.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise PackagingError("third-party licence file is not UTF-8 text") from exc
    for name in LEADING_DOCUMENTS:
        if "\n{}\n".format(name).encode("utf-8") not in licences:
            raise PackagingError("third-party licence file is missing " + name)
    return {"archive": archive.name, "target": host.target, "sha256": sha256_file(archive)}


def _arguments(argv=None):
    parser = argparse.ArgumentParser(description="Package or verify a gp64 release archive locally.")
    subparsers = parser.add_subparsers(dest="mode", required=True)
    binary = subparsers.add_parser("binary")
    binary.add_argument("--repo", required=True, help="clean gp64 checkout at the pinned tag")
    binary.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
    binary.add_argument("--revision", required=True, help="full commit ID that the tag and checkout resolve to")
    binary.add_argument("--output-dir", required=True, help="new or existing directory outside the checkout")
    binary.add_argument("--binary", required=True, help="prebuilt native gp64 executable")
    binary_verify = subparsers.add_parser("verify-binary")
    binary_verify.add_argument("--archive", required=True, help="release archive to verify")
    binary_verify.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
    binary_verify.add_argument(
        "--platform",
        required=True,
        choices=sorted(SUPPORTED_HOSTS),
        help="platform the archive is built for",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = _arguments(argv)
    try:
        if args.mode == "binary":
            files = package_binary(args.repo, args.binary, args.tag, args.revision, args.output_dir)
        else:
            result = verify_binary_archive(args.archive, args.tag, args.platform)
            print(json.dumps(dict(result, status="verified"), sort_keys=True))
            return 0
    except PackagingError as exc:
        print("package_release: {}".format(exc), file=sys.stderr)
        return 2
    print(json.dumps({"status": "created", "files": files}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
