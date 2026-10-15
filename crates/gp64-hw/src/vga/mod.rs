// EGA palette loops use indices for bitwise operations
#![allow(clippy::needless_range_loop)]

use crate::timer::PIT_FREQUENCY;
use gp64_memory::Memory;
use gp64_types::VgaPipelineState;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// Light gray on black, used when BIOS teletype scrolling creates blank cells.
pub const DEFAULT_TEXT_ATTRIBUTE: u8 = 0x07;

/// Canonical CGA/IRGB 16-color palette (entries 0-15).
///
/// This is the palette produced by IBM CGA monitors using IRGB encoding.
/// Entry 6 is brown (not dark yellow) due to the IBM 5153 monitor hardware
/// override that halves the green channel when only R+G are active without
/// intensity.
///
/// Used during initial DAC setup and as the normal-color bank when BIOS CGA
/// modes (04h, 05h, 06h) load their full 64-entry palette.
const CGA_IRGB_PALETTE: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // 0 - Black
    (0, 0, 170),     // 1 - Blue
    (0, 170, 0),     // 2 - Green
    (0, 170, 170),   // 3 - Cyan
    (170, 0, 0),     // 4 - Red
    (170, 0, 170),   // 5 - Magenta
    (170, 85, 0),    // 6 - Brown
    (170, 170, 170), // 7 - Light gray
    (85, 85, 85),    // 8 - Dark gray
    (85, 85, 255),   // 9 - Light blue
    (85, 255, 85),   // 10 - Light green
    (85, 255, 255),  // 11 - Light cyan
    (255, 85, 85),   // 12 - Light red
    (255, 85, 255),  // 13 - Light magenta
    (255, 255, 85),  // 14 - Yellow
    (255, 255, 255), // 15 - White
];

/// Translate an IRGB color selector to its slot in the 64-entry CGA BIOS
/// palette. Colors 8-15 occupy DAC entries 16-23 in `cga_palette_2`.
const fn cga_irgb_to_dac_index(color: u8) -> u8 {
    if (color & 0x08) != 0 {
        0x10 | (color & 0x07)
    } else {
        color & 0x07
    }
}

fn cga_palette_2_entry(index: usize) -> (u8, u8, u8) {
    // cga_palette_2 repeats each normal/bright eight-color bank twice.
    let intensity = if index & 0x10 != 0 { 8 } else { 0 };
    CGA_IRGB_PALETTE[(index & 7) + intensity]
}

#[cfg(test)]
thread_local! {
    static FORCE_RENDER_MODE_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static CGA_HEADER_BYTES_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static VGA_MODE_TRACE_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static VGA_PORT_TRACE_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static VGA_TRACE_ENABLED_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn force_render_mode_override() -> Option<String> {
    FORCE_RENDER_MODE_OVERRIDE.with(|cell| cell.borrow().clone())
}

#[cfg(not(test))]
fn force_render_mode_override() -> Option<String> {
    // KILN-00086: the force-render-mode env var was retired. No production override
    // exists; the render mode is chosen by mode-set logic alone.
    None
}

#[cfg(test)]
fn set_force_render_mode_override(value: Option<String>) {
    FORCE_RENDER_MODE_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = value;
    });
}

#[cfg(test)]
fn cga_header_bytes_override() -> Option<String> {
    CGA_HEADER_BYTES_OVERRIDE.with(|cell| cell.borrow().clone())
}

#[cfg(not(test))]
fn cga_header_bytes_override() -> Option<String> {
    None
}

#[cfg(test)]
fn set_cga_header_bytes_override(value: Option<String>) {
    CGA_HEADER_BYTES_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = value;
    });
}

#[cfg(test)]
fn set_vga_mode_trace_override(value: Option<bool>) {
    VGA_MODE_TRACE_OVERRIDE.with(|cell| cell.set(value));
}

#[cfg(test)]
fn set_vga_port_trace_override(value: Option<bool>) {
    VGA_PORT_TRACE_OVERRIDE.with(|cell| cell.set(value));
}

#[cfg(test)]
fn set_vga_trace_enabled_override(value: Option<bool>) {
    VGA_TRACE_ENABLED_OVERRIDE.with(|cell| cell.set(value));
}

/// Snapshot of VGA registers expected to remain stable within a single video mode.
/// Used by the VGA sentinel to detect unexpected register changes within a video mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VgaStableSnapshot {
    pub mode: u8,
    pub mode_x: bool,
    pub chain4: bool,
    pub seq4: u8,
    pub gfx5: u8,
    pub gfx6: u8,
    pub cr17: u8,
    pub line_compare: u16,
    pub fb_width: u32,
    pub fb_height: u32,
}

/// Register-derived state that selects the scanline renderer and its output geometry.
/// Palette and raster split registers are deliberately excluded because they may
/// change within a valid frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VgaRenderLayout {
    mode: u8,
    mode_x: bool,
    chain4: bool,
    seq4: u8,
    gfx5: u8,
    gfx6: u8,
    cr17: u8,
    fb_width: u32,
    fb_height: u32,
}

impl VgaStableSnapshot {
    /// Compare two snapshots and return a human-readable description of differences.
    /// Returns `None` if the snapshots are equal.
    pub fn describe_changes(old: &VgaStableSnapshot, new: &VgaStableSnapshot) -> Option<String> {
        if old == new {
            return None;
        }
        let mut diffs = Vec::new();
        if old.mode != new.mode {
            diffs.push(format!("mode 0x{:02X}→0x{:02X}", old.mode, new.mode));
        }
        if old.mode_x != new.mode_x {
            diffs.push(format!("mode_x {}→{}", old.mode_x, new.mode_x));
        }
        if old.chain4 != new.chain4 {
            diffs.push(format!("chain4 {}→{}", old.chain4, new.chain4));
        }
        if old.seq4 != new.seq4 {
            diffs.push(format!("seq4 0x{:02X}→0x{:02X}", old.seq4, new.seq4));
        }
        if old.gfx5 != new.gfx5 {
            diffs.push(format!("gfx5 0x{:02X}→0x{:02X}", old.gfx5, new.gfx5));
        }
        if old.gfx6 != new.gfx6 {
            diffs.push(format!("gfx6 0x{:02X}→0x{:02X}", old.gfx6, new.gfx6));
        }
        if old.cr17 != new.cr17 {
            diffs.push(format!("cr17 0x{:02X}→0x{:02X}", old.cr17, new.cr17));
        }
        if old.line_compare != new.line_compare {
            diffs.push(format!(
                "line_compare 0x{:03X}→0x{:03X}",
                old.line_compare, new.line_compare
            ));
        }
        if old.fb_width != new.fb_width || old.fb_height != new.fb_height {
            diffs.push(format!(
                "fb {}x{}→{}x{}",
                old.fb_width, old.fb_height, new.fb_width, new.fb_height
            ));
        }
        Some(diffs.join(", "))
    }
}

/// Check if VGA tracing is enabled. Guards expensive debug computation.
/// Enable via `RUST_LOG=gp64::hw::vga=debug`.
fn vga_trace_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = VGA_TRACE_ENABLED_OVERRIDE.with(|cell| cell.get()) {
        return enabled;
    }
    tracing::enabled!(target: "gp64::hw::vga", tracing::Level::DEBUG)
}

fn trace_path_from_env(raw: Option<String>, default_path: &str) -> std::path::PathBuf {
    match raw.as_deref().map(str::trim) {
        Some(val) if !val.is_empty() && val != "1" => std::path::PathBuf::from(val),
        _ => std::path::PathBuf::from(default_path),
    }
}

fn vga_mode_trace_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = VGA_MODE_TRACE_OVERRIDE.with(|cell| cell.get()) {
        return enabled;
    }
    tracing::enabled!(target: "gp64::hw::vga::mode", tracing::Level::DEBUG)
}

fn vga_port_trace_env_override() -> Option<bool> {
    // KILN-00086: the port-trace env var was retired. Runtime toggling still goes
    // through the VGA_PORT_TRACE_OVERRIDE thread-local (tests); the env gate is
    // permanently absent, so there is no override from this path.
    None
}

fn vga_port_trace_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = VGA_PORT_TRACE_OVERRIDE.with(|cell| cell.get()) {
        return enabled;
    }
    if let Some(enabled) = vga_port_trace_env_override() {
        return enabled;
    }
    // Cached once per process: this gate is on the VGA port-write hot path
    // (see `vga_port_trace_env_override` above). With the layered
    // per-layer-filtered subscriber, `tracing::enabled!` defeats callsite
    // interest caching and measured ~1 µs PER CALL even when the target is
    // disabled (Lane B EXP-B004). The subscriber topology is fixed at
    // process start, so a one-time evaluation observes the same answer a
    // per-call check would.
    static TRACING_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACING_ENABLED
        .get_or_init(|| tracing::enabled!(target: "gp64::hw::vga::port", tracing::Level::TRACE))
}

const VGA_PORT_TRACE_LIMIT: usize = 2000;

struct VgaPortTraceState {
    path: std::path::PathBuf,
    file: std::fs::File,
    count: usize,
}

fn vga_port_trace_path() -> std::path::PathBuf {
    // KILN-00086: the VGA-port-trace-file env var was retired; trace always uses the
    // default path when the (test-only) port-trace override is engaged.
    trace_path_from_env(None, "tmp/vga_ports.log")
}

fn open_vga_port_trace_file(path: &std::path::Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn vga_port_trace_emit(line: &str) {
    static STATE: std::sync::OnceLock<std::sync::Mutex<Option<VgaPortTraceState>>> =
        std::sync::OnceLock::new();

    let path = vga_port_trace_path();
    let Ok(mut state) = STATE.get_or_init(|| std::sync::Mutex::new(None)).lock() else {
        return;
    };

    let needs_open = state
        .as_ref()
        .map(|state| state.path != path)
        .unwrap_or(true);
    if needs_open {
        let Some(file) = open_vga_port_trace_file(&path) else {
            return;
        };
        *state = Some(VgaPortTraceState {
            path,
            file,
            count: 0,
        });
    }

    let Some(state) = state.as_mut() else {
        return;
    };
    if state.count >= VGA_PORT_TRACE_LIMIT {
        return;
    }
    state.count += 1;

    use std::io::Write;
    let _ = writeln!(state.file, "{line}");
    let _ = state.file.flush();
    tracing::debug!(target: "gp64::hw::vga::port", "{}", line);
}

/// Emit a VGA register-trace line, formatting it ONLY when tracing is on.
///
/// MDD-BUG-CRUCIBLE-00602: callers used to build the line with `format!` and
/// pass a `&str`, so the allocation happened before this function could check
/// whether anyone wanted it. INT 10h AH=F5h (EGA RIL) walks up to 65,535
/// guest-supplied register descriptors through these setters, so a valid
/// descriptor array forced 65,535 heap allocations whose strings were
/// discarded immediately during ordinary tracing-off execution.
///
/// Taking `fmt::Arguments` moves the formatting behind the gate: `format_args!`
/// at the call site captures references without allocating, and the `String`
/// is built only on the enabled path.
fn vga_reg_trace_emit(args: std::fmt::Arguments<'_>) {
    if vga_port_trace_enabled() {
        vga_port_trace_emit(&args.to_string());
    }
}

static VGA_RENDER_TRACE_ONCE: AtomicBool = AtomicBool::new(false);

/// Default frame timing before CRTC registers are initialized (70 Hz).
const DEFAULT_CYCLES_PER_FRAME: u64 = (PIT_FREQUENCY as u64 * 1000) / 70086;
const DEFAULT_RETRACE_CYCLES: u64 = (DEFAULT_CYCLES_PER_FRAME * 80) / 1000;
const VGA_PLANE_SIZE: usize = 65536;

// Font bank constants for VGA text mode
/// Size of each font bank in plane 2 (8KB)
const FONT_BANK_SIZE: usize = 0x2000;

/// Native plane-2 offset for an eight-bank VGA character-map selector.
/// Selector bits 1:0 address 16KB regions; bit 2 chooses the second 8KB bank.
pub const fn font_bank_offset(block: u8) -> usize {
    ((block & 0x03) as usize * 2 + ((block >> 2) & 0x01) as usize) * FONT_BANK_SIZE
}
/// Size of each character slot in font memory (32 bytes max)
const FONT_CHAR_SIZE: usize = 32;

mod font_cp437_8x16;
mod font_cp437_8x8;
pub use font_cp437_8x16::CP437_FONT_8X16;
pub use font_cp437_8x8::CP437_FONT_8X8;

/// Scroll region parameters
pub struct ScrollRegion {
    pub top: u8,
    pub left: u8,
    pub bottom: u8,
    pub right: u8,
}
#[derive(Clone)]
pub struct Vga {
    /// Text mode columns
    columns: u8,
    /// Text mode rows
    rows: u8,
    /// Active display page
    active_page: u8,
    /// Cursor position per page (row, col)
    cursor_pos: [(u8, u8); 8],
    /// DAC palette (256 RGB entries)
    dac: [(u8, u8, u8); 256],
    /// Attribute controller registers (indexes 0x00-0x1F)
    /// Note: Indices 0x00-0x0F are the palette registers (consolidated)
    attribute_regs: [u8; 0x20],
    /// EGA RIL default register tables (INT 10h AH=F7h/F6h)
    ril_defaults: [Option<Vec<u8>>; 8],
    /// Framebuffer for rendering (u32 0RGB format)
    framebuffer: Vec<u32>,
    /// Framebuffer width
    fb_width: usize,
    /// Framebuffer height
    fb_height: usize,
    /// Last complete scanline-composed frame, frozen for host presentation.
    /// The active framebuffer is swapped into this buffer only after every
    /// visible line has been rendered, so the next frame cannot tear it.
    completed_framebuffer: Vec<u32>,
    completed_fb_width: usize,
    completed_fb_height: usize,
    /// Display aspect latched with `completed_framebuffer`. Legacy VGA-family
    /// scanout targets a 4:3 CRT even when its storage pixels are non-square.
    completed_display_aspect_width: usize,
    completed_display_aspect_height: usize,
    // VGA register state for port I/O
    dac_read_index: u8,
    dac_write_index: u8,
    dac_component: u8,
    dac_state: u8,
    /// DAC palette width: 6 bits (standard VGA) or 8 bits (VBE extended)
    pub dac_bits: u8,
    misc_output: u8,
    feature_control: u8,
    pel_mask: u8,
    attribute_index: u8,
    attribute_flip_flop: bool,
    /// Palette Address Source (PAS) bit - when clear, display is blanked
    palette_address_source: bool,
    sequencer_index: u8,
    sequencer_regs: [u8; 8],
    graphics_index: u8,
    graphics_regs: [u8; 16],
    crtc_index: u8,
    crtc_regs: [u8; 32],
    pub cycles_since_vblank: u64,
    pub timing_cycles_per_frame: u64,
    pub timing_retrace_cycles: u64,
    /// VSync pulse start, in CPU cycles from VBI start.
    /// On real hardware the VSync pulse is a narrow window within the VBI
    /// (e.g. 2 scanlines out of 49 for Mode 13h).
    vsync_offset_cycles: u64,
    /// VSync pulse duration, in CPU cycles.
    vsync_duration_cycles: u64,
    /// One-shot override for port 3DAh status calculation. Set by the IoBus
    /// before calling `read_port(0x3DA)` to provide per-instruction accurate
    /// timing (computed from current CPU cycle count). None means use the
    /// stored `cycles_since_vblank` which has scanline-granularity accuracy.
    pub port_3da_override_position: Option<u64>,
    /// Current emulated guest cycles per second, set by the emulator
    emulated_ips: u64,
    /// Cached refresh rate in millihertz (e.g. 70086 for 70.086 Hz)
    cached_refresh_millihz: u32,
    /// Deferred timing update: applied at next frame boundary to avoid
    /// breaking the mid-frame VGA scanline event chain.
    pub pending_timing_cycles_per_frame: Option<u64>,
    // Per-scanline rendering state (HLD §4.6)
    pub scanline_counter: u32,
    /// EXP-N007a (M2): the scanline-grid index the NEXT queued VgaScanline
    /// event was scheduled for. Pops jump `scanline_counter` to this value —
    /// never `+= 1` and never a cycle-inverse derivation (off-by-one almost
    /// everywhere against the floor-form forward expression). Line-space, so a
    /// speed-change rescale (cycle-space) leaves it untouched.
    pub next_scanline_index: u32,
    /// EXP-N007a (M5): true once the frame event chain is live. Replaces the
    /// `scanline_counter > 0` proxy in `set_timing_cycles_per_frame` — under
    /// vblank coalescing the counter stays 0 for the whole VBI, which would
    /// re-open the immediate-apply-under-a-queued-deadline hazard that
    /// deferral exists to prevent.
    pub scanline_chain_active: bool,
    /// EXP-N007b: raster-sensitive register writes latched awaiting the
    /// render-before-apply service at the next device boundary
    /// (`service_raster_writes`, reached via `flush_synchronous_vga`).
    /// Arrival order preserved; depth >1 only within a single guest
    /// instruction (16-bit OUT pairs, REP OUTS bursts).
    pub(crate) raster_write_queue: [(u16, u8); 4],
    /// Live entries in `raster_write_queue`.
    pub(crate) raster_write_count: u8,
    /// Times the latch queue was full and a write applied directly without
    /// the render-before-apply step (bit-exactness fallback; census
    /// visibility).
    pub raster_write_overflows: u64,
    raster_output: bool,
    pub last_rendered_line: i32,
    pub total_scanlines_cached: u32,
    pub visible_scanlines_cached: u32,
    /// Test-only override: when `Some(v)`, `recompute_scanline_timing()`
    /// forces `total_scanlines_cached` to `v` instead of reading CRTC.
    /// Used by unit tests to exercise defensive guards that depend on
    /// the post-recompute cache value (e.g. zero-scanlines edge case).
    /// Gated behind the `test-util` feature so production builds carry
    /// no trace of this state.
    #[cfg(any(test, feature = "test-util"))]
    total_scanlines_test_override: Option<u32>,
    pub frame_start_cycle: u64,
    mode_change_pending: bool,
    /// CGA Color Select Register (0x3D9)
    color_select_reg: u8,
    /// CGA Mode Control Register (0x3D8)
    cga_mode_control_reg: u8,
    // Note: EGA/VGA planar memory is now stored in Memory struct, not VGA.
    // This avoids stack overflow (256KB) and ensures Memory::write_u8 can intercept
    // planar writes based on the Map Mask register.
    // VGA latch registers are also stored in Memory (for proper read-modify-write emulation).
    /// Frame counter for cursor/attribute blink (incremented each render)
    blink_frame_count: u32,
    /// Pre-computed palette lookup table (DAC -> u32 RGB)
    palette_lut: [u32; 256],
    /// Whether palette_lut needs rebuilding
    palette_lut_dirty: bool,
    /// Pre-computed 16-color palette LUT (for text/EGA modes via palette_to_rgb)
    palette16_lut: [u32; 16],
    /// Whether palette16_lut needs rebuilding
    palette16_lut_dirty: bool,
    hw_cursor_enabled: bool,
    /// Hardware cursor X position
    hw_cursor_x: u16,
    /// Hardware cursor Y position
    hw_cursor_y: u16,
    /// Custom font loaded via INT 10h AH=11h (use get_font_row instead of built-in)
    pub custom_font_loaded: bool,
    /// Pending capture events for headless debugging triggers
    pub pending_capture_events: Vec<gp64_types::FiredCaptureEvent>,
    /// VGA pipeline state changed since last take_vga_pipeline_state()
    pub(crate) pipeline_dirty: bool,
    /// Set when tick() detects a vblank-end crossing; consumed by caller via take_vblank_pending()
    vblank_pending: bool,
    /// Vertical retrace interrupt pending (IRQ 2). Set at vblank start when
    /// CRTC register 0x11 bit 5 = 0 (vertical interrupt enabled). Cleared by
    /// writing CR11 bit 4 = 0 then 1. Consumed by the timing loop to raise IRQ 2.
    vretrace_irq_pending: bool,
    /// CRTC start word latched at vertical-retrace start.
    /// On real VGA hardware the start address is sampled once at the beginning of the frame
    /// and held constant for all scanlines.  Reading the live `crtc_regs` mid-frame causes
    /// tearing when programs flip pages by writing 0x0C/0x0D between scanlines.
    latched_start_word: usize,
    /// Byte panning value latched at vertical-retrace start (CRTC 0x08 bits 5-6).
    latched_byte_panning: usize,
    /// Pixel panning value latched at vertical-retrace end (Attribute Controller 0x13).
    latched_pixel_panning: usize,
}
pub const VGA_REFRESH_HZ_MIN: u32 = 50;
pub const VGA_REFRESH_HZ_MAX: u32 = 85;
impl Vga {
    pub fn vga_mode_bpp(&self) -> Option<u8> {
        if self.is_text_mode() {
            None
        } else if self.is_256_color_mode() {
            Some(8)
        } else {
            Some(4)
        }
    }

    pub fn diag_display_state(&self) -> String {
        let crtc_start = ((self.crtc_regs[0x0C] as u16) << 8) | self.crtc_regs[0x0D] as u16;
        let line_compare = self.get_line_compare() as usize;
        let pixel_panning = self.get_pixel_panning();
        let gfx6 = self.graphics_regs[0x06];
        let map_select = (gfx6 >> 2) & 0x03;
        format!(
            "mode=0x{:02X} mode_x={} fb={}x{} \
             crtc_start=0x{:04X} cyc_vblank={} chain4={} seq4=0x{:02X} gfx5=0x{:02X} \
             gfx6=0x{:02X} map={} \
             cr11=0x{:02X} cr17=0x{:02X} lc=0x{:03X} \
             pan={} pel=0x{:02X} pas={} attr10=0x{:02X} attr13=0x{:02X}",
            self.mode(),
            self.is_mode_x(),
            self.fb_width,
            self.fb_height,
            crtc_start,
            self.cycles_since_vblank,
            self.is_chain4(),
            self.sequencer_regs[0x04],
            self.graphics_regs[0x05],
            gfx6,
            map_select,
            self.crtc_regs[0x11],
            self.crtc_regs[0x17],
            line_compare,
            pixel_panning,
            self.pel_mask,
            self.palette_address_source,
            self.attribute_regs[0x10],
            self.attribute_regs[0x13],
        )
    }
    pub fn new() -> Self {
        let mut dac = [(0u8, 0u8, 0u8); 256];

        // Initialize DAC with the standard VGA 256-color palette.
        //
        // The Attribute Controller's VPT (Video Parameter Table) maps pixel values
        // through palette registers to DAC indices in the 0-63 range. For example,
        // in text mode: pixel 6 → AC[6] = 0x14 → DAC[0x14] (brown), and
        // pixel 8 → AC[8] = 0x38 → DAC[0x38] (dark gray).
        //
        // Layout:
        // - Entries 0-15:   CGA/IRGB 16-color palette (used directly by CGA modes)
        // - Entries 16-63:  EGA 64-color palette (for VPT text/16-color mappings)
        // - Entries 64-231: 6x6x6 color cube (for Mode 13h etc.)
        // - Entries 232-255: grayscale ramp
        //
        // Entries 0-5 and 7 are identical between CGA and EGA palettes. Entry 6
        // differs: CGA has brown (170,85,0) due to the IBM monitor override,
        // while EGA's rgbRGB expression produces dark yellow (170,170,0).
        // Entries 8-15 also differ: CGA uses IRGB encoding while EGA uses
        // rgbRGB. Both are needed because CGA modes reference DAC[0-15]
        // directly, while text/EGA modes reference DAC[0x38-0x3F] via VPT
        // for the high-intensity colors.

        // CGA/IRGB 16-color palette at entries 0-15
        dac[..16].copy_from_slice(&CGA_IRGB_PALETTE);

        // EGA 64-color palette at entries 16-63.
        //
        // Each 6-bit index encodes rgbRGB where:
        //   bits 2-0 = primary (high intensity): bit2=R, bit1=G, bit0=B
        //   bits 5-3 = secondary (low intensity): bit5=r, bit4=g, bit3=b
        //
        // 6-bit DAC values expanded to 8-bit:
        //   primary contributes 0xAA (170), secondary contributes 0x55 (85)
        //   Combined: 0x00=0, 0x55=85, 0xAA=170, 0xFF=255
        //
        // Key VPT-referenced entries:
        //   0x14 (0b010100) = R_primary + g_secondary = (0xAA, 0x55, 0x00) = brown
        //   0x38 (0b111000) = r_sec + g_sec + b_sec = (0x55, 0x55, 0x55) = dark gray
        //   0x3F (0b111111) = all = (0xFF, 0xFF, 0xFF) = white
        for i in 16..64u8 {
            let r = (((i >> 2) & 1) as u16 * 0xAA + ((i >> 5) & 1) as u16 * 0x55) as u8;
            let g = (((i >> 1) & 1) as u16 * 0xAA + ((i >> 4) & 1) as u16 * 0x55) as u8;
            let b = ((i & 1) as u16 * 0xAA + ((i >> 3) & 1) as u16 * 0x55) as u8;
            dac[i as usize] = (r, g, b);
        }

        // 6x6x6 color cube (216 colors) at entries 16-231.
        // Only entries 64-231 are written (16-63 already hold EGA values above).
        let mut idx = 16;
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    if idx >= 64 {
                        let r8 = [0u8, 48, 101, 150, 203, 255][r as usize];
                        let g8 = [0u8, 48, 101, 150, 203, 255][g as usize];
                        let b8 = [0u8, 48, 101, 150, 203, 255][b as usize];
                        dac[idx] = (r8, g8, b8);
                    }
                    idx += 1;
                }
            }
        }

        // Grayscale ramp (24 colors) at entries 232-255
        for i in 0..24u8 {
            let v = 8 + i * 10;
            dac[232 + i as usize] = (v, v, v);
        }

        // Initialize attribute registers with default palette mapping in [0..16]
        // This maps pixel values 0-15 directly to DAC indices 0-15 where the
        // standard VGA 16-color palette is stored. Palette registers are stored
        // in attribute_regs[0..16] (consolidated - no separate palette_regs array).
        let mut attribute_regs = [0u8; 0x20];
        for i in 0..16 {
            attribute_regs[i] = i as u8;
        }
        // Color Plane Enable: all 4 planes enabled (BIOS default for all modes)
        attribute_regs[0x12] = 0x0F;

        Self {
            columns: 80,
            rows: 25,
            active_page: 0,
            cursor_pos: [(0, 0); 8],
            dac,
            attribute_regs,
            ril_defaults: std::array::from_fn(|_| None),
            framebuffer: vec![0u32; 640 * 400],
            fb_width: 640,
            fb_height: 400,
            // A coherent initial black surface keeps screenshots and other
            // boundary consumers off the in-progress construction buffer even
            // before the first emulated frame completes.
            completed_framebuffer: vec![0u32; 640 * 400],
            completed_fb_width: 640,
            completed_fb_height: 400,
            completed_display_aspect_width: 4,
            completed_display_aspect_height: 3,
            dac_read_index: 0,
            dac_write_index: 0,
            dac_component: 0,
            dac_state: 0,
            dac_bits: 6,       // Standard VGA: 6-bit DAC
            misc_output: 0x67, // Mode 03h default: color, RAM enabled, 28.322MHz clock, negative vsync
            feature_control: 0,
            pel_mask: 0xFF,
            attribute_index: 0,
            attribute_flip_flop: false,
            palette_address_source: true, // Display enabled by default
            sequencer_index: 0,
            sequencer_regs: {
                let mut r = [0u8; 8];
                // Mode 03h defaults: 9-dot chars, odd/even, extended memory
                r[0x01] = 0x00; // Clocking Mode: 9-dot characters
                r[0x02] = 0x03; // Map Mask: planes 0+1 enabled (text mode)
                r[0x03] = 0x00; // Character Map Select
                r[0x04] = 0x02; // Memory Mode: odd/even, extended memory
                r
            },
            graphics_index: 0,
            graphics_regs: {
                let mut r = [0u8; 16];
                r[6] = 0x0E; // GC Misc: text + odd/even + map=3 (B8000-BFFFF) — matches mode 03h
                r[8] = 0xFF;
                r
            },
            crtc_index: 0,
            crtc_regs: {
                // Mode 03h (80x25 text) CRTC defaults — must match the timing table
                // in set_mode_internal(). Without these, CRTC regs are all zeros until
                // the first INT 10h mode set. The initial register readback must
                // already describe the default text-mode geometry and timing.
                let mut r = [0u8; 32];
                let mode03: [u8; 25] = [
                    0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x2E, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
                ];
                let mut i = 0;
                while i < 25 {
                    r[i] = mode03[i];
                    i += 1;
                }
                // Clear CRTC protect bit (CR11 bit 7) so programs can freely modify
                // CRTC regs 0x00-0x07 — matches set_mode_internal() behavior.
                r[0x11] &= 0x7F;
                r
            },
            cycles_since_vblank: 0,
            timing_cycles_per_frame: DEFAULT_CYCLES_PER_FRAME,
            timing_retrace_cycles: DEFAULT_RETRACE_CYCLES,
            // Mode 03h defaults: vrstart=412, vdispend=400, vrend=414, vtotal=449
            // VSync offset = (412-400)/449 of frame, duration = 2/449 of frame
            vsync_offset_cycles: DEFAULT_CYCLES_PER_FRAME * 12 / 449,
            vsync_duration_cycles: DEFAULT_CYCLES_PER_FRAME * 2 / 449,
            port_3da_override_position: None,
            emulated_ips: DEFAULT_CYCLES_PER_FRAME * 70, // ~1.2M IPS default
            cached_refresh_millihz: 70086,
            pending_timing_cycles_per_frame: None,
            scanline_counter: 0,
            next_scanline_index: 0,
            scanline_chain_active: false,
            raster_write_queue: [(0, 0); 4],
            raster_write_count: 0,
            raster_write_overflows: 0,
            raster_output: true,
            last_rendered_line: -1,
            total_scanlines_cached: 449,   // 70 Hz default
            visible_scanlines_cached: 400, // 400-line default
            #[cfg(any(test, feature = "test-util"))]
            total_scanlines_test_override: None,
            frame_start_cycle: 0,
            mode_change_pending: false,
            color_select_reg: 0x30, // Default: Palette 1, High Intensity
            // CGA Mode Control Register: default to "video enabled" (bit 3).
            // The initial BIOS text-mode state has display output enabled.
            cga_mode_control_reg: 0x08,
            blink_frame_count: 0,
            palette_lut: [0; 256],
            palette_lut_dirty: true, // Force initial rebuild
            palette16_lut: [0; 16],
            palette16_lut_dirty: true, // Force initial rebuild
            hw_cursor_enabled: false,
            hw_cursor_x: 0,
            hw_cursor_y: 0,
            custom_font_loaded: false,
            pending_capture_events: Vec::new(),
            pipeline_dirty: true,
            vblank_pending: false,
            vretrace_irq_pending: false,
            latched_start_word: 0,
            latched_byte_panning: 0,
            latched_pixel_panning: 0,
        }
        .with_trace_probe()
    }

    fn with_trace_probe(self) -> Self {
        if vga_port_trace_enabled() {
            vga_port_trace_emit("VGA port trace enabled (Vga::new)");
        }
        self
    }

    /// Advance VGA timing by a number of CPU cycles
    pub fn tick(&mut self, cycles: u64) {
        let frame_cycles = self.timing_cycles_per_frame.max(1);
        if vga_port_trace_enabled() && !VGA_RENDER_TRACE_ONCE.swap(true, Ordering::Relaxed) {
            let vde = self.get_vertical_display_end();
            let overflow = self.crtc_regs[0x07];
            let max_scan = self.crtc_regs[0x09];
            let line_compare = self.crtc_regs[0x18];
            let attr_mode = self.attribute_regs[0x10];
            let overscan = self.attribute_regs[0x11];
            let color_select = self.attribute_regs[0x14];
            let seq_clock = self.sequencer_regs[0x01];
            let seq_mem = self.sequencer_regs[0x04];
            let gfx_mode = self.graphics_regs[0x05];
            let gfx_misc = self.graphics_regs[0x06];
            vga_port_trace_emit(&format!(
                "VGA render snapshot: mode=0x{mode:02x} fb={w}x{h} vde={vde} \
                 overflow=0x{overflow:02x} max_scan=0x{max_scan:02x} \
                 line_cmp=0x{line_compare:02x} attr_mode=0x{attr_mode:02x} \
                 overscan=0x{overscan:02x} sel=0x{color_select:02x} \
                 seq_clock=0x{seq_clock:02x} seq_mem=0x{seq_mem:02x} \
                 gfx_mode=0x{gfx_mode:02x} gfx_misc=0x{gfx_misc:02x}",
                mode = self.mode(),
                w = self.fb_width,
                h = self.fb_height
            ));
        }
        let prev = self.cycles_since_vblank;
        let total = prev.saturating_add(cycles);
        let retrace = self
            .timing_retrace_cycles
            .min(frame_cycles.saturating_sub(1));
        if total < frame_cycles && !(prev < retrace && total >= retrace) {
            self.cycles_since_vblank = total;
            return;
        }
        let new_cycles = total % frame_cycles;
        let wrap = total >= frame_cycles;
        let crossed_vretrace_start = cycles >= frame_cycles || wrap;
        let crossed_vblank_end = if cycles >= frame_cycles {
            true
        } else if !wrap {
            prev < retrace && new_cycles >= retrace
        } else {
            prev < retrace || new_cycles >= retrace
        };

        self.cycles_since_vblank = new_cycles;
        if crossed_vretrace_start {
            self.latch_display_start_registers();
        }
        if crossed_vblank_end {
            self.vblank_pending = true;
            self.latch_pixel_panning();
        }
    }

    /// Snapshot the CRTC start address and byte panning at vertical-retrace start.
    /// Per-scanline renderers must read these copies, not the live `crtc_regs`, so
    /// a page flip made after the latch waits for the following frame.
    pub fn latch_display_start_registers(&mut self) {
        self.latched_start_word =
            ((self.crtc_regs[0x0C] as usize) << 8) | (self.crtc_regs[0x0D] as usize);
        self.latched_byte_panning = self.get_byte_panning();
    }

    /// Snapshot Attribute Controller pixel panning at vertical-retrace end.
    pub fn latch_pixel_panning(&mut self) {
        self.latched_pixel_panning = self.effective_pixel_panning();
    }

    /// Snapshot all frame-latched registers for one-shot rendering and restore paths.
    pub fn latch_frame_start_registers(&mut self) {
        self.latch_display_start_registers();
        self.latch_pixel_panning();
    }

    /// Returns true if a vblank-end crossing has occurred since the last call,
    /// and resets the flag. Consume semantics: each crossing is reported once.
    pub fn take_vblank_pending(&mut self) -> bool {
        std::mem::replace(&mut self.vblank_pending, false)
    }

    /// Signal the start of vertical retrace. If vertical interrupt is enabled
    /// (CRTC register 0x11 bit 5 = 0), sets the pending IRQ flag so the
    /// caller can raise IRQ 2 on the PIC.
    pub fn signal_vretrace_start(&mut self) {
        // VGA samples display start at vertical-retrace start. In particular,
        // writes made during the retrace pulse must wait for the following frame.

        self.latch_display_start_registers();

        // CR11 bit 4 arms and bit 5 disables the vertical interrupt.
        let interrupt_enabled = self.crtc_regs[0x11] & 0x30 == 0x10;
        if interrupt_enabled {
            self.vretrace_irq_pending = true;
        }
    }

    /// Returns true if a vertical retrace IRQ is pending and resets the flag.
    /// Consume semantics: each interrupt is reported once. The timing loop
    /// calls this and raises IRQ 2 on the PIC when it returns true.
    pub fn take_vretrace_irq_pending(&mut self) -> bool {
        std::mem::replace(&mut self.vretrace_irq_pending, false)
    }

    /// Check and clear mode change notification flag.
    pub fn take_mode_change_pending(&mut self) -> bool {
        std::mem::replace(&mut self.mode_change_pending, false)
    }

    pub fn has_pending_timing_change(&self) -> bool {
        self.mode_change_pending || self.pending_timing_cycles_per_frame.is_some()
    }

    /// Snapshot of VGA registers expected to remain stable within a single video mode.
    /// Used by the VGA sentinel to detect unexpected register changes within a video mode.
    pub fn snapshot_stable_registers(&self) -> VgaStableSnapshot {
        VgaStableSnapshot {
            mode: self.mode(),
            mode_x: self.is_mode_x(),
            chain4: self.is_chain4(),
            seq4: self.sequencer_regs[0x04],
            gfx5: self.graphics_regs[0x05],
            gfx6: self.graphics_regs[0x06],
            cr17: self.crtc_regs[0x17],
            line_compare: self.get_line_compare(),
            fb_width: self.fb_width as u32,
            fb_height: self.fb_height as u32,
        }
    }

    /// Return the state whose mid-frame change would mix incompatible scanline
    /// renderers or geometries in one host-visible frame.
    pub fn scanline_render_layout(&self) -> VgaRenderLayout {
        VgaRenderLayout {
            mode: self.mode(),
            mode_x: self.is_mode_x(),
            chain4: self.is_chain4(),
            seq4: self.sequencer_regs[0x04],
            gfx5: self.graphics_regs[0x05],
            gfx6: self.graphics_regs[0x06],
            cr17: self.crtc_regs[0x17],
            fb_width: self.fb_width as u32,
            fb_height: self.fb_height as u32,
        }
    }

    /// Read vertical total from CRTC registers.
    /// CR06 stores (vertical_total - 2). CR07 bits 0 and 5 hold bits 8-9.
    fn crtc_vtotal(&self) -> u32 {
        let lo = self.crtc_regs[0x06] as u32;
        let ov = self.crtc_regs[0x07];

        let raw = lo | ((ov & 1) as u32) << 8 | (((ov >> 5) & 1) as u32) << 9;
        let total = raw + 2; // CR06 stores vertical_total - 2
        {
            total
        }
    }

    /// Vertical Retrace Start from CRTC registers.
    /// CR10[7:0] + CR07 bit 2 (bit 8) + CR07 bit 7 (bit 9).
    fn crtc_vrstart(&self) -> u16 {
        let low = self.crtc_regs[0x10] as u16;
        let bit8 = ((self.crtc_regs[0x07] >> 2) & 1) as u16;
        let bit9 = ((self.crtc_regs[0x07] >> 7) & 1) as u16;
        let bit10 = { 0 };
        low | (bit8 << 8) | (bit9 << 9) | (bit10 << 10)
    }

    /// Vertical Retrace End from CRTC registers.
    /// Replace low 4 bits of vrstart with CR11[3:0].
    /// If result <= vrstart, add 16 (4-bit wrap-around).
    fn crtc_vrend(&self) -> u16 {
        let vrstart = self.crtc_vrstart();
        let mut vrend = (vrstart & !0x0F) | (self.crtc_regs[0x11] & 0x0F) as u16;
        if vrend <= vrstart {
            vrend += 16;
        }
        vrend
    }

    fn crtc_vbstart(&self) -> u16 {
        let low = self.crtc_regs[0x15] as u16;
        let bit8 = ((self.crtc_regs[0x07] >> 3) & 1) as u16;
        let bit9 = ((self.crtc_regs[0x09] >> 5) & 1) as u16;
        let bit10 = { 0 };
        low | (bit8 << 8) | (bit9 << 9) | (bit10 << 10)
    }

    /// Vertical blank end is an eight-bit counter relative to blank start.
    fn crtc_vbend(&self) -> u16 {
        let start = self.crtc_vbstart();
        let mut end = (start & !0x00FF) | u16::from(self.crtc_regs[0x16]);
        if end <= start {
            end += 0x100;
        }
        end
    }

    /// Compute refresh rate from current CRTC, Sequencer, and Misc Output registers.
    /// Returns (cycles_per_frame, refresh_millihz) where refresh_millihz is the
    /// rate in millihertz (e.g. 70086 for 70.086 Hz).
    fn compute_crtc_refresh(&self) -> (u64, u32) {
        let vtotal = self.crtc_vtotal();
        let htotal = (self.crtc_regs[0x00] as u32 | { 0 }) + 5; // CR00 stores htotal - 5

        // Dots per character clock: 9 dots default, 8 if SR01 bit 0 set
        let dots_per_char = if (self.sequencer_regs[0x01] & 0x01) != 0 {
            8u32
        } else {
            9u32
        };

        // Pixel clock from Misc Output bit 2
        let clock_select = (self.misc_output >> 2) & 0x03;
        let pixel_clock_khz: u64 = {
            match clock_select {
                0x00 => 25175, // 25.175 MHz
                0x01 => 28322, // 28.322 MHz
                _ => 25175,    // external clock — not present on standard VGA
            }
        };

        // SR01 bit 3: Dot Clock Divide by 2
        let pixel_clock_khz = if (self.sequencer_regs[0x01] & 0x08) != 0 {
            pixel_clock_khz / 2
        } else {
            pixel_clock_khz
        };

        let total_pixels = vtotal as u64 * htotal as u64 * dots_per_char as u64;
        if total_pixels == 0 {
            return (DEFAULT_CYCLES_PER_FRAME, 70086);
        }

        let refresh_millihz = pixel_clock_khz * 1_000_000 / total_pixels;

        let refresh_millihz = {
            refresh_millihz.clamp(
                u64::from(VGA_REFRESH_HZ_MIN) * 1000,
                u64::from(VGA_REFRESH_HZ_MAX) * 1000,
            )
        };

        let cycles_per_frame = PIT_FREQUENCY as u64 * 1000 / refresh_millihz.max(1);

        (cycles_per_frame, refresh_millihz as u32)
    }

    /// Set the emulated CPU speed. VGA computes cycles_per_frame from this
    /// and the CRTC-derived refresh rate.
    pub fn set_emulated_ips(&mut self, ips: u64) {
        self.emulated_ips = ips;
        self.recompute_timing();
    }

    /// Current refresh rate in Hz (integer, for external consumers).
    /// CRTC-derived refresh rate without whole-Hz rounding.
    pub fn refresh_millihz(&self) -> u32 {
        self.cached_refresh_millihz
    }

    pub fn refresh_hz(&self) -> u32 {
        (self.cached_refresh_millihz + 500) / 1000
    }

    /// Recompute frame timing from current CRTC state and emulated IPS.
    fn recompute_timing(&mut self) {
        if self.emulated_ips == 0 {
            return; // Defensive — Vga::new() initializes to nonzero
        }
        let (_, refresh_millihz) = self.compute_crtc_refresh();
        self.cached_refresh_millihz = refresh_millihz;
        let cpf = self.emulated_ips * 1000 / refresh_millihz as u64;
        self.set_timing_cycles_per_frame(cpf);
    }

    /// Override VGA timing to match the emulator's cycle budget per frame.
    ///
    /// If a scanline event chain is active (scanline_counter > 0), the update
    /// is deferred to the next `on_vga_start_of_frame()` to avoid breaking the
    /// mid-frame event chain. Immediate application when the chain is mid-frame
    /// makes the chain's proportional scheduling expression compute wildly
    /// different targets, stalling it and preventing VgaStartOfFrame from ever
    /// being rescheduled — resulting in a permanent black screen.
    pub fn set_timing_cycles_per_frame(&mut self, cycles_per_frame: u64) {
        if cycles_per_frame == 0 {
            return;
        }
        if self.scanline_chain_active || self.scanline_counter > 0 {
            // Chain live (or legacy mid-frame proxy): defer to next frame
            // boundary. EXP-N007a: the counter alone stays 0 for the whole
            // coalesced VBI, so the explicit chain flag carries the decision.
            self.pending_timing_cycles_per_frame = Some(cycles_per_frame);
            return;
        }
        self.apply_timing_cycles_per_frame(cycles_per_frame);
    }

    /// Apply the timing update immediately (called at frame boundary or when
    /// no scanline chain is active).
    pub fn apply_timing_cycles_per_frame(&mut self, cycles_per_frame: u64) {
        self.pending_timing_cycles_per_frame = None;
        self.timing_cycles_per_frame = cycles_per_frame;

        // VBI duration for bit 0 of port 3DAh, derived from the programmed
        // vertical blank start/end counters. Frame *rate*
        // (cycles_per_frame) is unchanged — only the vblank window widens.
        // timing_retrace_cycles also drives tick(), get_current_scanline(),
        // is_in_vblank(), and the scanline event chain, which all consume it
        // consistently. The 8% value survives only as a degenerate-register
        // fallback.
        let cpf = cycles_per_frame;
        let vtotal = (self.crtc_vtotal() as u64).max(1);
        let vdispend = self.get_vertical_display_end() as u64 + 1; // VDE is last visible line
        let vbstart = u64::from(self.crtc_vbstart());
        let vbend = u64::from(self.crtc_vbend());
        let vrstart = self.crtc_vrstart() as u64;
        let vrend = self.crtc_vrend() as u64;

        let blank_lines = vbend.saturating_sub(vbstart).min(vtotal.saturating_sub(1));
        let mut retrace = if blank_lines != 0 && vbstart < vtotal && cpf >= vtotal {
            (cpf * blank_lines) / vtotal
        } else if vdispend < vtotal && cpf >= vtotal {
            // Some software leaves blank registers degenerate; retain the
            // display-end fallback for standard VGA compatibility.
            (cpf * (vtotal - vdispend)) / vtotal
        } else {
            (cpf * 80) / 1000 // degenerate-register fallback (~8%)
        };
        if retrace == 0 {
            retrace = 1;
        }
        if retrace >= self.timing_cycles_per_frame {
            retrace = self.timing_cycles_per_frame.saturating_sub(1).max(1);
        }
        self.timing_retrace_cycles = retrace;

        // Compute CRTC-derived VSync pulse timing for bit 3 of port 3DAh.
        // The VSync pulse is a narrow window within the VBI, separate from
        // the VBI duration above.
        if vbstart >= vtotal || vrstart >= vtotal || cpf < vtotal {
            // Degenerate CRTC: VSync pulse covers entire VBI (old behavior for bit 3)
            self.vsync_offset_cycles = 0;
            self.vsync_duration_cycles = retrace;
        } else {
            let cycles_per_scanline = cpf / vtotal;
            let vsync_offset_lines = if vrstart >= vbstart {
                vrstart - vbstart
            } else {
                vtotal - vbstart + vrstart
            };
            let vsync_duration_lines = vrend.saturating_sub(vrstart).max(1);
            self.vsync_offset_cycles = vsync_offset_lines * cycles_per_scanline;
            self.vsync_duration_cycles = (vsync_duration_lines * cycles_per_scanline).max(1);

            // Clamp: VSync pulse must fit within the VBI
            if self.vsync_offset_cycles + self.vsync_duration_cycles > retrace {
                self.vsync_duration_cycles =
                    retrace.saturating_sub(self.vsync_offset_cycles).max(1);
            }
        }

        self.cycles_since_vblank %= self.timing_cycles_per_frame;
        self.recompute_scanline_timing();
    }

    /// Refresh cached scanline counts from current VGA mode.
    /// Called from set_timing_cycles_per_frame() and mode change handlers.
    pub fn recompute_scanline_timing(&mut self) {
        let total = self.get_total_scanlines() as u32;
        // Test-only override: see `set_total_scanlines_test_override`.
        // Compiled out of production builds via the `test-util` gate.
        #[cfg(any(test, feature = "test-util"))]
        let total = self.total_scanlines_test_override.unwrap_or(total);
        self.total_scanlines_cached = total;
        self.visible_scanlines_cached = self.get_scanlines_per_frame() as u32;
    }

    /// Test-only hook: force `recompute_scanline_timing()` to set
    /// `total_scanlines_cached` to the supplied value (or restore CRTC
    /// behaviour with `None`).
    ///
    /// Exists because `crtc_vtotal()` always returns `>= 2`, making the
    /// post-recompute zero-scanlines case otherwise unreachable from a
    /// unit test — yet the defensive `> 0` guard in
    /// `on_vga_start_of_frame` still needs coverage to prevent a regression
    /// that would divide by `total_scanlines_cached` when scheduling the
    /// first scanline event.
    ///
    /// Gated behind the `test-util` feature; not callable from release
    /// builds of `gp64`.
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_total_scanlines_test_override(&mut self, value: Option<u32>) {
        self.total_scanlines_test_override = value;
    }

    /// EXP-N007b: render exactly the lines the per-line event chain would
    /// have rendered by `now_cycle` — catch up to the EVENT GRID, never the
    /// analytic beam. Grid index = largest `n` with
    /// `floor(n*cpf/total) <= elapsed`, i.e. `ceil((elapsed+1)*total/cpf)-1`
    /// (grid and beam thresholds are phase-offset, so rendering past the
    /// last grid point would render a line the per-line chain renders only
    /// AFTER the pending register write applies — panel lens-3 F2).
    ///
    /// Called at the emulator boundary BEFORE a deferred raster-sensitive
    /// register write is applied; replicates the render step of
    /// `on_vga_scanline` exactly.
    /// EXP-N007b: the catch-up target — the visible line the per-line event
    /// chain would have rendered through by `now_cycle`, derived from the
    /// EVENT GRID, never the analytic beam (grid and beam thresholds are
    /// phase-offset — panel lens-3 F2). Grid index = largest `n` with
    /// `floor(n*cpf/total) <= elapsed`, i.e. `ceil((elapsed+1)*total/cpf)-1`.
    /// `None` when the grid position is still in vblank or timing is
    /// degenerate — nothing to render.
    fn catch_up_target_line(&self, now_cycle: u64) -> Option<u32> {
        let total = self.total_scanlines_cached as u64;
        let cpf = self.timing_cycles_per_frame;
        if total == 0 || cpf == 0 {
            return None;
        }
        let elapsed_now = now_cycle.saturating_sub(self.frame_start_cycle);
        let n = elapsed_now
            .saturating_add(1)
            .saturating_mul(total)
            .div_ceil(cpf)
            .saturating_sub(1)
            .min(total.saturating_sub(1));
        let grid_elapsed = (n * cpf) / total;
        let retrace = self.timing_retrace_cycles;
        if grid_elapsed < retrace {
            return None;
        }
        let visible_lines = self.visible_scanlines_cached;
        if visible_lines == 0 {
            return None;
        }
        let visible_line = Self::compute_visible_line(grid_elapsed, retrace, cpf, visible_lines);
        if visible_line >= visible_lines {
            return None;
        }
        Some(visible_line)
    }

    /// EXP-N007b: render exactly the lines the per-line event chain would
    /// have rendered by `now_cycle` (see [`Self::catch_up_target_line`]).
    /// Called at the device boundary BEFORE a deferred raster-sensitive
    /// register write is applied; replicates the render step of
    /// `on_vga_scanline` exactly.
    pub fn render_catch_up_to_grid(&mut self, now_cycle: u64, memory: &mut Memory) {
        let Some(visible_line) = self.catch_up_target_line(now_cycle) else {
            return;
        };
        if visible_line as i32 == self.last_rendered_line {
            return;
        }
        let last = self.last_rendered_line;
        let start = if last < 0 || (visible_line as i32) < last {
            visible_line
        } else {
            (last + 1) as u32
        };
        for line in start..=visible_line {
            self.render_scanline(line, memory);
        }
        self.last_rendered_line = visible_line as i32;
    }

    /// EXP-N007b: latch a VGA-window port write for render-before-apply, or
    /// apply it immediately when deferral buys nothing. A write defers only
    /// when ALL hold: (1) the port can alter what the per-scanline renderers
    /// read ([`Self::raster_write_is_sensitive`]); (2) the event-grid beam
    /// has passed at least one line the frame has not yet rendered (so the
    /// catch-up would actually render something — this one gate subsumes the
    /// vblank exemption AND rate-limits bursts to one device boundary per
    /// scanline: a 768-byte DAC fade inside one line-time breaks the batch
    /// once, not 768 times); (3) no earlier write is already latched (order
    /// preservation — anything behind a latched write queues too).
    ///
    /// The queue drains at the next device boundary via
    /// [`Self::service_raster_writes`] (reached from `flush_synchronous_vga`);
    /// `should_break_batch` forces that boundary before the next guest
    /// instruction, so guest readback never observes a latched-but-unapplied
    /// write. Queue overflow (>4 deferring writes inside ONE instruction)
    /// falls back to direct apply and is counted, not hidden.
    pub fn latch_or_apply_raster_write(&mut self, port: u16, value: u8, now_cycle: u64) {
        let must_defer = self.raster_write_count > 0
            || (self.raster_write_is_sensitive(port)
                && self
                    .catch_up_target_line(now_cycle)
                    .is_some_and(|line| line as i32 != self.last_rendered_line));
        if !must_defer {
            self.write_port(port, value);
            return;
        }
        if (self.raster_write_count as usize) < self.raster_write_queue.len() {
            self.raster_write_queue[self.raster_write_count as usize] = (port, value);
            self.raster_write_count += 1;
        } else {
            self.raster_write_overflows += 1;
            self.write_port(port, value);
        }
    }

    /// EXP-N007b: latched raster writes awaiting the render-before-apply
    /// boundary. Feeds `should_break_batch`.
    pub fn has_pending_raster_writes(&self) -> bool {
        self.raster_write_count > 0
    }

    /// EXP-N007b: the render-before-apply service. Renders catch-up to the
    /// event grid, then applies the latched writes in arrival order. Runs at
    /// the device boundary (`flush_synchronous_vga`), where `Memory` is
    /// available on every engine's path.
    pub fn service_raster_writes(&mut self, now_cycle: u64, memory: &mut Memory) {
        if self.raster_write_count == 0 {
            return;
        }
        self.render_catch_up_to_grid(now_cycle, memory);
        for i in 0..self.raster_write_count as usize {
            let (port, value) = self.raster_write_queue[i];
            self.write_port(port, value);
        }
        self.raster_write_count = 0;
    }

    /// EXP-N007b: can a write to `port` alter what the per-scanline
    /// renderers read? Pure port-space classification; the beam-position
    /// gate lives in [`Self::latch_or_apply_raster_write`].
    pub fn raster_write_is_sensitive(&self, port: u16) -> bool {
        match port {
            // Index/pointer registers: alter no rendered state themselves.
            0x3B4 | 0x3D4 | 0x3C4 | 0x3CE | 0x3C7 | 0x3C8 => false,
            // Feature control (write at 3BA/3DA): no renderer reads it.
            0x3BA | 0x3DA => false,
            0x3B5 | 0x3D5 => !matches!(self.crtc_index, 0x0C | 0x0D),
            _ => true,
        }
    }

    /// Map elapsed active display cycles to visible line index (HLD §4.16).
    /// Handles double-scan modes by linear interpolation.
    pub fn compute_visible_line(
        elapsed: u64,
        retrace_cycles: u64,
        frame_cycles: u64,
        visible_lines: u32,
    ) -> u32 {
        let active_elapsed = elapsed.saturating_sub(retrace_cycles);
        let active_total = frame_cycles.saturating_sub(retrace_cycles);
        if active_total == 0 {
            return 0;
        }
        ((active_elapsed * visible_lines as u64) / active_total) as u32
    }

    /// Set CGA Color Select Register (for BIOS).
    /// Also syncs the VGA AC palette registers so that programs using the
    /// VGA palette pipeline see correct CGA colors.
    pub fn set_cga_color_select(&mut self, val: u8) {
        self.color_select_reg = val;
        if matches!(self.mode(), 0x04..=0x06) {
            self.init_ac_palette_for_mode();
        }
    }

    /// Initialize VGA Attribute Controller palette registers (AC[0]-AC[15])
    /// for the current video mode. On real VGA, the BIOS programs these from
    /// the Video Parameter Table during mode set.
    ///
    /// This is also called when the CGA color select register changes,
    /// to keep AC[0]-AC[3] in sync with the CGA palette state.
    fn init_ac_palette_for_mode(&mut self) {
        self.init_ac_palette_for_mode_as(self.mode());
    }

    /// Like `init_ac_palette_for_mode`, but accepts an explicit mode tag.
    /// Used inside `set_mode_internal` where `self.mode()` still returns
    /// the *old* mode (registers haven't been programmed yet).
    fn init_ac_palette_for_mode_as(&mut self, mode: u8) {
        // Standard VGA BIOS VPT palette for text and EGA/VGA 16-color modes.
        // Maps pixel indices to 64-color EGA DAC entries.
        const VPT_PALETTE_16: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D,
            0x3E, 0x3F,
        ];
        const VPT_PALETTE_MONO: [u8; 16] = [
            0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x10, 0x18, 0x18, 0x18, 0x18, 0x18,
            0x18, 0x18,
        ];

        match mode {
            // CGA 320x200 4-color modes — derive from color_select_reg. The
            // BIOS CGA DAC layout stores bright IRGB colors 8-15 at entries
            // 16-23, so translate each selector before storing it in AC.
            0x04 => {
                let bg = self.color_select_reg & 0x0F;
                let intensity: u8 = if (self.color_select_reg & 0x10) != 0 {
                    8
                } else {
                    0
                };
                let palette_sel = (self.color_select_reg & 0x20) != 0;
                self.attribute_regs[0] = cga_irgb_to_dac_index(bg);
                if palette_sel {
                    // Palette 1: Cyan / Magenta / White
                    self.attribute_regs[1] = cga_irgb_to_dac_index(3 + intensity);
                    self.attribute_regs[2] = cga_irgb_to_dac_index(5 + intensity);
                    self.attribute_regs[3] = cga_irgb_to_dac_index(7 + intensity);
                } else {
                    // Palette 0: Green / Red / Brown
                    self.attribute_regs[1] = cga_irgb_to_dac_index(2 + intensity);
                    self.attribute_regs[2] = cga_irgb_to_dac_index(4 + intensity);
                    self.attribute_regs[3] = cga_irgb_to_dac_index(6 + intensity);
                }
            }
            0x05 => {
                let bg = self.color_select_reg & 0x0F;
                let intensity: u8 = if (self.color_select_reg & 0x10) != 0 {
                    8
                } else {
                    0
                };
                self.attribute_regs[0] = cga_irgb_to_dac_index(bg);
                // Mode 05: Cyan / Red / White (ignores palette select bit)
                self.attribute_regs[1] = cga_irgb_to_dac_index(3 + intensity);
                self.attribute_regs[2] = cga_irgb_to_dac_index(4 + intensity);
                self.attribute_regs[3] = cga_irgb_to_dac_index(7 + intensity);
            }
            0x06 => {
                // CGA 640x200 2-color: pixel 0 = black, pixel 1 = foreground
                self.attribute_regs[0] = 0;
                self.attribute_regs[1] = cga_irgb_to_dac_index(self.color_select_reg & 0x0F);
            }
            // MDA text mode
            0x07 => {
                self.attribute_regs[..16].copy_from_slice(&VPT_PALETTE_MONO);
            }
            // EGA 640x350 monochrome (Mode 0Fh) — special 2-plane mono palette
            0x0F => {
                // Plane 0 = video, Plane 2 = intensity
                // pixel values: 0=off, 1=video, 4=intensity, 5=video+intensity
                // Real VGA BIOS uses EGA-compatible AC values (0x08, 0x18) which
                // index into the EGA 64-color DAC. Our DAC entries 0-15 have
                // standard CGA/EGA colors; remap to use those directly:
                //   AC[0] = 0x00 (black) — video=0, intensity=0
                //   AC[1] = 0x07 (light gray, DAC[7]=(170,170,170)) — video=1
                //   AC[4] = 0x00 (black) — intensity alone produces nothing
                //   AC[5] = 0x0F (white, DAC[15]=(255,255,255)) — video+intensity
                const MONO_0F: [u8; 16] = [
                    0x00, 0x07, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
                    0x0F, 0x00, 0x00,
                ];
                self.attribute_regs[..16].copy_from_slice(&MONO_0F);
            }
            // VGA 640x480 2-color monochrome (Mode 11h) — monochrome palette
            0x11 => {
                // Pixel 0 = black (DAC[0]), pixel 1 = white (DAC[15]).
                // Real VGA BIOS loads a monochrome DAC table where DAC[1]=white,
                // but our DAC keeps the standard 16-color table where DAC[1]=blue.
                // Map all non-zero pixel values to DAC[15] (white) instead.
                self.attribute_regs[0] = 0x00;
                for i in 1..16 {
                    self.attribute_regs[i] = 0x0F;
                }
            }
            // Text and EGA/VGA 16-color graphics modes
            0x00 | 0x01 | 0x02 | 0x03 | 0x0D | 0x0E | 0x10 | 0x12 => {
                self.attribute_regs[..16].copy_from_slice(&VPT_PALETTE_16);
            }
            // Mode 13h and others: identity mapping
            _ => {
                for i in 0..16 {
                    self.attribute_regs[i] = i as u8;
                }
            }
        }
        self.palette16_lut_dirty = true;
    }

    /// Get CGA Color Select Register
    pub fn get_cga_color_select(&self) -> u8 {
        self.color_select_reg
    }

    #[must_use]
    pub const fn is_supported_bios_mode(mode: u8) -> bool {
        matches!(mode & 0x7F, 0x00..=0x0A | 0x0D..=0x13)
    }

    /// Set video mode. Returns `false` — leaving every register, the
    /// framebuffer and the mode number untouched — when the adapter has no
    /// such mode (see [`Self::is_supported_bios_mode`]).
    pub fn set_mode_bios(&mut self, mode: u8) -> bool {
        self.set_mode_internal(mode, true)
    }

    fn default_cga_mode_control_for_bios_mode(mode: u8) -> Option<u8> {
        match mode & 0x7F {
            // 40x25 b/w text: video enable (bit 3) + b/w bit (bit 2)
            0x00 => Some(0x0C),
            // 40x25 colour text: video enable only
            0x01 => Some(0x08),
            // 80x25 b/w text: video enable + 80-col + b/w bit
            0x02 => Some(0x0D),
            // 80x25 colour text / mono text: video enable + 80-col
            0x03 | 0x07 => Some(0x09),
            // 320x200 4-color (mode 04) / 320x200 b/w (mode 05)
            0x04 => Some(0x0A),
            0x05 => Some(0x0E),
            // 640x200 2-color (mode 06)
            0x06 => Some(0x1A),
            _ => None,
        }
    }

    fn display_gate_blanks_render_mode(&self, render_mode: u8) -> bool {
        match render_mode {
            0x04..=0x06 => (self.cga_mode_control_reg & 0x08) == 0,
            _ => false,
        }
    }

    fn cga_mode_control_graphics_mode(&self) -> Option<u8> {
        if (self.cga_mode_control_reg & 0x02) == 0 {
            return None;
        }
        if (self.cga_mode_control_reg & 0x10) != 0 {
            return Some(0x06);
        }
        if (self.cga_mode_control_reg & 0x04) != 0 {
            return Some(0x05);
        }
        Some(0x04)
    }

    fn set_mode_internal(&mut self, mode: u8, bios: bool) -> bool {
        let new_mode = mode & 0x7F;
        if !Self::is_supported_bios_mode(new_mode) {
            // Refuse before touching anything. Falling through used to leave
            // the adapter in a state no real mode produces: a 640x400
            // framebuffer with text-mode graphics-controller mapping (B8000)
            // but a *graphics* attribute mode control, over whatever CRTC
            // timing the previous mode had programmed (MDD-BUG-KILN-00641).
            tracing::debug!(
                requested = format_args!("{:02X}", new_mode),
                "VGA mode set refused: no such BIOS mode"
            );
            return false;
        }
        let old_mode = self.mode();
        tracing::debug!(
            old_mode = format_args!("{:02X}", old_mode),
            new_mode = format_args!("{:02X}", new_mode),
            "VGA mode change"
        );

        if old_mode != new_mode {
            self.pending_capture_events
                .push(gp64_types::FiredCaptureEvent {
                    kind: gp64_types::CaptureEventKind::VgaModeChange { old_mode, new_mode },
                });
        }
        self.attribute_flip_flop = false; // Reset attribute controller flip-flop per VGA spec
        let is_text_mode = matches!(new_mode, 0x00 | 0x01 | 0x02 | 0x03 | 0x07);

        // Reset Graphics Controller registers to BIOS defaults.
        // Critical: stale values from a previous mode corrupt writes.
        // Text presets need GC5 odd/even reads for interleaved text cells;
        // graphics presets keep odd/even reads disabled.
        self.graphics_regs[0x00] = 0x00; // Set/Reset: all planes use CPU data
        self.graphics_regs[0x01] = 0x00; // Enable Set/Reset: disabled for all planes
        self.graphics_regs[0x02] = 0x00; // Color Compare
        self.graphics_regs[0x03] = 0x00; // Data Rotate: no rotation, no logical op
        self.graphics_regs[0x04] = 0x00; // Read Map Select: plane 0
        self.graphics_regs[0x05] = if is_text_mode { 0x10 } else { 0x00 };
        self.graphics_regs[0x07] = 0x0F; // Color Don't Care: all planes
        self.graphics_regs[0x08] = 0xFF; // Bit Mask: all bits
                                         // Note: GC reg 0x06 (Misc) is set per-mode below.
        self.palette_address_source = true; // Enable display (PAS=1) - real BIOS always enables display on mode set

        // Attribute Mode Control (reg 0x10): set per VGA BIOS defaults.
        // Text modes: bit 2 = Line Graphics Enable (9th-col duplication for
        //   box-drawing chars 0xC0-0xDF), bit 3 = Blink Enable.
        // Graphics modes: bit 0 = Graphics mode, no line graphics or blink.
        if is_text_mode {
            // 0x0C = Line Graphics Enable (bit 2) + Blink Enable (bit 3)
            self.attribute_regs[0x10] = 0x0C;
            // AR13 = 0x08 maps to "no pixel shift" in 9-dot text modes
            // per the VGA specification.
            self.attribute_regs[0x13] = 0x08;
        } else {
            // Graphics: bit 0 = graphics mode, clear 8-bit color (bit 6)
            self.attribute_regs[0x10] = (self.attribute_regs[0x10] & !0x40) | 0x01;
            // Graphics / 8-dot text modes: no pel panning (raw 0 = shift 0).
            self.attribute_regs[0x13] = 0x00;
        }
        // Reset Color Plane Enable to all planes (BIOS default)
        self.attribute_regs[0x12] = 0x0F;

        // On BIOS-driven mode set, reset CGA control regs to reasonable defaults for that mode.
        if bios {
            if let Some(default_control) = Self::default_cga_mode_control_for_bios_mode(new_mode) {
                self.cga_mode_control_reg = default_control;
            }
            // Reset color_select_reg for CGA graphics modes.
            // Per IBM VGA BIOS video parameter table defaults:
            self.color_select_reg = match new_mode {
                // Mode 04h/05h: Palette 1 (Cyan/Magenta/White) + High Intensity
                0x04 | 0x05 => 0x30,
                // Mode 06h: White foreground (0x0F in low nibble)
                0x06 => 0x0F,
                _ => self.color_select_reg,
            };
        }

        // Initialize VGA Attribute Controller palette registers (AC[0]-AC[15])
        // for the new mode. On real VGA, the BIOS programs these from the Video
        // Parameter Table. Programs use INT 10h AH=10h to modify
        // these registers; the initial values must match real hardware.
        // Use _as(new_mode) because self.mode() still returns the old mode
        // at this point (registers haven't been programmed yet).
        self.init_ac_palette_for_mode_as(new_mode);

        // Load the BIOS DAC table (entries 0-63). Color modes start with the
        // rgbRGB layout used by VGA text/EGA modes; CGA modes replace it with
        // their CGA-specific layout after this initialization. The 6-bit EGA
        // palette encodes RGB using:
        //   bits 5-3: R/G/B secondary (intensity), bits 2-0: R/G/B primary.
        // Color modes load the standard EGA 64-color palette.
        // Monochrome modes (07h, 0Fh, 11h) load a grayscale version.
        if bios {
            let mono = matches!(new_mode, 0x07 | 0x0F | 0x11);
            for i in 0u8..64 {
                let r_pri = (i >> 2) & 1;
                let g_pri = (i >> 1) & 1;
                let b_pri = i & 1;
                let r_sec = (i >> 5) & 1;
                let g_sec = (i >> 4) & 1;
                let b_sec = (i >> 3) & 1;

                // Each channel: 4 levels based on primary+secondary bits
                //   00 = 0x00, 01 = 0x2A, 10 = 0x15, 11 = 0x3F  (6-bit DAC)
                fn ega_channel(pri: u8, sec: u8) -> u8 {
                    let v6 = match (pri, sec) {
                        (0, 0) => 0x00u8,
                        (1, 0) => 0x2A,
                        (0, 1) => 0x15,
                        (1, 1) => 0x3F,
                        _ => 0,
                    };
                    // 6-bit → 8-bit
                    (v6 << 2) | (v6 >> 4)
                }

                let (r, g, b) = if mono {
                    // Monochrome: collapse to grayscale using max channel.
                    // Any non-zero color component produces visible gray/white.
                    // This matches real VGA monochrome DAC behavior where the
                    // brightest channel determines the display intensity.
                    let r8 = ega_channel(r_pri, r_sec);
                    let g8 = ega_channel(g_pri, g_sec);
                    let b8 = ega_channel(b_pri, b_sec);
                    let gray = r8.max(g8).max(b8);
                    (gray, gray, gray)
                } else {
                    // Standard EGA 64-color palette
                    // Exception: entry 6 (brown) uses dark yellow (0x2A,0x15,0x00)
                    // instead of pure dark yellow (0x2A,0x2A,0x00) on real VGA.
                    if i == 0x06 {
                        (0xAA, 0x55, 0x00) // Brown (8-bit)
                    } else {
                        (
                            ega_channel(r_pri, r_sec),
                            ega_channel(g_pri, g_sec),
                            ega_channel(b_pri, b_sec),
                        )
                    }
                };

                self.dac[i as usize] = (r, g, b);
            }

            self.palette_lut_dirty = true;
            self.palette16_lut_dirty = true;

            // The palette for both two-color and four-color CGA modes
            // has 64 entries containing repeated normal and
            // bright 8-color banks; updating only DAC[0..16] leaves indices
            // 16-63 on the EGA table.
            if matches!(new_mode, 0x04..=0x06) {
                for i in 0..64 {
                    self.dac[i] = cga_palette_2_entry(i);
                }
            }
        }

        // VGA BIOS parameter tables: standard CRTC timing, Misc Output, and Sequencer
        // register values for each mode. These are the values a real VGA BIOS programs
        // during INT 10h mode set. Register readback must describe the selected
        // mode's dot clock, geometry and refresh timing.
        //
        // Sources: IBM VGA Technical Reference, FreeVGA project, real BIOS dumps.
        // Format: [CRTC 0x00..0x18], misc_output, [SEQ 0x01..0x04]
        struct VgaTimingParams {
            crtc: [u8; 25],  // CRTC regs 0x00-0x18
            misc_output: u8, // Misc Output Register (port 3C2)
            seq: [u8; 4],    // Sequencer regs 0x01-0x04
        }

        let timing = match new_mode {
            // Mode 00h/01h: 40x25 text (9-dot, 360x400, 70Hz)
            0x00 | 0x01 => Some(VgaTimingParams {
                crtc: [
                    0x2D, 0x27, 0x28, 0x90, 0x2B, 0xA0, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
                ],
                misc_output: 0x67,
                seq: [0x08, 0x03, 0x00, 0x02],
            }),
            // Mode 02h/03h: 80x25 text (9-dot, 720x400, 70Hz)
            0x02 | 0x03 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
                ],
                misc_output: 0x67,
                seq: [0x00, 0x03, 0x00, 0x02],
            }),
            // Mode 04h/05h: 320x200 4-color CGA (8-dot, 320x200, 70Hz double-scan)
            0x04 | 0x05 => Some(VgaTimingParams {
                crtc: [
                    0x2D, 0x27, 0x28, 0x90, 0x2B, 0x80, 0xBF, 0x1F, 0x00, 0xC1, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x00, 0x96, 0xB9, 0xA2, 0xFF,
                ],
                misc_output: 0x63,
                seq: [0x09, 0x03, 0x00, 0x02],
            }),
            // Mode 06h: 640x200 2-color CGA (8-dot, 640x200, 70Hz double-scan)
            0x06 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0xC1, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x00, 0x96, 0xB9, 0xC2, 0xFF,
                ],
                misc_output: 0x63,
                seq: [0x01, 0x01, 0x00, 0x06],
            }),
            // Mode 07h: 80x25 monochrome text (9-dot, 720x400, 70Hz)
            0x07 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
                ],
                misc_output: 0x66,
                seq: [0x00, 0x03, 0x00, 0x02],
            }),
            // Mode 0Dh: 320x200 16-color EGA (8-dot, 320x200, 70Hz double-scan)
            0x0D => Some(VgaTimingParams {
                crtc: [
                    0x2D, 0x27, 0x28, 0x90, 0x2B, 0x80, 0xBF, 0x1F, 0x00, 0xC0, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x00, 0x96, 0xB9, 0xE3, 0xFF,
                ],
                misc_output: 0x63,
                seq: [0x09, 0x0F, 0x00, 0x06],
            }),
            // Mode 0Eh: 640x200 16-color EGA (8-dot, 640x200, 70Hz double-scan)
            0x0E => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0xC0, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x00, 0x96, 0xB9, 0xE3, 0xFF,
                ],
                misc_output: 0x63,
                seq: [0x01, 0x0F, 0x00, 0x06],
            }),
            // Mode 0Fh: 640x350 monochrome (9-dot, 640x350, 70Hz)
            0x0F => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x40, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x83, 0x85, 0x5D, 0x28, 0x0F, 0x63, 0xBA, 0xE3, 0xFF,
                ],
                misc_output: 0xA2,
                seq: [0x01, 0x05, 0x00, 0x06],
            }),
            // Mode 10h: 640x350 16-color EGA (8-dot, 640x350, 70Hz)
            0x10 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x40, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x83, 0x85, 0x5D, 0x28, 0x0F, 0x63, 0xBA, 0xE3, 0xFF,
                ],
                misc_output: 0xA3,
                seq: [0x01, 0x0F, 0x00, 0x06],
            }),
            // Mode 11h: 640x480 2-color VGA (8-dot, 640x480, 60Hz)
            0x11 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0x0B, 0x3E, 0x00, 0x40, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0xEA, 0x8C, 0xDF, 0x28, 0x00, 0xE7, 0x04, 0xC3, 0xFF,
                ],
                misc_output: 0xE3,
                seq: [0x01, 0x01, 0x00, 0x06],
            }),
            // Mode 12h: 640x480 16-color VGA (8-dot, 640x480, 60Hz)
            0x12 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0x0B, 0x3E, 0x00, 0x40, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0xEA, 0x8C, 0xDF, 0x28, 0x00, 0xE7, 0x04, 0xE3, 0xFF,
                ],
                misc_output: 0xE3,
                seq: [0x01, 0x0F, 0x00, 0x06],
            }),
            // Mode 13h: 320x200 256-color VGA (8-dot, 320x200, 70Hz double-scan)
            0x13 => Some(VgaTimingParams {
                crtc: [
                    0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x41, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x40, 0x96, 0xB9, 0xA3, 0xFF,
                ],
                misc_output: 0x63,
                seq: [0x01, 0x0F, 0x00, 0x0E],
            }),
            _ => None,
        };

        // Apply VGA BIOS timing parameters if available for this mode.
        if let Some(t) = &timing {
            // Program all 25 CRTC registers (0x00-0x18).
            for (i, &val) in t.crtc.iter().enumerate() {
                self.crtc_regs[i] = val;
            }
            // Clear CR11 bit 7 (CRTC protect) so programs can freely modify
            // CRTC registers 0x00-0x07 without explicitly unlocking first.
            // Real VGA BIOS leaves the protect bit set, but many DOS programs
            // (especially Mode X setup code) assume CRTC is writable after
            // INT 10h mode set. Keep these registers writable for mode setup.
            self.crtc_regs[0x11] &= 0x7F;
            self.misc_output = t.misc_output;

            // Program Sequencer registers 0x01-0x04.
            for (i, &val) in t.seq.iter().enumerate() {
                self.sequencer_regs[i + 1] = val;
            }
            self.recompute_timing();
        }

        match new_mode {
            0x00 | 0x01 => {
                // 40x25 text — double-wide chars (18 dots each) give same 720px as 80-col
                self.columns = 40;
                self.rows = 25;
                self.fb_width = 720;
                self.fb_height = 400;
                self.graphics_regs[0x06] = 0x0E; // text + odd/even + map=3 (B8000-BFFFF)
            }
            0x02 | 0x03 => {
                // 80x25 text
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 400;
                self.graphics_regs[0x06] = 0x0E; // text + odd/even + map=3 (B8000-BFFFF)
            }
            0x04 | 0x05 => {
                // 320x200 4-color
                self.columns = 40;
                self.rows = 25;
                self.fb_width = 320;
                self.fb_height = 200;
                self.graphics_regs[0x06] = 0x0F; // graphics + odd/even + map=3 (B8000-BFFFF)
            }
            0x06 => {
                // 640x200 2-color
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 200;
                self.graphics_regs[0x06] = 0x0F; // graphics + odd/even + map=3 (B8000-BFFFF)
            }
            0x0D => {
                // 320x200 16-color
                self.columns = 40;
                self.rows = 25;
                self.fb_width = 320;
                self.fb_height = 200;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x0E => {
                // 640x200 16-color
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 200;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x10 => {
                // 640x350 16-color
                self.columns = 80;
                self.rows = 43;
                self.fb_width = 640;
                self.fb_height = 350;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x12 => {
                // 640x480 16-color
                self.columns = 80;
                self.rows = 60;
                self.fb_width = 640;
                self.fb_height = 480;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x0F => {
                // EGA 640x350 monochrome (2-plane mode: planes 0 and 2)
                self.columns = 80;
                self.rows = 25; // 350/14 = 25 rows with 8x14 font
                self.fb_width = 640;
                self.fb_height = 350;
                // Set graphics mode bit
                self.attribute_regs[0x10] |= 0x01;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x11 => {
                // VGA 640x480 2-color monochrome (1-plane)
                self.columns = 80;
                self.rows = 30; // 480/16 = 30 rows with 8x16 font
                self.fb_width = 640;
                self.fb_height = 480;
                self.attribute_regs[0x10] |= 0x01;
                self.graphics_regs[0x06] = 0x05; // graphics + map=1 (A0000-AFFFF)
            }
            0x13 => {
                // 320x200 256-color (Mode 13h)
                // BIOS teletype typically uses 8x8 font = 40x25 character grid.
                self.columns = 40;
                self.rows = 25;
                self.fb_width = 320;
                self.fb_height = 200;
                // Ensure chain-4 is enabled by default in Mode 13h (linear 256-color).
                // INT 10h mode set establishes the sequencer memory layout.
                self.sequencer_regs[4] |= 0x08;
                // 8-bit color enable (bit 6) — required for correct 256-color palette lookup
                // and pel panning value interpretation.
                self.attribute_regs[0x10] |= 0x41; // bit 6 (256-color) + bit 0 (graphics)
                                                   // Program Graphics Controller Misc register to VGA BIOS-compatible Mode 13h default:
                                                   // graphics mode + A0000-AFFFF (64K) aperture.
                self.graphics_regs[0x06] = 0x05;
            }
            0x08 => {
                self.columns = 20;
                self.rows = 25;
                self.fb_width = 160;
                self.fb_height = 200;
            }
            0x09 => {
                self.columns = 40;
                self.rows = 25;
                self.fb_width = 320;
                self.fb_height = 200;
            }
            0x0A => {
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 200;
            }
            0x07 => {
                // 80x25 monochrome text
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 400;
                self.graphics_regs[0x06] = 0x0A; // text + odd/even + map=2 (B0000-B7FFF)
            }
            _ => {
                // Unreachable: `is_supported_bios_mode` gates entry to exactly
                // the arms above. Kept only to make the `u8` match exhaustive.
                debug_assert!(
                    false,
                    "unsupported BIOS mode {new_mode:#04X} reached geometry table"
                );
                self.columns = 80;
                self.rows = 25;
                self.fb_width = 640;
                self.fb_height = 400;
                self.graphics_regs[0x06] = 0x0E; // text + odd/even + map=3 (B8000-BFFFF)
            }
        }

        // Reallocating the construction base invalidates the completed
        // frame even at the same length (MDD-BUG-CRUCIBLE-00347).
        self.framebuffer = vec![0u32; self.fb_width * self.fb_height];

        // Note: Planar memory clearing is now handled externally via Memory.
        // When switching to planar modes, the caller should clear Memory's planes.

        // Reset cursor
        self.cursor_pos.fill((0, 0));
        self.active_page = 0;
        // Reset CRTC start address.
        self.crtc_regs[0x0C] = 0;
        self.crtc_regs[0x0D] = 0;
        // Default line compare to disabled (0x3FF) to avoid unintended split-screen behavior.
        self.crtc_regs[0x18] = 0xFF;
        self.crtc_regs[0x07] |= 0x10; // Line compare bit 8
        self.crtc_regs[0x09] |= 0x40; // Line compare bit 9
        if vga_mode_trace_enabled() {
            tracing::debug!(
                target: "gp64::hw::vga::mode",
                old_mode = format_args!("{:02X}", old_mode),
                new_mode = format_args!("{:02X}", new_mode),
                bios,
                mode_x = self.is_mode_x(),
                cols = self.columns,
                rows = self.rows,
                fb_width = self.fb_width,
                fb_height = self.fb_height,
                "VGA mode change"
            );
        }
        let vde = self.get_vertical_display_end();
        let overflow = self.crtc_regs[0x07];
        let max_scan = self.crtc_regs[0x09];
        let line_compare = self.crtc_regs[0x18];
        vga_reg_trace_emit(format_args!(
            "SET MODE regs: mode=0x{new_mode:02x} vde={vde} overflow=0x{overflow:02x} \
             max_scan=0x{max_scan:02x} line_cmp=0x{line_compare:02x}"
        ));
        self.mode_change_pending = true;
        self.pipeline_dirty = true;
        true
    }

    fn default_graphics_char_height(&self) -> usize {
        match self.mode() {
            0x0F | 0x10 => 14,
            0x11 | 0x12 => 16,
            _ => 8,
        }
    }

    fn graphics_font_vector(memory: &Memory, vector: usize) -> Option<usize> {
        let offset = memory.read_u16(vector * 4);
        let segment = memory.read_u16(vector * 4 + 2);
        (segment != 0 || offset != 0).then(|| Memory::seg_offset_to_linear(segment, offset))
    }

    fn graphics_font_height(&self, memory: &Memory) -> usize {
        if matches!(self.mode(), 4..=6) {
            return 8;
        }
        let height = usize::from(memory.read_u16(0x485));
        if height == 0 || Self::graphics_font_vector(memory, 0x43).is_none() {
            self.default_graphics_char_height()
        } else {
            height
        }
    }

    fn graphics_char_cell(&self, memory: &Memory) -> (usize, usize) {
        // Bound drawing and scrolling, but keep the accepted font's stride.
        (
            8,
            self.graphics_font_height(memory).min(self.fb_height.max(1)),
        )
    }

    /// CGA uses INT 1Fh only for its upper half. EGA/VGA uses all 256
    /// characters from INT 43h, with the selected BDA scanline stride.
    fn graphics_font_row(&self, memory: &Memory, ch: u8, cy: usize) -> u8 {
        let cga = matches!(self.mode(), 4..=6);
        let source = if cga {
            if ch >= 128 {
                Self::graphics_font_vector(memory, 0x1F)
                    .map(|base| (base, usize::from(ch - 128), 8))
            } else {
                None
            }
        } else {
            Self::graphics_font_vector(memory, 0x43)
                .map(|base| (base, usize::from(ch), self.graphics_font_height(memory)))
        };
        if let Some((base, index, stride)) = source {
            return memory.read_u8(base + index * stride + cy);
        }
        let height = self.default_graphics_char_height();
        if cy >= height {
            return 0;
        }
        if height == 8 {
            CP437_FONT_8X8[usize::from(ch) * 8 + cy]
        } else {
            CP437_FONT_8X16[usize::from(ch) * 16 + cy]
        }
    }

    fn put_pixel_mode13(memory: &mut Memory, x: usize, y: usize, color: u8) {
        if x < 320 && y < 200 {
            let addr = 0xA0000 + y * 320 + x;
            memory.write_u8(addr, color);
        }
    }

    fn put_pixel_modex(
        memory: &mut Memory,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u8,
    ) {
        if x >= width || y >= height {
            return;
        }
        // Mode X planar: 4 pixels per byte address, selected by plane (x & 3).
        let plane = (x & 3) as u8;
        let offset = y * (width / 4) + (x / 4);
        memory.write_video_planar(offset, color, 1 << plane);
    }

    fn scroll_up_graphics(&self, memory: &mut Memory, bg_color: u8) {
        let (_cell_w, cell_h) = self.graphics_char_cell(memory);

        match (self.mode(), self.is_mode_x()) {
            (0x13, false) => {
                // Shift A0000 linear framebuffer up by one text row (cell_h pixels).
                let shift = cell_h * 320;
                let total = 200 * 320;
                for i in 0..(total - shift) {
                    let v = memory.read_u8(0xA0000 + i + shift);
                    memory.write_u8(0xA0000 + i, v);
                }
                for i in (total - shift)..total {
                    memory.write_u8(0xA0000 + i, bg_color);
                }
            }
            (_, true) => {
                let width = self.fb_width;
                let height = self.fb_height;
                let shift_px = cell_h;
                if !width.is_multiple_of(4) || shift_px >= height {
                    return;
                }
                // Copy planar rows upward.
                for y in 0..(height - shift_px) {
                    let src_y = y + shift_px;
                    for x in 0..width {
                        let plane = x & 3;
                        let offset = src_y * (width / 4) + (x / 4);
                        let v = memory.read_video_planar(offset, plane as u8);
                        Self::put_pixel_modex(memory, x, y, width, height, v);
                    }
                }
                // Clear bottom rows.
                for y in (height - shift_px)..height {
                    for x in 0..width {
                        Self::put_pixel_modex(memory, x, y, width, height, bg_color);
                    }
                }
            }
            (0x0D..=0x12, false) => {
                // EGA/VGA 16-color planar modes
                let (width, height) = match self.mode() {
                    0x0D => (320u16, 200u16),
                    0x0E => (640, 200),
                    0x0F | 0x10 => (640, 350),
                    0x11 | 0x12 => (640, 480),
                    _ => unreachable!(),
                };
                let bytes_per_row = (width / 8) as usize;
                let shift = cell_h;
                if shift >= height as usize {
                    return;
                }
                for plane_idx in 0usize..4 {
                    let fill = if (bg_color >> plane_idx) & 1 != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let plane = memory.plane_mut(plane_idx);
                    // Shift all rows up by cell_h
                    for y in 0..(height as usize - shift) {
                        let src = (y + shift) * bytes_per_row;
                        let dst = y * bytes_per_row;
                        for bx in 0..bytes_per_row {
                            plane[dst + bx] = plane[src + bx];
                        }
                    }
                    // Clear the bottom cell_h rows
                    for y in (height as usize - shift)..(height as usize) {
                        let dst = y * bytes_per_row;
                        for bx in 0..bytes_per_row {
                            plane[dst + bx] = fill;
                        }
                    }
                }
            }
            (0x04 | 0x05, false) => {
                // CGA 320x200 4-color
                let shift = cell_h;
                if shift >= 200 {
                    return;
                }
                for y in 0..(200 - shift) {
                    let src_y = y + shift;
                    let dst_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    let src_offset = if (src_y & 1) == 0 {
                        (src_y / 2) * 80
                    } else {
                        0x2000 + (src_y / 2) * 80
                    };
                    for bx in 0..80 {
                        memory.write_u8(
                            0xB8000 + dst_offset + bx,
                            memory.read_u8(0xB8000 + src_offset + bx),
                        );
                    }
                }
                for y in (200 - shift)..200 {
                    let offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    for bx in 0..80 {
                        memory.write_u8(0xB8000 + offset + bx, 0);
                    }
                }
            }
            (0x06, false) => {
                // CGA 640x200 2-color
                let shift = cell_h;
                if shift >= 200 {
                    return;
                }
                for y in 0..(200 - shift) {
                    let src_y = y + shift;
                    let dst_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    let src_offset = if (src_y & 1) == 0 {
                        (src_y / 2) * 80
                    } else {
                        0x2000 + (src_y / 2) * 80
                    };
                    for bx in 0..80 {
                        memory.write_u8(
                            0xB8000 + dst_offset + bx,
                            memory.read_u8(0xB8000 + src_offset + bx),
                        );
                    }
                }
                for y in (200 - shift)..200 {
                    let offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    for bx in 0..80 {
                        memory.write_u8(0xB8000 + offset + bx, 0);
                    }
                }
            }
            _ => {}
        }
    }

    /// Scroll up (or clear) in graphics modes.
    /// `lines` is in text rows; 0 means clear the entire region.
    /// `attr` is the fill color for the vacated/cleared area.
    fn scroll_up_gfx(&self, lines: u8, attr: u8, region: ScrollRegion, memory: &mut Memory) {
        let (cell_w, cell_h) = self.graphics_char_cell(memory);

        match self.mode() {
            // EGA/VGA 16-color planar modes
            0x0D..=0x12 => {
                let (width, height) = match self.mode() {
                    0x0D => (320u16, 200u16),
                    0x0E => (640, 200),
                    0x0F | 0x10 => (640, 350),
                    0x11 | 0x12 => (640, 480),
                    _ => unreachable!(),
                };
                let bytes_per_row = (width / 8) as usize;
                // Convert text-cell region to pixel coordinates
                let px_left = (region.left as usize) * cell_w;
                let px_right = ((region.right as usize) + 1) * cell_w;
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(height as usize);
                if px_top >= px_bottom {
                    return;
                }
                let byte_left = px_left / 8;
                let byte_right = px_right.div_ceil(8);
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };

                for plane_idx in 0usize..4 {
                    let fill = if (attr >> plane_idx) & 1 != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let plane = memory.plane_mut(plane_idx);
                    // Shift rows upward
                    for y in px_top..px_bottom {
                        let src_y = y + scroll_pixels;
                        for bx in byte_left..byte_right.min(bytes_per_row) {
                            if src_y < px_bottom {
                                plane[y * bytes_per_row + bx] = plane[src_y * bytes_per_row + bx];
                            } else {
                                plane[y * bytes_per_row + bx] = fill;
                            }
                        }
                    }
                }
            }
            // CGA 320x200 4-color
            0x04 | 0x05 => {
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(200);
                if px_top >= px_bottom {
                    return;
                }
                let byte_left = (region.left as usize) * (cell_w / 4); // 4 pixels per byte
                let byte_right = ((region.right as usize) + 1) * (cell_w / 4);
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };

                for y in px_top..px_bottom {
                    let src_y = y + scroll_pixels;
                    let y_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    for bx in byte_left..byte_right.min(80) {
                        let addr = 0xB8000 + y_offset + bx;
                        if src_y < px_bottom {
                            let src_offset = if (src_y & 1) == 0 {
                                (src_y / 2) * 80
                            } else {
                                0x2000 + (src_y / 2) * 80
                            };
                            memory.write_u8(addr, memory.read_u8(0xB8000 + src_offset + bx));
                        } else {
                            memory.write_u8(addr, (attr & 0x03) * 0x55);
                        }
                    }
                }
            }
            // CGA 640x200 2-color
            0x06 => {
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(200);
                if px_top >= px_bottom {
                    return;
                }
                let byte_left = region.left as usize; // 8 pixels per byte, 1 char = 1 byte
                let byte_right = (region.right as usize) + 1;
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };

                for y in px_top..px_bottom {
                    let src_y = y + scroll_pixels;
                    let y_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };
                    for bx in byte_left..byte_right.min(80) {
                        let addr = 0xB8000 + y_offset + bx;
                        if src_y < px_bottom {
                            let src_offset = if (src_y & 1) == 0 {
                                (src_y / 2) * 80
                            } else {
                                0x2000 + (src_y / 2) * 80
                            };
                            memory.write_u8(addr, memory.read_u8(0xB8000 + src_offset + bx));
                        } else {
                            memory.write_u8(addr, if attr & 0x01 != 0 { 0xFF } else { 0x00 });
                        }
                    }
                }
            }
            // Mode 13h - linear 320x200x256
            0x13 if !self.is_mode_x() => {
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(200);
                if px_top >= px_bottom {
                    return;
                }
                let px_left = (region.left as usize) * cell_w;
                let px_right = ((region.right as usize) + 1) * cell_w;
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };

                for y in px_top..px_bottom {
                    let src_y = y + scroll_pixels;
                    for x in px_left..px_right.min(320) {
                        let addr = 0xA0000 + y * 320 + x;
                        if src_y < px_bottom {
                            memory.write_u8(addr, memory.read_u8(0xA0000 + src_y * 320 + x));
                        } else {
                            memory.write_u8(addr, attr);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Scroll down (or clear) in graphics modes.
    fn scroll_down_gfx(&self, lines: u8, attr: u8, region: ScrollRegion, memory: &mut Memory) {
        let (cell_w, cell_h) = self.graphics_char_cell(memory);

        match self.mode() {
            0x0D..=0x12 => {
                let (width, height) = match self.mode() {
                    0x0D => (320u16, 200u16),
                    0x0E => (640, 200),
                    0x0F | 0x10 => (640, 350),
                    0x11 | 0x12 => (640, 480),
                    _ => unreachable!(),
                };
                let bytes_per_row = (width / 8) as usize;
                let px_left = (region.left as usize) * cell_w;
                let px_right = ((region.right as usize) + 1) * cell_w;
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(height as usize);
                if px_top >= px_bottom {
                    return;
                }
                let byte_left = px_left / 8;
                let byte_right = px_right.div_ceil(8);
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };

                for plane_idx in 0usize..4 {
                    let fill = if (attr >> plane_idx) & 1 != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let plane = memory.plane_mut(plane_idx);
                    for y in (px_top..px_bottom).rev() {
                        let src_y = if y >= px_top + scroll_pixels {
                            y - scroll_pixels
                        } else {
                            usize::MAX // will trigger fill
                        };
                        for bx in byte_left..byte_right.min(bytes_per_row) {
                            if src_y < px_bottom && src_y >= px_top {
                                plane[y * bytes_per_row + bx] = plane[src_y * bytes_per_row + bx];
                            } else {
                                plane[y * bytes_per_row + bx] = fill;
                            }
                        }
                    }
                }
            }
            // CGA interleaves even and odd scanlines in separate banks.
            0x04..=0x06 => {
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(200);
                if px_top >= px_bottom {
                    return;
                }
                let (bytes_per_cell, fill) = if self.mode() == 0x06 {
                    (cell_w / 8, if attr & 0x01 != 0 { 0xFF } else { 0x00 })
                } else {
                    (cell_w / 4, (attr & 0x03) * 0x55)
                };
                let byte_left = (region.left as usize) * bytes_per_cell;
                let byte_right = ((region.right as usize) + 1) * bytes_per_cell;
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };
                // Copy bottom first so overlapping source rows remain intact.
                for y in (px_top..px_bottom).rev() {
                    let y_offset = (y & 1) * 0x2000 + (y / 2) * 80;
                    for bx in byte_left..byte_right.min(80) {
                        let value = if y >= px_top + scroll_pixels {
                            let src_y = y - scroll_pixels;
                            let src_offset = (src_y & 1) * 0x2000 + (src_y / 2) * 80;
                            memory.read_u8(0xB8000 + src_offset + bx)
                        } else {
                            fill
                        };
                        memory.write_u8(0xB8000 + y_offset + bx, value);
                    }
                }
            }
            // Mode 13h - linear 320x200x256
            0x13 if !self.is_mode_x() => {
                let px_top = (region.top as usize) * cell_h;
                let px_bottom = (((region.bottom as usize) + 1) * cell_h).min(200);
                if px_top >= px_bottom {
                    return;
                }
                let px_left = (region.left as usize) * cell_w;
                let px_right = ((region.right as usize) + 1) * cell_w;
                let scroll_pixels = if lines == 0 {
                    px_bottom - px_top
                } else {
                    (lines as usize) * cell_h
                };
                for y in (px_top..px_bottom).rev() {
                    for x in px_left..px_right.min(320) {
                        let value = if y >= px_top + scroll_pixels {
                            memory.read_u8(0xA0000 + (y - scroll_pixels) * 320 + x)
                        } else {
                            attr
                        };
                        memory.write_u8(0xA0000 + y * 320 + x, value);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn mode(&self) -> u8 {
        // 256-colour: registers fully disambiguate. Mode X vs Mode 13h is
        // a separate concern owned by `is_mode_x()`; both report 0x13 here.
        if self.is_256_color_mode() {
            return 0x13;
        }

        if self.is_text_mode() {
            return self.text_mode_from_registers();
        }

        // Graphics modes driven by the Attribute Controller (EGA/VGA).
        //
        // Disambiguation: the HLD's priority table places VDE checks before
        // HDE, but mode 0x0D and 0x0E both use double-scan (CRTC[0x09] bit
        // 7) with VDE = 398, which collides with mode 0x10 (VDE = 349) at
        // the physical-scanline level. We disambiguate via:
        //   1. HDE: 320-wide modes always have HDE = 39, 640-wide HDE = 79.
        //   2. CRTC[0x09] bit 7 (double-scan): 200-line modes set this; the
        //      350/480-line modes do not.
        //   3. VDE: only meaningful once double-scan has been excluded.
        if (self.attribute_regs[0x10] & 0x01) != 0 {
            // CGA graphics — odd/even chaining on (GC[6] bit 1 = 1) AND
            // CGA mode-control's graphics bit set.
            if (self.graphics_regs[6] & 0x02) != 0 {
                if let Some(cga_mode) = self.cga_mode_control_graphics_mode() {
                    return cga_mode;
                }
            }

            let hde = self.crtc_regs[0x01];
            if hde < 79 {
                return 0x0D;
            }
            // 640-wide. If double-scan is on, this is a 200-line mode (0x0E).
            let double_scan = (self.crtc_regs[0x09] & 0x80) != 0;
            if double_scan {
                return 0x0E;
            }
            // Mono vs colour planar: misc_output bit 0 selects the I/O
            // address range. Bit 0 = 0 → mono (3B4h), bit 0 = 1 → colour
            // (3D4h). This cleanly disambiguates 0x0F (misc=0xA2) vs 0x10
            // (misc=0xA3). For 640x480, both 0x11 and 0x12 use misc=0xE3
            // (bit 0 = 1), so we default to 0x12 — the safe choice. We
            // deliberately do NOT use SR[2] (Map Mask) here: apps in mode
            // 0x12 routinely mask SR[2] to write individual planes, which
            // would cause transient misclassification as 0x11.
            let mono = (self.misc_output & 0x01) == 0;
            let vde = self.get_vertical_display_end();
            if vde >= 479 {
                // 640x480 planar. Mono (0x11) vs colour (0x12).
                if mono {
                    return 0x11;
                }
                return 0x12;
            }
            if vde >= 349 {
                // 640x350 planar. Mono (0x0F) vs colour (0x10).
                if mono {
                    return 0x0F;
                }
                return 0x10;
            }
            // 640-wide, no double-scan, fewer than 350 lines — fall through
            // to 0x0E as the closest match (no canonical mode for this state).
            return 0x0E;
        }

        self.text_mode_from_registers()
    }

    fn text_mode_from_registers(&self) -> u8 {
        // CRTC[0x01] is "horizontal display end - 1", so 80-col text yields
        // HDE = 79 and 40-col text yields HDE = 39.
        let hde = self.crtc_regs[0x01];
        if hde < 79 {
            return if (self.cga_mode_control_reg & 0x04) != 0 {
                0x00
            } else {
                0x01
            };
        }
        if (self.misc_output & 0x01) == 0 {
            return 0x07;
        }
        if (self.cga_mode_control_reg & 0x04) != 0 {
            return 0x02;
        }
        0x03
    }

    /// Get current video mode info.
    ///
    /// The mode field is register-derived via [`Self::mode`] so callers see
    /// what the hardware actually says, regardless of whether HLE INT 10h or
    /// direct register programming set the mode. `columns` and `active_page`
    /// remain cached.
    pub fn get_mode(&self) -> (u8, u8, u8) {
        (self.mode(), self.columns, self.active_page)
    }

    /// Sync text layout and cursor metadata without changing framebuffer geometry.
    pub fn sync_text_layout(&mut self, memory: &Memory) {
        let cols = memory.read_u8(0x44A);
        let rows_minus_1 = memory.read_u8(0x484);
        let rows = rows_minus_1.saturating_add(1);

        if cols != 0 {
            self.columns = cols;
        }
        self.rows = rows;

        self.active_page = memory.read_u8(0x462);
        for page in 0..8 {
            let base = 0x450 + page * 2;
            let col = memory.read_u8(base);
            let row = memory.read_u8(base + 1);
            self.cursor_pos[page] = (row, col);
        }
        let cursor_shape = memory.read_u16(0x460);
        self.set_cursor_shape((cursor_shape >> 8) as u8, (cursor_shape & 0xFF) as u8);
    }

    /// Sync high-level VGA state from the BIOS Data Area (used after state restore).
    pub fn sync_bda_state(&mut self, memory: &Memory) {
        self.sync_text_layout(memory);
        let mode = memory.read_u8(0x449) & 0x7F;

        if mode <= 0x07 {
            let bda_cga_mode_control = memory.read_u8(0x465);
            self.cga_mode_control_reg = if bda_cga_mode_control == 0 {
                match mode {
                    0x00 | 0x01 | 0x02 | 0x03 | 0x07 => {
                        Self::default_cga_mode_control_for_bios_mode(mode).unwrap_or(0)
                    }
                    0x04..=0x06 => {
                        Self::default_cga_mode_control_for_bios_mode(mode).unwrap_or(0) & !0x08
                    }
                    _ => 0,
                }
            } else {
                bda_cga_mode_control
            };
            self.color_select_reg = memory.read_u8(0x466);
        }

        let (mut fb_width, mut fb_height) = match mode {
            0x00 | 0x01 => (320, 200),
            0x02 | 0x03 | 0x07 => (640, 400),
            0x04 | 0x05 => (320, 200),
            0x06 => (640, 200),
            0x08 => (160, 200),
            0x09 => (320, 200),
            0x0A => (640, 200),
            0x0D => (320, 200),
            0x0E => (640, 200),
            0x10 => (640, 350),
            0x12 => (640, 480),
            0x13 => (320, 200),
            _ => (self.fb_width, self.fb_height),
        };

        if mode == 0x13 && self.is_mode_x() {
            // Mode X: framebuffer geometry is register-derived.
            fb_height = self.mode_x_visible_height().min(480);
            fb_width = self.mode_x_width();
        }

        if self.fb_width != fb_width || self.fb_height != fb_height {
            self.fb_width = fb_width;
            self.fb_height = fb_height;
            // Reallocating the construction base invalidates the completed
            // frame even at the same length (MDD-BUG-CRUCIBLE-00347).
            self.framebuffer = vec![0u32; self.fb_width * self.fb_height];
        }
    }

    /// Set cursor position
    pub fn set_cursor(&mut self, page: u8, row: u8, col: u8) {
        let page = self.normalize_cursor_page(page);
        self.cursor_pos[page] = (row, col);
        if page == self.active_page as usize {
            self.update_hw_cursor(page as u8);
        }
    }

    /// Set cursor shape (CRTC 0x0A/0x0B)
    pub fn set_cursor_shape(&mut self, start: u8, end: u8) {
        self.crtc_regs[0x0A] = start;
        self.crtc_regs[0x0B] = end;
    }

    /// Get cursor position
    pub fn get_cursor(&self, page: u8) -> (u8, u8) {
        self.cursor_pos[self.normalize_cursor_page(page)]
    }

    /// The cursor position CONTAINED to the current text geometry, for use by
    /// output routines that form a memory address or do cursor arithmetic.
    ///
    /// INT 10h AH=02h stores whatever `DH`/`DL` the guest supplies, exactly as
    /// a real BIOS does — AH=03h must read the same bytes back, and DOS
    /// programs do stash values there. So the raw position stays unvalidated
    /// in `cursor_pos` and containment happens HERE, at the point of use
    /// (MDD-BUG-CRUCIBLE-00599).
    ///
    /// Without it, `row = 0xFF` made teletype's line feed overflow `row += 1`,
    /// `col = 0xFF` made tab overflow `col + 8` and an ordinary character
    /// overflow `col += 1` — panics in an overflow-checked build — and in
    /// release the character path first formed the cell address
    /// `base + (row * columns + col) * 2` from the off-screen pair and wrote
    /// beyond the text aperture. Two ordinary BIOS calls reached that from
    /// the guest.
    ///
    /// Clamping (rather than ignoring or wrapping) is the deliberate choice:
    /// it is deterministic, keeps every write inside the active page, and
    /// leaves the cursor somewhere a subsequent write can legitimately land.
    /// A real BIOS would happily scribble outside the page here; that is the
    /// one behaviour we decline to reproduce, because it is memory-unsafe for
    /// us and no correct program depends on it.
    fn cursor_for_output(&self, page: u8) -> (u8, u8) {
        let (row, col) = self.get_cursor(page);
        (
            row.min(self.rows.saturating_sub(1)),
            col.min(self.columns.saturating_sub(1)),
        )
    }

    fn update_hw_cursor(&mut self, page: u8) {
        if !self.is_text_mode() {
            return;
        }

        let (row, col) = self.get_cursor(page);
        let cursor_pos = (page as usize) * self.text_page_size_words()
            + (row as usize) * (self.columns as usize)
            + (col as usize);
        self.crtc_regs[0x0E] = ((cursor_pos >> 8) & 0xFF) as u8;
        self.crtc_regs[0x0F] = (cursor_pos & 0xFF) as u8;
    }

    pub fn set_hw_cursor_pos(&mut self, x: u16, y: u16) {
        self.hw_cursor_x = x;
        self.hw_cursor_y = y;
    }

    pub fn get_hw_cursor_pos(&self) -> (u16, u16) {
        (self.hw_cursor_x, self.hw_cursor_y)
    }

    pub fn set_hw_cursor_enabled(&mut self, enabled: bool) {
        self.hw_cursor_enabled = enabled;
    }

    /// Check if hardware cursor is enabled
    pub fn is_hw_cursor_enabled(&self) -> bool {
        self.hw_cursor_enabled
    }

    /// Set active display page
    pub fn set_active_page(&mut self, page: u8) {
        self.active_page = page;
        // Mirror BIOS page selection into the CRTC start address so render paths that honor
        // the start address show the correct page.
        if self.is_text_mode() {
            let words_per_page = self.text_page_size_words();
            let start_words = (page as usize) * words_per_page;
            self.crtc_regs[0x0C] = ((start_words >> 8) & 0xFF) as u8;
            self.crtc_regs[0x0D] = (start_words & 0xFF) as u8;
        }
        self.update_hw_cursor(page);
    }

    pub fn set_crtc_start_words(&mut self, start_words: usize) {
        self.crtc_regs[0x0C] = ((start_words >> 8) & 0xFF) as u8;
        self.crtc_regs[0x0D] = (start_words & 0xFF) as u8;
    }

    /// Set palette register (stored in attribute_regs[0..16])
    pub fn set_palette_reg(&mut self, reg: u8, val: u8) {
        if (reg as usize) < 16 {
            self.attribute_regs[reg as usize] = val;
            self.palette16_lut_dirty = true;
        }
    }

    /// Set DAC register (RGB values are 6-bit, 0-63)
    pub fn set_dac_reg(&mut self, reg: u16, r: u8, g: u8, b: u8) {
        if (reg as usize) < self.dac.len() {
            // Convert 6-bit to 8-bit
            self.dac[reg as usize] = (
                (r & 0x3F) << 2 | (r & 0x3F) >> 4,
                (g & 0x3F) << 2 | (g & 0x3F) >> 4,
                (b & 0x3F) << 2 | (b & 0x3F) >> 4,
            );
            self.palette_lut_dirty = true;
            self.palette16_lut_dirty = true;
        }
    }

    /// Set DAC register with raw 8-bit values (no conversion)
    /// Used for state restore where values are already 8-bit
    pub fn set_dac_raw(&mut self, reg: u16, r: u8, g: u8, b: u8) {
        if (reg as usize) < self.dac.len() {
            self.dac[reg as usize] = (r, g, b);
            self.palette_lut_dirty = true;
            self.palette16_lut_dirty = true;
        }
    }

    /// Scroll up
    pub fn scroll_up(&mut self, lines: u8, attr: u8, region: ScrollRegion, memory: &mut Memory) {
        if region.bottom < region.top || region.right < region.left {
            return;
        }

        if !self.is_text_mode() {
            self.scroll_up_gfx(lines, attr, region, memory);
            return;
        }

        let base = self.text_memory_base();
        self.scroll_up_text(lines, attr, region, memory, base);
    }

    /// Scroll a specific text page up without changing the active display page.
    ///
    /// Graphics modes have no independent text-page regen windows, so they retain
    /// the same behavior as [`Self::scroll_up`].
    pub fn scroll_up_page(
        &mut self,
        page: u8,
        lines: u8,
        attr: u8,
        region: ScrollRegion,
        memory: &mut Memory,
    ) {
        if region.bottom < region.top || region.right < region.left {
            return;
        }

        if !self.is_text_mode() {
            self.scroll_up_gfx(lines, attr, region, memory);
            return;
        }

        let base = self.text_page_base(page);
        self.scroll_up_text(lines, attr, region, memory, base);
    }

    fn scroll_up_text(
        &mut self,
        lines: u8,
        attr: u8,
        region: ScrollRegion,
        memory: &mut Memory,
        base: usize,
    ) {
        let height = region.bottom as u16 - region.top as u16 + 1;
        let lines = if lines == 0 { height as u8 } else { lines };

        for row in region.top..=region.bottom {
            for col in region.left..=region.right {
                let src_row = row as u16 + lines as u16;
                if src_row <= region.bottom as u16 {
                    let src_addr =
                        base + ((src_row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    let dst_addr =
                        base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    let ch = memory.read_u8(src_addr);
                    let at = memory.read_u8(src_addr + 1);
                    memory.write_u8(dst_addr, ch);
                    memory.write_u8(dst_addr + 1, at);
                } else {
                    let dst_addr =
                        base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    memory.write_u8(dst_addr, b' ');
                    memory.write_u8(dst_addr + 1, attr);
                }
            }
        }
    }

    /// Scroll down
    pub fn scroll_down(&mut self, lines: u8, attr: u8, region: ScrollRegion, memory: &mut Memory) {
        if region.bottom < region.top || region.right < region.left {
            return;
        }

        if !self.is_text_mode() {
            self.scroll_down_gfx(lines, attr, region, memory);
            return;
        }

        let base = self.text_memory_base();
        let height = region.bottom as u16 - region.top as u16 + 1;
        let lines = if lines == 0 { height as u8 } else { lines };

        for row in (region.top..=region.bottom).rev() {
            for col in region.left..=region.right {
                if row as u16 >= lines as u16 + region.top as u16 {
                    let src_row = row - lines;
                    let src_addr =
                        base + ((src_row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    let dst_addr =
                        base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    let ch = memory.read_u8(src_addr);
                    let at = memory.read_u8(src_addr + 1);
                    memory.write_u8(dst_addr, ch);
                    memory.write_u8(dst_addr + 1, at);
                } else {
                    let dst_addr =
                        base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    memory.write_u8(dst_addr, b' ');
                    memory.write_u8(dst_addr + 1, attr);
                }
            }
        }
    }

    /// Read character and attribute at cursor
    pub fn read_char_attr(&self, page: u8, memory: &Memory) -> (u8, u8) {
        if !self.is_text_mode() {
            return (0, 0);
        }

        let (row, col) = self.cursor_for_output(page);
        let base = self.text_page_base(page);
        let addr = base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
        (memory.read_u8(addr), memory.read_u8(addr + 1))
    }

    /// Write character and attribute at cursor
    pub fn write_char_attr(&mut self, page: u8, ch: u8, attr: u8, count: u16, memory: &mut Memory) {
        // Handle CGA graphics modes (04h, 05h) - draw character glyph as pixels
        if matches!(self.mode(), 0x04 | 0x05) {
            let (row, col) = self.cursor_for_output(page);
            let fg_color = attr & 0x03; // Foreground color (0-3)
            let xor_mode = (attr & 0x80) != 0;

            for i in 0..count {
                let char_col = (col as u16 + i) as usize;
                if char_col >= 40 {
                    break;
                }

                let x0 = char_col * 8;
                let y0 = (row as usize) * 8;

                for cy in 0..8 {
                    let font_row = self.graphics_font_row(memory, ch, cy);
                    let y = y0 + cy;
                    if y >= 200 {
                        break;
                    }

                    let line_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };

                    for cx in 0..8 {
                        let x = x0 + cx;
                        let pixel_on = (font_row >> (7 - cx)) & 1 != 0;

                        // Per IBM PC Technical Reference: when bit 7 is clear,
                        // the character *replaces* what was on screen (fg + bg).
                        // When bit 7 is set, the character is XOR'd onto screen.
                        let color = if pixel_on { fg_color } else { 0 };

                        let byte_offset = x / 4;
                        let pixel_in_byte = 3 - (x % 4);
                        let shift = pixel_in_byte * 2;
                        let mask = 0x03u8 << shift;

                        let addr = 0xB8000 + line_offset + byte_offset;
                        let old_byte = memory.read_u8(addr);

                        let new_byte = if xor_mode {
                            old_byte ^ ((color << shift) & mask)
                        } else {
                            (old_byte & !mask) | ((color << shift) & mask)
                        };

                        memory.write_u8(addr, new_byte);
                    }
                }
            }
            return;
        }

        // CGA 640x200 2-color mode (06h) — 1 bit per pixel
        if self.mode() == 0x06 {
            let (row, col) = self.cursor_for_output(page);
            let fg_bit: u8 = if (attr & 0x01) != 0 { 1 } else { 0 };
            let xor_mode = (attr & 0x80) != 0;

            for i in 0..count {
                let char_col = (col as u16 + i) as usize;
                if char_col >= 80 {
                    break;
                }

                let x0 = char_col * 8;
                let y0 = (row as usize) * 8;

                for cy in 0..8 {
                    let font_row = self.graphics_font_row(memory, ch, cy);
                    let y = y0 + cy;
                    if y >= 200 {
                        break;
                    }

                    let line_offset = if (y & 1) == 0 {
                        (y / 2) * 80
                    } else {
                        0x2000 + (y / 2) * 80
                    };

                    // Each byte stores 8 pixels (1 bit each, MSB = leftmost)
                    let byte_offset = x0 / 8;
                    let addr = 0xB8000 + line_offset + byte_offset;
                    let old_byte = memory.read_u8(addr);
                    let mut new_byte = old_byte;

                    for cx in 0..8 {
                        let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                        let bit_val = if pixel_on { fg_bit } else { 0 };
                        let bit_pos = 7 - cx;
                        if xor_mode {
                            if bit_val != 0 {
                                new_byte ^= 1 << bit_pos;
                            }
                        } else {
                            new_byte = (new_byte & !(1 << bit_pos)) | (bit_val << bit_pos);
                        }
                    }
                    memory.write_u8(addr, new_byte);
                }
            }
            return;
        }

        // EGA/VGA planar modes (16-color and monochrome)
        if matches!(self.mode(), 0x0D..=0x12) {
            let (row, col) = self.cursor_for_output(page);
            let (cell_w, cell_h) = self.graphics_char_cell(memory);
            let (width, height) = match self.mode() {
                0x0D => (320u16, 200u16),
                0x0E => (640, 200),
                0x0F | 0x10 => (640, 350),
                0x11 | 0x12 => (640, 480),
                _ => unreachable!(),
            };
            let fg = attr & 0x0F;
            let bg = (attr >> 4) & 0x0F; // Upper nibble is background in EGA
            let xor_mode = false; // EGA AH=09h doesn't use XOR via bit 7 in planar modes

            for i in 0..count {
                let char_col = (col as u16 + i) as usize;
                if char_col >= self.columns as usize {
                    break;
                }

                let x0 = char_col * cell_w;
                let y0 = (row as usize) * cell_h;

                for cy in 0..cell_h {
                    let font_row = self.graphics_font_row(memory, ch, cy);
                    for cx in 0..cell_w {
                        let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                        let color = if pixel_on { fg } else { bg };
                        if !xor_mode {
                            Self::write_pixel_planar(
                                memory,
                                (x0 + cx) as u16,
                                (y0 + cy) as u16,
                                color,
                                width,
                                height,
                            );
                        }
                    }
                }
            }
            return;
        }

        // Mode 13h linear
        if self.mode() == 0x13 && !self.is_mode_x() {
            let (row, col) = self.cursor_for_output(page);
            let (cell_w, cell_h) = self.graphics_char_cell(memory);
            let fg = attr;
            let bg = 0u8;

            for i in 0..count {
                let char_col = (col as u16 + i) as usize;
                if char_col >= self.columns as usize {
                    break;
                }

                let x0 = char_col * cell_w;
                let y0 = (row as usize) * cell_h;

                for cy in 0..cell_h {
                    let font_row = self.graphics_font_row(memory, ch, cy);
                    for cx in 0..cell_w {
                        let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                        let color = if pixel_on { fg } else { bg };
                        Self::put_pixel_mode13(memory, x0 + cx, y0 + cy, color);
                    }
                }
            }
            return;
        }

        if !self.is_text_mode() {
            return;
        }

        let (row, col) = self.cursor_for_output(page);
        let base = self.text_page_base(page);

        for i in 0..count {
            let c = (col as u16 + i) as usize;
            if c >= self.columns as usize {
                break;
            }
            let addr = base + ((row as usize) * (self.columns as usize) + c) * 2;
            memory.write_u8(addr, ch);
            memory.write_u8(addr + 1, attr);
        }
    }

    /// Write a single character with attribute for INT 10h AH=13h (Write String).
    ///
    /// Unlike `write_char_attr` (AH=09h), this draws **both** foreground and
    /// background pixels in CGA graphics modes, matching the real VGA BIOS
    /// AH=13h behavior. For non-CGA modes, delegates to `write_char_attr`.
    pub fn write_char_for_string(&mut self, page: u8, ch: u8, attr: u8, memory: &mut Memory) {
        // Only CGA modes need special handling — EGA/VGA write_char_attr
        // already draws both fg and bg via the attribute's upper nibble.
        if !matches!(self.mode(), 0x04 | 0x05) {
            self.write_char_attr(page, ch, attr, 1, memory);
            return;
        }

        let (row, col) = self.cursor_for_output(page);
        if col >= 40 {
            return;
        }
        let fg_color = attr & 0x03;
        let x0 = (col as usize) * 8;
        let y0 = (row as usize) * 8;

        for cy in 0..8 {
            let font_row = self.graphics_font_row(memory, ch, cy);
            let y = y0 + cy;
            if y >= 200 {
                break;
            }
            let line_offset = if (y & 1) == 0 {
                (y / 2) * 80
            } else {
                0x2000 + (y / 2) * 80
            };
            for cx in 0..8 {
                let x = x0 + cx;
                let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                let color = if pixel_on { fg_color } else { 0x00 };
                let byte_offset = x / 4;
                let pixel_in_byte = 3 - (x % 4);
                let shift = pixel_in_byte * 2;
                let mask = 0x03u8 << shift;
                let addr = 0xB8000 + line_offset + byte_offset;
                let old_byte = memory.read_u8(addr);
                let new_byte = (old_byte & !mask) | ((color << shift) & mask);
                memory.write_u8(addr, new_byte);
            }
        }
    }

    /// Write character only at cursor
    pub fn write_char(&mut self, page: u8, ch: u8, count: u16, memory: &mut Memory) {
        // Handle graphics modes - use default foreground color
        if matches!(self.mode(), 0x04 | 0x05) {
            self.write_char_attr(page, ch, 0x03, count, memory);
            return;
        }
        if self.mode() == 0x06 {
            self.write_char_attr(page, ch, 0x01, count, memory);
            return;
        }
        if matches!(self.mode(), 0x0D..=0x12) {
            // EGA/VGA planar: white (0x0F) foreground, black background
            self.write_char_attr(page, ch, 0x0F, count, memory);
            return;
        }
        if self.mode() == 0x13 {
            self.write_char_attr(page, ch, 0x0F, count, memory);
            return;
        }

        if !self.is_text_mode() {
            return;
        }

        let (row, col) = self.cursor_for_output(page);
        let base = self.text_page_base(page);

        for i in 0..count {
            let c = (col as u16 + i) as usize;
            if c >= self.columns as usize {
                break;
            }
            let addr = base + ((row as usize) * (self.columns as usize) + c) * 2;
            memory.write_u8(addr, ch);
        }
    }

    /// Teletype output
    pub fn teletype(&mut self, ch: u8, page: u8, memory: &mut Memory) {
        let color = match self.mode() {
            0x04 | 0x05 => 3,
            0x06 => 1,
            _ => 15,
        };
        self.teletype_with_color(ch, page, color, memory);
    }

    /// Teletype output with a graphics foreground color. Text attributes are preserved.
    pub fn teletype_with_color(&mut self, ch: u8, page: u8, color: u8, memory: &mut Memory) {
        let (mut row, mut col) = self.cursor_for_output(page);

        match ch {
            0x07 => {} // Bell
            0x08 => {
                // Backspace
                col = col.saturating_sub(1);
            }
            0x09 => {
                // Tab
                col = (col + 8) & !7;
            }
            0x0A => {
                // Line feed
                row += 1;
            }
            0x0D => {
                // Carriage return
                col = 0;
            }
            _ => {
                // Normal character
                if self.is_text_mode() {
                    let base = self.text_page_base(page);
                    let addr =
                        base + ((row as usize) * (self.columns as usize) + (col as usize)) * 2;
                    // Character byte
                    memory.write_u8(addr, ch);
                    // Attribute byte: if unset, default to light gray on black (07h).
                    // Many DOS apps rely on BIOS/console default attribute.
                    if memory.read_u8(addr + 1) == 0 {
                        memory.write_u8(addr + 1, DEFAULT_TEXT_ATTRIBUTE);
                    }
                } else {
                    // Graphics-mode teletype: draw glyph into video memory.
                    // Many DOS programs print prompts while in graphics mode (e.g. mode 13h),
                    // expecting BIOS teletype output to still be visible.
                    let (cell_w, cell_h) = self.graphics_char_cell(memory);
                    let x0 = (col as usize).saturating_mul(cell_w);
                    let y0 = (row as usize).saturating_mul(cell_h);

                    let fg = color;
                    let bg = 0x00;

                    match (self.mode(), self.is_mode_x()) {
                        (0x13, false) => {
                            for cy in 0..cell_h {
                                let font_row = self.graphics_font_row(memory, ch, cy);
                                for cx in 0..cell_w {
                                    let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                                    let color = if pixel_on { fg } else { bg };
                                    Self::put_pixel_mode13(memory, x0 + cx, y0 + cy, color);
                                }
                            }
                        }
                        (_, true) => {
                            let width = self.fb_width;
                            let height = self.fb_height;
                            for cy in 0..cell_h {
                                let font_row = self.graphics_font_row(memory, ch, cy);
                                for cx in 0..cell_w {
                                    let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                                    let color = if pixel_on { fg } else { bg };
                                    Self::put_pixel_modex(
                                        memory,
                                        x0 + cx,
                                        y0 + cy,
                                        width,
                                        height,
                                        color,
                                    );
                                }
                            }
                        }
                        (0x0D..=0x12, false) => {
                            // EGA/VGA planar modes (16-color and monochrome)
                            let (width, height) = match self.mode() {
                                0x0D => (320, 200),
                                0x0E => (640, 200),
                                0x0F | 0x10 => (640, 350),
                                0x11 | 0x12 => (640, 480),
                                _ => (320, 200),
                            };
                            for cy in 0..cell_h {
                                let font_row = self.graphics_font_row(memory, ch, cy);
                                for cx in 0..cell_w {
                                    let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                                    let color = if pixel_on { fg } else { bg };
                                    Self::write_pixel_planar(
                                        memory,
                                        (x0 + cx) as u16,
                                        (y0 + cy) as u16,
                                        color,
                                        width,
                                        height,
                                    );
                                }
                            }
                        }
                        (0x06, false) => {
                            // CGA 640x200 2-color mode — 1 bit per pixel
                            for cy in 0..cell_h {
                                let font_row = self.graphics_font_row(memory, ch, cy);
                                let y = y0 + cy;
                                if y >= 200 {
                                    break;
                                }
                                let line_offset = if (y & 1) == 0 {
                                    (y / 2) * 80
                                } else {
                                    0x2000 + (y / 2) * 80
                                };
                                let byte_offset = x0 / 8;
                                let addr = 0xB8000 + line_offset + byte_offset;
                                // The full cell has a one-bit foreground and zero background.
                                memory.write_u8(addr, if fg & 1 != 0 { font_row } else { 0 });
                            }
                        }
                        (0x04 | 0x05, false) => {
                            // CGA 320x200 4-color modes
                            // Teletype draws the full character cell (fg + bg),
                            // unlike AH=09h which only draws foreground pixels.
                            let cga_fg = fg & 0x03;
                            for cy in 0..cell_h {
                                let font_row = self.graphics_font_row(memory, ch, cy);
                                let y = y0 + cy;
                                if y >= 200 {
                                    break;
                                }
                                // CGA interleaved: even lines at B8000, odd at B8000+2000
                                let line_offset = if (y & 1) == 0 {
                                    (y / 2) * 80
                                } else {
                                    0x2000 + (y / 2) * 80
                                };
                                for cx in 0..cell_w {
                                    let x = x0 + cx;
                                    let pixel_on = (font_row >> (7 - cx)) & 1 != 0;
                                    let color = if pixel_on { cga_fg } else { 0x00 };
                                    let byte_offset = x / 4;
                                    let pixel_in_byte = 3 - (x % 4);
                                    let shift = pixel_in_byte * 2;
                                    let mask = 0x03_u8 << shift;
                                    let addr = 0xB8000 + line_offset + byte_offset;
                                    let old_byte = memory.read_u8(addr);
                                    let new_byte = (old_byte & !mask) | ((color << shift) & mask);
                                    memory.write_u8(addr, new_byte);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                col += 1;
            }
        }

        // Handle line wrap
        if col >= self.columns {
            col = 0;
            row += 1;
        }

        // Handle scroll
        if row >= self.rows {
            if self.is_text_mode() {
                self.scroll_up(
                    1,
                    DEFAULT_TEXT_ATTRIBUTE,
                    ScrollRegion {
                        top: 0,
                        left: 0,
                        bottom: self.rows - 1,
                        right: self.columns - 1,
                    },
                    memory,
                );
            } else {
                self.scroll_up_graphics(memory, 0x00);
            }
            row = self.rows - 1;
        }

        self.set_cursor(page, row, col);
    }

    /// Byte offset within each VGA plane for a BIOS pixel page.
    /// Invalid pages fall back to zero, as the cursor page helpers do.
    /// Single-page CGA and packed modes ignore BH.
    fn pixel_page_offset(&self, page: u8) -> usize {
        let allocation = match self.mode() {
            0x0D => 0x2000,
            0x0E => 0x4000,
            0x0F | 0x10 => 0x8000,
            _ => return 0,
        };
        let page = usize::from(page);
        // Each VGA plane has 64 KiB; BIOS allocations include frame padding.
        if page < VGA_PLANE_SIZE / allocation {
            page * allocation
        } else {
            0
        }
    }

    /// Write pixel (graphics modes)
    pub fn write_pixel(&mut self, page: u8, x: u16, y: u16, color: u8, memory: &mut Memory) {
        let color = if self.mode() != 0x13 && color & 0x80 != 0 {
            self.read_pixel(page, x, y, memory) ^ (color & 0x7F)
        } else {
            color
        };
        let page_offset = self.pixel_page_offset(page);
        match self.mode() {
            0x13
                // Mode 13h - linear 320x200x256
                if x < 320 && y < 200 => {
                    let addr = 0xA0000 + (y as usize) * 320 + (x as usize);
                    memory.write_u8(addr, color);
                }
            0x04 | 0x05
                // CGA 320x200 4-color
                if x < 320 && y < 200 => {
                    let bank_offset = if (y & 1) == 0 { 0 } else { 0x2000 };
                    let row_offset = (y as usize / 2) * 80;
                    let byte_offset = x as usize / 4;
                    let pixel_in_byte = 3 - (x as usize % 4); // 0=rightmost, 3=leftmost
                    let shift = pixel_in_byte * 2;
                    let addr = 0xB8000 + bank_offset + row_offset + byte_offset;
                    let mut byte = memory.read_u8(addr);
                    byte &= !(0x03 << shift); // Clear the 2 bits
                    byte |= (color & 0x03) << shift; // Set new color
                    memory.write_u8(addr, byte);
                }
            0x06
                // CGA 640x200 2-color
                if x < 640 && y < 200 => {
                    let bank_offset = if (y & 1) == 0 { 0 } else { 0x2000 };
                    let row_offset = (y as usize / 2) * 80;
                    let byte_offset = x as usize / 8;
                    let bit_in_byte = 7 - (x as usize % 8);
                    let addr = 0xB8000 + bank_offset + row_offset + byte_offset;
                    let mut byte = memory.read_u8(addr);
                    if color & 1 != 0 {
                        byte |= 1 << bit_in_byte;
                    } else {
                        byte &= !(1 << bit_in_byte);
                    }
                    memory.write_u8(addr, byte);
                }
            0x0D => {
                // EGA 320x200 16-color planar
                Self::write_pixel_planar_at(memory, page_offset, x, y, color, 320, 200);
            }
            0x0E => {
                // EGA 640x200 16-color planar
                Self::write_pixel_planar_at(memory, page_offset, x, y, color, 640, 200);
            }
            0x0F | 0x10 => {
                // EGA 640x350 mono/16-color planar
                Self::write_pixel_planar_at(memory, page_offset, x, y, color, 640, 350);
            }
            0x11 | 0x12 => {
                // VGA 640x480 mono/16-color planar
                Self::write_pixel_planar_at(memory, page_offset, x, y, color, 640, 480);
            }
            _ => {}
        }
    }

    /// Write pixel for planar 16-color modes
    fn write_pixel_planar(memory: &mut Memory, x: u16, y: u16, color: u8, width: u16, height: u16) {
        Self::write_pixel_planar_at(memory, 0, x, y, color, width, height);
    }

    fn write_pixel_planar_at(
        memory: &mut Memory,
        page_offset: usize,
        x: u16,
        y: u16,
        color: u8,
        width: u16,
        height: u16,
    ) {
        if x >= width || y >= height {
            return;
        }
        let bytes_per_row = (width / 8) as usize;
        let byte_offset = page_offset + (y as usize) * bytes_per_row + (x as usize / 8);
        let bit_position = 7 - (x as usize % 8);
        let mask = 1u8 << bit_position;

        // Write to each of the 4 planes based on color bits
        for plane_idx in 0..4 {
            let plane_bit = (color >> plane_idx) & 1;
            let plane = memory.plane_mut(plane_idx);
            if plane_bit != 0 {
                plane[byte_offset] |= mask;
            } else {
                plane[byte_offset] &= !mask;
            }
        }
    }

    /// Read pixel (graphics modes)
    pub fn read_pixel(&self, page: u8, x: u16, y: u16, memory: &Memory) -> u8 {
        let page_offset = self.pixel_page_offset(page);
        match self.mode() {
            // CGA 320x200 4-color modes
            0x04 | 0x05 => {
                if x >= 320 || y >= 200 {
                    return 0;
                }
                // CGA uses interlaced banks: even lines at 0xB8000, odd at 0xBA000
                let bank_offset: usize = if (y & 1) == 0 { 0 } else { 0x2000 };
                let row_offset = (y as usize / 2) * 80;
                let byte_offset = x as usize / 4;
                let pixel_in_byte = 3 - (x as usize % 4);
                let shift = pixel_in_byte * 2;
                let addr = 0xB8000 + bank_offset + row_offset + byte_offset;
                (memory.read_u8(addr) >> shift) & 0x03
            }
            // CGA 640x200 2-color mode
            0x06 => {
                if x >= 640 || y >= 200 {
                    return 0;
                }
                let bank_offset: usize = if (y & 1) == 0 { 0 } else { 0x2000 };
                let row_offset = (y as usize / 2) * 80;
                let byte_offset = x as usize / 8;
                let bit_in_byte = 7 - (x as usize % 8);
                let addr = 0xB8000 + bank_offset + row_offset + byte_offset;
                (memory.read_u8(addr) >> bit_in_byte) & 0x01
            }
            // EGA/VGA 16-color planar modes
            0x0D..=0x12 => {
                let (width, height): (u16, u16) = match self.mode() {
                    0x0D => (320, 200),
                    0x0E => (640, 200),
                    0x0F | 0x10 => (640, 350),
                    _ => (640, 480), // 0x11, 0x12
                };
                if x >= width || y >= height {
                    return 0;
                }
                let bytes_per_row = (width / 8) as usize;
                let byte_offset = page_offset + (y as usize) * bytes_per_row + (x as usize / 8);
                let bit_position = 7 - (x as usize % 8);
                let mask = 1u8 << bit_position;
                let mut color = 0u8;
                for plane in 0..4 {
                    if memory.plane(plane)[byte_offset] & mask != 0 {
                        color |= 1 << plane;
                    }
                }
                color
            }
            // Mode 13h (256 color)
            0x13 if x < 320 && y < 200 => {
                let addr = 0xA0000 + (y as usize) * 320 + (x as usize);
                memory.read_u8(addr)
            }
            _ => 0,
        }
    }

    /// Render video memory to framebuffer (u32 0RGB format)
    pub fn render(&mut self, memory: &mut Memory) {
        // Increment blink frame counter for cursor/attribute blink timing
        self.blink_frame_count = self.blink_frame_count.wrapping_add(1);
        if self.video_output_blanked() {
            self.framebuffer.fill(0);
            return;
        }

        // Propagate the Memory-side plane-2 font-upload latch into Vga.
        // Memory evaluates the predicate at planar-write time against its
        // cached VgaPipelineState; we mirror the result here because
        // `gp64-memory` cannot reach into the full `Vga` struct.
        // One-frame latency between guest write and flag pickup is acceptable
        // — font uploads happen during BIOS init or vertical blanking, not
        // mid-render.
        if memory.planar_font_latch_pending() {
            self.custom_font_loaded = true;
        }

        // Rebuild 16-color palette LUT if dirty
        self.rebuild_palette16_lut_if_dirty();

        // Debugging aid: override the rendering interpretation without changing guest state.
        // Selects the framebuffer decoding mode independently of the current
        // VGA register state for diagnosing memory layout and mode derivation.
        let forced_mode = force_render_mode_override().and_then(|s| {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u8::from_str_radix(hex, 16).ok()
            } else {
                s.parse::<u8>().ok()
            }
        });
        let render_mode = forced_mode.unwrap_or_else(|| self.mode());

        // Apply display-enable gates before rendering. CGA bit 3 is authoritative
        // for CGA graphics modes, but it must not blank VGA text modes.
        // Use overscan (border) color for blanking instead of pure black.
        if self.display_gate_blanks_render_mode(render_mode) {
            let overscan = self.overscan_rgb();
            self.framebuffer.fill(overscan);
            return;
        }

        if vga_trace_enabled() {
            // Debug: count non-zero pixels in architectural video memory.
            static DBG_CNT: AtomicU32 = AtomicU32::new(0);
            let cnt = DBG_CNT.fetch_add(1, Ordering::Relaxed);
            if cnt.is_multiple_of(60) {
                // A0000..AFFFF (VGA-ish linear/planar area)
                let vm = memory.video_memory();
                let mut a000_nz = 0usize;
                let mut a000_gt63 = 0usize;
                let mut a000_gt95 = 0usize;
                let mut a000_max = 0u8;
                for &b in &vm[..64000] {
                    if b != 0 {
                        a000_nz += 1;
                    }
                    if b > 63 {
                        a000_gt63 += 1;
                    }
                    if b > 95 {
                        a000_gt95 += 1;
                    }
                    if b > a000_max {
                        a000_max = b;
                    }
                }

                // B8000..BBFFF (CGA graphics/text area, 16KB window)
                let mut b800_nz = 0usize;
                let mut b800_attr_nonzero = 0usize;
                let mut b800_attr_total = 0usize;
                for (i, addr) in (0xB8000..0xBC000).enumerate() {
                    let v = memory.read_u8(addr);
                    if v != 0 {
                        b800_nz += 1;
                    }
                    // Treat odd bytes as "attributes" if interpreted as text.
                    if (i & 1) == 1 {
                        b800_attr_total += 1;
                        if v != 0 {
                            b800_attr_nonzero += 1;
                        }
                    }
                }
                // Check text mode VRAM (B8000)
                let mut text_nz = 0usize;
                let mut text_content = String::new();
                let text_base = self.text_memory_base();
                let start_words =
                    ((self.crtc_regs[0x0C] as usize) << 8) | (self.crtc_regs[0x0D] as usize);
                for i in (0xB8000..0xB8FA0).step_by(2) {
                    // Scan 4000 bytes (80x25x2)
                    let ch = memory.read_u8(i);
                    if ch != 0 && ch != 32 {
                        // Ignore null and space
                        text_nz += 1;
                        if text_content.len() < 50 {
                            text_content.push(ch as char);
                        }
                    }
                }

                let mut p0_nz = 0usize;
                let mut p1_nz = 0usize;
                let mut p2_nz = 0usize;
                let mut p3_nz = 0usize;
                for (idx, &b) in memory.plane(0).iter().enumerate() {
                    if b != 0 {
                        p0_nz += 1;
                    }
                    if memory.plane(1)[idx] != 0 {
                        p1_nz += 1;
                    }
                    if memory.plane(2)[idx] != 0 {
                        p2_nz += 1;
                    }
                    if memory.plane(3)[idx] != 0 {
                        p3_nz += 1;
                    }
                }

                tracing::debug!(
                    target: "gp64::hw::vga",
                    mode = format_args!("{:02X}", self.mode()),
                    render_mode = format_args!("{:02X}", render_mode),
                    chain4 = memory.vga.chain4,
                    mode_x = self.is_mode_x(),
                    plane_mask = format_args!("{:02X}", memory.vga.plane_mask),
                    write_mode = memory.vga.write_mode,
                    a000_nz,
                    a000_gt63,
                    a000_gt95,
                    a000_max,
                    p0_nz,
                    p1_nz,
                    p2_nz,
                    p3_nz,
                    b800_nz,
                    b800_attr_nonzero,
                    b800_attr_total,
                    text_base = format_args!("{:05X}", text_base),
                    start_words = format_args!("{:04X}", start_words),
                    text_nz,
                    text_content,
                    "VGA frame debug"
                );
            }
        }

        if (self.is_mode_x() || self.is_unchained_16color_planar()) && forced_mode.is_none() {
            if self.is_256_color_mode() {
                self.render_mode_x(memory);
            } else {
                // Unchained 16-color planar mode
                // Use the standard 16-color planar renderer with unchained dimensions
                let width = self.mode_x_width();
                let height = self.mode_x_visible_height().min(480);
                self.render_planar_16color(memory, width, height, 1);
            }
            return;
        }

        match render_mode {
            0x13 => {
                // Mode 13h - 320x200x256 (chained/linear)
                const WIDTH: usize = 320;
                const HEIGHT: usize = 200;
                if self.framebuffer.len() != WIDTH * HEIGHT {
                    self.framebuffer = vec![0u32; WIDTH * HEIGHT];
                    self.fb_width = WIDTH;
                    self.fb_height = HEIGHT;
                }
                let video_mem = memory.video_memory();

                // Read display start from live CRTC registers.
                let start_words = self.render_start_word();
                let start_byte = self.render_start_byte();
                let byte_panning = start_byte.wrapping_sub(start_words) & 0x03;
                let start_offset = start_words * 4 + byte_panning; // Chain-4: *4, plus byte panning

                // Get pixel panning and split screen values
                let pixel_panning = self.render_pixel_panning();
                let line_compare = self.render_line_compare();

                let split_active = line_compare < HEIGHT;
                let vde = self.get_vertical_display_end() as usize;
                let visible_height = (vde + 1).min(HEIGHT);

                // Rebuild palette LUT if needed
                self.rebuild_palette_lut_if_dirty();
                let pel_mask = self.pel_mask;
                let overscan = self.overscan_rgb();
                // Blank if PAS (Palette Address Source) is clear
                let pas_blank = !self.palette_address_source;

                for y in 0..HEIGHT {
                    if y >= visible_height || pas_blank {
                        let row_start = y * WIDTH;
                        self.framebuffer[row_start..row_start + WIDTH].fill(overscan);
                        continue;
                    }

                    // Use current palette LUT directly
                    let palette = &self.palette_lut;

                    // Split screen: below line_compare, display resets to address 0
                    // and pixel panning is disabled
                    let (row_base, panning) = if split_active && y >= line_compare {
                        ((y - line_compare) * WIDTH, 0)
                    } else {
                        (start_offset + y * WIDTH, pixel_panning)
                    };

                    for x in 0..WIDTH {
                        // Apply horizontal pixel panning (source shifts right)
                        let src_x = (x + panning) % WIDTH;
                        let src_addr = row_base + src_x;
                        let color_idx = if src_addr < video_mem.len() {
                            (video_mem[src_addr] & pel_mask) as usize
                        } else {
                            0
                        };
                        self.framebuffer[y * WIDTH + x] = palette[color_idx];
                    }
                }
            }
            0x00 | 0x01 | 0x02 | 0x03 | 0x07 => {
                // Text modes - render with built-in font
                self.render_text_mode(memory);
            }
            0x04 | 0x05 => {
                // CGA 320x200 4-color
                self.render_cga_mode04(memory);
            }
            0x06 => {
                // CGA 640x200 2-color
                self.render_cga_mode06(memory);
            }
            0x0D => {
                // EGA 320x200 16-color planar
                self.render_planar_16color(memory, 320, 200, 2);
            }
            0x0E => {
                // EGA 640x200 16-color planar
                self.render_planar_16color(memory, 640, 200, 2);
            }
            0x10 => {
                // EGA 640x350 16-color planar
                self.render_planar_16color(memory, 640, 350, 1);
            }
            0x12 => {
                // VGA 640x480 16-color planar
                self.render_planar_16color(memory, 640, 480, 1);
            }
            0x0F => {
                // EGA 640x350 monochrome (2-plane: planes 0 and 2)
                self.render_ega_mono_mode(memory);
            }
            0x11 => {
                // VGA 640x480 2-color monochrome (1-plane)
                self.render_vga_mono_mode(memory);
            }

            _ => {}
        }
    }

    /// Render CGA Mode 06h (640x200 2-color)
    fn render_cga_mode06(&mut self, memory: &Memory) {
        let width = 640;
        let height = 200;

        if self.framebuffer.len() != width * height {
            self.framebuffer = vec![0u32; width * height];
            self.fb_width = width;
            self.fb_height = height;
        }

        // AC[0]/AC[1] are refreshed by the colour-select write paths, not
        // here — see the note in `render_cga_mode06_scanline`.
        self.rebuild_palette16_lut_if_dirty();

        let base_even = 0xB8000;
        let base_odd = 0xB8000 + 0x2000;

        // Use VGA palette pipeline: pixel 0 → AC[0] → DAC, pixel 1 → AC[1] → DAC.
        // sync_cga_to_ac_palette sets AC[1] from color_select_reg for mode 06h.
        let bg = self.palette_to_rgb(0);
        let fg = self.palette_to_rgb(1);

        for y in 0..height {
            let is_odd = (y & 1) != 0;
            let offset = (y / 2) * 80; // 80 bytes per scanline
            let base = if is_odd { base_odd } else { base_even };

            for col_byte in 0..80 {
                let addr = base + offset + col_byte;
                let byte = memory.read_u8(addr);
                for bit in 0..8 {
                    let x = col_byte * 8 + bit;
                    let on = ((byte >> (7 - bit)) & 1) != 0;
                    self.framebuffer[y * width + x] = if on { fg } else { bg };
                }
            }
        }
    }

    /// Render CGA Mode 04h/05h (320x200 4-color)
    fn render_cga_mode04(&mut self, memory: &Memory) {
        let width = 320;
        let height = 200;

        // Ensure framebuffer is correct size
        if self.framebuffer.len() != width * height {
            self.framebuffer = vec![0u32; width * height];
            self.fb_width = width;
            self.fb_height = height;
        }

        // AC[0]..AC[3] are refreshed by the colour-select write paths, not
        // here — see the note in `render_cga_mode06_scanline`.
        self.rebuild_palette16_lut_if_dirty();

        // Use the VGA palette pipeline for all 4 CGA color indices.
        // On real VGA, CGA pixels go through: 2-bit color → AC palette reg → DAC → RGB.
        // Programs can change colors via CGA mechanisms (port 0x3D9 / INT 10h AH=0Bh)
        // or VGA mechanisms (INT 10h AH=10h), both reflected through AC palette registers.
        let palette = [
            self.palette_to_rgb(0),
            self.palette_to_rgb(1),
            self.palette_to_rgb(2),
            self.palette_to_rgb(3),
        ];

        // CGA graphics memory for 320x200 modes:
        // - even scanlines at B800:0000
        // - odd  scanlines at B800:2000
        //
        // The selected CGA layout determines how raw bitplane bytes are scanned out.
        let mut base_even = 0xB8000;
        let mut base_odd = 0xB8000 + 0x2000;
        // KILN-00086: the CGA-header-bytes env var was retired; only the (test-only)
        // override remains.
        if let Some(s) = cga_header_bytes_override() {
            let n = s.trim().parse::<usize>().unwrap_or(0).min(0x1FFF);
            base_even += n;
            base_odd += n;
        }

        for y in 0..height {
            let is_odd = (y & 1) != 0;
            let offset = (y / 2) * 80; // 80 bytes per line
            let base = if is_odd { base_odd } else { base_even };

            for col_byte in 0..80 {
                let addr = base + offset + col_byte;
                let byte = memory.read_u8(addr);

                for p in 0..4 {
                    // Pixel 0 is bits 7-6, Pixel 1 is 5-4, etc.
                    let shift = 6 - (p * 2);
                    let color_idx = (byte >> shift) & 0x03;
                    let color = palette[color_idx as usize];

                    let x = col_byte * 4 + p;
                    self.framebuffer[y * width + x] = color;
                }
            }
        }
    }

    /// Render EGA/VGA 16-color planar modes (0Dh, 0Eh, 10h, 12h)
    ///
    /// `scanlines_per_fb_row` is how many physical CRT scanlines one
    /// framebuffer row stands for — see [`Vga::planar_row_repeat`].
    fn render_planar_16color(
        &mut self,
        memory: &Memory,
        width: usize,
        height: usize,
        scanlines_per_fb_row: usize,
    ) {
        // Ensure framebuffer is correct size
        if self.framebuffer.len() != width * height {
            self.framebuffer = vec![0u32; width * height];
            self.fb_width = width;
            self.fb_height = height;
        }

        let mut bytes_per_row = self.render_bytes_per_row(width, false);
        if bytes_per_row > VGA_PLANE_SIZE {
            bytes_per_row = VGA_PLANE_SIZE;
        }

        let start_offset = self.render_start_byte() % VGA_PLANE_SIZE;

        // Get pixel panning and line compare for split screen
        let pixel_panning = self.render_pixel_panning();
        let line_compare = self.render_line_compare();
        let split_active = line_compare < height;

        // Debug: trace CRTC start address changes (deduped)
        {
            static LAST: AtomicUsize = AtomicUsize::new(usize::MAX);
            let last = LAST.swap(start_offset, Ordering::Relaxed);
            if last != start_offset {
                tracing::debug!(
                    start = format_args!("{:04X}", start_offset),
                    "CRTC render start address changed"
                );
            }
        }

        // Visible bytes per scanline = display width in pixels / 8.
        // bytes_per_row (from CRTC Offset register) is the VRAM stride between
        // rows, which may be larger when a virtual screen wider than the display
        // is used for hardware scrolling.
        let visible_bytes = width / 8;

        // Get references to all 4 planes for efficient access
        let p0 = memory.plane(0);
        let p1 = memory.plane(1);
        let p2 = memory.plane(2);
        let p3 = memory.plane(3);

        // CR09 row repetition (same logic as render_planar_16color_scanline).
        let row_repeat = self.planar_row_repeat(scanlines_per_fb_row);

        for y in 0..height {
            // Split screen: below line_compare, display resets to address 0
            // and pixel panning is disabled
            let (row_start, panning) = if split_active && y >= line_compare {
                // Below split: start from 0, no panning, relative row
                (((y - line_compare) / row_repeat) * bytes_per_row, 0)
            } else {
                // Above split: normal start address + panning
                (
                    start_offset + (y / row_repeat) * bytes_per_row,
                    pixel_panning,
                )
            };
            let row_start = row_start % VGA_PLANE_SIZE;

            for x_byte in 0..visible_bytes {
                let offset = (row_start + x_byte) % VGA_PLANE_SIZE;
                debug_assert!(offset < p0.len());

                // Read all 4 planes at this offset from Memory's planes
                let plane0 = p0[offset];
                let plane1 = p1[offset];
                let plane2 = p2[offset];
                let plane3 = p3[offset];

                // Each byte contains 8 pixels
                for bit in 0..8 {
                    let mask = 0x80 >> bit;
                    let color_idx = (if plane0 & mask != 0 { 1 } else { 0 })
                        | (if plane1 & mask != 0 { 2 } else { 0 })
                        | (if plane2 & mask != 0 { 4 } else { 0 })
                        | (if plane3 & mask != 0 { 8 } else { 0 });
                    // Apply Color Plane Enable mask (Attribute Register 0x12, bits 3-0)
                    let color_idx = color_idx & (self.attribute_regs[0x12] & 0x0F);

                    let rgb = self.render_palette16_rgb(color_idx);
                    let src_x = x_byte * 8 + bit;
                    // Apply pixel panning: shift destination left (scroll content right)
                    let fb_x = (src_x + width - panning) % width;
                    self.framebuffer[y * width + fb_x] = rgb;
                }
            }
        }
    }

    /// Render Mode X (unchained 256-color planar mode)
    fn render_mode_x(&mut self, memory: &Memory) {
        // Rebuild 256-color palette LUT if dirty
        self.rebuild_palette_lut_if_dirty();

        let width = self.fb_width;
        let height = self.mode_x_visible_height().min(480); // Clamp to reasonable max

        // Ensure framebuffer is correct size
        let expected_size = width * height;
        if self.framebuffer.len() != expected_size {
            self.framebuffer = vec![0u32; expected_size];
            self.fb_width = width;
            self.fb_height = height;
        }

        // Get pixel panning and line compare for split screen
        let pixel_panning = self.render_pixel_panning();
        let line_compare = self.render_line_compare();
        let split_active = line_compare < height;

        // In Mode X, video memory is planar with 4 planes.
        // Each byte address contains 4 pixels (one from each plane).
        // Memory layout: plane = x & 3, offset = (y * width/4) + (x / 4)
        // CRTC offset/start are word-addressed (2 bytes). Convert to byte offsets.
        let mut bytes_per_row = self.render_bytes_per_row(width, true);
        if bytes_per_row > VGA_PLANE_SIZE {
            bytes_per_row = VGA_PLANE_SIZE;
        }
        let start_offset = self.render_start_byte() % VGA_PLANE_SIZE;

        for y in 0..height {
            // Split screen: below line_compare, display resets to address 0
            // and pixel panning is disabled
            let (row_base, panning) = if split_active && y >= line_compare {
                ((y - line_compare) * bytes_per_row, 0)
            } else {
                (start_offset + y * bytes_per_row, pixel_panning)
            };
            let row_base = row_base % VGA_PLANE_SIZE;

            for x in 0..width {
                // Apply pixel panning to source coordinate
                let src_x = (x + panning) % width;
                let plane = src_x & 3;
                let offset = (row_base + (src_x / 4)) % VGA_PLANE_SIZE;
                let plane_data = memory.plane(plane);
                debug_assert!(offset < plane_data.len());
                // Apply PEL mask to color index before LUT lookup
                let color_idx = (plane_data[offset] & self.pel_mask) as usize;
                self.framebuffer[y * width + x] = self.palette_lut[color_idx];
            }
        }
    }

    /// Render EGA 640x350 monochrome mode (Mode 0Fh)
    /// Uses planes 0 and 2 for video and intensity
    fn render_ega_mono_mode(&mut self, memory: &Memory) {
        const WIDTH: usize = 640;
        const HEIGHT: usize = 350;
        const BYTES_PER_ROW: usize = WIDTH / 8;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        // Get pixel panning and line compare for split screen
        let pixel_panning = self.render_pixel_panning();
        let line_compare = self.render_line_compare();
        let split_active = line_compare < HEIGHT;

        // Get CRTC start address for page flipping/hardware scrolling
        // Start address is in words (2 bytes), convert to byte offset
        let start_addr = self.render_start_byte() % VGA_PLANE_SIZE;

        // Get references to planes 0 and 2
        let p0 = memory.plane(0);
        let p2 = memory.plane(2);

        // Use VGA palette pipeline: 2-bit pixel from planes 0+2 → AC palette → DAC → RGB
        // Plane 0 = video bit (bit 0), Plane 2 = intensity bit (bit 2)
        // Pixel values: 0=off, 1=video, 4=intensity, 5=video+intensity

        for y in 0..HEIGHT {
            // Split screen: below line_compare, display resets to address 0
            // and pixel panning is disabled
            let (row_base, panning) = if split_active && y >= line_compare {
                ((y - line_compare) * BYTES_PER_ROW, 0)
            } else {
                (start_addr + y * BYTES_PER_ROW, pixel_panning)
            };
            let row_base = row_base % VGA_PLANE_SIZE;

            for x_byte in 0..BYTES_PER_ROW {
                let offset = (row_base + x_byte) % VGA_PLANE_SIZE;
                debug_assert!(offset < p0.len());

                let video_byte = p0[offset];
                let intensity_byte = p2[offset];

                for bit in 0..8 {
                    let mask = 0x80 >> bit;
                    let color_idx = (if video_byte & mask != 0 { 1 } else { 0 })
                        | (if intensity_byte & mask != 0 { 4 } else { 0 });
                    let color_idx = color_idx & (self.attribute_regs[0x12] & 0x0F);

                    let color = self.render_palette16_rgb(color_idx);
                    let src_x = x_byte * 8 + bit;
                    let fb_x = (src_x + WIDTH - panning) % WIDTH;
                    self.framebuffer[y * WIDTH + fb_x] = color;
                }
            }
        }
    }

    /// Render VGA 640x480 2-color monochrome mode (Mode 11h)
    /// Uses only plane 0 (single-bit pixels)
    fn render_vga_mono_mode(&mut self, memory: &Memory) {
        const WIDTH: usize = 640;
        const HEIGHT: usize = 480;
        const BYTES_PER_ROW: usize = WIDTH / 8;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        // Get pixel panning and line compare for split screen
        let pixel_panning = self.render_pixel_panning();
        let line_compare = self.render_line_compare();
        let split_active = line_compare < HEIGHT;

        // Get CRTC start address for page flipping/hardware scrolling
        // Start address is in words (2 bytes), convert to byte offset
        let start_addr = self.render_start_byte() % VGA_PLANE_SIZE;

        let p0 = memory.plane(0);

        // Use VGA palette pipeline: pixel 0 → AC[0] → DAC, pixel 1 → AC[1] → DAC
        let color_off = self.palette_to_rgb(0);
        let color_on = self.palette_to_rgb(1);

        for y in 0..HEIGHT {
            // Split screen: below line_compare, display resets to address 0
            // and pixel panning is disabled
            let (row_base, panning) = if split_active && y >= line_compare {
                ((y - line_compare) * BYTES_PER_ROW, 0)
            } else {
                (start_addr + y * BYTES_PER_ROW, pixel_panning)
            };
            let row_base = row_base % VGA_PLANE_SIZE;

            for x_byte in 0..BYTES_PER_ROW {
                let offset = (row_base + x_byte) % VGA_PLANE_SIZE;
                debug_assert!(offset < p0.len());

                let byte = p0[offset];

                for bit in 0..8 {
                    let mask = 0x80 >> bit;
                    let pixel_on = (byte & mask) != 0;
                    let color = if pixel_on { color_on } else { color_off };

                    let src_x = x_byte * 8 + bit;
                    let fb_x = (src_x + WIDTH - panning) % WIDTH;
                    self.framebuffer[y * WIDTH + fb_x] = color;
                }
            }
        }
    }

    /// Render text mode with built-in font
    fn render_text_mode(&mut self, memory: &Memory) {
        let base = self.text_memory_base();
        let char_width = (self.fb_width / self.columns as usize).max(1);
        let char_height = self.text_character_height();
        let use_8x16 = char_height >= 16;

        // Ensure framebuffer is correct size for text mode
        let expected_size = self.fb_width * self.fb_height;
        if self.framebuffer.len() != expected_size {
            self.framebuffer = vec![0u32; expected_size];
        }

        // PAS blanking. Palette Address Source is the display-enable gate on the
        // Attribute Controller in EVERY mode, not just graphics: with it clear
        // the palette is not driven and the CRT shows overscan. The graphics
        // full-frame and scanline paths already honoured it; text did not, so a
        // guest that cleared PAS in text mode kept a fully lit screen
        // (MDD-BUG-CRUCIBLE-00560). Mode set enables PAS (`set_mode_bios`), and
        // a real VGA BIOS ends its own attribute programming with bit 5 set, so
        // ordinary text output is unaffected.
        if !self.palette_address_source {
            let overscan = self.overscan_rgb();
            self.framebuffer.fill(overscan);
            return;
        }

        // Get pixel panning and line compare for split screen
        let pixel_panning = self.render_pixel_panning();
        let line_compare = self.render_line_compare();
        let split_active = line_compare < self.fb_height;

        let blink_on = (self.blink_frame_count / 8).is_multiple_of(2);
        let blink_enabled = (self.attribute_regs[0x10] & 0x08) != 0;

        // Fast path: no pixel panning and no split screen - render per-character
        if pixel_panning == 0 && !split_active {
            self.render_text_mode_fast(
                memory,
                base,
                char_width,
                char_height,
                use_8x16,
                blink_on,
                blink_enabled,
            );
        } else {
            // Slow path: pixel-level rendering for panning/split screen
            self.render_text_mode_slow(
                memory,
                base,
                char_width,
                char_height,
                use_8x16,
                blink_on,
                blink_enabled,
                pixel_panning,
                line_compare,
                split_active,
            );
        }

        // Render text cursor after characters
        self.render_text_cursor(memory, char_width, char_height);
    }

    /// Fast text mode rendering - per-character iteration
    /// Used when no pixel panning or split screen is active
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn render_text_mode_fast(
        &mut self,
        memory: &Memory,
        base: usize,
        char_width: usize,
        char_height: usize,
        use_8x16: bool,
        blink_on: bool,
        blink_enabled: bool,
    ) {
        let fb_width = self.fb_width;
        let columns = self.columns as usize;
        let rows = self.text_render_rows(char_height);

        for row in 0..rows {
            for col in 0..columns {
                let addr = base + (row * columns + col) * 2;
                let ch = memory.read_u8(addr);
                let attr = memory.read_u8(addr + 1);

                let fg_color = attr & 0x0F;
                let bg_color = if blink_enabled {
                    (attr >> 4) & 0x07
                } else {
                    (attr >> 4) & 0x0F
                };

                let fg_rgb = self.render_palette16_rgb(fg_color);
                let bg_rgb = self.render_palette16_rgb(bg_color);

                let char_blink_off = blink_enabled && (attr & 0x80) != 0 && !blink_on;

                let base_x = col * char_width;
                let base_y = row * char_height;

                // Render all pixels of this character
                for cy in 0..char_height {
                    let font_row = if self.custom_font_loaded {
                        // Use custom font from plane 2 (falls back to built-in if not present)
                        self.get_font_row(memory, ch, cy, char_height, attr)
                    } else if use_8x16 {
                        CP437_FONT_8X16[ch as usize * 16 + cy.min(15)]
                    } else {
                        let src_row = (cy.saturating_mul(8) / char_height.max(1)).min(7);
                        CP437_FONT_8X8[ch as usize * 8 + src_row]
                    };

                    let pixel_y = base_y + cy;
                    if pixel_y >= self.fb_height {
                        break;
                    }

                    // Double-width: 40-column modes use 18 dots per character
                    // (each font pixel appears twice horizontally).
                    let double_wide = char_width > 9;

                    for cx in 0..char_width {
                        let pixel_x = base_x + cx;
                        if pixel_x >= fb_width {
                            break;
                        }

                        // Map framebuffer pixel to font column (0-8)
                        let font_cx = if double_wide { cx / 2 } else { cx };

                        let mut pixel_on = if font_cx < 8 {
                            // Normal font pixel (bits 7-0 map to columns 0-7)
                            let bit = 7 - font_cx;
                            (font_row >> bit) & 1 != 0
                        } else {
                            // 9th column: duplicate 8th pixel only for line-drawing chars
                            // VGA duplicates for chars 0xC0-0xDF when Line Graphics Enable is set
                            // (Attribute Controller reg 0x10 bit 2)
                            let line_graphics_enable =
                                self.attribute_regs.get(0x10).is_none_or(|v| v & 0x04 != 0);
                            if line_graphics_enable && (0xC0..=0xDF).contains(&ch) {
                                (font_row & 1) != 0 // Duplicate bit 0 (8th pixel)
                            } else {
                                false // Background color for non-line-drawing chars
                            }
                        };
                        if char_blink_off {
                            pixel_on = false;
                        }

                        let fb_idx = pixel_y * fb_width + pixel_x;
                        self.framebuffer[fb_idx] = if pixel_on { fg_rgb } else { bg_rgb };
                    }
                }
            }
        }
    }

    /// Slow text mode rendering - per-pixel iteration
    /// Used when pixel panning or split screen is active
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn render_text_mode_slow(
        &mut self,
        memory: &Memory,
        base: usize,
        char_width: usize,
        char_height: usize,
        use_8x16: bool,
        blink_on: bool,
        blink_enabled: bool,
        pixel_panning: usize,
        line_compare: usize,
        split_active: bool,
    ) {
        let fb_width = self.fb_width;
        let fb_height = self.fb_height;
        let columns = self.columns as usize;
        let rows = self.text_render_rows(char_height);
        let aperture_base = self.text_memory_aperture_base();

        // Double-width: 40-column modes use 18 dots per character
        let double_wide = char_width > 9;

        for pixel_y in 0..fb_height {
            let (effective_y, panning, addr_base) = if split_active && pixel_y >= line_compare {
                (pixel_y - line_compare, 0, aperture_base)
            } else {
                (pixel_y, pixel_panning, base)
            };

            for pixel_x in 0..fb_width {
                let src_x = (pixel_x + panning) % fb_width;
                let col = src_x / char_width;
                let row = effective_y / char_height;
                let cx = src_x % char_width;
                let cy = effective_y % char_height;

                // Map framebuffer pixel to font column (0-8)
                let font_cx = if double_wide { cx / 2 } else { cx };

                if row >= rows || col >= columns {
                    continue;
                }

                let addr = addr_base + (row * columns + col) * 2;
                let ch = memory.read_u8(addr);
                let attr = memory.read_u8(addr + 1);

                let fg_color = attr & 0x0F;
                let bg_color = if blink_enabled {
                    (attr >> 4) & 0x07
                } else {
                    (attr >> 4) & 0x0F
                };

                let fg_rgb = self.render_palette16_rgb(fg_color);
                let bg_rgb = self.render_palette16_rgb(bg_color);

                let font_row = if self.custom_font_loaded {
                    self.get_font_row(memory, ch, cy, char_height, attr)
                } else if use_8x16 {
                    CP437_FONT_8X16[ch as usize * 16 + cy.min(15)]
                } else {
                    let src_row = (cy.saturating_mul(8) / char_height.max(1)).min(7);
                    CP437_FONT_8X8[ch as usize * 8 + src_row]
                };

                let mut pixel_on = if font_cx < 8 {
                    // Normal font pixel (bits 7-0 map to columns 0-7)
                    let bit = 7 - font_cx;
                    (font_row >> bit) & 1 != 0
                } else {
                    // 9th column: duplicate 8th pixel only for line-drawing chars
                    let line_graphics_enable =
                        self.attribute_regs.get(0x10).is_none_or(|v| v & 0x04 != 0);
                    if line_graphics_enable && (0xC0..=0xDF).contains(&ch) {
                        (font_row & 1) != 0
                    } else {
                        false
                    }
                };
                if blink_enabled && (attr & 0x80) != 0 && !blink_on {
                    pixel_on = false;
                }

                let fb_idx = pixel_y * fb_width + pixel_x;
                self.framebuffer[fb_idx] = if pixel_on { fg_rgb } else { bg_rgb };
            }
        }
    }

    /// Render the text mode cursor
    fn render_text_cursor(&mut self, memory: &Memory, char_width: usize, char_height: usize) {
        // Check if cursor is disabled (bit 5 of CRTC register 0x0A)
        let cursor_start_reg = self.crtc_regs[0x0A];
        if (cursor_start_reg & 0x20) != 0 {
            return; // Cursor disabled
        }

        // Cursor blink: ~1.875 Hz at 60 Hz refresh (8 frames on, 8 frames off)
        // Only draw cursor during the "on" phase of the blink cycle
        if !(self.blink_frame_count / 8).is_multiple_of(2) {
            return; // Cursor in "off" phase of blink
        }

        // Get cursor shape (start and end scanlines within character)
        let cursor_start = (cursor_start_reg & 0x1F) as usize;
        let cursor_end = (self.crtc_regs[0x0B] & 0x1F) as usize;

        // Get cursor position from CRTC registers 0x0E/0x0F, made relative to
        // the CRTC display start so it works on pages > 0.
        let cursor_abs = ((self.crtc_regs[0x0E] as usize) << 8) | (self.crtc_regs[0x0F] as usize);
        let cursor_pos = cursor_abs.wrapping_sub(self.render_start_word());

        // Convert position to row/col
        let cursor_col = cursor_pos % self.columns as usize;
        let cursor_row = cursor_pos / self.columns as usize;

        // Validate cursor is within visible area
        if cursor_row >= self.rows as usize {
            return;
        }

        // Get foreground color from character attribute at cursor position
        let base = self.text_memory_base();
        let addr = base + cursor_pos * 2;
        let attr = memory.read_u8(addr + 1);
        let fg_color = attr & 0x0F;
        let cursor_rgb = self.render_palette16_rgb(fg_color);

        // Calculate pixel position of cursor character cell
        let base_x = cursor_col * char_width;
        let base_y = cursor_row * char_height;

        // CRTC cursor start/end are in character scanlines for the active font height.
        let max_scan = char_height.saturating_sub(1);
        let cursor_start = cursor_start.min(max_scan);
        let cursor_end = cursor_end.min(max_scan);
        if cursor_end < cursor_start {
            return;
        }
        let scaled_start = cursor_start;
        let scaled_end = (cursor_end + 1).min(char_height);

        // Draw cursor scanlines
        for scanline in scaled_start..scaled_end {
            let y = base_y + scanline;
            if y >= self.fb_height {
                continue;
            }
            for x in base_x..base_x + char_width {
                if x >= self.fb_width {
                    continue;
                }
                let fb_idx = y * self.fb_width + x;
                if fb_idx < self.framebuffer.len() {
                    self.framebuffer[fb_idx] = cursor_rgb;
                }
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // Per-scanline rendering methods
    // ──────────────────────────────────────────────────────────────────────

    /// Render a single scanline into the framebuffer using live VGA state.
    /// Called by the per-scanline event handler for each visible line.
    pub fn render_scanline(&mut self, line: u32, memory: &mut Memory) {
        if self.video_output_blanked() {
            let start = line as usize * self.fb_width;
            let end = start
                .saturating_add(self.fb_width)
                .min(self.framebuffer.len());
            if start < end && self.raster_output {
                self.framebuffer[start..end].fill(0);
            }
            return;
        }
        // Propagate the Memory-side plane-2 font-upload latch once per
        // frame (line 0).  See `Vga::render` for the full rationale.
        if line == 0 && memory.planar_font_latch_pending() {
            self.custom_font_loaded = true;
        }

        // Detect mode from register state (see render() for rationale).
        // Cache mode() once — it reads ~8 registers per call.
        let current_mode = self.mode();
        let render_mode = if self.is_256_color_mode() {
            0x13
        } else if !matches!(current_mode, 0x00..=0x03 | 0x07) {
            current_mode
        } else if (self.attribute_regs[0x10] & 0x01) != 0 {
            let vde = self.get_vertical_display_end();
            if vde >= 479 {
                0x12
            } else if vde >= 349 {
                0x10
            } else {
                0x0D
            }
        } else {
            current_mode
        };

        // Apply the same display-enable policy as full-frame rendering.
        if self.display_gate_blanks_render_mode(render_mode) {
            let overscan = self.overscan_rgb();
            let row_start = line as usize * self.fb_width;
            if self.raster_output && row_start + self.fb_width <= self.framebuffer.len() {
                self.framebuffer[row_start..row_start + self.fb_width].fill(overscan);
            }
            return;
        }

        // Mode X: separate handler
        if self.is_mode_x() || self.is_unchained_16color_planar() {
            if self.is_256_color_mode() {
                self.render_mode_x_scanline(line, memory);
            } else {
                // Unchained 16-color planar mode
                let width = self.mode_x_width();
                let height = self.mode_x_visible_height().min(480);
                self.render_planar_16color_scanline(line, memory, width, height, 1);
            }
            return;
        }

        match render_mode {
            0x13 => self.render_mode13h_scanline(line, memory),
            0x00 | 0x01 | 0x02 | 0x03 | 0x07 => self.render_text_mode_scanline(line, memory),
            0x04 | 0x05 => self.render_cga_mode04_scanline(line, memory),
            0x06 => self.render_cga_mode06_scanline(line, memory),
            0x0D => self.render_planar_16color_scanline(line, memory, 320, 200, 2),
            0x0E => self.render_planar_16color_scanline(line, memory, 640, 200, 2),
            0x10 => self.render_planar_16color_scanline(line, memory, 640, 350, 1),
            0x12 => self.render_planar_16color_scanline(line, memory, 640, 480, 1),
            0x0F => self.render_ega_mono_scanline(line, memory),
            0x11 => self.render_vga_mono_scanline(line, memory),

            _ => {}
        }
    }

    fn video_output_blanked(&self) -> bool {
        (self.sequencer_regs[0x01] & 0x20) != 0
    }

    /// Ensure the framebuffer is correctly sized for the current mode.
    /// Called before per-scanline rendering begins each frame.
    pub fn ensure_framebuffer_for_mode(&mut self) {
        let (width, height) = self.get_framebuffer_dimensions();
        if width != self.fb_width || height != self.fb_height {
            tracing::debug!(
                target: "gp64::hw::vga",
                old = format_args!("{}x{}", self.fb_width, self.fb_height),
                new = format_args!("{}x{}", width, height),
                mode = format_args!("{:02X}", self.mode()),
                mode_x = self.is_mode_x(),
                "ensure_framebuffer_for_mode: dimensions changed"
            );
        }
        if self.framebuffer.len() != width * height {
            // Reallocating the construction base invalidates the completed
            // frame even at the same length (MDD-BUG-CRUCIBLE-00347).
            self.framebuffer = vec![0u32; width * height];
        }
        // Always sync fb_width/fb_height even when the total pixel count
        // is unchanged (e.g. switching between 720×400 text modes where
        // set_mode_bios may have written a stale value).
        self.fb_width = width;
        self.fb_height = height;
    }

    /// Increment the blink frame counter (called once per frame by the event handler).
    pub fn increment_blink_frame_count(&mut self) {
        self.blink_frame_count = self.blink_frame_count.wrapping_add(1);
    }

    /// Rebuild the 16-color palette LUT if dirty (called at frame start by the event handler).
    pub fn rebuild_palette_lut_if_needed(&mut self) {
        self.rebuild_palette16_lut_if_dirty();
    }

    /// Get the expected framebuffer dimensions for the current mode.
    ///
    /// Uses `self.mode()` which is register-derived, so it works correctly
    /// under both HLE INT 10h and `--real-boot` (direct register programming).
    fn get_framebuffer_dimensions(&self) -> (usize, usize) {
        if self.is_mode_x() {
            return (self.mode_x_width(), self.mode_x_visible_height().min(480));
        }
        match self.mode() {
            0x13 => (320, 200),
            0x00 | 0x01 => {
                // 40-column text — same pixel width as 80-column.
                // Real VGA divides the dot clock by 2, so each character pixel
                // is doubled horizontally (18 dots per 40-col char vs 9 per 80-col).
                let char_width = 18;
                (
                    self.columns as usize * char_width,
                    self.text_visible_height(),
                )
            }
            0x02 | 0x03 | 0x07 => {
                // 80-column text
                let char_width = 9;
                (
                    self.columns as usize * char_width,
                    self.text_visible_height(),
                )
            }
            0x04 | 0x05 => (320, 200),
            0x06 => (640, 200),
            0x0D => (320, 200),
            0x0E => (640, 200),
            0x10 => (640, 350),
            0x12 => (640, 480),
            0x0F => (640, 350),
            0x11 => (640, 480),
            0x08 => (160, 200),
            0x09 => (320, 200),
            0x0A => (640, 200),
            _ => (self.fb_width.max(1), self.fb_height.max(1)),
        }
    }

    /// Character scanlines for text modes, from CRTC Maximum Scan Line (index 09h).
    fn text_character_height(&self) -> usize {
        ((self.crtc_regs[0x09] & 0x1F) as usize + 1).max(1)
    }

    /// Visible physical scanlines for text modes, from CRTC Vertical Display End.
    fn text_visible_height(&self) -> usize {
        (self.get_vertical_display_end() as usize + 1).max(1)
    }

    /// Number of visible text rows implied by the current physical height and font height.
    fn text_render_rows(&self, char_height: usize) -> usize {
        (self.fb_height / char_height.max(1)).max(1)
    }

    /// Mode 13h (320x200x256 chained/linear) — single scanline
    fn render_mode13h_scanline(&mut self, line: u32, memory: &Memory) {
        const WIDTH: usize = 320;
        const HEIGHT: usize = 200;
        let y = line as usize;

        // Ensure framebuffer is the right size
        if self.fb_width != WIDTH || self.fb_height != HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        if y >= HEIGHT || !self.raster_output {
            return;
        }

        let row_start = y * WIDTH;
        if row_start + WIDTH > self.framebuffer.len() {
            return;
        }

        // PAS blanking: if PAS bit is cleared, blank this scanline
        if !self.palette_address_source {
            let overscan = self.overscan_rgb();
            self.framebuffer[row_start..row_start + WIDTH].fill(overscan);
            return;
        }

        let video_mem = memory.video_memory();

        // Use the frame-latched CRTC start address and panning.
        let start_offset = self.latched_start_word * 4 + self.latched_byte_panning; // Chain-4: *4, plus byte panning

        let pixel_panning = self.latched_pixel_panning;

        let line_compare = self.get_line_compare() as usize;

        let pel_mask = self.pel_mask;

        // Rebuild palette LUT if needed, then copy to avoid borrow conflict
        self.rebuild_palette_lut_if_dirty();
        let palette: [u32; 256] = self.palette_lut;

        // Split screen: below line_compare, display resets to address 0 with no panning
        let (row_base, panning) = if y >= line_compare && line_compare < HEIGHT {
            ((y - line_compare) * WIDTH, 0)
        } else {
            (start_offset + y * WIDTH, pixel_panning)
        };

        if panning == 0 {
            // Fast path: no panning, skip modulo entirely
            for x in 0..WIDTH {
                let src_addr = row_base + x;
                let color_idx = if src_addr < video_mem.len() {
                    (video_mem[src_addr] & pel_mask) as usize
                } else {
                    0
                };
                self.framebuffer[row_start + x] = palette[color_idx];
            }
        } else {
            // Panning active: conditional subtract instead of modulo
            for x in 0..WIDTH {
                let mut src_x = x + panning;
                if src_x >= WIDTH {
                    src_x -= WIDTH;
                }
                let src_addr = row_base + src_x;
                let color_idx = if src_addr < video_mem.len() {
                    (video_mem[src_addr] & pel_mask) as usize
                } else {
                    0
                };
                self.framebuffer[row_start + x] = palette[color_idx];
            }
        }
    }

    /// Mode X (unchained 256-color planar) — single scanline
    fn render_mode_x_scanline(&mut self, line: u32, memory: &Memory) {
        let width = self.fb_width;
        let height = self.mode_x_visible_height().min(480);
        let y = line as usize;

        // Ensure framebuffer is the right size
        let expected_size = width * height;
        if self.framebuffer.len() != expected_size {
            self.framebuffer = vec![0u32; expected_size];
            self.fb_width = width;
            self.fb_height = height;
        }

        if y >= height || !self.raster_output {
            return;
        }

        // Rebuild palette LUT if needed
        self.rebuild_palette_lut_if_dirty();

        // Use the frame-latched CRTC start address and panning.
        let start_offset = self.latched_start_byte_for_mode() % VGA_PLANE_SIZE;

        let pixel_panning = self.latched_pixel_panning;
        let line_compare = self.get_line_compare() as usize;
        let split_active = line_compare < height;

        let mut bytes_per_row = self.compute_bytes_per_row(width, true);
        if bytes_per_row > VGA_PLANE_SIZE {
            bytes_per_row = VGA_PLANE_SIZE;
        }

        let pel_mask = self.pel_mask;

        let cr09 = self.crtc_regs[0x09];
        let scan_double = (cr09 & 0x80) != 0;
        let row_repeat = if scan_double { 2 } else { 1 };

        // Split screen: below line_compare, display resets to address 0
        let (row_base, panning) = if split_active && y >= line_compare {
            (((y - line_compare) / row_repeat) * bytes_per_row, 0)
        } else {
            (
                start_offset + (y / row_repeat) * bytes_per_row,
                pixel_panning,
            )
        };
        let row_base = row_base % VGA_PLANE_SIZE;

        let row_start = y * width;
        let planes = [
            memory.plane(0),
            memory.plane(1),
            memory.plane(2),
            memory.plane(3),
        ];
        if panning == 0 {
            // Fast path: no panning, skip modulo entirely
            for x in 0..width {
                let offset = (row_base + (x / 4)) % VGA_PLANE_SIZE;
                let color_idx = (planes[x & 3][offset] & pel_mask) as usize;
                self.framebuffer[row_start + x] = self.palette_lut[color_idx];
            }
        } else {
            // Panning active: conditional subtract instead of modulo
            for x in 0..width {
                let mut src_x = x + panning;
                if src_x >= width {
                    src_x -= width;
                }
                let offset = (row_base + (src_x / 4)) % VGA_PLANE_SIZE;
                let color_idx = (planes[src_x & 3][offset] & pel_mask) as usize;
                self.framebuffer[row_start + x] = self.palette_lut[color_idx];
            }
        }
    }

    fn planar_row_repeat(&self, scanlines_per_fb_row: usize) -> usize {
        let cr09 = self.crtc_regs[0x09];
        let max_scan = ((cr09 & 0x1F) as usize) + 1;
        let scan_double = (cr09 & 0x80) != 0;
        let scanlines_per_vram_row = max_scan * if scan_double { 2 } else { 1 };
        (scanlines_per_vram_row / scanlines_per_fb_row.max(1)).max(1)
    }

    /// EGA/VGA 16-color planar modes (0Dh, 0Eh, 10h, 12h) — single scanline
    fn render_planar_16color_scanline(
        &mut self,
        line: u32,
        memory: &Memory,
        width: usize,
        height: usize,
        scanlines_per_fb_row: usize,
    ) {
        let y = line as usize;
        if y >= height {
            return;
        }

        // Ensure framebuffer is correct size
        if self.framebuffer.len() != width * height {
            self.framebuffer = vec![0u32; width * height];
            self.fb_width = width;
            self.fb_height = height;
        }
        if !self.raster_output {
            return;
        }

        // Rebuild 16-color palette LUT if dirty
        self.rebuild_palette16_lut_if_dirty();

        let mut bytes_per_row = self.compute_bytes_per_row(width, false);
        if bytes_per_row > VGA_PLANE_SIZE {
            bytes_per_row = VGA_PLANE_SIZE;
        }

        // Use the frame-latched CRTC start address and panning.
        let start_offset = self.latched_start_byte_for_mode() % VGA_PLANE_SIZE;

        let pixel_panning = self.latched_pixel_panning;
        let line_compare = self.get_line_compare() as usize;
        let split_active = line_compare < height;
        let color_plane_enable = self.attribute_regs[0x12] & 0x0F;

        // Visible bytes per scanline (display width / 8).  bytes_per_row is the
        // VRAM stride which may be larger for virtual-screen hardware scrolling.
        let visible_bytes = width / 8;

        let p0 = memory.plane(0);
        let p1 = memory.plane(1);
        let p2 = memory.plane(2);
        let p3 = memory.plane(3);

        // The effective VRAM row is `y / row_repeat`, with the framebuffer row
        // kept at `y`.
        let row_repeat = self.planar_row_repeat(scanlines_per_fb_row);

        // Split screen
        let (row_start, panning) = if split_active && y >= line_compare {
            (((y - line_compare) / row_repeat) * bytes_per_row, 0)
        } else {
            (
                start_offset + (y / row_repeat) * bytes_per_row,
                pixel_panning,
            )
        };
        let row_start = row_start % VGA_PLANE_SIZE;

        for x_byte in 0..visible_bytes {
            let offset = (row_start + x_byte) % VGA_PLANE_SIZE;

            let plane0 = p0[offset];
            let plane1 = p1[offset];
            let plane2 = p2[offset];
            let plane3 = p3[offset];

            for bit in 0..8 {
                let mask = 0x80 >> bit;
                let color_idx = (if plane0 & mask != 0 { 1 } else { 0 })
                    | (if plane1 & mask != 0 { 2 } else { 0 })
                    | (if plane2 & mask != 0 { 4 } else { 0 })
                    | (if plane3 & mask != 0 { 8 } else { 0 });
                let color_idx = color_idx & color_plane_enable;

                let rgb = self.palette16_lut[(color_idx & 0x0F) as usize];
                let src_x = x_byte * 8 + bit;
                let fb_x = (src_x + width - panning) % width;
                self.framebuffer[y * width + fb_x] = rgb;
            }
        }
    }

    /// Text modes (00h-03h, 07h) — single scanline
    fn render_text_mode_scanline(&mut self, line: u32, memory: &Memory) {
        let y = line as usize;

        // Rebuild 16-color palette LUT if dirty
        self.rebuild_palette16_lut_if_dirty();

        let char_width = (self.fb_width / self.columns as usize).max(1);
        let char_height = self.text_character_height();
        let use_8x16 = char_height >= 16;

        // Ensure framebuffer is correct size
        let expected_size = self.fb_width * self.fb_height;
        if self.framebuffer.len() != expected_size {
            self.framebuffer = vec![0u32; expected_size];
        }

        if y >= self.fb_height || !self.raster_output {
            return;
        }

        // PAS blanking, matching the graphics scanline path (see
        // `render_text_mode` for why text needs the same gate).
        if !self.palette_address_source {
            let overscan = self.overscan_rgb();
            let row_start = y * self.fb_width;
            let row_end = row_start + self.fb_width;
            if row_end <= self.framebuffer.len() {
                self.framebuffer[row_start..row_end].fill(overscan);
            }
            return;
        }

        // Use the frame-latched CRTC start address and panning.
        let aperture_base = self.text_memory_aperture_base();
        let base = aperture_base + self.latched_start_word * 2;

        let pixel_panning = self.latched_pixel_panning;
        let line_compare = self.get_line_compare() as usize;
        let split_active = line_compare < self.fb_height;

        let blink_on = (self.blink_frame_count / 8).is_multiple_of(2);
        let blink_enabled = (self.attribute_regs[0x10] & 0x08) != 0;

        let fb_width = self.fb_width;
        let columns = self.columns as usize;
        let rows = self.text_render_rows(char_height);

        // Determine effective Y and panning for split screen
        let (effective_y, panning) = if split_active && y >= line_compare {
            (y - line_compare, 0)
        } else {
            (y, pixel_panning)
        };

        let char_row = effective_y / char_height;
        let cy = effective_y % char_height;

        if char_row >= rows {
            // Fill with background
            let bg_rgb = self.palette16_lut[0];
            let row_start = y * fb_width;
            if row_start + fb_width <= self.framebuffer.len() {
                self.framebuffer[row_start..row_start + fb_width].fill(bg_rgb);
            }
            return;
        }

        // Double-width: 40-column modes use 18 dots per character
        let double_wide = char_width > 9;
        let row_start = y * fb_width;
        let cell_row_base = if split_active && y >= line_compare {
            aperture_base
        } else {
            base
        };
        let cell_row_start = cell_row_base + char_row * columns * 2;
        let line_graphics_enable = self.attribute_regs.get(0x10).is_none_or(|v| v & 0x04 != 0);
        let blink_forces_background = blink_enabled && !blink_on;

        // Consecutive framebuffer pixels normally belong to the same text
        // cell. Cache the cell state for this scanline instead of rereading
        // character RAM, attributes, palette entries, and font data for every
        // one of its 8, 9, or 18 pixels. Pixel panning can wrap mid-row, so the
        // cache is keyed by the source column rather than assuming a fixed
        // output-cell boundary.
        let mut cached_col = usize::MAX;
        let mut cached_font_row = 0u8;
        let mut cached_fg_rgb = 0u32;
        let mut cached_bg_rgb = 0u32;
        let mut cached_blink_off = false;
        let mut cached_ninth_pixel_on = false;

        for pixel_x in 0..fb_width {
            let src_x = (pixel_x + panning) % fb_width;
            let col = src_x / char_width;
            let cx = src_x % char_width;

            // Map framebuffer pixel to font column (0-8)
            let font_cx = if double_wide { cx / 2 } else { cx };

            if col >= columns {
                self.framebuffer[row_start + pixel_x] = 0;
                continue;
            }

            if cached_col != col {
                cached_col = col;
                let addr = cell_row_start + col * 2;
                let ch = memory.read_u8(addr);
                let attr = memory.read_u8(addr + 1);

                let fg_color = attr & 0x0F;
                let bg_color = if blink_enabled {
                    (attr >> 4) & 0x07
                } else {
                    (attr >> 4) & 0x0F
                };
                cached_fg_rgb = self.palette16_lut[(fg_color & 0x0F) as usize];
                cached_bg_rgb = self.palette16_lut[(bg_color & 0x0F) as usize];

                cached_font_row = if self.custom_font_loaded {
                    self.get_font_row(memory, ch, cy, char_height, attr)
                } else if use_8x16 {
                    CP437_FONT_8X16[ch as usize * 16 + cy.min(15)]
                } else {
                    let src_row = (cy.saturating_mul(8) / char_height.max(1)).min(7);
                    CP437_FONT_8X8[ch as usize * 8 + src_row]
                };
                cached_blink_off = blink_forces_background && (attr & 0x80) != 0;
                cached_ninth_pixel_on = line_graphics_enable
                    && (0xC0..=0xDF).contains(&ch)
                    && (cached_font_row & 1) != 0;
            }

            let pixel_on = if cached_blink_off {
                false
            } else if font_cx < 8 {
                let bit = 7 - font_cx;
                (cached_font_row >> bit) & 1 != 0
            } else {
                // 9th column: duplicate 8th pixel for line-drawing chars
                cached_ninth_pixel_on
            };

            self.framebuffer[row_start + pixel_x] = if pixel_on {
                cached_fg_rgb
            } else {
                cached_bg_rgb
            };
        }

        // Render text cursor for this scanline (if applicable)
        // Cursor is relative to the CRTC start address, so skip in split screen region
        let in_split = split_active && y >= line_compare;
        let cursor_start_reg = self.crtc_regs[0x0A];
        if !in_split
            && (cursor_start_reg & 0x20) == 0
            && (self.blink_frame_count / 8).is_multiple_of(2)
        {
            let cursor_start = (cursor_start_reg & 0x1F) as usize;
            let cursor_end_reg = (self.crtc_regs[0x0B] & 0x1F) as usize;
            let cursor_abs =
                ((self.crtc_regs[0x0E] as usize) << 8) | (self.crtc_regs[0x0F] as usize);
            let cursor_pos = cursor_abs.wrapping_sub(self.latched_start_word);
            let cursor_col = cursor_pos % columns;
            let cursor_row = cursor_pos / columns;

            // Check if cursor is on this scanline's character row and sub-row
            let max_scan = char_height.saturating_sub(1);
            let cs = cursor_start.min(max_scan);
            let ce = cursor_end_reg.min(max_scan);
            if cursor_row == char_row && ce >= cs && cy >= cs && cy <= ce {
                // This scanline intersects the cursor — draw cursor pixels
                let cursor_addr = base + cursor_pos * 2;
                let cursor_attr = memory.read_u8(cursor_addr + 1);
                let cursor_fg = cursor_attr & 0x0F;
                let cursor_rgb = self.palette16_lut[(cursor_fg & 0x0F) as usize];

                // Match the full-frame cursor: include the physical ninth dot
                // and both doubled copies in 40-column cells.
                let cursor_base_x = cursor_col * char_width;
                for x in cursor_base_x..cursor_base_x + char_width {
                    if x < fb_width {
                        self.framebuffer[y * fb_width + x] = cursor_rgb;
                    }
                }
            }
        }
    }

    /// CGA Mode 04h/05h (320x200 4-color) — single scanline
    fn render_cga_mode04_scanline(&mut self, line: u32, memory: &Memory) {
        const WIDTH: usize = 320;
        const HEIGHT: usize = 200;
        let y = line as usize;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        if y >= HEIGHT || !self.raster_output {
            return;
        }

        // Rebuild the palette LUT if dirty. AC[0]..AC[3] are *not* re-derived
        // here — see the note in `render_cga_mode06_scanline`.
        if line == 0 {
            self.rebuild_palette16_lut_if_dirty();
        }

        // Use VGA palette pipeline (same as render_cga_mode04)
        let palette = [
            self.palette_to_rgb(0),
            self.palette_to_rgb(1),
            self.palette_to_rgb(2),
            self.palette_to_rgb(3),
        ];

        let base_even = 0xB8000usize;
        let base_odd = 0xB8000 + 0x2000;

        let is_odd = (y & 1) != 0;
        let offset = (y / 2) * 80;
        let base = if is_odd { base_odd } else { base_even };

        let row_start = y * WIDTH;
        for col_byte in 0..80 {
            let addr = base + offset + col_byte;
            let byte = memory.read_u8(addr);

            for p in 0..4 {
                let shift = 6 - (p * 2);
                let color_idx = (byte >> shift) & 0x03;
                let color = palette[color_idx as usize];

                let x = col_byte * 4 + p;
                self.framebuffer[row_start + x] = color;
            }
        }
    }

    /// CGA Mode 06h (640x200 2-color) — single scanline
    fn render_cga_mode06_scanline(&mut self, line: u32, memory: &Memory) {
        const WIDTH: usize = 640;
        const HEIGHT: usize = 200;
        let y = line as usize;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        if y >= HEIGHT || !self.raster_output {
            return;
        }

        // Rebuild 16-color palette LUT if dirty. AC[0]/AC[1] are *not*
        // re-derived here: `set_cga_color_select` and the port-0x3D9 write
        // path already refresh them whenever the CGA colour-select register
        // changes, and a mode set programs them from the parameter table.
        // Re-deriving per frame would destroy any palette a guest programmed
        // through the VGA attribute controller (INT 10h AH=10h / port 0x3C0),
        self.rebuild_palette16_lut_if_dirty();

        // Use VGA palette pipeline (same as render_cga_mode06)
        let bg = self.palette_to_rgb(0);
        let fg = self.palette_to_rgb(1);

        let base_even = 0xB8000usize;
        let base_odd = 0xB8000 + 0x2000;

        let is_odd = (y & 1) != 0;
        let offset = (y / 2) * 80;
        let base = if is_odd { base_odd } else { base_even };

        let row_start = y * WIDTH;
        for col_byte in 0..80 {
            let addr = base + offset + col_byte;
            let byte = memory.read_u8(addr);
            for bit in 0..8 {
                let x = col_byte * 8 + bit;
                let on = ((byte >> (7 - bit)) & 1) != 0;
                self.framebuffer[row_start + x] = if on { fg } else { bg };
            }
        }
    }

    /// EGA 640x350 monochrome (Mode 0Fh) — single scanline
    fn render_ega_mono_scanline(&mut self, line: u32, memory: &Memory) {
        const WIDTH: usize = 640;
        const HEIGHT: usize = 350;
        const BYTES_PER_ROW: usize = WIDTH / 8;
        let y = line as usize;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        if y >= HEIGHT || !self.raster_output {
            return;
        }

        // Rebuild 16-color palette LUT if dirty
        self.rebuild_palette16_lut_if_dirty();

        // Use the frame-latched CRTC start address and panning.
        let pixel_panning = self.latched_pixel_panning;
        let line_compare = self.get_line_compare() as usize;
        let split_active = line_compare < HEIGHT;

        let start_addr = self.latched_start_byte_for_mode() % VGA_PLANE_SIZE;

        let p0 = memory.plane(0);
        let p2 = memory.plane(2);

        // Use VGA palette pipeline (same as full-frame renderer)
        let (row_base, panning) = if split_active && y >= line_compare {
            ((y - line_compare) * BYTES_PER_ROW, 0)
        } else {
            (start_addr + y * BYTES_PER_ROW, pixel_panning)
        };
        let row_base = row_base % VGA_PLANE_SIZE;

        for x_byte in 0..BYTES_PER_ROW {
            let offset = (row_base + x_byte) % VGA_PLANE_SIZE;
            let video_byte = p0[offset];
            let intensity_byte = p2[offset];

            for bit in 0..8 {
                let mask = 0x80 >> bit;
                let color_idx = (if video_byte & mask != 0 { 1 } else { 0 })
                    | (if intensity_byte & mask != 0 { 4 } else { 0 });
                let color_idx = color_idx & (self.attribute_regs[0x12] & 0x0F);

                let color = self.render_palette16_rgb(color_idx);
                let src_x = x_byte * 8 + bit;
                let fb_x = (src_x + WIDTH - panning) % WIDTH;
                self.framebuffer[y * WIDTH + fb_x] = color;
            }
        }
    }

    /// VGA 640x480 2-color monochrome (Mode 11h) — single scanline
    fn render_vga_mono_scanline(&mut self, line: u32, memory: &Memory) {
        const WIDTH: usize = 640;
        const HEIGHT: usize = 480;
        const BYTES_PER_ROW: usize = WIDTH / 8;
        let y = line as usize;

        if self.framebuffer.len() != WIDTH * HEIGHT {
            self.framebuffer = vec![0u32; WIDTH * HEIGHT];
            self.fb_width = WIDTH;
            self.fb_height = HEIGHT;
        }

        if y >= HEIGHT || !self.raster_output {
            return;
        }

        // Rebuild 16-color palette LUT if dirty
        self.rebuild_palette16_lut_if_dirty();

        // Use the frame-latched CRTC start address and panning.
        let pixel_panning = self.latched_pixel_panning;
        let line_compare = self.get_line_compare() as usize;
        let split_active = line_compare < HEIGHT;

        let start_addr = self.latched_start_byte_for_mode() % VGA_PLANE_SIZE;

        let p0 = memory.plane(0);

        // Use VGA palette pipeline (same as full-frame renderer)
        let color_off = self.palette_to_rgb(0);
        let color_on = self.palette_to_rgb(1);

        let (row_base, panning) = if split_active && y >= line_compare {
            ((y - line_compare) * BYTES_PER_ROW, 0)
        } else {
            (start_addr + y * BYTES_PER_ROW, pixel_panning)
        };
        let row_base = row_base % VGA_PLANE_SIZE;

        for x_byte in 0..BYTES_PER_ROW {
            let offset = (row_base + x_byte) % VGA_PLANE_SIZE;
            let byte = p0[offset];

            for bit in 0..8 {
                let mask = 0x80 >> bit;
                let pixel_on = (byte & mask) != 0;
                let color = if pixel_on { color_on } else { color_off };

                let src_x = x_byte * 8 + bit;
                let fb_x = (src_x + WIDTH - panning) % WIDTH;
                self.framebuffer[y * WIDTH + fb_x] = color;
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // End per-scanline rendering methods
    // ──────────────────────────────────────────────────────────────────────

    /// Convert palette index to RGB u32 (uses pre-computed LUT)
    #[inline]
    fn palette_to_rgb(&self, idx: u8) -> u32 {
        self.palette16_lut[(idx & 0x0F) as usize]
    }

    /// Convert CGA IRGB color index (0-15) to RGB, using the canonical
    /// CGA 16-color palette. This bypasses the VGA DAC/attribute chain
    /// which may not be initialized correctly for CGA graphics modes.
    #[cfg(test)]
    #[inline]
    fn cga_irgb_to_rgb(idx: u8) -> u32 {
        const CGA_PALETTE: [u32; 16] = [
            0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, // Black, Blue, Green, Cyan
            0xAA0000, 0xAA00AA, 0xAA5500, 0xAAAAAA, // Red, Magenta, Brown, LightGray
            0x555555, 0x5555FF, 0x55FF55,
            0x55FFFF, // DarkGray, LightBlue, LightGreen, LightCyan
            0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF, // LightRed, LightMagenta, Yellow, White
        ];
        CGA_PALETTE[(idx & 0x0F) as usize]
    }

    /// Get the overscan (border) color as RGB
    /// Attribute register 0x11 contains the overscan color index
    fn overscan_rgb(&self) -> u32 {
        let overscan_idx = self.attribute_regs.get(0x11).copied().unwrap_or(0) as usize;
        // Apply PEL mask and look up in DAC
        let masked_idx = overscan_idx & (self.pel_mask as usize);
        let (r, g, b) = self.dac[masked_idx];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }

    /// Get framebuffer for display (u32 0RGB format)
    pub fn framebuffer(&self) -> &[u32] {
        &self.framebuffer
    }

    /// Copy live framebuffer indices `[start, end)` into the completed frame.
    fn copy_to_completed_frame(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.completed_framebuffer[start..end].copy_from_slice(&self.framebuffer[start..end]);
    }

    /// Freeze the fully scanline-composed active buffer for host presentation.
    /// Swapping avoids a full-frame copy on every refresh; the old completed
    /// buffer becomes the construction buffer for the next frame.
    pub fn publish_completed_frame(&mut self) {
        let (display_aspect_width, display_aspect_height) = self.display_aspect_ratio();
        {
            std::mem::swap(&mut self.framebuffer, &mut self.completed_framebuffer);
        }
        self.completed_fb_width = self.fb_width;
        self.completed_fb_height = self.fb_height;
        self.completed_display_aspect_width = display_aspect_width;
        self.completed_display_aspect_height = display_aspect_height;
        self.framebuffer
            .resize(self.fb_width.saturating_mul(self.fb_height), 0);
    }

    /// Whether the scanline renderers produce pixels (see the field doc).
    pub fn raster_output(&self) -> bool {
        self.raster_output
    }

    pub fn set_raster_output(&mut self, enabled: bool) {
        self.raster_output = enabled;
    }

    /// Publish the whole live framebuffer as the completed frame by copy,
    /// leaving the construction buffer intact. For a frame rendered on demand
    /// outside the scanline chain (MDD-BUG-FLU-03410).
    pub fn publish_live_frame_copy(&mut self) {
        let len = self.framebuffer.len();
        self.completed_framebuffer.resize(len, 0);
        self.copy_to_completed_frame(0, len);
        let (display_aspect_width, display_aspect_height) = self.display_aspect_ratio();
        self.completed_fb_width = self.fb_width;
        self.completed_fb_height = self.fb_height;
        self.completed_display_aspect_width = display_aspect_width;
        self.completed_display_aspect_height = display_aspect_height;
    }

    pub fn completed_framebuffer(&self) -> &[u32] {
        &self.completed_framebuffer
    }

    pub fn completed_dimensions(&self) -> (usize, usize) {
        (self.completed_fb_width, self.completed_fb_height)
    }

    /// Physical display aspect of the frozen frame returned by
    /// `completed_framebuffer()`.
    pub fn completed_display_aspect_ratio(&self) -> (usize, usize) {
        (
            self.completed_display_aspect_width,
            self.completed_display_aspect_height,
        )
    }

    pub fn has_completed_frame(&self) -> bool {
        self.completed_fb_width > 0
            && self.completed_fb_height > 0
            && self.completed_framebuffer.len()
                >= self
                    .completed_fb_width
                    .saturating_mul(self.completed_fb_height)
    }

    /// Discard host-side pixels after restoring guest VGA state from a
    /// checkpoint. The framebuffer itself is not checkpointed, so retaining it
    /// would expose pixels from the future until the restored raster completes.
    pub fn reset_presentation_after_checkpoint_restore(&mut self) {
        // Rewrites both buffers outside the render phase.
        self.ensure_framebuffer_for_mode();
        let len = self.fb_width.saturating_mul(self.fb_height);
        self.framebuffer.resize(len, 0);
        self.framebuffer.fill(0);
        self.completed_framebuffer.resize(len, 0);
        self.completed_framebuffer.fill(0);
        self.completed_fb_width = self.fb_width;
        self.completed_fb_height = self.fb_height;
        let (display_aspect_width, display_aspect_height) = self.display_aspect_ratio();
        self.completed_display_aspect_width = display_aspect_width;
        self.completed_display_aspect_height = display_aspect_height;
        self.last_rendered_line = -1;
    }

    /// Get framebuffer dimensions
    pub fn dimensions(&self) -> (usize, usize) {
        (self.fb_width, self.fb_height)
    }

    pub fn display_aspect_ratio(&self) -> (usize, usize) {
        {
            (self.fb_width.max(1), self.fb_height.max(1))
        }
    }

    pub fn is_text_mode(&self) -> bool {
        (self.attribute_regs[0x10] & 0x01) == 0 && !self.is_256_color_mode()
    }

    /// Register-derived: text mode + Misc Output bit 0 == 0 (mono I/O range).
    fn is_mono_text_mode(&self) -> bool {
        self.is_text_mode() && (self.misc_output & 0x01) == 0
    }

    /// Get font bank offset in plane 2 from sequencer Character Map Select register
    /// Returns (bank_a_offset, bank_b_offset) for the two possible font selections
    ///
    /// Font bank A is used when attribute bit 3 = 1
    /// Font bank B is used when attribute bit 3 = 0
    pub fn get_font_bank_offsets(&self) -> (usize, usize) {
        let seq3 = self.sequencer_regs[3];
        // Bank A (attribute bit 3 = 1): bits 5,3,2
        let bank_a = ((seq3 >> 3) & 0x04) | ((seq3 >> 2) & 0x03);
        // Bank B (attribute bit 3 = 0): bits 4,1,0
        let bank_b = ((seq3 >> 2) & 0x04) | (seq3 & 0x03);
        // Selector IDs interleave the two 8KB banks in each 16KB region.
        (font_bank_offset(bank_a), font_bank_offset(bank_b))
    }

    /// Check if 512-character mode is active (dual font banks)
    /// 512-char mode is enabled when bank A and bank B point to different locations
    pub fn is_512_char_mode(&self) -> bool {
        let seq3 = self.sequencer_regs[3];
        let bank_a = ((seq3 >> 3) & 0x04) | ((seq3 >> 2) & 0x03);
        let bank_b = ((seq3 >> 2) & 0x04) | (seq3 & 0x03);
        bank_a != bank_b
    }

    /// Get a font row for rendering, using custom font from plane 2 if loaded
    /// Falls back to built-in CP437 font
    ///
    /// VGA custom fonts are stored in plane 2 at FONT_CHAR_SIZE-byte intervals per character.
    /// When `custom_font_loaded` is true, this method reads from plane 2 directly;
    /// otherwise it returns the built-in CP437 font. The flag is a one-way latch —
    /// set by HLE AH=11h or by observation of a guest plane-2 font upload, and
    /// never auto-cleared.
    ///
    /// The `attr` parameter is used for 512-character mode: when bit 3 is set and
    /// dual font banks are configured, bank A is used instead of bank B.
    pub fn get_font_row(
        &self,
        memory: &Memory,
        ch: u8,
        row: usize,
        char_height: usize,
        attr: u8,
    ) -> u8 {
        if self.custom_font_loaded {
            let (bank_a, bank_b) = self.get_font_bank_offsets();
            // Attribute bit 3 selects map A when set, map B when clear.
            let bank_offset = if (attr & 0x08) != 0 { bank_a } else { bank_b };
            let font_offset = bank_offset + (ch as usize * FONT_CHAR_SIZE) + row;
            let plane2 = memory.plane(2);
            if let Some(&byte) = plane2.get(font_offset) {
                return byte;
            }
            // Offset out of bounds — fall through to the embedded font.
        }
        // Fall back to built-in font
        if char_height >= 16 {
            CP437_FONT_8X16
                .get(ch as usize * 16 + row.min(15))
                .copied()
                .unwrap_or(0)
        } else {
            let src_row = (row.saturating_mul(8) / char_height.max(1)).min(7);
            CP437_FONT_8X8
                .get(ch as usize * 8 + src_row)
                .copied()
                .unwrap_or(0)
        }
    }

    pub fn is_mode_x(&self) -> bool {
        self.is_256_color_mode()
            && !self.is_chain4()
            && ((self.crtc_regs[0x09] & 0x1F) == 0 || (self.crtc_regs[0x17] & 0x40) != 0)
    }

    /// Check for Mode-13h-derived unchained 16-color planar output.
    ///
    /// This setup clears chain-4 like Mode X, but leaves the
    /// Attribute Controller in 16-color shift mode. Treat it as a sibling of
    /// Mode X only after the same CRTC timing markers are present, so transient
    /// chain-4 toggles during Mode 13h initialization stay on the linear path.
    fn is_unchained_16color_planar(&self) -> bool {
        self.mode() == 0x13
            && !self.is_256_color_mode()
            && !self.is_chain4()
            && ((self.crtc_regs[0x09] & 0x1F) == 0 || (self.crtc_regs[0x17] & 0x40) != 0)
    }

    /// Check if chain-4 is enabled (sequencer memory mode bit 3)
    pub fn is_chain4(&self) -> bool {
        (self.sequencer_regs[4] & 0x08) != 0
    }

    /// Get the current plane mask from sequencer Map Mask register (index 2)
    pub fn get_plane_mask(&self) -> u8 {
        self.sequencer_regs[2] & 0x0F
    }

    /// Get the current read plane from graphics Read Map Select register (index 4)
    pub fn get_read_plane(&self) -> u8 {
        self.graphics_regs[4] & 0x03
    }

    /// Get VGA Graphics Controller memory map select (GC index 6 bits 2-3).
    pub fn get_memory_map_select(&self) -> u8 {
        (self.graphics_regs[6] >> 2) & 0x03
    }

    /// Get the write mode from Graphics Mode Register (index 5) bits 0-1
    pub fn get_write_mode(&self) -> u8 {
        self.graphics_regs[5] & 0x03
    }

    /// Get the Bit Mask Register (index 8)
    pub fn get_bit_mask(&self) -> u8 {
        self.graphics_regs[8]
    }

    /// Get the Set/Reset Register (index 0)
    pub fn get_set_reset(&self) -> u8 {
        self.graphics_regs[0] & 0x0F
    }

    /// Get the Enable Set/Reset Register (index 1)
    pub fn get_enable_set_reset(&self) -> u8 {
        self.graphics_regs[1] & 0x0F
    }

    /// Get the Data Rotate Register (index 3)
    /// Bits 0-2: rotate count, Bits 3-4: logical operation
    pub fn get_data_rotate(&self) -> u8 {
        self.graphics_regs[3]
    }

    /// Get the Read Mode from Graphics Mode Register (index 5) bit 3
    pub fn get_read_mode(&self) -> u8 {
        (self.graphics_regs[5] >> 3) & 0x01
    }

    /// Get the Color Compare Register (index 2)
    pub fn get_color_compare(&self) -> u8 {
        self.graphics_regs[2] & 0x0F
    }

    /// Get the Color Don't Care Register (index 7)
    pub fn get_color_dont_care(&self) -> u8 {
        self.graphics_regs[7] & 0x0F
    }

    /// Build a snapshot of the current VGA pipeline state.
    /// Used by IoBus::take_vga_pipeline_state() and direct callers (int10, checkpoint restore).
    pub fn build_pipeline_state(&self) -> VgaPipelineState {
        VgaPipelineState {
            video_mode: self.mode(),
            chain4: self.is_chain4(),
            seq_odd_even_disabled: self.sequencer_regs[0x04] & 0x04 != 0,
            gc_odd_even_read: self.graphics_regs[0x05] & 0x10 != 0,
            // Strict Mode-X/Y predicate (requires CR9 max-scan or CR17 byte
            // addressing in addition to 256-colour + chain-4-disabled).
            // Memory uses this to avoid mistreating a transient
            // `(0x13, !chain4)` state during Mode 13h init as Mode X.
            is_mode_x: self.is_mode_x(),
            plane_mask: self.get_plane_mask(),
            read_plane: self.get_read_plane(),
            memory_map_select: self.get_memory_map_select(),
            gc_graphics_mode: (self.graphics_regs[0x06] & 0x01) != 0,
            write_mode: self.get_write_mode(),
            bit_mask: self.get_bit_mask(),
            set_reset: self.get_set_reset(),
            enable_set_reset: self.get_enable_set_reset(),
            data_rotate: self.get_data_rotate(),
            read_mode: self.get_read_mode(),
            color_compare: self.get_color_compare(),
            color_dont_care: self.get_color_dont_care(),
        }
    }

    /// Returns true if VGA pipeline state changed since last flush.
    pub fn pipeline_dirty(&self) -> bool {
        self.pipeline_dirty
    }

    /// Mark the VGA pipeline state as dirty (changed).
    pub fn mark_pipeline_dirty(&mut self) {
        self.pipeline_dirty = true;
    }

    /// Clear the pipeline dirty flag.
    pub fn clear_pipeline_dirty(&mut self) {
        self.pipeline_dirty = false;
    }

    /// Get horizontal pixel panning value from Attribute Controller register 13h
    /// Returns 0-7 for text modes (9-dot), 0-7 for 8-dot modes
    pub fn get_pixel_panning(&self) -> u8 {
        self.attribute_regs[0x13] & 0x0F
    }

    /// Check if 256-color mode is active (AC mode control bit 6 = 8-bit color enable)
    pub fn is_256_color_mode(&self) -> bool {
        (self.attribute_regs[0x10] & 0x40) != 0
    }

    /// Get effective pixel panning in pixels for the current mode.
    /// In 256-color mode, the register value is divided by 2 per VGA hardware spec
    /// (only even values 0,2,4,6 are valid, mapping to 0,1,2,3 pixel shifts).
    /// In 9-dot text mode, AR13 uses a special encoding: 0..7 → shift 1..8,
    /// 8 → shift 0 (normal). The BIOS mode 3 default is AR13 = 8, so
    /// without this remap the first column is panned
    /// off the left and wraps to the right.
    /// In other modes, the raw register value is the pixel count.
    fn effective_pixel_panning(&self) -> usize {
        let raw = (self.attribute_regs[0x13] & 0x0F) as usize;
        if self.is_256_color_mode() {
            raw / 2
        } else if self.is_text_mode() && (self.sequencer_regs[0x01] & 0x01) == 0 {
            if raw == 8 {
                0
            } else {
                raw + 1
            }
        } else {
            raw
        }
    }

    /// Get byte panning value from CRTC register 0x08 bits 5-6.
    /// Returns 0-3, the number of bytes to add to the start address.
    pub fn get_byte_panning(&self) -> usize {
        ((self.crtc_regs[0x08] >> 5) & 0x03) as usize
    }

    /// Get the line compare value from CRTC registers for split screen
    /// Returns the scanline at which the display address resets to 0
    pub fn get_line_compare(&self) -> u16 {
        let lc_low = self.crtc_regs[0x18] as u16;
        let overflow = self.crtc_regs[0x07];
        let max_scan = self.crtc_regs[0x09];
        // Bit 8 is in overflow register bit 4
        let lc_bit8 = ((overflow >> 4) & 1) as u16;
        // Bit 9 is in max scan line register bit 6
        let lc_bit9 = ((max_scan >> 6) & 1) as u16;
        let lc_bit10 = { 0 };
        lc_low | (lc_bit8 << 8) | (lc_bit9 << 9) | (lc_bit10 << 10)
    }

    /// Compute unchained mode width from CRTC Horizontal Display End register.
    /// CRTC[0x01] gives character clocks - 1.
    /// In 256-color Mode X: each character clock = 4 pixels.
    /// In 16-color unchained: each character clock = 8 pixels (same as standard planar).
    fn mode_x_width(&self) -> usize {
        let pixels_per_clock = if self.is_256_color_mode() { 4 } else { 8 };
        (self.crtc_regs[0x01] as usize + 1) * pixels_per_clock
    }

    /// Get the vertical display end value from CRTC registers.
    /// Returns the last visible scanline (VDE), so visible lines = VDE + 1.
    pub fn get_vertical_display_end(&self) -> u16 {
        let vde_low = self.crtc_regs[0x12] as u16;
        let overflow = self.crtc_regs[0x07];
        let vde_bit8 = ((overflow >> 1) & 1) as u16;
        let vde_bit9 = ((overflow >> 6) & 1) as u16;
        let vde_bit10 = { 0 };
        vde_low | (vde_bit8 << 8) | (vde_bit9 << 9) | (vde_bit10 << 10)
    }

    /// Compute Mode X visible height from CRTC VDE and Max Scan Line registers.
    /// VDE counts physical scanlines; divide by (max_scan_line + 1) to get
    /// logical content lines.  This correctly handles double-scan modes where
    /// each logical line occupies two physical scanlines (e.g., Mode 13h has
    /// VDE=399 and max_scan_line=1, giving 200 logical lines).
    fn mode_x_visible_height(&self) -> usize {
        let vde = self.get_vertical_display_end() as usize;
        let max_scan = (self.crtc_regs[0x09] & 0x1F) as usize; // bits 4:0
        let physical_lines = vde + 1;
        let divisor = max_scan + 1;
        physical_lines / divisor
    }

    fn compute_bytes_per_row(&self, width: usize, mode_x: bool) -> usize {
        let crtc_offset = self.crtc_regs[0x13] as usize;
        if crtc_offset != 0 {
            // Full K-aware stride expression per VGA spec.
            // K depends on addressing mode set by CRTC 0x14 (DW) and 0x17 (byte).
            let dword_mode = (self.crtc_regs[0x14] & 0x40) != 0;
            if dword_mode {
                crtc_offset * 8 // Doubleword addressing (K=8)
            } else if self.crtc_byte_mode() {
                crtc_offset * 2 // Byte addressing (K=2)
            } else {
                crtc_offset * 4 // Word addressing (K=4)
            }
        } else if mode_x {
            width / 4
        } else {
            width / 8
        }
    }

    fn crtc_byte_mode(&self) -> bool {
        self.crtc_regs
            .get(0x17)
            .map(|val| (val & 0x40) != 0)
            .unwrap_or(false)
    }

    fn crtc_write_protected(&self, index: u8) -> bool {
        (self.crtc_regs[0x11] & 0x80) != 0 && index <= 0x07
    }

    fn write_crtc_data(&mut self, index: u8, value: u8) -> bool {
        let idx = index as usize;
        if idx >= self.crtc_regs.len() {
            return false;
        }

        let prev = self.crtc_regs[idx];
        let next = if self.crtc_write_protected(index) {
            if index == 0x07 {
                // VGA allows line-compare bit 8 (overflow bit 4) to be updated
                // even when CRTC 0x00..0x07 are protected by CR11 bit 7.
                (prev & !0x10) | (value & 0x10)
            } else {
                prev
            }
        } else {
            value
        };

        // CR11 (Vertical Retrace End): bit 4 controls vertical interrupt
        // acknowledge. Writing bit 4 = 0 clears the pending vertical interrupt.
        // Programs typically write CR11 with bit 4 = 0, then bit 4 = 1.
        if idx == 0x11 && (next & 0x10) == 0 {
            self.vretrace_irq_pending = false;
        }

        if next != prev {
            self.crtc_regs[idx] = next;

            // CRTC 0x07 (Overflow) and CRTC 0x12 (Vertical Display End) feed
            // get_vertical_display_end(), which feeds mode().
            // CRTC 0x09 (Maximum Scan Line) and CRTC 0x17 (CRTC Mode Control)
            // feed is_mode_x() — without them, Memory's planar dispatch can
            // misclassify a Mode-X transition as a mode-set transient.
            // All of them gate Memory's mode classification, so the pipeline
            // must re-sync.
            if idx == 0x07 || idx == 0x09 || idx == 0x12 || idx == 0x17 {
                self.pipeline_dirty = true;
            }

            // Recompute frame timing when CRTC registers affecting refresh rate
            // or VSync pulse timing change:
            // CR00 = Horizontal Total, CR06 = Vertical Total,
            // CR07 = Overflow (vtotal/vrstart/vdispend overflow bits),
            // CR10 = Vertical Retrace Start, CR11 = Vertical Retrace End,
            // CR12 = Vertical Display End
            if idx == 0x00
                || idx == 0x06
                || idx == 0x07
                || idx == 0x09
                || idx == 0x10
                || idx == 0x11
                || idx == 0x12
                || idx == 0x15
                || idx == 0x16
            {
                self.recompute_timing();
            }

            true
        } else {
            // Even if the value didn't change, CR11 bit 4 = 0 still clears
            // the vertical interrupt (already handled above).
            false
        }
    }

    fn crtc_start_byte_for_mode(&self, start_word: usize) -> usize {
        let byte_panning = self.get_byte_panning();
        if self.is_text_mode() {
            start_word * 2 + byte_panning
        } else {
            // The graphics start address and byte panning select the scanout origin.
            start_word + byte_panning
        }
    }

    fn render_palette16_rgb(&self, idx: u8) -> u32 {
        self.palette_to_rgb(idx)
    }

    fn render_pixel_panning(&self) -> usize {
        self.effective_pixel_panning()
    }

    fn render_line_compare(&self) -> usize {
        self.get_line_compare() as usize
    }

    fn render_start_byte(&self) -> usize {
        let start_word = ((self.crtc_regs[0x0C] as usize) << 8) | (self.crtc_regs[0x0D] as usize);
        self.crtc_start_byte_for_mode(start_word)
    }

    fn render_start_word(&self) -> usize {
        ((self.crtc_regs[0x0C] as usize) << 8) | (self.crtc_regs[0x0D] as usize)
    }

    /// Return the latched CRTC start byte for per-scanline rendering.
    /// Uses the values captured at vertical-retrace start.
    fn latched_start_byte_for_mode(&self) -> usize {
        let byte_panning = self.latched_byte_panning;
        if self.is_text_mode() {
            self.latched_start_word * 2 + byte_panning
        } else {
            self.latched_start_word + byte_panning
        }
    }

    fn render_bytes_per_row(&self, width: usize, mode_x: bool) -> usize {
        self.compute_bytes_per_row(width, mode_x)
    }

    /// BIOS save/restore helpers for VGA state.
    pub fn get_attribute_index(&self) -> u8 {
        self.attribute_index & 0x1F
    }

    pub fn set_attribute_index(&mut self, index: u8) {
        self.attribute_index = index & 0x1F;
    }

    pub fn reset_attribute_flip_flop(&mut self) {
        self.attribute_flip_flop = false;
    }

    /// Get palette registers (from attribute_regs[0..16])
    pub fn get_palette_regs(&self) -> [u8; 16] {
        let mut result = [0u8; 16];
        result.copy_from_slice(&self.attribute_regs[..16]);
        result
    }

    /// Set palette registers (writes to attribute_regs[0..16])
    pub fn set_palette_regs(&mut self, regs: [u8; 16]) {
        self.attribute_regs[..16].copy_from_slice(&regs);
        self.palette16_lut_dirty = true;
    }

    /// Get scanlines per frame based on current video mode.
    ///
    /// Uses `self.mode()` which is register-derived, so it works correctly
    /// under both HLE INT 10h and `--real-boot` (direct register programming).
    ///
    /// The 256-colour arm distinguishes Mode 13h (chain-4, fixed 200) from
    /// Mode X (chain-4 disabled; CRTC VDE programmed to 200 / 240 / 480).
    pub fn get_scanlines_per_frame(&self) -> u64 {
        match self.mode() {
            // 200-line modes (CGA, EGA 320×200 / 640×200)
            0x04 | 0x05 | 0x06 | 0x0D | 0x0E => 200,
            // 256-colour: Mode 13h (chain-4, 200 lines) vs Mode X (CRTC-derived).
            0x13 => {
                if self.is_mode_x() {
                    // Mode X programs CRTC VDE to 200/240/480 — use register.
                    self.mode_x_visible_height() as u64
                } else {
                    200
                }
            }
            // 350-line modes (EGA)
            0x0F | 0x10 => 350,
            // 480-line modes (VGA)
            0x11 | 0x12 => 480,
            // Text modes and default
            _ => 400,
        }
    }

    /// Get the total number of scanlines including vblank period,
    /// read directly from CRTC registers.
    pub fn get_total_scanlines(&self) -> u64 {
        self.crtc_vtotal() as u64
    }

    fn scanline_for_position(&self, position: u64) -> u64 {
        let visible_scanlines = self.get_scanlines_per_frame();
        let frame_cycles = self.timing_cycles_per_frame.max(1);
        let retrace_cycles = self
            .timing_retrace_cycles
            .min(frame_cycles.saturating_sub(1));

        if visible_scanlines == 0 {
            return 0;
        }

        // Keep active display start aligned with VSYNC timing:
        // - bit3 at 0x3DA is driven by `timing_retrace_cycles`
        // - scanline 0 should begin exactly when retrace ends.
        if position < retrace_cycles {
            let total_scanlines = self.get_total_scanlines();
            let vblank_scanlines = total_scanlines.saturating_sub(visible_scanlines).max(1);
            let retrace_cycles_u128 = retrace_cycles.max(1) as u128;
            let vblank_pos = ((position as u128) * (vblank_scanlines as u128) / retrace_cycles_u128)
                .min((vblank_scanlines.saturating_sub(1)) as u128)
                as u64;
            return visible_scanlines + vblank_pos;
        }

        let active_cycles = frame_cycles.saturating_sub(retrace_cycles).max(1);
        let cycles_into_active = position.saturating_sub(retrace_cycles);
        let active_scanline = ((cycles_into_active as u128) * (visible_scanlines as u128)
            / (active_cycles as u128)) as u64;
        active_scanline.min(visible_scanlines.saturating_sub(1))
    }

    /// Get the current scanline based on timing cycles.
    /// Returns the scanline number (0 = first visible line, or negative for vblank).
    /// During vblank (at frame start), returns a value >= visible scanlines.
    pub fn get_current_scanline(&self) -> u64 {
        self.scanline_for_position(self.cycles_since_vblank)
    }

    /// Check if the current scanline is in the active display region (not vblank).
    pub fn is_in_active_display(&self) -> bool {
        let visible = self.get_scanlines_per_frame();
        self.get_current_scanline() < visible
    }

    /// Check if currently in vertical blanking interval
    pub fn is_in_vblank(&self) -> bool {
        self.cycles_since_vblank < self.timing_retrace_cycles
    }

    /// Read the character-map selection shared by BIOS and VGA port writes.
    pub fn get_font_block_spec(&self) -> u8 {
        self.sequencer_regs[3]
    }

    pub fn set_font_block_spec(&mut self, spec: u8) {
        self.sequencer_regs[3] = spec;
        self.mark_pipeline_dirty();
    }

    pub fn set_ril_defaults(&mut self, group: u8, values: Vec<u8>) {
        let idx = group as usize;
        if idx < self.ril_defaults.len() {
            self.ril_defaults[idx] = Some(values);
        }
    }

    pub fn get_ril_defaults(&self, group: u8) -> Option<&[u8]> {
        let idx = group as usize;
        if idx < self.ril_defaults.len() {
            self.ril_defaults[idx].as_deref()
        } else {
            None
        }
    }

    pub fn get_attribute_regs(&self) -> [u8; 0x20] {
        self.attribute_regs
    }

    pub fn set_attribute_regs(&mut self, regs: [u8; 0x20]) {
        self.attribute_regs = regs;
        self.pipeline_dirty = true;
        self.palette_lut_dirty = true;
        // Mark 16-color palette LUT dirty since palette/attribute regs changed
        // (palette regs are stored in attribute_regs[0..16])
        self.palette16_lut_dirty = true;
        let mode = self.attribute_regs[0x10];
        let overscan = self.attribute_regs[0x11];
        let color_plane = self.attribute_regs[0x12];
        let hpel = self.attribute_regs[0x13];
        let color_select = self.attribute_regs[0x14];
        vga_reg_trace_emit(format_args!(
            "SET ATTR regs: mode=0x{mode:02x} overscan=0x{overscan:02x} \
             plane=0x{color_plane:02x} hpel=0x{hpel:02x} sel=0x{color_select:02x}"
        ));
    }

    pub fn get_sequencer_index(&self) -> u8 {
        self.sequencer_index
    }

    pub fn set_sequencer_index(&mut self, index: u8) {
        self.sequencer_index = index;
    }

    pub fn get_sequencer_regs(&self) -> [u8; 8] {
        self.sequencer_regs
    }

    pub fn set_sequencer_regs(&mut self, regs: [u8; 8]) {
        self.sequencer_regs = regs;
        self.pipeline_dirty = true;
        self.recompute_timing();
        let reset = self.sequencer_regs[0x00];
        let clocking = self.sequencer_regs[0x01];
        let map_mask = self.sequencer_regs[0x02];
        let mem_mode = self.sequencer_regs[0x04];
        vga_reg_trace_emit(format_args!(
            "SET SEQ regs: reset=0x{reset:02x} clock=0x{clocking:02x} \
             mask=0x{map_mask:02x} mem=0x{mem_mode:02x}"
        ));
    }

    pub fn get_graphics_index(&self) -> u8 {
        self.graphics_index
    }

    pub fn set_graphics_index(&mut self, index: u8) {
        self.graphics_index = index;
    }

    pub fn get_graphics_regs(&self) -> [u8; 16] {
        self.graphics_regs
    }

    pub fn set_graphics_regs(&mut self, regs: [u8; 16]) {
        self.graphics_regs = regs;
        self.pipeline_dirty = true;
        let set_reset = self.graphics_regs[0x00];
        let enable_set_reset = self.graphics_regs[0x01];
        let read_map = self.graphics_regs[0x04];
        let mode = self.graphics_regs[0x05];
        let misc = self.graphics_regs[0x06];
        let bitmask = self.graphics_regs[0x08];
        vga_reg_trace_emit(format_args!(
            "SET GFX regs: sr=0x{set_reset:02x} esr=0x{enable_set_reset:02x} \
             rmap=0x{read_map:02x} mode=0x{mode:02x} misc=0x{misc:02x} \
             mask=0x{bitmask:02x}"
        ));
    }

    pub fn get_crtc_index(&self) -> u8 {
        self.crtc_index
    }

    pub fn set_crtc_index(&mut self, index: u8) {
        self.crtc_index = index;
    }

    pub fn get_crtc_regs(&self) -> [u8; 32] {
        self.crtc_regs
    }

    pub fn set_crtc_regs(&mut self, regs: [u8; 32]) {
        self.crtc_regs = regs;
        self.pipeline_dirty = true;
        self.recompute_timing();
        let vde = self.get_vertical_display_end();
        let overflow = self.crtc_regs[0x07];
        let max_scan = self.crtc_regs[0x09];
        let line_compare = self.crtc_regs[0x18];
        vga_reg_trace_emit(format_args!(
            "SET CRTC regs: vde={vde} overflow=0x{overflow:02x} \
             max_scan=0x{max_scan:02x} line_cmp=0x{line_compare:02x}"
        ));
    }

    pub fn get_misc_output(&self) -> u8 {
        self.misc_output
    }

    pub fn set_misc_output(&mut self, value: u8) {
        self.misc_output = value;
        self.recompute_timing();
    }

    pub fn get_feature_control(&self) -> u8 {
        self.feature_control
    }

    pub fn set_feature_control(&mut self, value: u8) {
        self.feature_control = value;
    }

    pub fn get_pel_mask(&self) -> u8 {
        self.pel_mask
    }

    pub fn set_pel_mask(&mut self, value: u8) {
        self.pel_mask = value;
    }

    /// Snapshot the full 256-entry DAC palette as raw RGB bytes (768 bytes).
    /// Observation-only; used by determinism/parity trail hashing.
    pub fn dac_bytes(&self) -> [u8; 768] {
        let mut out = [0u8; 768];
        for (i, &(r, g, b)) in self.dac.iter().enumerate() {
            out[i * 3] = r;
            out[i * 3 + 1] = g;
            out[i * 3 + 2] = b;
        }
        out
    }

    pub fn get_dac_state(&self) -> u8 {
        self.dac_state
    }

    pub fn set_dac_state(&mut self, value: u8) {
        // VGA spec: 0x00 = write mode, 0x03 = read mode
        self.dac_state = value & 0x03;
    }

    pub fn get_dac_read_index(&self) -> u8 {
        self.dac_read_index
    }

    pub fn set_dac_read_index(&mut self, value: u8) {
        self.dac_read_index = value;
    }

    pub fn get_dac_write_index(&self) -> u8 {
        self.dac_write_index
    }

    pub fn set_dac_write_index(&mut self, value: u8) {
        self.dac_write_index = value;
    }

    pub fn get_dac_component(&self) -> u8 {
        self.dac_component
    }

    pub fn set_dac_component(&mut self, value: u8) {
        self.dac_component = value % 3;
    }

    pub fn get_dac_entries(&self) -> &[(u8, u8, u8); 256] {
        &self.dac
    }

    pub fn set_dac_entries(&mut self, entries: &[(u8, u8, u8); 256]) {
        self.dac = *entries;
    }

    pub fn debug_cycles_since_vblank(&self) -> u64 {
        self.cycles_since_vblank
    }

    pub fn debug_palette_address_source(&self) -> bool {
        self.palette_address_source
    }

    // ── Checkpoint save/restore accessors ────────────────────────────────

    /// Get attribute flip-flop state (for checkpoint).
    pub fn get_attribute_flip_flop(&self) -> bool {
        self.attribute_flip_flop
    }

    /// Set attribute flip-flop state (checkpoint restore).
    pub fn set_attribute_flip_flop(&mut self, state: bool) {
        self.attribute_flip_flop = state;
    }

    /// Get palette address source (PAS) bit (for checkpoint).
    pub fn get_palette_address_source(&self) -> bool {
        self.palette_address_source
    }

    /// Set palette address source (PAS) bit (checkpoint restore).
    pub fn set_palette_address_source(&mut self, state: bool) {
        self.palette_address_source = state;
    }

    /// Get Mode X height (for checkpoint). Register-derived from CRTC
    /// VDE/MaxScanLine via [`mode_x_visible_height`], clamped to 480.
    pub fn get_mode_x_height(&self) -> usize {
        self.mode_x_visible_height().min(480)
    }

    /// Get text mode rows (for checkpoint).
    pub fn get_rows(&self) -> u8 {
        self.rows
    }

    pub fn get_cga_mode_control(&self) -> u8 {
        self.cga_mode_control_reg
    }

    pub fn set_cga_mode_control(&mut self, value: u8) {
        self.cga_mode_control_reg = value;
    }

    pub fn is_planar_16color_mode(&self) -> bool {
        (self.attribute_regs[0x10] & 0x01) != 0
            && !self.is_256_color_mode()
            && !matches!(self.mode(), 0x04..=0x06 | 0x08..=0x0A)
    }

    // Note: write_planar_memory and read_planar_memory removed.
    // Planar memory operations are now handled by Memory::write_u8/read_u8
    // which intercept video memory accesses in planar modes.

    fn text_memory_aperture_base(&self) -> usize {
        if self.is_mono_text_mode() {
            0xB0000
        } else {
            0xB8000
        }
    }

    fn text_memory_base(&self) -> usize {
        // Honor the CRTC start address (0x0C/0x0D) so page flipping / smooth scrolling works
        // even when programs manipulate the display start directly.
        let start_words = ((self.crtc_regs[0x0C] as usize) << 8) | (self.crtc_regs[0x0D] as usize);
        self.text_memory_aperture_base() + start_words * 2
    }

    /// Read the entire VGA text-mode screen as a plain-text string.
    ///
    /// Returns one line per row, trailing spaces trimmed, joined with `\n`.
    /// Returns `Err` if the VGA is not in a text mode.
    pub fn read_text_screen(&self, memory: &Memory) -> Result<String, &'static str> {
        if !self.is_text_mode() {
            return Err("not in text mode");
        }
        let base = self.text_memory_base();
        let cols = self.columns as usize;
        let rows = self.rows as usize;
        let mut out = String::with_capacity(cols * rows + rows);
        for row in 0..rows {
            let mut line = String::with_capacity(cols);
            for col in 0..cols {
                let addr = base + (row * cols + col) * 2;
                let ch = memory.read_u8(addr);
                line.push(if (0x20..0x7F).contains(&ch) {
                    ch as char
                } else {
                    ' '
                });
            }
            let trimmed = line.trim_end();
            out.push_str(trimmed);
            if row + 1 < rows {
                out.push('\n');
            }
        }
        Ok(out)
    }

    /// Read a single row from the VGA text-mode screen (0-indexed).
    ///
    /// Returns the row text with trailing spaces trimmed.
    /// Returns `Err` if not in text mode or if the row is out of range.
    pub fn read_text_row(&self, row: u8, memory: &Memory) -> Result<String, &'static str> {
        if !self.is_text_mode() {
            return Err("not in text mode");
        }
        if row >= self.rows {
            return Err("row out of range");
        }
        let base = self.text_memory_base();
        let cols = self.columns as usize;
        let row_usize = row as usize;
        let mut line = String::with_capacity(cols);
        for col in 0..cols {
            let addr = base + (row_usize * cols + col) * 2;
            let ch = memory.read_u8(addr);
            line.push(if (0x20..0x7F).contains(&ch) {
                ch as char
            } else {
                ' '
            });
        }
        Ok(line.trim_end().to_string())
    }

    fn text_page_base(&self, page: u8) -> usize {
        let base = if self.is_mono_text_mode() {
            0xB0000
        } else {
            0xB8000
        };
        base + self.normalize_cursor_page(page) * self.text_page_size_words() * 2
    }

    /// VGA BIOS cursor storage has eight slots. Fall back to page zero
    /// when BH selects a page outside that table; keeping the
    /// normalization here prevents any character helper from addressing beyond
    /// the 32 KiB text aperture.
    pub fn normalize_cursor_page(&self, page: u8) -> usize {
        let page = page as usize;
        let page_count = if self.is_text_mode() {
            (0x8000 / (self.text_page_size_words() * 2))
                .min(self.cursor_pos.len())
                .max(1)
        } else {
            self.cursor_pos.len()
        };
        if page < page_count {
            page
        } else {
            0
        }
    }

    fn text_page_size_words(&self) -> usize {
        let visible_words = (self.columns as usize) * (self.rows as usize);
        if self.is_text_mode() {
            // Text pages occupy whole 2 KiB allocations, including font layouts.
            visible_words.div_ceil(0x400) * 0x400
        } else {
            visible_words
        }
    }

    /// Read a standard VGA register or the retrace status.
    pub fn read_port(&mut self, port: u16) -> u8 {
        match port {
            0x3c0 => self.attribute_index | if self.palette_address_source { 0x20 } else { 0 },
            0x3c1 => self.attribute_regs[(self.attribute_index & 31) as usize],
            0x3c2 => {
                (if self.misc_output & 12 == 0 { 16 } else { 0 })
                    | if self.vretrace_irq_pending { 128 } else { 0 }
            }
            0x3c4 => self.sequencer_index,
            0x3c5 => self
                .sequencer_regs
                .get(self.sequencer_index as usize)
                .copied()
                .unwrap_or(0),
            0x3c6 => self.pel_mask,
            0x3c7 => self.dac_state,
            0x3c8 => self.dac_write_index,
            0x3c9 => {
                let rgb = self.dac[self.dac_read_index as usize];
                let value = match self.dac_component {
                    0 => rgb.0,
                    1 => rgb.1,
                    _ => rgb.2,
                } >> 2;
                self.dac_component = (self.dac_component + 1) % 3;
                if self.dac_component == 0 {
                    self.dac_read_index = self.dac_read_index.wrapping_add(1);
                }
                value
            }
            0x3d8 => self.cga_mode_control_reg,
            0x3d9 => self.color_select_reg,
            0x3ca => self.feature_control,
            0x3cc => self.misc_output,
            0x3ce => self.graphics_index,
            0x3cf => self
                .graphics_regs
                .get(self.graphics_index as usize)
                .copied()
                .unwrap_or(0),
            0x3b4 | 0x3d4 => self.crtc_index,
            0x3b5 | 0x3d5 => self
                .crtc_regs
                .get(self.crtc_index as usize)
                .copied()
                .unwrap_or(0),
            0x3ba | 0x3da => {
                self.attribute_flip_flop = false;
                let position = self
                    .port_3da_override_position
                    .unwrap_or(self.cycles_since_vblank);
                let vblank = position < self.timing_retrace_cycles;
                let vsync = vblank
                    && position >= self.vsync_offset_cycles
                    && position < self.vsync_offset_cycles + self.vsync_duration_cycles;
                let cycles_per_line =
                    self.timing_cycles_per_frame / self.get_total_scanlines().max(1);
                let htotal = u64::from(self.crtc_regs[0]) + 5;
                let hdisplay = u64::from(self.crtc_regs[1]) + 1;
                let programmed_start = u64::from(self.crtc_regs[2]);
                let start = if programmed_start > hdisplay && programmed_start < htotal {
                    programmed_start
                } else {
                    hdisplay.min(htotal.saturating_sub(1))
                };
                let end_low = u64::from(self.crtc_regs[3] & 31)
                    | (u64::from((self.crtc_regs[5] >> 7) & 1) << 5);
                let mut end = (start & !63) | end_low;
                if end <= start {
                    end += 64;
                }
                let counter = position
                    .checked_rem(cycles_per_line)
                    .and_then(|phase| (phase * htotal).checked_div(cycles_per_line));
                let hblank = if let Some(counter) = counter {
                    let distance = (counter + htotal - start) % htotal;
                    distance < end.saturating_sub(start).min(htotal)
                } else {
                    false
                };
                u8::from(vblank || hblank) | if vsync { 8 } else { 0 }
            }
            _ => 0,
        }
    }
    /// Write a standard VGA register. Memory's pipeline must consume dirty state.
    pub fn write_port(&mut self, port: u16, value: u8) {
        match port {
            0x3c0 => {
                if self.attribute_flip_flop {
                    self.attribute_regs[(self.attribute_index & 31) as usize] = value;
                    self.palette_lut_dirty = true;
                    self.palette16_lut_dirty = true;
                    self.pipeline_dirty = true;
                } else {
                    self.attribute_index = value & 31;
                    self.palette_address_source = value & 32 != 0;
                }
                self.attribute_flip_flop = !self.attribute_flip_flop;
            }
            0x3c2 => {
                self.misc_output = value;
                self.recompute_timing();
            }
            0x3c4 => self.sequencer_index = value,
            0x3c5 => {
                if let Some(reg) = self.sequencer_regs.get_mut(self.sequencer_index as usize) {
                    *reg = value;
                    self.pipeline_dirty = true;
                    self.recompute_timing();
                    if self.is_mode_x() {
                        let height = self.mode_x_visible_height().min(480);
                        if self.fb_height != height {
                            self.fb_height = height;
                            self.framebuffer = vec![0; self.fb_width * self.fb_height];
                        }
                    }
                }
            }
            0x3c6 => {
                self.pel_mask = value;
                self.palette16_lut_dirty = true;
            }
            0x3c7 => {
                self.dac_read_index = value;
                self.dac_component = 0;
                self.dac_state = 3;
            }
            0x3c8 => {
                self.dac_write_index = value;
                self.dac_component = 0;
                self.dac_state = 0;
            }
            0x3c9 => {
                let value = value & 63;
                let value = (value << 2) | (value >> 4);
                let rgb = &mut self.dac[self.dac_write_index as usize];
                match self.dac_component {
                    0 => rgb.0 = value,
                    1 => rgb.1 = value,
                    _ => rgb.2 = value,
                }
                self.dac_component = (self.dac_component + 1) % 3;
                if self.dac_component == 0 {
                    self.dac_write_index = self.dac_write_index.wrapping_add(1);
                }
                self.palette_lut_dirty = true;
                self.palette16_lut_dirty = true;
            }
            0x3ce => self.graphics_index = value,
            0x3cf => {
                if let Some(reg) = self.graphics_regs.get_mut(self.graphics_index as usize) {
                    *reg = value;
                    self.pipeline_dirty = true;
                }
            }
            0x3b4 | 0x3d4 => self.crtc_index = value,
            0x3b5 | 0x3d5 => {
                if self.write_crtc_data(self.crtc_index, value)
                    && self.is_mode_x()
                    && matches!(self.crtc_index, 0x12 | 0x07 | 0x09)
                {
                    let height = self.mode_x_visible_height().min(480);
                    if self.fb_height != height {
                        self.fb_height = height;
                        self.framebuffer = vec![0; self.fb_width * self.fb_height];
                    }
                }
            }
            0x3d8 => {
                let current = self.mode();
                if matches!(current, 4..=6) {
                    self.cga_mode_control_reg = value;
                    let next = if value & 2 != 0 {
                        if value & 16 != 0 {
                            6
                        } else if value & 4 != 0 {
                            5
                        } else {
                            4
                        }
                    } else if value & 1 != 0 {
                        3
                    } else {
                        1
                    };
                    if next != current {
                        self.set_mode_internal(next, false);
                    }
                }
            }
            0x3d9 => self.set_cga_color_select(value),
            0x3ba | 0x3da => self.feature_control = value,
            _ => {}
        }
    }
    fn rebuild_palette_lut_if_dirty(&mut self) {
        if !self.palette_lut_dirty {
            return;
        }
        for i in 0..256 {
            let (r, g, b) = self.dac[i];
            self.palette_lut[i] = (r as u32) << 16 | (g as u32) << 8 | (b as u32);
        }
        self.palette_lut_dirty = false;
    }
    fn rebuild_palette16_lut_if_dirty(&mut self) {
        if !self.palette16_lut_dirty {
            return;
        }

        let reg10 = self.attribute_regs.get(0x10).copied().unwrap_or(0);
        let color_select = self.attribute_regs.get(0x14).copied().unwrap_or(0);

        for idx in 0..16 {
            // Palette regs are stored in attribute_regs[0..16]
            let palette_idx = self.attribute_regs[idx] as usize;
            let dac_index = if (reg10 & 0x80) != 0 {
                // PAS=1: High 4 bits from color_select, low 4 bits from palette
                let page = (color_select & 0x0F) as usize;
                (page << 4) | (palette_idx & 0x0F)
            } else {
                // PAS=0: High 2 bits from color_select, low 6 bits from palette
                let page = ((color_select >> 2) & 0x03) as usize;
                (page << 6) | (palette_idx & 0x3F)
            };
            // Apply PEL mask to DAC index
            let masked_idx = dac_index & (self.pel_mask as usize);
            let (r, g, b) = self.dac[masked_idx];
            self.palette16_lut[idx] = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }
        self.palette16_lut_dirty = false;
    }
}

impl Default for Vga {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
