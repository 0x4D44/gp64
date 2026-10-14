# Windows x64 build

Use an x64 Windows host with stable Rust's `x86_64-pc-windows-msvc` target,
Visual Studio C++ x64 tools, Windows SDK, GitHub CLI and Python 3.9+. Use the
public-tag worktree selected by SKILL.md, with build/package output in registered
scratch outside it. PowerShell 5.1 is supported; inspect each native command's
`$LASTEXITCODE`, and write text files with `-Encoding utf8`.

Locate the installed toolchain with Visual Studio Installer's `vswhere.exe`:

```powershell
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or -not $vs) { throw 'Visual Studio x64 tools unavailable' }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvarsall.bat'
```

Set a task-specific Cargo target directory in scratch. Release builds omit debug
tables and strip symbols; keep the crate's optimization and panic settings.
Set `RUSTFLAGS` to remap the source checkout and Cargo registry paths to neutral
labels so local paths do not enter the distributable, and add
`-C target-feature=+crt-static` so the executable does not need the Visual C++
runtime (`VCRUNTIME140.dll`) on the player's machine. Preserve existing flags or
report conflicting ones rather than silently changing the build.

```powershell
if ($env:RUSTFLAGS) { throw "RUSTFLAGS already set: $env:RUSTFLAGS" }
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$env:RUSTFLAGS = "--remap-path-prefix=$BuildWorktree=/gp64 --remap-path-prefix=$cargoHome=/cargo -C target-feature=+crt-static"
```

Run `vcvarsall.bat x64` and Cargo in the **same cmd.exe process**. Calling the
batch file in a child process and Cargo later in its parent loses the toolchain
environment. Do not import or print `cmd set`; it can disclose unrelated secrets.

```powershell
$env:CARGO_TARGET_DIR = $TargetDirectory
$env:CARGO_PROFILE_RELEASE_DEBUG = '0'
$env:CARGO_PROFILE_RELEASE_STRIP = 'symbols'
$build = 'call "' + $vcvars + '" x64 >nul && deltic timeout 1800 cargo build --release --locked -p gp64 --target x86_64-pc-windows-msvc <nul'
& $env:ComSpec /d /s /c $build
if ($LASTEXITCODE -ne 0) { throw 'gp64 Windows release build failed' }
$binary = Join-Path $TargetDirectory 'x86_64-pc-windows-msvc\release\gp64.exe'
& $binary --version
if ($LASTEXITCODE -ne 0) { throw 'Version smoke failed' }
& $binary --help
if ($LASTEXITCODE -ne 0) { throw 'Help smoke failed' }
```

Verify the version output against the tag. Inspect the PE machine as x86-64 and
`dumpbin /DEPENDENTS` using the same VC environment. Retain the import list in the
build evidence. Expect only Windows system DLLs: no `VCRUNTIME140.dll` and no
`api-ms-win-crt-*` entries, which would mean the static C runtime flag was lost.
Account for every other non-system DLL before distribution; do not bundle the
Visual Studio installation or arbitrary host DLLs.

Package from the clean tag worktree:

```powershell
python .agents/skills/gp64-release/scripts/package_release.py binary --repo $BuildWorktree --binary $binary --tag $Tag --revision $SourceSha --output-dir $AssetDirectory
if ($LASTEXITCODE -ne 0) { throw 'Package failed' }
$archive = Join-Path $AssetDirectory "gp64-$Tag-windows-x86_64.zip"
python .agents/skills/gp64-release/scripts/package_release.py verify-binary --archive $archive --tag $Tag --platform windows-x86_64
if ($LASTEXITCODE -ne 0) { throw 'Verify failed' }
```

Do not create signing or installer claims: this is an unsigned portable ZIP
containing `gp64.exe`, `LICENSE` and `THIRD_PARTY_LICENSES.txt`. Running it from
the player's program folder remains the normal launch behavior.
