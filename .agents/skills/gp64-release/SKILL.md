---
name: gp64-release
description: Build, stage, publish, or resume gp64 release assets for the current Windows x64 or Apple Silicon macOS host. Use for releases of 0x4D44/gp64, including attaching the other platform; not for private audit publication or other applications.
---

# Release gp64 on this platform

Target **`0x4D44/gp64`**, the public repository. The private development and audit
repository is **`0x4D44/gp64-private`**. Never release, tag, or upload its history.
An instruction to author or explain this skill is not an instruction to run a
release. Require an explicit release instruction before remote writes; it covers
the version commit, its push to `main`, the tag, the draft and the uploads.
Publishing needs the instruction to cover publication too.

This workflow uses local native builds, Git, Cargo, Python 3.9+ standard library
and GitHub CLI. It adds no hosted CI. Builds are unsigned; macOS linker ad-hoc
signatures are not Developer ID signing or notarization.

| Current host | Rust target | Release asset |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | `gp64-v<version>-windows-x86_64.zip` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `gp64-v<version>-macos-arm64.tar.gz` |

Each archive holds exactly three files: the executable (`gp64.exe` or `gp64`),
`LICENSE` and `THIRD_PARTY_LICENSES.txt`. There are no checksum, build-info or
source assets: GitHub attaches the tag's source archives, and `Cargo.lock` pins
every dependency. Linux, Intel macOS and Windows ARM64 are outside this workflow.
Build only this host's archive; either platform may cut the release. Read only
the matching platform reference: [Windows](references/windows.md) or
[macOS](references/macos.md).

## 1. Resume or cut the version

Read the actual public repository identity, its remote tags
(`git ls-remote --tags origin "refs/tags/v*"`) and releases
(`gh release list --repo 0x4D44/gp64`). Do not rely on the clone's directory name:
this fleet's original `gp64` workspace can point to `gp64-private`. Leave main
clones on trunk and preserve dirty work.

**Resume** when the caller names an existing tag, or the newest tag has a draft
release still missing a platform archive. That tag's commit is the source; a newer
`main` never changes it. Skip to step 2. To ship new changes under an already-used
version, stop: a new version is needed. Never move, delete, force-push or reuse a tag.

**Cut** otherwise. The previous release is the highest stable `v*` tag. List what
changed since it on clean `origin/main`:

```sh
git log --no-merges --format="%h %s" <previous-tag>..origin/main
git diff --stat <previous-tag> origin/main
```

No commits means nothing to release: stop and say so. Otherwise choose the bump
from those changes, prefer the diff where a commit title oversells, and never
under-bump:

| Change | From 1.0.0 | Before 1.0.0 |
| --- | --- | --- |
| Breaks saved settings, command-line options or documented behaviour | major | minor |
| New user-visible capability, compatible | minor | minor |
| Fixes, performance, internal work, docs, packaging | patch | patch |

A version the caller names wins over this table. It must be greater than the
previous tag. State the chosen version and the reason in one line, then continue.

Write the release note `releases/<version>.md` (format below) from the log and
diff. Then, in a task worktree off `origin/main`
(`deltic start-task "release gp64 <version>"`):

1. Set `version` under `[workspace.package]` in the root `Cargo.toml`, then sync the
   lock file with `cargo update --workspace --offline`. Only workspace entries
   in `Cargo.lock` may change.
2. Add the release note, run `cargo fmt --all -- --check` and
   `cargo build --locked -p gp64`, and commit both files as `Release gp64 <version>`.
3. Integrate from the main clone: `deltic integrate --worktree "<path>" --push`.
   Note the pushed commit and check that its `Cargo.toml` carries the new version.
4. Tag that exact commit: `git tag -a v<version> -m "gp64 <version>" <sha>`, then
   `git push origin v<version>`. If the push is rejected, another run cut the version
   first. Resolve its tag. The same source means resume; a different source is a
   version conflict, not permission to overwrite.
5. Create the draft immediately:
   `gh release create v<version> --repo 0x4D44/gp64 --verify-tag --draft --title "gp64 <version>" --notes-file releases/<version>.md`.
   Only the run that pushed the tag creates the draft. If a tag has no draft, inspect
   before acting; never create duplicates or delete releases automatically.

## 2. Build this platform

Create a build worktree at the tag:
`deltic start-task "build gp64 <tag> <platform>" --repo <public-clone> --base <tag>`.
Verify a clean tree, its full commit SHA and the remote tag again. Keep build and
package output in registered scratch outside the checkout. Use that tag's helper.

Run `cargo fetch --locked --target <rust-target>` in the worktree, then the native
build and smoke checks from the platform reference. Package with the helper:

```sh
python3 .agents/skills/gp64-release/scripts/package_release.py binary --repo <worktree> --binary <exe> --tag <tag> --revision <sha> --output-dir <scratch-dir>
python3 .agents/skills/gp64-release/scripts/package_release.py verify-binary --archive <archive> --tag <tag> --platform <platform>
```

`binary` refuses a dirty checkout, a wrong origin, tag, revision or version, a
wrong architecture, and a dependency without licence text.
`THIRD_PARTY_LICENSES.txt` lists the repository notices and every licence text for
crates Cargo resolves for this target. Record the native library dependencies from
`dumpbin`/`otool`. An unexplained non-system dependency blocks the release until
its redistribution and runtime requirements are accounted for.

Before upload, list the draft's assets. Upload with
`gh release upload <tag> <archive> --repo 0x4D44/gp64` **without `--clobber`**.
An asset already present for this host is a completed step: download it and run
`verify-binary` instead of rebuilding. After an uncertain upload, download and
verify before retrying. A conflicting asset stops the release; never replace one
silently.

## 3. Finish or hand off

A complete release has both platform archives. Download each and run
`verify-binary`. This also checks the other platform's archive without running
its binary. If the other platform is missing, leave the draft and report this
platform staged and the other pending. The next run resumes the same tag.

When both archives verify, publish that exact draft if the release instruction
covers publication: `gh release edit <tag> --repo 0x4D44/gp64 --draft=false`.
Re-read visibility, the tag commit and the asset digests afterwards. A published
release that already has this host's verified archive is complete: report it,
don't mutate it.

Land the internal journal through the private docs lane, retire build worktrees
and remove this run's scratch. Report the tag, source SHA, release URL, assets,
the checks you observed and whether it is published or waiting for the other
platform. Preserve uploaded assets and interrupted work; retries inspect state.

## Release note format

`releases/<version>.md` is the GitHub release body. Write it for players, from
the log and diff. Lead with the effect, not the mechanism. Never invent a change.
Skip pure internals unless they change what a player sees. Use one line per item
and omit empty sections. No proprietary media, captures, host secrets, private
audit records or bug-ledger IDs.

```markdown
# gp64 <version> — <YYYY-MM-DD>

<One or two sentences: what this release is about.>

## Added
## Fixed
## Changed

## Downloads

- Windows x64: `gp64-v<version>-windows-x86_64.zip`
- Apple Silicon macOS: `gp64-v<version>-macos-arm64.tar.gz`

Each archive holds the `gp64` executable (`gp64.exe` on Windows), `LICENSE`
(GPL-3.0) and `THIRD_PARTY_LICENSES.txt`. Builds are unsigned: macOS is not
Developer ID signed or notarized; Windows is not Authenticode signed.

## Source

gp64 is licensed under GPL-3.0-or-later. The Corresponding Source for these
binaries is this repository at tag `v<version>` (GitHub's source archives below),
together with the crates pinned by version and checksum in its `Cargo.lock`,
available from crates.io. Build with `cargo build --release --locked -p gp64`.
```
