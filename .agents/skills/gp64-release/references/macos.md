# Apple Silicon macOS build

Use native arm64 macOS with stable Rust's `aarch64-apple-darwin` target, Xcode
Command Line Tools, GitHub CLI and Python 3.9+. Use the clean public-tag worktree
selected by SKILL.md. Intel or a Rosetta-only build environment is not this
release target. Build/package output belongs in registered scratch outside the
checkout.

Confirm `uname -m` reports `arm64`, `xcrun --find clang` succeeds, and the Rust
target is installed. Set a task-specific Cargo target directory in scratch.
Release builds omit debug tables and strip symbols; keep the crate's optimization
and panic settings. Remap the source checkout and Cargo registry paths with
`RUSTFLAGS` to neutral labels so local paths do not enter the distributable.
Preserve existing flags or report conflicting ones.

```sh
export CARGO_TARGET_DIR="$TARGET_DIRECTORY"
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_PROFILE_RELEASE_STRIP=symbols
deltic timeout 1800 cargo build --release --locked -p gp64 --target aarch64-apple-darwin </dev/null
binary="$TARGET_DIRECTORY/aarch64-apple-darwin/release/gp64"
"$binary" --version
"$binary" --help
file "$binary"
otool -L "$binary"
```

Check every exit status and compare the reported version with the tag. `file`
must identify an arm64 executable. Retain the `otool` dependency list in build
evidence. Only Apple system frameworks/libraries are expected; an unexplained
Homebrew or other host dylib blocks a portable package until accounted for.

```sh
python3 .agents/skills/gp64-release/scripts/package_release.py binary \
  --repo "$BUILD_WORKTREE" --binary "$binary" --tag "$TAG" \
  --revision "$SOURCE_SHA" --output-dir "$ASSET_DIRECTORY"
python3 .agents/skills/gp64-release/scripts/package_release.py verify-binary \
  --archive "$ASSET_DIRECTORY/gp64-$TAG-macos-arm64.tar.gz" --tag "$TAG" \
  --platform macos-arm64
```

The tarball holds `gp64`, `LICENSE` and `THIRD_PARTY_LICENSES.txt` and preserves
the executable bit. No Developer ID signing,
notarization, `.app` bundle or universal/Intel binary is implied. The linker may
apply the ad-hoc signature arm64 executables need; that does not make this a
signed/notarized distribution. Do not disable system security or clear quarantine
as part of validation. Report any Gatekeeper limitation actually observed.
