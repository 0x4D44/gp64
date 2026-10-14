import hashlib
import gzip
import contextlib
import copy
import importlib.util
import io
import json
import os
import pathlib
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("package_release.py")
SPEC = importlib.util.spec_from_file_location("package_release", SCRIPT)
package_release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = package_release
SPEC.loader.exec_module(package_release)


def write_fixture_git_repo(parent, version="0.1.0"):
    repo = pathlib.Path(parent) / "repo"
    repo.mkdir()
    (repo / "Cargo.toml").write_text(
        '[workspace]\nresolver = "2"\nmembers = ["crates/gp64"]\n',
        encoding="utf-8",
    )
    app = repo / "crates" / "gp64"
    (app / "src").mkdir(parents=True)
    (app / "Cargo.toml").write_text(
        "[package]\n"
        'name = "gp64"\n'
        f'version = "{version}"\n'
        'edition = "2021"\n',
        encoding="utf-8",
    )
    (app / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (repo / "Cargo.lock").write_text(
        "version = 4\n\n[[package]]\nname = \"gp64\"\n"
        f'version = "{version}"\n',
        encoding="utf-8",
    )
    (repo / "LICENSE").write_text("fixture licence\n", encoding="utf-8")
    (repo / "NOTICE").write_text("fixture notice\n", encoding="utf-8")
    (repo / "README.md").write_text("fixture readme\n", encoding="utf-8")
    (repo / "THIRD_PARTY_NOTICES.md").write_text("fixture third-party notices\n", encoding="utf-8")
    (repo / "licenses").mkdir()
    (repo / "licenses" / "vga-0.2.9-LICENSE-MIT.txt").write_text("VGA MIT\n", encoding="utf-8")
    (repo / "licenses" / "vga-0.2.9-LICENSE-APACHE.txt").write_text("VGA Apache\n", encoding="utf-8")
    (repo / "crates" / "gp64-types").mkdir(parents=True)
    (repo / "crates" / "gp64-types" / "NOTICE.md").write_text("fixture crate notice\n", encoding="utf-8")
    (repo / "crates" / "gp64-types" / "HOST_DIRS_LICENSE.txt").write_text("fixture host-dir MIT\n", encoding="utf-8")
    subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.name", "Fixture"], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.email", "fixture@example.invalid"], check=True)
    subprocess.run(["git", "-C", str(repo), "remote", "add", "origin", "https://github.com/0x4D44/gp64.git"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "add",
            "Cargo.toml",
            "Cargo.lock",
            "LICENSE",
            "NOTICE",
            "README.md",
            "THIRD_PARTY_NOTICES.md",
            "licenses",
            "crates/gp64",
            "crates/gp64-types",
        ],
        check=True,
    )
    subprocess.run(["git", "-C", str(repo), "commit", "--quiet", "-m", "fixture"], check=True)
    revision = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    subprocess.run(["git", "-C", str(repo), "tag", f"v{version}"], check=True)
    return repo, revision


def pe_image(machine=0x8664):
    image = bytearray(96)
    image[:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, 64)
    image[64:68] = b"PE\0\0"
    struct.pack_into("<H", image, 68, machine)
    return bytes(image)


def arm64_macho(cputype=0x0100000C):
    return b"\xcf\xfa\xed\xfe" + struct.pack("<I", cputype) + bytes(48)


def dependency_manifest(prefix=""):
    path = prefix + "dependency-notices/demo-1.0.0/LICENSE-MIT"
    return {
        "schema_version": 1,
        "packages": [
            {"name": "gp64", "version": "0.1.0", "source": "workspace", "files": []},
            {
                "name": "demo",
                "version": "1.0.0",
                "source": "registry",
                "files": [{"path": path, "sha256": hashlib.sha256(b"demo MIT text\n").hexdigest()}],
            },
        ],
    }


def common_legal_files(prefix=""):
    files = {
        "LICENSE": b"project licence\n",
        "NOTICE": b"project notice\n",
        "README.md": b"gp64 release\n",
        "THIRD_PARTY_NOTICES.md": b"third party notices\n",
        "licenses/vga-0.2.9-LICENSE-MIT.txt": b"VGA MIT text\n",
        "licenses/vga-0.2.9-LICENSE-APACHE.txt": b"VGA Apache text\n",
        "crates/gp64-types/HOST_DIRS_LICENSE.txt": b"MIT host dirs\n",
        "crates/gp64-types/NOTICE.md": b"gp64-types notice\n",
        prefix + "dependency-notices/demo-1.0.0/LICENSE-MIT": b"demo MIT text\n",
    }
    files[prefix + "dependency-notices/MANIFEST.json"] = package_release._json_bytes(dependency_manifest(prefix))
    return files


def create_binary_asset(directory, platform_name, revision="a" * 40, tag="v0.1.0", overrides=None):
    directory = pathlib.Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    host = package_release.SUPPORTED_HOSTS[platform_name]
    extension = ".zip" if platform_name == "windows-x86_64" else ".tar.gz"
    archive_name = f"gp64-{tag}-{host.archive_suffix}{extension}"
    archive_path = directory / archive_name
    package_tree = directory / f"package-{host.archive_suffix}"
    package_tree.mkdir()
    files = common_legal_files()
    files[host.binary_name] = pe_image() if platform_name == "windows-x86_64" else arm64_macho()
    for relative, content in files.items():
        path = package_tree / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    if platform_name == "macos-arm64":
        (package_tree / host.binary_name).chmod(0o755)
    archive_root = f"gp64-{tag}-{host.archive_suffix}"
    if platform_name == "windows-x86_64":
        package_release.write_deterministic_zip(package_tree, archive_path, archive_root, 1_800_000_000)
    else:
        package_release.write_deterministic_tar_gz(package_tree, archive_path, 1_800_000_000, archive_root)
    package_release.write_checksum_file(archive_path, pathlib.Path(str(archive_path) + ".sha256"))
    version = tag[1:]
    info = {
        "schema_version": 1,
        "tag": tag,
        "version": version,
        "revision": revision,
        "target": host.target,
        "rustc": "rustc 1.90.0",
        "checksum": package_release.sha256_file(archive_path),
        "unsigned": True,
    }
    info.update(overrides or {})
    pathlib.Path(str(archive_path) + ".buildinfo.json").write_bytes(package_release._json_bytes(info))
    return archive_path


def create_source_asset(directory, revision="b" * 40, tag="v0.1.0", traversal=False):
    directory = pathlib.Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    archive_path = directory / f"gp64-{tag}-source.tar.gz"
    source_tree = directory / "source-fixture"
    for relative, content in common_legal_files("licenses/").items():
        path = source_tree / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    (source_tree / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/gp64"]\n', encoding="utf-8")
    app_manifest = source_tree / "crates" / "gp64" / "Cargo.toml"
    app_manifest.parent.mkdir(parents=True)
    app_manifest.write_text('[package]\nname = "gp64"\nversion = "0.1.0"\n', encoding="utf-8")
    (app_manifest.parent / "src").mkdir()
    (app_manifest.parent / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (source_tree / ".cargo").mkdir(parents=True)
    (source_tree / ".cargo" / "config.toml").write_text(
        '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[net]\noffline = true\n', encoding="utf-8"
    )
    (source_tree / "vendor" / "demo-1.0.0").mkdir(parents=True)
    (source_tree / "vendor" / "demo-1.0.0" / "LICENSE-MIT").write_bytes(b"demo MIT text\n")
    (source_tree / "Cargo.lock").write_text("fixture lock\n", encoding="utf-8")
    vendor_files, vendor_raw, vendor_hash = package_release._tree_manifest(source_tree / "vendor")
    (source_tree / "VENDOR_MANIFEST.json").write_bytes(vendor_raw)
    source_manifest = {
        "schema_version": 1,
        "tag": tag,
        "version": tag[1:],
        "source_commit": revision,
        "cargo_lock_sha256": package_release.sha256_file(source_tree / "Cargo.lock"),
        "vendor_file_count": len(vendor_files),
        "vendor_manifest_sha256": vendor_hash,
    }
    (source_tree / "SOURCE_MANIFEST.json").write_bytes(package_release._json_bytes(source_manifest))
    package_release.write_deterministic_tar_gz(source_tree, archive_path, 1_800_000_000)
    if traversal:
        temporary = pathlib.Path(str(archive_path) + ".unsafe")
        with tarfile.open(temporary, mode="w:gz") as archive:
            member = tarfile.TarInfo("../outside.txt")
            member.size = 6
            archive.addfile(member, io.BytesIO(b"unsafe"))
        temporary.replace(archive_path)
    package_release.write_checksum_file(archive_path, pathlib.Path(str(archive_path) + ".sha256"))
    return archive_path


def rewrite_tar_gz(archive_path, omit_member=None, mode_update=None):
    archive_path = pathlib.Path(archive_path)
    temporary = archive_path.with_name(archive_path.name + ".mutated")
    with tarfile.open(archive_path, mode="r:gz") as source:
        with tarfile.open(temporary, mode="w:gz", format=tarfile.PAX_FORMAT) as target:
            for member in source:
                if member.name == omit_member:
                    continue
                rewritten = copy.copy(member)
                if mode_update is not None and member.name == mode_update[0]:
                    rewritten.mode = mode_update[1]
                content = source.extractfile(member) if member.isfile() else None
                target.addfile(rewritten, content)
    temporary.replace(archive_path)
    checksum_path = pathlib.Path(str(archive_path) + ".sha256")
    checksum_path.write_text(
        "{}  {}\n".format(package_release.sha256_file(archive_path), archive_path.name), encoding="ascii"
    )
    buildinfo_path = pathlib.Path(str(archive_path) + ".buildinfo.json")
    if buildinfo_path.is_file():
        buildinfo = json.loads(buildinfo_path.read_text(encoding="utf-8"))
        buildinfo["checksum"] = package_release.sha256_file(archive_path)
        buildinfo_path.write_bytes(package_release._json_bytes(buildinfo))


class PackageReleaseTests(unittest.TestCase):
    def test_host_and_binary_architecture_policy(self):
        windows = package_release.detect_host("win32", "AMD64")
        macos = package_release.detect_host("darwin", "arm64")
        self.assertEqual(windows.target, "x86_64-pc-windows-msvc")
        self.assertEqual(macos.target, "aarch64-apple-darwin")
        with self.assertRaises(package_release.PackagingError):
            package_release.detect_host("linux", "x86_64")
        with self.assertRaises(package_release.PackagingError):
            package_release.detect_host("darwin", "x86_64")

        with tempfile.TemporaryDirectory() as temp:
            binary = pathlib.Path(temp) / "gp64.exe"
            binary.write_bytes(pe_image())
            package_release.verify_binary_architecture(binary, "windows-x86_64")
            binary.write_bytes(pe_image(machine=0x014C))
            with self.assertRaises(package_release.PackagingError):
                package_release.verify_binary_architecture(binary, "windows-x86_64")
            binary.write_bytes(arm64_macho())
            package_release.verify_binary_architecture(binary, "macos-arm64")
            binary.write_bytes(arm64_macho(cputype=0x01000007))
            with self.assertRaises(package_release.PackagingError):
                package_release.verify_binary_architecture(binary, "macos-arm64")

    def test_dependency_legal_text_keeps_package_paths_and_verifies_fallback(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            crate = base / "registry" / "demo-1.2.3"
            (crate / "docs").mkdir(parents=True)
            (crate / "Cargo.toml").write_text("[package]\nname='demo'\n", encoding="utf-8")
            (crate / "LICENSE-MIT").write_text("MIT text from crate\n", encoding="utf-8")
            (crate / "docs" / "NOTICE.txt").write_text("copyright notice\n", encoding="utf-8")
            package = {
                "name": "demo",
                "version": "1.2.3",
                "source": "registry+https://example.invalid/index",
                "license": "MIT OR Apache-2.0",
                "license_file": None,
                "manifest_path": str(crate / "Cargo.toml"),
            }

            direct = package_release.collect_package_license_files(package, {}, base)
            self.assertEqual(
                {item["archive_path"] for item in direct},
                {
                    "dependency-notices/demo-1.2.3/LICENSE-MIT",
                    "dependency-notices/demo-1.2.3/docs/NOTICE.txt",
                },
            )

            asset_root = base / "fallback"
            (asset_root / "upstream").mkdir(parents=True)
            fallback_text = b"verified upstream licence text\n"
            (asset_root / "upstream" / "LICENSE-APACHE").write_bytes(fallback_text)
            manifest = {
                "schema_version": 1,
                "packages": [
                    {
                        "name": "empty-crate",
                        "version": "2.4.0",
                        "repository": "https://example.invalid/repo",
                        "commit": "a" * 40,
                        "path_in_vcs": "",
                        "provenance_kind": "pinned-upstream-text",
                        "selected_license": "Apache-2.0",
                        "files": [
                            {
                                "path": "upstream/LICENSE-APACHE",
                                "source_url": "https://example.invalid/repo/blob/commit/LICENSE-APACHE",
                                "upstream_path": "LICENSE-APACHE",
                                "sha256": hashlib.sha256(fallback_text).hexdigest(),
                            }
                        ],
                    }
                ],
            }
            (asset_root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            fallback_map = package_release.load_fallback_manifest(asset_root)
            self.assertIsNone(fallback_map[("empty-crate", "2.4.0")]["path_in_vcs"])
            empty_package = dict(package, name="empty-crate", version="2.4.0", manifest_path=str(base / "empty" / "Cargo.toml"))
            (base / "empty").mkdir()
            (base / "empty" / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
            fallback = package_release.collect_package_license_files(empty_package, fallback_map, asset_root)
            self.assertEqual(len(fallback), 1)
            self.assertEqual(fallback[0]["bytes"], fallback_text)
            self.assertEqual(
                fallback[0]["archive_path"],
                "dependency-notices/empty-crate-2.4.0/supplement/upstream/LICENSE-APACHE",
            )
            self.assertNotIn(str(base), fallback[0]["archive_path"])

            (asset_root / "upstream" / "LICENSE-APACHE").write_bytes(b"changed after verification\n")
            with self.assertRaisesRegex(package_release.PackagingError, "SHA-256"):
                package_release.collect_package_license_files(empty_package, fallback_map, asset_root)

    def test_missing_dependency_license_text_is_blocked(self):
        with tempfile.TemporaryDirectory() as temp:
            crate = pathlib.Path(temp) / "registry" / "no-license-1.0.0"
            crate.mkdir(parents=True)
            manifest = crate / "Cargo.toml"
            manifest.write_text("[package]\n", encoding="utf-8")
            package = {
                "name": "no-license",
                "version": "1.0.0",
                "source": "registry+https://example.invalid/index",
                "license": "MIT",
                "license_file": None,
                "manifest_path": str(manifest),
            }
            with self.assertRaisesRegex(package_release.PackagingError, r"no-license@1\.0\.0"):
                package_release.collect_package_license_files(package, {}, pathlib.Path(temp))

    def test_workspace_legal_text_is_in_dependency_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            crate = root / "crates" / "workspace-demo"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text("[package]\nname='workspace-demo'\n", encoding="utf-8")
            (crate / "NOTICE.md").write_text("workspace copyright notice\n", encoding="utf-8")
            package = {
                "name": "workspace-demo",
                "version": "1.0.0",
                "source": None,
                "license": "MIT",
                "license_file": None,
                "manifest_path": str(crate / "Cargo.toml"),
                "id": "path+workspace-demo#1.0.0",
            }
            documents, manifest = package_release.collect_dependency_documents(
                {"packages": [package]}, root / "no-fallback-cache"
            )
            self.assertEqual(len(documents), 1)
            self.assertEqual(
                documents[0]["archive_path"], "dependency-notices/workspace-demo-1.0.0/NOTICE.md"
            )
            self.assertEqual(manifest["packages"][0]["files"][0]["path"], documents[0]["archive_path"])

    def test_workspace_package_scan_excludes_skill_assets_and_nested_members(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / "Cargo.toml").write_text("[package]\nname='gp64'\n", encoding="utf-8")
            (root / "NOTICE").write_text("root notice\n", encoding="utf-8")
            child = root / "crates" / "child"
            child.mkdir(parents=True)
            (child / "Cargo.toml").write_text("[package]\nname='child'\n", encoding="utf-8")
            (child / "NOTICE.md").write_text("child notice\n", encoding="utf-8")
            skill = root / ".agents" / "skills" / "release" / "NOTICE.md"
            skill.parent.mkdir(parents=True)
            skill.write_text("helper asset notice\n", encoding="utf-8")
            packages = [
                {
                    "name": "gp64",
                    "version": "0.1.0",
                    "source": None,
                    "license": "GPL-3.0-only",
                    "license_file": None,
                    "manifest_path": str(root / "Cargo.toml"),
                    "id": "path+gp64#0.1.0",
                },
                {
                    "name": "child",
                    "version": "1.0.0",
                    "source": None,
                    "license": "MIT",
                    "license_file": None,
                    "manifest_path": str(child / "Cargo.toml"),
                    "id": "path+child#1.0.0",
                },
            ]
            documents, manifest = package_release.collect_dependency_documents(
                {"packages": packages}, root / "no-fallback-cache"
            )
            paths = {item["archive_path"] for item in documents}
            self.assertEqual(
                paths,
                {
                    "dependency-notices/gp64-0.1.0/NOTICE",
                    "dependency-notices/child-1.0.0/NOTICE.md",
                },
            )
            self.assertEqual(sum(len(item["files"]) for item in manifest["packages"]), 2)

    def test_dependency_legal_path_traversal_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp) / "crate"
            root.mkdir()
            manifest = root / "Cargo.toml"
            manifest.write_text("[package]\n", encoding="utf-8")
            outside = pathlib.Path(temp) / "LICENSE"
            outside.write_text("outside text\n", encoding="utf-8")
            package = {
                "name": "traversal",
                "version": "1.0.0",
                "source": "registry+https://example.invalid/index",
                "license": "MIT",
                "license_file": "../LICENSE",
                "manifest_path": str(manifest),
            }
            with self.assertRaises(package_release.PackagingError):
                package_release.collect_package_license_files(package, {}, pathlib.Path(temp))

    def test_zip_content_and_checksum_are_deterministic(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            content = base / "package"
            content.mkdir()
            (content / "gp64.exe").write_bytes(pe_image())
            (content / "LICENSE").write_text("GPL text\n", encoding="utf-8")
            first = base / "first.zip"
            second = base / "second.zip"
            package_release.write_deterministic_zip(content, first, "gp64-v0.1.0-windows-x86_64", 1_800_000_000)
            package_release.write_deterministic_zip(content, second, "gp64-v0.1.0-windows-x86_64", 1_800_000_000)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(first) as archive:
                self.assertEqual(
                    archive.namelist(),
                    [
                        "gp64-v0.1.0-windows-x86_64/LICENSE",
                        "gp64-v0.1.0-windows-x86_64/gp64.exe",
                    ],
                )
                self.assertEqual(archive.read("gp64-v0.1.0-windows-x86_64/LICENSE"), b"GPL text\n")
                self.assertEqual(archive.getinfo(archive.namelist()[0]).date_time, package_release.zip_timestamp(1_800_000_000))
            self.assertEqual(package_release.sha256_file(first), hashlib.sha256(first.read_bytes()).hexdigest())
            checksum_path = base / "first.zip.sha256"
            package_release.write_checksum_file(first, checksum_path)
            self.assertEqual(
                checksum_path.read_text(encoding="ascii"),
                "{}  first.zip\n".format(hashlib.sha256(first.read_bytes()).hexdigest()),
            )
            checkout = package_release.Checkout(base, "v0.1.0", "a" * 40, "0.1.0", 1_800_000_000, {})
            host = package_release.detect_host("win32", "AMD64")
            buildinfo = package_release.buildinfo_document(checkout, host, "rustc 1.90.0", first)
            self.assertEqual(
                {key: buildinfo[key] for key in ("revision", "target", "rustc", "unsigned", "checksum")},
                {
                    "revision": "a" * 40,
                    "target": "x86_64-pc-windows-msvc",
                    "rustc": "rustc 1.90.0",
                    "unsigned": True,
                    "checksum": hashlib.sha256(first.read_bytes()).hexdigest(),
                },
            )

    def test_source_tar_gzip_and_member_metadata_are_deterministic(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            source = base / "source"
            (source / "vendor" / "demo-1.0.0").mkdir(parents=True)
            (source / "Cargo.lock").write_text("locked\n", encoding="utf-8")
            (source / "vendor" / "demo-1.0.0" / "LICENSE").write_text("MIT\n", encoding="utf-8")
            first = base / "first.tar.gz"
            second = base / "second.tar.gz"
            package_release.write_deterministic_tar_gz(source, first, 1_800_000_000)
            package_release.write_deterministic_tar_gz(source, second, 1_800_000_000)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with first.open("rb") as compressed:
                compressed.read(4)
                self.assertEqual(struct.unpack("<I", compressed.read(4))[0], 0)
            with gzip.open(first, "rb") as uncompressed:
                with tarfile.open(fileobj=io.BytesIO(uncompressed.read()), mode="r:") as archive:
                    members = {member.name: member for member in archive.getmembers()}
            self.assertEqual(
                sorted(members),
                ["Cargo.lock", "vendor", "vendor/demo-1.0.0", "vendor/demo-1.0.0/LICENSE"],
            )
            for member in members.values():
                self.assertEqual(member.mtime, 1_800_000_000)
                self.assertEqual(member.uid, 0)
                self.assertEqual(member.gid, 0)
                self.assertEqual(member.uname, "")
                self.assertEqual(member.gname, "")

    def test_output_must_be_outside_checkout_and_existing_assets_are_preserved(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            repo = base / "repo"
            repo.mkdir()
            output = repo / "release"
            with self.assertRaises(package_release.PackagingError):
                package_release.validate_output_directory(output, repo)

            output.mkdir()
            existing = output / "gp64-v0.1.0-source.tar.gz"
            existing.write_bytes(b"existing user output")
            with self.assertRaises(package_release.PackagingError):
                package_release.ensure_outputs_available(output, [existing.name])
            self.assertEqual(existing.read_bytes(), b"existing user output")

    def test_git_archive_extraction_rejects_path_traversal(self):
        with tempfile.TemporaryDirectory() as temp:
            archive_bytes = io.BytesIO()
            with tarfile.open(fileobj=archive_bytes, mode="w") as archive:
                member = tarfile.TarInfo("../escaped.txt")
                member.size = len(b"unsafe")
                archive.addfile(member, io.BytesIO(b"unsafe"))
            archive_bytes.seek(0)
            with self.assertRaises(package_release.PackagingError):
                package_release.extract_git_archive(archive_bytes, pathlib.Path(temp) / "tree")
            self.assertFalse((pathlib.Path(temp) / "escaped.txt").exists())

    def test_checkout_requires_clean_matching_tag_revision_and_version(self):
        with tempfile.TemporaryDirectory() as temp:
            repo, revision = write_fixture_git_repo(temp)
            context = package_release.validate_checkout(repo, "v0.1.0", revision)
            self.assertEqual(context.version, "0.1.0")
            self.assertEqual(context.revision, revision)

            (repo / "README.md").write_text("uncommitted\n", encoding="utf-8")
            with self.assertRaisesRegex(package_release.PackagingError, "clean"):
                package_release.validate_checkout(repo, "v0.1.0", revision)
            subprocess.run(["git", "-C", str(repo), "add", "README.md"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "--quiet", "-m", "second fixture commit"], check=True)
            second_revision = subprocess.run(
                ["git", "-C", str(repo), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            subprocess.run(["git", "-C", str(repo), "tag", "v0.2.0", second_revision], check=True)
            with self.assertRaisesRegex(package_release.PackagingError, "tag"):
                package_release.validate_checkout(repo, "v0.1.0", second_revision)
            with self.assertRaisesRegex(package_release.PackagingError, "version"):
                package_release.validate_checkout(repo, "v0.2.0", second_revision)

    def test_checkout_refuses_private_origin_without_echoing_remote_url(self):
        for public_origin in (
            "https://github.com/0x4D44/gp64.git",
            "git@github.com:0x4D44/gp64.git",
            "ssh://git@github.com/0x4D44/gp64.git",
        ):
            self.assertTrue(package_release._is_official_public_origin(public_origin))
        for non_public_origin in (
            "https://github.com/0x4D44/gp64-private.git",
            "https://github.com/0x4D44/gp64.git.evil",
            "https://github.com.attacker/0x4D44/gp64.git",
        ):
            self.assertFalse(package_release._is_official_public_origin(non_public_origin))
        with tempfile.TemporaryDirectory() as temp:
            repo, revision = write_fixture_git_repo(temp)
            private_url = "https://github.com/0x4D44/gp64-private.git"
            subprocess.run(
                ["git", "-C", str(repo), "remote", "set-url", "origin", private_url], check=True
            )
            with self.assertRaisesRegex(package_release.PackagingError, "official public repository") as raised:
                package_release.validate_checkout(repo, "v0.1.0", revision)
            self.assertNotIn(private_url, str(raised.exception))

    def test_foreign_binary_assets_verify_without_host_execution(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            revision = "c" * 40
            for platform_name in ("windows-x86_64", "macos-arm64"):
                archive = create_binary_asset(base / platform_name, platform_name, revision=revision)
                result = package_release.verify_binary_archive(archive, "v0.1.0", revision, platform_name)
                self.assertEqual(result["revision"], revision)
                self.assertEqual(result["target"], package_release.SUPPORTED_HOSTS[platform_name].target)
                self.assertTrue(archive.name.endswith(".zip" if platform_name == "windows-x86_64" else ".tar.gz"))
                self.assertTrue(pathlib.Path(str(archive) + ".buildinfo.json").is_file())
            mac_archive = base / "macos-arm64" / "gp64-v0.1.0-macos-arm64.tar.gz"
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                status = package_release.main(
                    [
                        "verify-binary",
                        "--archive",
                        str(mac_archive),
                        "--tag",
                        "v0.1.0",
                        "--revision",
                        revision,
                        "--platform",
                        "macos-arm64",
                    ]
                )
            self.assertEqual(status, 0)
            receipt = json.loads(output.getvalue())
            self.assertEqual(receipt["status"], "verified")
            self.assertEqual(receipt["archive"], mac_archive.name)

    def test_binary_verification_rejects_checksum_and_target_mismatches(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            revision = "d" * 40
            archive = create_binary_asset(base / "digest", "windows-x86_64", revision=revision)
            with archive.open("ab") as output:
                output.write(b"changed")
            with self.assertRaisesRegex(package_release.PackagingError, "checksum"):
                package_release.verify_binary_archive(archive, "v0.1.0", revision, "windows-x86_64")

            wrong_target = create_binary_asset(
                base / "target",
                "macos-arm64",
                revision=revision,
                overrides={"target": "x86_64-pc-windows-msvc"},
            )
            with self.assertRaisesRegex(package_release.PackagingError, "buildinfo"):
                package_release.verify_binary_archive(wrong_target, "v0.1.0", revision, "macos-arm64")

    def test_binary_verification_requires_notice_closure(self):
        with tempfile.TemporaryDirectory() as temp:
            archive = create_binary_asset(pathlib.Path(temp) / "missing", "windows-x86_64")
            with zipfile.ZipFile(archive, "r") as source:
                retained = {
                    info.filename: source.read(info)
                    for info in source.infolist()
                    if not info.filename.endswith("/THIRD_PARTY_NOTICES.md")
                }
            archive.unlink()
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as target:
                for name, content in retained.items():
                    target.writestr(name, content)
            checksum_path = pathlib.Path(str(archive) + ".sha256")
            checksum_path.write_text(
                "{}  {}\n".format(package_release.sha256_file(archive), archive.name), encoding="ascii"
            )
            info_path = pathlib.Path(str(archive) + ".buildinfo.json")
            info = json.loads(info_path.read_text(encoding="utf-8"))
            info["checksum"] = package_release.sha256_file(archive)
            info_path.write_bytes(package_release._json_bytes(info))
            with self.assertRaisesRegex(package_release.PackagingError, "required legal"):
                package_release.verify_binary_archive(archive, "v0.1.0", "a" * 40, "windows-x86_64")

    def test_macos_binary_verification_requires_executable_member_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            revision = "1" * 40
            archive = create_binary_asset(pathlib.Path(temp), "macos-arm64", revision=revision)
            rewrite_tar_gz(
                archive,
                mode_update=("gp64-v0.1.0-macos-arm64/gp64", 0o644),
            )
            with self.assertRaisesRegex(package_release.PackagingError, "not executable"):
                package_release.verify_binary_archive(archive, "v0.1.0", revision, "macos-arm64")

    def test_source_asset_verifies_manifests_and_pinned_revision(self):
        with tempfile.TemporaryDirectory() as temp:
            revision = "e" * 40
            archive = create_source_asset(pathlib.Path(temp) / "valid", revision=revision)
            result = package_release.verify_source_archive(archive, "v0.1.0", revision)
            self.assertEqual(result["revision"], revision)
            with self.assertRaisesRegex(package_release.PackagingError, "source manifest"):
                package_release.verify_source_archive(archive, "v0.1.0", "f" * 40)

    def test_source_asset_verification_rejects_tar_path_traversal(self):
        with tempfile.TemporaryDirectory() as temp:
            archive = create_source_asset(pathlib.Path(temp), traversal=True)
            with self.assertRaisesRegex(package_release.PackagingError, "unsafe source archive member"):
                package_release.verify_source_archive(archive, "v0.1.0", "b" * 40)

    def test_source_verification_requires_application_entrypoint(self):
        with tempfile.TemporaryDirectory() as temp:
            revision = "2" * 40
            archive = create_source_asset(pathlib.Path(temp), revision=revision)
            rewrite_tar_gz(archive, omit_member="crates/gp64/src/main.rs")
            with self.assertRaisesRegex(package_release.PackagingError, "application source"):
                package_release.verify_source_archive(archive, "v0.1.0", revision)

    def test_source_packaging_writes_offline_config_after_vendor_and_verifies(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            repo, revision = write_fixture_git_repo(base)
            output_dir = base / "release-assets"
            real_run = package_release._run

            def vendor_fixture(args, cwd, description, input_data=None):
                if args[:2] == ["cargo", "vendor"]:
                    vendor_root = pathlib.Path(cwd) / "vendor"
                    self.assertFalse(vendor_root.exists())
                    crate = vendor_root / "demo-1.0.0"
                    crate.mkdir(parents=True)
                    (crate / "LICENSE-MIT").write_text("vendored MIT\n", encoding="utf-8")
                    return b""
                return real_run(args, cwd, description, input_data)

            with mock.patch.object(package_release, "detect_host", return_value=package_release.SUPPORTED_HOSTS["macos-arm64"]):
                with mock.patch.object(package_release, "_run", side_effect=vendor_fixture):
                    files = package_release.package_source(repo, "v0.1.0", revision, output_dir)
            self.assertEqual(files, ["gp64-v0.1.0-source.tar.gz", "gp64-v0.1.0-source.tar.gz.sha256"])
            result = package_release.verify_source_archive(
                output_dir / files[0], "v0.1.0", revision
            )
            self.assertEqual(result["revision"], revision)


if __name__ == "__main__":
    unittest.main()
