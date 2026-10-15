# Building and testing

These instructions are for people building gp64 or maintaining a fork.
The upstream [maintenance policy](CONTRIBUTING.md) applies to this repository.

## Prerequisites

Use a current stable Rust toolchain with Cargo, rustfmt and Clippy. Native
dependencies also need a C compiler and linker:

- macOS: install the Xcode Command Line Tools.
- Windows: install the Visual Studio C++ build tools and Windows SDK for an
  MSVC Rust target.
- Linux: install a C build toolchain, pkg-config, ALSA development headers, the
  X11/Wayland development libraries required by winit and softbuffer, and
  Fontconfig development files for Wayland title text. `freetype-sys` uses
  system FreeType when pkg-config finds `freetype2` version `24.3.18` or later.
  Otherwise it compiles the FreeType source included in the crate and needs a
  C++ compiler. A FreeType development package selects the system library path
  when it meets the version requirement.

Build on the target platform; cross-checking Rust code alone does not verify
the window, audio backend or native linker. Package names vary by distribution.

## Build

From the repository root:

```sh
cargo build --release --locked
```

The executable is `target/release/gp64` (`gp64.exe` on Windows). Keep
`Cargo.lock` when building a reproducible source revision.

## Checks without proprietary inputs

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p gp64-hygiene --locked
```

For a focused change, start with `cargo test -p <crate> --locked`.
The workspace includes synthetic instruction and device fixtures. The ordinary
checks do not require a DOS installation, ROM image or proprietary program.
Ignored tests are not included in an ordinary test run.

Hygiene checks scan tracked and non-ignored untracked files, reachable Git
history and commit messages. Run them in a Git checkout with the history you
intend to publish. A source archive alone cannot establish history hygiene.

## Optional external inputs

Set these variables locally and keep all inputs outside tracked files:

| Variable | Input |
| --- | --- |
| `GP64_GAME_DIR` | A folder containing your legally owned program and its launch files |
| `GP64_CPU_TESTS_DIR` | The generated real-mode 80386 v1 SingleStepTests suite |
| `GP64_AUDIT_LIST` | A private newline-separated denylist of hexadecimal segment:offset pairs |

Run these from a shell with the relevant variables set:

```sh
cargo test -p gp64-machine --locked --test owned_launch -- --ignored
cargo test -p gp64-machine --locked --test cpu_386_conformance -- --ignored
cargo test -p gp64-hygiene --locked -- --ignored
```

The owned-input checks skip when their inputs are unset. Check the output;
a skipped audit is not evidence that the private checks passed.
Do not commit owned media, captures, denylists or debugging notes containing
program-specific internals. Generated fixtures should demonstrate generic DOS
or hardware behaviour rather than encode one program's implementation.

## Measuring speed

gp64 logs guest speed every five seconds under `gp64::speed` in the diagnostics
log: guest MHz against the paced rate, the emulation thread's host load, and how
many displayed frames changed per second. Set `GP64_MAX_MHZ` to cap the automatic
rate, for example `GP64_MAX_MHZ=16`, to see how the program behaves on a slower
host. Script `log` lines land in the same file, so a script can mark the span to
read. Two scripted copies with `modem on` or `serial on` make a link test that
needs no one at the keyboard.

## Manual verification

On each platform you claim to support, check launch, display and scaling,
keyboard and mouse input, audio under load, recording finalisation, and modem
operation between two instances. Automated headless tests do not establish that
the native window or audio works correctly.
