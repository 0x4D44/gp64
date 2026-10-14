---
name: gp64-release
description: Build, stage, publish, or resume gp64 release assets for the current Windows x64 or Apple Silicon macOS host. Use for releases of 0x4D44/gp64, including attaching the other platform; not for private audit publication or other applications.
---

# Release gp64 on this platform

Target **`0x4D44/gp64`**, the public repository. The private development and audit
repository is **`0x4D44/gp64-private`**. Never release, tag, or upload its history.
An instruction to author or explain this skill is not an instruction to run a
release. Require an explicit release/staging instruction before remote writes.

This workflow uses local native builds, Git, Cargo, Python 3.9+ standard library
and GitHub CLI. It adds no hosted CI. Packages are unsigned; macOS linker ad-hoc
signatures are not Developer ID signing or notarization.

| Current host | Rust target | Platform asset |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | `gp64-<tag>-windows-x86_64.zip` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `gp64-<tag>-macos-arm64.tar.gz` |

Linux, Intel macOS and native Windows ARM64 are outside this release workflow.
Build only this host's package. Either platform may initialize the release;
there is no Windows-first rule. Read only the matching platform reference:
[Windows](references/windows.md) or [macOS](references/macos.md).

## Pin the shared release before building

1. Read the actual public repository identity, remote main and existing tag/draft.
   Do not rely on the current clone's directory name. This fleet's original
   `gp64` workspace can point to `gp64-private`; use a separate public clone if
   necessary. Leave existing main clones on trunk and preserve dirty work.
2. Use the caller's tag/version if named. Otherwise use the frontend `gp64`
   package version from locked Cargo metadata. The workspace stays `0.1.0`
   until Arthur selects a new version. Do not bump versions in this workflow.
   Tags use `v<version>` and must match the version at their source commit.
3. **Existing remote tag:** resolve its commit and resume it. A newer main never
   changes that release's source. If the request is to ship new changes under an
   already-used version, stop and explain that a new integrated version is needed.
4. **New tag:** fetch and inspect clean public `origin/main`, verify the version,
   and create/push one annotated tag at that exact commit. Inspect the remote
   after an unclear failure. Never move, delete, force-push or reuse a tag.
   If another initializer wins, resolve its tag and verify the same source;
   a different source is a version conflict, not permission to overwrite it.
5. Initialize one GitHub draft immediately after the new tag. Use
   `gh release create <tag> --repo 0x4D44/gp64 --verify-tag --draft`
   with a title and notes file. The process that wins tag creation owns draft
   initialization. Other runs reuse that draft rather than racing to create one.
   If a tag exists without a draft, inspect the initializer's state: resume its
   failed initialization when authorized, or report the short initialization
   gap. Do not blindly create duplicate drafts. Duplicate or ambiguous releases
   require reconciliation before upload; do not delete them automatically.
6. Create an isolated build worktree based on the resolved public tag, for example
   `deltic start-task "build gp64 <tag> <platform>" --repo <public-clone> --base <tag>`.
   Verify a clean tree, its full commit SHA and the remote tag again. Keep outputs
   in registered scratch outside the checkout. Use that tag's packaging helper.

## Build and stage this platform

Run the native build and smoke checks from the platform reference. Then use
`scripts/package_release.py binary` to package the executable, legal texts,
dependency notices, source identity, compiler identity and checksum. Pass the
resolved full SHA, tag, binary and an output directory outside the checkout.
Do not accept a wrong architecture or an executable reporting another version.

The first successful platform run also makes `gp64-<tag>-source.tar.gz` with
`scripts/package_release.py source`. It contains the tagged application source,
locked vendored dependencies and offline Cargo configuration. This source asset
and its checksum must accompany binary distribution. If it already exists,
download and verify it instead of regenerating or replacing it.

For each platform stage these three assets:

- Platform archive.
- `<archive>.sha256`.
- `<archive>.buildinfo.json`.

The source archive has its own `.sha256`. Exact filenames come from the helper's
JSON result. Use `gh release upload <tag> <files> --repo 0x4D44/gp64` **without
`--clobber`**. Before upload, inspect existing names. An already verified asset
for this tag is a completed step, not a reason to rebuild. For an uncertain
upload result, re-read and verify the hosted asset before retrying. A conflicting
asset, source SHA or checksum stops the release; never silently replace it.

## Finish or hand off

Download and verify hosted assets using the helper's `verify-binary` and
`verify-source` commands. These may inspect the other platform's archive without
executing its binary.
Require the same tag, source commit, version and declared architecture throughout.
Record the actual native library dependencies from `dumpbin`/`otool`; system
libraries are expected. An unexplained non-system dependency blocks packaging
until its redistribution and runtime requirements are accounted for.

A complete release requires **both platform archives, both checksum/build-info
pairs, and the source archive/checksum**. If the other platform is absent, leave
the draft intact and report this platform staged with the other platform pending.
The next invocation resumes the same tag; it does not cut another version.

When all eight assets are verified, publish that exact draft if the user's
release instruction covers publication. Use
`gh release edit <tag> --repo 0x4D44/gp64 --draft=false`. Re-read visibility,
tag commit and asset identities afterward. A published release with this host's
verified assets is already complete: report it rather than mutating it.

Release notes name Windows x64 and Apple Silicon macOS, unsigned status, source
bundle, checksums and observed checks. Distinguish CLI/build checks from native
window/audio acceptance. No proprietary media, captures, host secrets or private
audit records enter archives or notes.

Land the internal journal through the private docs lane, then retire build
worktrees and remove this run's scratch. Report tag, source SHA, release URL,
platform assets and whether the release is published or waiting for the other
platform. Preserve uploaded assets and interrupted work; retries inspect state.
