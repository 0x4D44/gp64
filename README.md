# gp64

Run Geoff Crammond's original Formula One Grand Prix (F1GP) on Windows 11 and macOS.
Copy gp64 into your existing game folder and run it. The game starts as it
did on a DOS PC, with its own graphics, sound, controls and modem play.

gp64 runs your own unmodified copy by emulation, not decompilation. Under the
hood it is a cut-down DOS emulator in Rust: a real-mode PC, DOS and BIOS
services, VGA video, keyboard and mouse, PC speaker and AdLib audio, and a
serial modem over direct TCP. No DOS installation or ROM image is required.

This is a personal project with irregular maintenance. Contributions and pull
requests are not accepted; forks are welcome under GPL-3.0-or-later. There is no
promise of support, fixes or future releases. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Drop-in launch

Copy gp64 into your own F1GP folder and run it. It starts the normal batch launcher, or the executable if the batch file is absent.

The folder is mounted as writable drive C:, so the program can save its own
settings and progress. An explicit program path mounts that program's folder
instead. Recording starts only when requested with `--record` or from the
F12 menu. Menu recordings go to the user's video folder, with a fallback when
that folder is unavailable.

```sh
./gp64
./gp64 --mute
./gp64 /path/to/program.exe argument
```

## Options and controls

```text
gp64 [--mute] [--record FILE] [--host PORT | --join HOST:PORT]
     [--com 1|2] [program [args]]
```

- `--mute`: disable host audio output.
- `--record FILE`: record video and audio to an MKV/WebM file, finalised on exit.
- `--host PORT`: listen for a direct TCP modem connection.
- `--join HOST:PORT`: connect to a TCP modem host; mutually exclusive with `--host`.
- `--com 1|2`: choose the emulated modem port; default COM2.
- `--help` and `--version`: print help or version.

For multiplayer, start hosting from F12 → Modem → Host Port on one machine.
On the other, open F12 → Modem → LAN Hosts and select that host, then use the
program's serial-modem setup. Hosts advertise automatically on the local IPv4
network. Discovery uses UDP ports 23517–23520; the modem uses the chosen TCP
host port. Direct `--host 1234` and `--join hostname:1234` also work.

- F12 opens the host menu: scaler, fullscreen, mute, recording and a modem
  setup submenu with line status, COM port, host port, LAN hosts and join address.
  Enter opens modem setup; Escape goes back and F12 closes the menu.
- F11 cycles integer nearest-neighbour, smooth, area and CRT scaling, with 4:3 aspect correction.
- Alt+Enter toggles fullscreen.
- Click inside the window to capture the mouse; Ctrl+F10 releases it.
- Close the window to exit.

## Limits

The CPU runs real-mode DOS programs through an interpreter. Its rate adjusts
automatically to the fastest sustainable host throughput while timers, video
and audio retain normal emulated timing. Saved CPU speed settings from older
versions are ignored. Conventional memory
and XMS are provided; EMS and DPMI are not. VGA supports standard text and
256-colour modes, including mode 13h and Mode X. The gameport has no controller
attached. AdLib percussion is currently inert.

Sound Blaster support is limited to FM audio and a minimal DSP interface;
digitised audio playback is not emulated. Choose AdLib or PC speaker for audio.
MT-32, GUS, CMS, EMU8000 and MPU synthesis are not emulated. Win16, Win32, other
machines, SVGA chipsets, composite CGA, PCI, ATA/ATAPI, NE2000 networking, JIT
compilation, gamepads, emulator netplay and save states are outside this version.
Menu preferences persist in the user's configuration folder. Only a host folder
can be mounted, not disk or ISO images.

## Build from source

Install a current stable Rust toolchain and the platform's native C/linker tools.
On Linux, the window and audio dependencies require development packages for
ALSA and the X11/Wayland libraries selected by winit and softbuffer.

```sh
cargo build --release --locked
cargo test --workspace --locked
cargo test -p gp64-hygiene --locked
```

The executable is `target/release/gp64` (`gp64.exe` on Windows). The intended
platforms are macOS, Windows and Linux, on ARM64 and x86-64. Verification varies
by target; an intended platform is not a guarantee of tested desktop operation.
See [DEVELOPMENT.md](DEVELOPMENT.md) for native tool requirements, checks and
optional external-input tests, and [SECURITY.md](SECURITY.md) for security reports.

## Licence and non-affiliation

gp64 is GPL-3.0-or-later; see [LICENSE](LICENSE), [NOTICE](NOTICE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

No game files are included. You must own the game. gp64 is not affiliated with or endorsed by the rights holders of F1GP (Formula One Grand Prix).
