import hashlib
import contextlib
import importlib.util
import io
import json
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

GPL_TEXT = b"                    GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007\n"


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
    # Committed with CRLF, as a core.autocrlf=true checkout presents it.
    (repo / "LICENSE").write_bytes(GPL_TEXT.replace(b"\n", b"\r\n"))
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
    subprocess.run(["git", "-C", str(repo), "config", "core.autocrlf", "false"], check=True)
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


def native_binary(platform_name):
    return pe_image() if platform_name == "windows-x86_64" else arm64_macho()


def licence_file():
    return package_release.third_party_licenses(
        {"NOTICE": b"project notice\n", "THIRD_PARTY_NOTICES.md": b"third party notices\n"},
        [dependency_document("demo", "1.0.0", "LICENSE-MIT", b"demo MIT text\n")],
    )


def section_titles(text):
    rule = "=" * 78
    lines = text.splitlines()
    return [lines[index + 1] for index in range(len(lines) - 2) if lines[index] == rule and lines[index + 2] == rule]


def dependency_document(package, version, upstream_path, content, source_url=None):
    return {
        "archive_path": f"dependency-notices/{package}-{version}/{upstream_path}",
        "package": package,
        "version": version,
        "bytes": content,
        "source_url": source_url,
        "upstream_path": upstream_path,
    }


def create_release_archive(directory, platform_name, tag="v0.1.0", members=None, binary_mode=0o755):
    directory = pathlib.Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    host = package_release.SUPPORTED_HOSTS[platform_name]
    archive_path = directory / package_release.release_archive_name(tag, host)
    if members is None:
        members = [
            (host.binary_name, native_binary(platform_name), binary_mode),
            ("LICENSE", GPL_TEXT, 0o644),
            ("THIRD_PARTY_LICENSES.txt", licence_file(), 0o644),
        ]
    archive_root = f"gp64-{tag}-{host.archive_suffix}"
    if host.archive_extension == ".zip":
        package_release.write_release_zip(members, archive_path, archive_root, 1_800_000_000)
    else:
        package_release.write_release_tar_gz(members, archive_path, archive_root, 1_800_000_000)
    return archive_path


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

            # A core.autocrlf checkout rewrites the committed LF text as CRLF.
            (asset_root / "upstream" / "LICENSE-APACHE").write_bytes(fallback_text.replace(b"\n", b"\r\n"))
            self.assertEqual(
                len(package_release.collect_package_license_files(empty_package, fallback_map, asset_root)), 1
            )

            (asset_root / "upstream" / "LICENSE-APACHE").write_bytes(b"changed after verification\n")
            with self.assertRaisesRegex(package_release.PackagingError, "SHA-256"):
                package_release.collect_package_license_files(empty_package, fallback_map, asset_root)

    def test_binary_files_with_licence_names_are_not_legal_text(self):
        with tempfile.TemporaryDirectory() as temp:
            crate = pathlib.Path(temp) / "registry" / "winapi-gnu-0.4.0"
            (crate / "lib").mkdir(parents=True)
            (crate / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
            (crate / "LICENSE-MIT").write_bytes(b"MIT text\r\n")
            (crate / "lib" / "libwinapi_oemlicense.a").write_bytes(b"!<arch>\n\x00\x00\x01")
            package = {
                "name": "winapi-gnu",
                "version": "0.4.0",
                "source": "registry+https://example.invalid/index",
                "license": "MIT",
                "license_file": None,
                "manifest_path": str(crate / "Cargo.toml"),
            }
            documents = package_release.collect_package_license_files(package, {}, pathlib.Path(temp))
            self.assertEqual(
                [item["archive_path"] for item in documents],
                ["dependency-notices/winapi-gnu-0.4.0/LICENSE-MIT"],
            )

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
            for skill_root in (".agents", ".claude"):
                skill = root / skill_root / "skills" / "release" / "NOTICE.md"
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

    def test_combined_licence_file_leads_with_notices_and_normalises_text(self):
        combined = package_release.third_party_licenses(
            {
                "THIRD_PARTY_NOTICES.md": b"third party\r\n",
                "COPYRIGHT": b"sorts ahead of NOTICE\n",
                "NOTICE": b"gp64 notice\n",
                "licenses/vga-MIT.txt": b"caf\xe9 MIT\n",
            },
            [dependency_document("demo", "1.0.0", "LICENSE", b"demo\r\nMIT\r\n")],
        ).decode("utf-8")
        self.assertEqual(
            section_titles(combined),
            [
                "NOTICE",
                "THIRD_PARTY_NOTICES.md",
                "COPYRIGHT",
                "licenses/vga-MIT.txt",
                "Dependency licence text 1 of 1",
            ],
        )
        self.assertNotIn("\r", combined)
        self.assertIn("demo\nMIT\n", combined)
        self.assertIn("café MIT", combined)

    def test_identical_dependency_texts_are_printed_once_with_every_crate(self):
        apache = b"Apache License\nVersion 2.0, January 2004\n\nTERMS AND CONDITIONS\n"
        rewrapped = b"   Apache License\r\n   Version 2.0, January 2004\r\n\r\n   TERMS AND\r\n   CONDITIONS\r\n"
        combined = package_release.third_party_licenses(
            {"NOTICE": b"gp64 notice\n", "THIRD_PARTY_NOTICES.md": b"third party\n"},
            [
                dependency_document("zeta", "2.0.0", "LICENSE-APACHE", apache),
                dependency_document("alpha", "1.0.0", "LICENSE-APACHE", rewrapped),
                dependency_document(
                    "beta", "0.3.0", "LICENSE-APACHE", apache, source_url="https://example.invalid/beta/LICENSE"
                ),
                dependency_document("alpha", "1.0.0", "LICENSE-MIT", b"MIT License\nCopyright (c) Alpha\n"),
                dependency_document("zeta", "2.0.0", "LICENSE-MIT", b"MIT License\nCopyright (c) Zeta\n"),
            ],
        ).decode("utf-8")
        self.assertEqual(combined.count("TERMS AND"), 1)
        self.assertEqual(combined.count("MIT License"), 2)
        self.assertEqual(
            section_titles(combined)[2:],
            ["Dependency licence text {} of 3".format(index) for index in (1, 2, 3)],
        )
        self.assertIn(
            "Applies to:\n"
            "  alpha 1.0.0: LICENSE-APACHE\n"
            "  beta 0.3.0: LICENSE-APACHE from https://example.invalid/beta/LICENSE\n"
            "  zeta 2.0.0: LICENSE-APACHE\n",
            combined,
        )
        self.assertIn("Applies to:\n  alpha 1.0.0: LICENSE-MIT\n\nMIT License\nCopyright (c) Alpha", combined)
        self.assertIn("Applies to:\n  zeta 2.0.0: LICENSE-MIT\n\nMIT License\nCopyright (c) Zeta", combined)

    def test_only_normal_dependencies_of_gp64_are_linked(self):
        def package(name, source="registry+https://example.invalid/index"):
            return {"id": name + "-id", "name": name, "version": "1.0.0", "source": source}

        def edge(name, *kinds):
            return {"pkg": name + "-id", "dep_kinds": [{"kind": kind, "target": None} for kind in kinds]}

        metadata = {
            "packages": [
                package("gp64", source=None),
                package("gp64-cpu", source=None),
                package("gp64-hygiene", source=None),
                package("shared"),
                package("transitive"),
                package("bench-only"),
                package("build-only"),
                package("both-normal-and-dev"),
            ],
            "resolve": {
                "nodes": [
                    {"id": "gp64-id", "deps": [
                        edge("gp64-cpu", None),
                        edge("bench-only", "dev"),
                        edge("build-only", "build"),
                        edge("both-normal-and-dev", "dev", None),
                    ]},
                    {"id": "gp64-cpu-id", "deps": [edge("shared", None)]},
                    {"id": "gp64-hygiene-id", "deps": [edge("shared", None)]},
                    {"id": "shared-id", "deps": [edge("transitive", None)]},
                    {"id": "transitive-id", "deps": []},
                    {"id": "bench-only-id", "deps": [edge("transitive", None)]},
                    {"id": "build-only-id", "deps": []},
                    {"id": "both-normal-and-dev-id", "deps": []},
                ]
            },
        }
        linked = package_release.linked_packages(metadata)
        self.assertEqual(
            sorted(item["name"] for item in linked["packages"]),
            ["both-normal-and-dev", "gp64", "gp64-cpu", "shared", "transitive"],
        )
        with self.assertRaisesRegex(package_release.PackagingError, "did not resolve"):
            package_release.linked_packages({"packages": metadata["packages"][1:], "resolve": metadata["resolve"]})
        with self.assertRaisesRegex(package_release.PackagingError, "did not resolve"):
            package_release.linked_packages(
                {"packages": metadata["packages"], "resolve": {"nodes": metadata["resolve"]["nodes"][1:]}}
            )

    def test_release_archives_are_deterministic_with_exactly_three_members(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            for platform_name in ("windows-x86_64", "macos-arm64"):
                first = create_release_archive(base / "first", platform_name)
                second = create_release_archive(base / "second", platform_name)
                self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(base / "first" / "gp64-v0.1.0-windows-x86_64.zip") as archive:
                self.assertEqual(
                    archive.namelist(),
                    [
                        "gp64-v0.1.0-windows-x86_64/gp64.exe",
                        "gp64-v0.1.0-windows-x86_64/LICENSE",
                        "gp64-v0.1.0-windows-x86_64/THIRD_PARTY_LICENSES.txt",
                    ],
                )
                self.assertEqual(archive.read("gp64-v0.1.0-windows-x86_64/LICENSE"), GPL_TEXT)
                self.assertEqual(
                    archive.getinfo(archive.namelist()[0]).date_time, package_release.zip_timestamp(1_800_000_000)
                )
            with tarfile.open(base / "first" / "gp64-v0.1.0-macos-arm64.tar.gz", mode="r:gz") as archive:
                members = archive.getmembers()
            self.assertEqual(
                [(member.name, member.mode) for member in members],
                [
                    ("gp64-v0.1.0-macos-arm64/gp64", 0o755),
                    ("gp64-v0.1.0-macos-arm64/LICENSE", 0o644),
                    ("gp64-v0.1.0-macos-arm64/THIRD_PARTY_LICENSES.txt", 0o644),
                ],
            )
            for member in members:
                self.assertEqual((member.mtime, member.uid, member.gid, member.uname, member.gname), (1_800_000_000, 0, 0, "", ""))

    def test_output_must_be_outside_checkout_and_existing_assets_are_preserved(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            repo = base / "repo"
            repo.mkdir()
            output = repo / "release"
            with self.assertRaises(package_release.PackagingError):
                package_release.validate_output_directory(output, repo)

            output.mkdir()
            existing = output / "gp64-v0.1.0-windows-x86_64.zip"
            existing.write_bytes(b"existing user output")
            with self.assertRaises(package_release.PackagingError):
                package_release.ensure_outputs_available(output, [existing.name])
            self.assertEqual(existing.read_bytes(), b"existing user output")

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
            "https://github.com/0x4D44/gp64",
            "git@github.com:0x4D44/gp64.git",
            "git@github.com:0x4D44/gp64",
            "ssh://git@github.com/0x4D44/gp64.git",
        ):
            self.assertTrue(package_release._is_official_public_origin(public_origin), public_origin)
        for non_public_origin in (
            "https://github.com/0x4D44/gp64-private.git",
            "https://github.com/0x4D44/gp64-private",
            "https://github.com/0x4D44/gp64.git.evil",
            "https://github.com/0x4D44/gp64/",
            "https://github.com.attacker/0x4D44/gp64.git",
        ):
            self.assertFalse(package_release._is_official_public_origin(non_public_origin), non_public_origin)
        with tempfile.TemporaryDirectory() as temp:
            repo, revision = write_fixture_git_repo(temp)
            private_url = "https://github.com/0x4D44/gp64-private.git"
            subprocess.run(
                ["git", "-C", str(repo), "remote", "set-url", "origin", private_url], check=True
            )
            with self.assertRaisesRegex(package_release.PackagingError, "official public repository") as raised:
                package_release.validate_checkout(repo, "v0.1.0", revision)
            self.assertNotIn(private_url, str(raised.exception))

    def test_foreign_release_archives_verify_without_host_execution(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            for platform_name in ("windows-x86_64", "macos-arm64"):
                archive = create_release_archive(base / platform_name, platform_name)
                result = package_release.verify_binary_archive(archive, "v0.1.0", platform_name)
                self.assertEqual(result["target"], package_release.SUPPORTED_HOSTS[platform_name].target)
                self.assertEqual(result["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())
            mac_archive = base / "macos-arm64" / "gp64-v0.1.0-macos-arm64.tar.gz"
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                status = package_release.main(
                    ["verify-binary", "--archive", str(mac_archive), "--tag", "v0.1.0", "--platform", "macos-arm64"]
                )
            self.assertEqual(status, 0)
            receipt = json.loads(output.getvalue())
            self.assertEqual(receipt["status"], "verified")
            self.assertEqual(receipt["archive"], mac_archive.name)

    def test_verification_rejects_wrong_platform_name_and_architecture(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            archive = create_release_archive(base / "name", "windows-x86_64")
            with self.assertRaisesRegex(package_release.PackagingError, "basename"):
                package_release.verify_binary_archive(archive, "v0.2.0", "windows-x86_64")
            wrong_arch = create_release_archive(
                base / "arch",
                "windows-x86_64",
                members=[
                    ("gp64.exe", pe_image(machine=0x014C), 0o755),
                    ("LICENSE", GPL_TEXT, 0o644),
                    ("THIRD_PARTY_LICENSES.txt", licence_file(), 0o644),
                ],
            )
            with self.assertRaisesRegex(package_release.PackagingError, "x86-64"):
                package_release.verify_binary_archive(wrong_arch, "v0.1.0", "windows-x86_64")

    def test_verification_requires_exactly_the_executable_and_licences(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            missing = create_release_archive(
                base / "missing",
                "windows-x86_64",
                members=[("gp64.exe", pe_image(), 0o755), ("LICENSE", GPL_TEXT, 0o644)],
            )
            with self.assertRaisesRegex(package_release.PackagingError, "exactly"):
                package_release.verify_binary_archive(missing, "v0.1.0", "windows-x86_64")
            extra = create_release_archive(
                base / "extra",
                "macos-arm64",
                members=[
                    ("gp64", arm64_macho(), 0o755),
                    ("LICENSE", GPL_TEXT, 0o644),
                    ("THIRD_PARTY_LICENSES.txt", licence_file(), 0o644),
                    ("README.md", b"extra\n", 0o644),
                ],
            )
            with self.assertRaisesRegex(package_release.PackagingError, "exactly"):
                package_release.verify_binary_archive(extra, "v0.1.0", "macos-arm64")
            wrong_licence = create_release_archive(
                base / "licence",
                "windows-x86_64",
                members=[
                    ("gp64.exe", pe_image(), 0o755),
                    ("LICENSE", b"MIT License\n", 0o644),
                    ("THIRD_PARTY_LICENSES.txt", licence_file(), 0o644),
                ],
            )
            with self.assertRaisesRegex(package_release.PackagingError, "GPL"):
                package_release.verify_binary_archive(wrong_licence, "v0.1.0", "windows-x86_64")

    def test_macos_verification_requires_executable_member_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            archive = create_release_archive(pathlib.Path(temp), "macos-arm64", binary_mode=0o644)
            with self.assertRaisesRegex(package_release.PackagingError, "not executable"):
                package_release.verify_binary_archive(archive, "v0.1.0", "macos-arm64")

    def test_packaging_builds_a_verifiable_archive_for_each_platform(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            repo, revision = write_fixture_git_repo(base)
            for platform_name in ("windows-x86_64", "macos-arm64"):
                host = package_release.SUPPORTED_HOSTS[platform_name]
                binary = base / platform_name / host.binary_name
                binary.parent.mkdir()
                binary.write_bytes(native_binary(platform_name))
                output_dir = base / "assets"
                with mock.patch.object(package_release, "detect_host", return_value=host):
                    files = package_release.package_binary(repo, binary, "v0.1.0", revision, output_dir)
                self.assertEqual(files, [package_release.release_archive_name("v0.1.0", host)])
                archive = output_dir / files[0]
                package_release.verify_binary_archive(archive, "v0.1.0", platform_name)
                root = files[0][: -len(host.archive_extension)]
                if host.archive_extension == ".zip":
                    with zipfile.ZipFile(archive) as packaged:
                        self.assertEqual(packaged.read(root + "/LICENSE"), GPL_TEXT)
                else:
                    with tarfile.open(archive, mode="r:gz") as packaged:
                        self.assertEqual(packaged.extractfile(root + "/LICENSE").read(), GPL_TEXT)
                with mock.patch.object(package_release, "detect_host", return_value=host):
                    with self.assertRaisesRegex(package_release.PackagingError, "overwrite"):
                        package_release.package_binary(repo, binary, "v0.1.0", revision, output_dir)


if __name__ == "__main__":
    unittest.main()
