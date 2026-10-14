# Third-party notices

gp64's first-party code is GPL-3.0-or-later; see NOTICE and LICENSE.
Third-party components retain their own copyright notices and licences.

## Embedded font

`crates/gp64-hw/src/vga/font_cp437_8x8.rs` and `font_cp437_8x16.rs` are
generated from `vga` 0.2.9, `src/fonts.rs`. Their source headers record the
dual licence **MIT OR Apache-2.0**. The font is embedded code/data, not merely a
behavioural reference. Retain its source headers when redistributing it.

The upstream licence texts, including their copyright notices, are included as
[MIT](licenses/vga-0.2.9-LICENSE-MIT.txt) and
[Apache-2.0](licenses/vga-0.2.9-LICENSE-APACHE.txt).
The arrays were converted from `TEXT_8X8_FONT` and `TEXT_8X16_FONT` at
[upstream commit b91c2286](https://github.com/rust-osdev/vga/blob/b91c2286cbd06ac4b618420037e1951975518877/src/fonts.rs).
Both upstream tables identify their font data as acquired from
[Graphics in pmode](https://www.singlix.com/trdos/archive/vga/Graphics%20in%20pmode.pdf).
The conversion changes the Rust declaration and formatting; the font bytes are
unchanged.

## Retained host directory code

`crates/gp64-types/src/host_dirs.rs` retains host-folder behavior formerly
provided by `dirs` 5.0.1. Its Unix account-home fallback and XDG video-directory
parser are derived from `dirs-sys` 0.4.1, as noted in the source comments.
Those derived portions are licensed under MIT, chosen from upstream's
`MIT OR Apache-2.0` offer. Their copyright and full MIT text are in
[HOST_DIRS_LICENSE.txt](crates/gp64-types/HOST_DIRS_LICENSE.txt); the reviewed
upstream source is [dirs-sys 0.4.1](https://github.com/dirs-dev/dirs-sys-rs/tree/f369f0904bec9833572f24c988c7e48454173983).

## Direct crates.io dependencies

Generated from `cargo metadata --locked --format-version 1`, including target,
development and build dependencies of every workspace member. SPDX expressions
are copied from each resolved package’s Cargo licence metadata.

| Package | Resolved version | Licence |
| --- | --- | --- |
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception |
| cpal | 0.17.3 | Apache-2.0 |
| criterion | 0.5.1 | Apache-2.0 OR MIT |
| iced-x86 | 1.21.0 | MIT |
| ieee-apsqrt | 0.1.1 | Apache-2.0 WITH LLVM-exception |
| libc | 0.2.190 | MIT OR Apache-2.0 |
| ropus | 0.11.1 | BSD-3-Clause |
| rustc_apfloat | 0.2.3+llvm-462a31f5a5ab | Apache-2.0 WITH LLVM-exception |
| rvp9-decoder | 0.2.0-alpha.3 | BSD-3-Clause |
| rvp9-encoder | 0.2.0-alpha.3 | BSD-3-Clause |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| softbuffer | 0.4.8 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| tracing | 0.1.44 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| winit | 0.30.13 | Apache-2.0 |

## Winit font backend

The Wayland title-text feature resolves these additional Cargo packages. The
licences are copied from the resolved package metadata; Cargo locks
crossfont's platform-specific dependencies and compiles only those selected
for the build target.

| Package | Resolved version | Licence |
| --- | --- | --- |
| core-text | 20.1.0 | MIT OR Apache-2.0 |
| crossfont | 0.8.1 | Apache-2.0 |
| cstr | 0.2.12 | MIT |
| dwrote | 0.11.5 | MPL-2.0 |
| freetype-rs | 0.36.0 | MIT |
| freetype-sys | 0.20.1 | MIT |
| winapi | 0.3.9 | MIT/Apache-2.0 |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| wio | 0.2.2 | MIT/Apache-2.0 |
| yeslogic-fontconfig-sys | 5.0.0 | MIT |

On Linux, `yeslogic-fontconfig-sys` uses `pkg-config` to find system Fontconfig
by default. Fontconfig remains a system dependency. Its upstream `COPYING` uses
the HPND-sell-variant licence for the main code and records separate notices
for included files; check the selected system package's notices when
redistributing Fontconfig.

The `freetype-sys` row above reports the Rust crate's MIT package metadata, but
the crate also contains FreeType source. Its build script uses system
`freetype2` when `pkg-config` finds version `24.3.18` or later. Otherwise it
compiles the included FreeType sources in C++ mode. The included
`freetype2/LICENSE.TXT` describes the upstream FreeType License (FTL) and
GPL-2.0-or-later choices, and notes separately licensed files. Their full
texts are included as `freetype2/docs/FTL.TXT` and
`freetype2/docs/GPLv2.TXT`. When this fallback is built into a distributed
binary, carry through the applicable upstream licence and notices. The FTL
requires citing FreeType in product documentation. When using system FreeType,
check the selected system package's notices. See the [FreeType licence
terms](https://freetype.org/license), [Fontconfig's upstream licence
file](https://gitlab.freedesktop.org/fontconfig/fontconfig/-/blob/main/COPYING),
and [crossfont 0.8.1 package metadata](https://github.com/alacritty/crossfont/blob/v0.8.1/Cargo.toml).

The codec family also includes the transitive `rvp9-core` 0.2.0-alpha.3,
BSD-3-Clause. `ropus` and all resolved `rvp9-*` licences were checked in their
registry Cargo.toml metadata. Codec dependencies retain their upstream notices.

## CPU oracle data

A small number of interpreter regression inputs are explicitly attributed to
Daniel Balsom's SingleStepTests 80386 data. The upstream [licence](https://github.com/SingleStepTests/80386/blob/459d49fbe6280e9ed46fee887b58dacd9cb880ab/LICENSE)
releases data files into the public domain unless otherwise noted and licences
software under MIT. The full suite is an optional external test input; it is
not bundled. The licence link above identifies the reviewed upstream revision.

## Behavioural references

The following projects and documents are identified in the source as references
for hardware and system behaviour. A reference does not itself grant permission
to copy an implementation; any copied material requires its own licence review.

- DOSBox-family BIOS compatibility material: the functionality, video-parameter
  and display-combination tables in `crates/gp64-bios/src/lib.rs` match the
  DOSBox-X tables. The IRQ0 callback stub is explicitly documented as adapted
  from DOSBox's `CB_IRQ0`. Retain Copyright (C) 2002-2021 The DOSBox Team and
  GPL-2.0-or-later for this material; GPL-3.0-or-later permits the later-version
  option. See [the crate notice](crates/gp64-bios/NOTICE.md) and
  [the pinned DOSBox-X BIOS source](https://github.com/joncampbell123/dosbox-x/blob/378e6f8a25768594df87c9f5c99b5df9b3f6a8b0/src/ints/bios.cpp). `int15.rs` also identifies
  `INT15_Handler` case 0x89 as a behavioural reference.
- DOSBox-family DOS kernel model: `crates/gp64-dos/src/lib.rs` identifies
  `DOS_PSP::MakeNew`, `DOS_InfoBlock::SetLocation` and `dos_tables.cpp`;
  `filesystem.rs` identifies `SortByDirName`. The unnamed "DOS model" in these
  comments corresponds to DOSBox-family source. Retain Copyright (C) 2002-2021
  The DOSBox Team, GPL-2.0-or-later, for any adapted compatibility material;
  see [the crate notice](crates/gp64-dos/NOTICE.md) and
  [the pinned DOSBox-X kernel source](https://github.com/joncampbell123/dosbox-x/blob/378e6f8a25768594df87c9f5c99b5df9b3f6a8b0/src/dos/dos_classes.cpp).
  This credit does not attribute every shared DOS ABI fact to that project.
- SpanDSP: V.8/V.34 signalling and DSP comparisons in `crates/gp64-modem/src/`.
  The checked library source headers state LGPL-2.1. Local attribution comments
  now identify that licence and the correct upstream filenames. The reviewed
  implementations have distinct control flow; shared protocol constants alone
  do not establish an implementation copy.
- QEMU: PIC edge/acknowledge semantics, `crates/gp64-pic/src/lib.rs` (GPL-2.0
  for the referenced `hw/intc/i8259.c`).
- Bochs/SeaBIOS: CMOS layout and firmware behaviour; SeaVGABIOS: VGA oracle
  comparisons (Bochs LGPL-2.1-or-later; SeaBIOS/SeaVGABIOS LGPL-3.0).
- FreeDOS and Ralf Brown’s Interrupt List: DOS/keyboard behaviour and interrupt
  documentation; IBM, Intel, YM3812 and ITU-T specifications, 3am Systems modem
  reference: hardware/protocol facts and algorithm references. These mentions
  do not import their implementations.
- Howard Hinnant’s [civil-date formulas](https://howardhinnant.github.io/date_algorithms.html):
  Rust ports in DOS, recording and BIOS test helpers. The author donates these
  formulas to the public domain; this is separate from his date library.

The dependency table records licence metadata, not a complete distribution
bundle. Before distributing binaries, review the full resolved dependency tree
and linked native libraries, and include their required licence and notice texts.

Primary upstream licence references checked for this notice:
- [DOSBox-X BIOS source header](https://github.com/joncampbell123/dosbox-x/blob/master/src/ints/bios.cpp)
- [vga 0.2.9 package metadata](https://github.com/rust-osdev/vga/blob/b91c2286cbd06ac4b618420037e1951975518877/Cargo.toml)
- [spandsp V.8 source header](https://github.com/freeswitch/spandsp/blob/master/src/v8.c)
- [QEMU PIC source header](https://github.com/qemu/qemu/blob/master/hw/intc/i8259.c)
- [Bochs licence](https://github.com/bochs-emu/Bochs/blob/master/bochs/COPYING)
- [SeaBIOS LGPL text](https://github.com/coreboot/seabios/blob/master/COPYING.LESSER)
