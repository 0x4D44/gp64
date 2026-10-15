//! Generic INT 33h mouse driver. Callbacks run as guest far calls, with RETF
//! returning through a private firmware address. AX is the event mask, BX the
//! buttons, CX/DX the position, SI/DI the AX0B horizontal/vertical motion counters,
//! and DS the driver data segment.
//! The interrupted registers and flags are preserved, and callbacks never nest.
//! Software cursors use the standard AND/XOR masks in packed VGA mode 13h.
//! Sensitivity settings are retained for queries; acceleration is not simulated.

use gp64_cpu::{Cpu, CpuError, Flags, Registers, SegmentCache};
use gp64_memory::Memory;

#[derive(Debug, Clone, Copy, Default)]
struct Handler {
    mask: u16,
    segment: u16,
    offset: u16,
}

#[derive(Debug)]
struct Callback {
    registers: Registers,
    interrupts_enabled: bool,
    interrupt_inhibit: u8,
}

#[derive(Debug)]
pub struct Mouse {
    position: [i16; 2],
    limits: [(i16, i16); 2],
    buttons: u8,
    presses: [u16; 3],
    releases: [u16; 3],
    press_position: [[i16; 2]; 3],
    release_position: [[i16; 2]; 3],
    motion: [i16; 2],
    ratio: [u16; 2],
    remainder: [i64; 2],
    visibility: i16,
    sensitivity: [u16; 3],
    page: u16,
    handler: Handler,
    alternate: [Handler; 3],
    pending: u16,
    callback: Option<Callback>,
    screen_mask: [u16; 16],
    cursor_mask: [u16; 16],
    hotspot: [i16; 2],
    saved_pixels: Vec<(usize, u8)>,
    cursor_dirty: bool,
    enabled: bool,
}

impl Default for Mouse {
    fn default() -> Self {
        Self::new()
    }
}

impl Mouse {
    pub fn new() -> Self {
        Self {
            position: [0; 2],
            limits: [(0, 639), (0, 199)],
            buttons: 0,
            presses: [0; 3],
            releases: [0; 3],
            press_position: [[0; 2]; 3],
            release_position: [[0; 2]; 3],
            motion: [0; 2],
            ratio: [8, 16],
            remainder: [0; 2],
            visibility: -1,
            sensitivity: [50, 50, 64],
            page: 0,
            handler: Handler::default(),
            alternate: [Handler::default(); 3],
            pending: 0,
            callback: None,
            screen_mask: [
                0x3fff, 0x1fff, 0x0fff, 0x07ff, 0x03ff, 0x01ff, 0x00ff, 0x007f, 0x003f, 0x001f,
                0x01ff, 0x10ff, 0x30ff, 0xf87f, 0xf87f, 0xfc3f,
            ],
            cursor_mask: [
                0, 0x4000, 0x6000, 0x7000, 0x7800, 0x7c00, 0x7e00, 0x7f00, 0x7f80, 0x7c00, 0x6c00,
                0x4600, 0x0600, 0x0300, 0x0300, 0,
            ],
            hotspot: [0; 2],
            saved_pixels: Vec::new(),
            cursor_dirty: true,
            enabled: true,
        }
    }

    /// Host deltas are signed mickeys. Fractional pixels survive later moves.
    pub fn move_relative(&mut self, dx: i32, dy: i32) {
        if !self.enabled {
            return;
        }
        if dx != 0 || dy != 0 {
            self.pending |= 1;
            self.cursor_dirty = true;
        }
        for (axis, delta) in [dx, dy].into_iter().enumerate() {
            self.motion[axis] = (i64::from(self.motion[axis]) + i64::from(delta))
                .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                as i16;
            let scaled = i64::from(delta) * 8 + self.remainder[axis];
            let divisor = i64::from(self.ratio[axis]);
            let pixels = scaled / divisor;
            self.remainder[axis] = scaled % divisor;
            let (min, max) = self.limits[axis];
            self.position[axis] = (i64::from(self.position[axis]) + pixels)
                .clamp(i64::from(min), i64::from(max)) as i16;
        }
    }

    /// Puts the pointer over frame pixel `(x, y)` of a `width` x `height`
    /// frame, mapped linearly onto the current limits and clamped. The
    /// motion counters move by the equivalent mickeys.
    pub fn place(&mut self, x: i32, y: i32, width: usize, height: usize) {
        if !self.enabled || width < 2 || height < 2 {
            return;
        }
        for (axis, (pixel, size)) in [(x, width), (y, height)].into_iter().enumerate() {
            let (min, max) = self.limits[axis];
            let (min, max) = (i64::from(min), i64::from(max));
            let span = size as i64 - 1;
            let target = (i64::from(pixel) * (max - min) / span + min).clamp(min, max);
            let delta = target - i64::from(self.position[axis]);
            if delta != 0 {
                self.pending |= 1;
                self.cursor_dirty = true;
            }
            let mickeys = delta * i64::from(self.ratio[axis]) / 8;
            self.motion[axis] = (i64::from(self.motion[axis]) + mickeys)
                .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                as i16;
            self.remainder[axis] = 0;
            self.position[axis] = target as i16;
        }
    }

    /// Buttons are numbered left=0, right=1, middle=2.
    pub fn button(&mut self, n: u8, down: bool) {
        if n >= 3 || !self.enabled {
            return;
        }
        let mask = 1 << n;
        if (self.buttons & mask != 0) == down {
            return;
        }
        let index = usize::from(n);
        self.pending |= 1 << (1 + n * 2 + u8::from(!down));
        if down {
            self.buttons |= mask;
            self.presses[index] = self.presses[index].saturating_add(1);
            self.press_position[index] = self.position;
        } else {
            self.buttons &= !mask;
            self.releases[index] = self.releases[index].saturating_add(1);
            self.release_position[index] = self.position;
        }
    }

    /// Restore a drawn cursor before a BIOS video-mode change discards its backing store.
    pub(crate) fn video_mode_change(&mut self, memory: &mut Memory) {
        self.restore_cursor(memory);
        self.cursor_dirty = true;
    }

    fn restore_cursor(&mut self, memory: &mut Memory) {
        let mode13 = memory.read_u8(0x449) & 0x7f == 0x13;
        for (address, pixel) in self.saved_pixels.drain(..) {
            if mode13 && memory.vga_chain4_plain_store_range(address, 1) {
                memory.commit_vga_chain4_plain_byte(address, pixel);
            }
        }
    }

    pub(crate) fn refresh_cursor(&mut self, memory: &mut Memory) {
        if !self.cursor_dirty {
            return;
        }
        self.restore_cursor(memory);
        self.cursor_dirty = false;
        if !self.enabled
            || self.visibility < 0
            || self.page != 0
            || memory.read_u8(0x449) & 0x7f != 0x13
            || !memory.vga_chain4_plain_store_range(0xa0000, 64000)
        {
            return;
        }
        let origin = [
            i32::from(self.position[0]) / 2 - i32::from(self.hotspot[0]),
            i32::from(self.position[1]) - i32::from(self.hotspot[1]),
        ];
        for row in 0..16 {
            for column in 0..16 {
                let x = origin[0] + column;
                let y = origin[1] + row as i32;
                if !(0..320).contains(&x) || !(0..200).contains(&y) {
                    continue;
                }
                let bit = 0x8000 >> column;
                let address = 0xa0000 + y as usize * 320 + x as usize;
                let old = memory.read_u8(address);
                self.saved_pixels.push((address, old));
                let and = if self.screen_mask[row] & bit != 0 {
                    0xff
                } else {
                    0
                };
                let xor = if self.cursor_mask[row] & bit != 0 {
                    0xff
                } else {
                    0
                };
                memory.commit_vga_chain4_plain_byte(address, (old & and) ^ xor);
            }
        }
    }

    /// Called only at interpreter exits, before executing another instruction.
    /// Mouse callback delivery wakes HLT just as the driver's hardware IRQ does.
    /// A callback is running and its RETF must end the interpreter run.
    pub(crate) fn awaiting_return(&self) -> bool {
        self.callback.is_some()
    }
    pub(crate) fn poll_boundary_wanted(&self) -> bool {
        self.pending != 0 || self.cursor_dirty
    }
    pub(crate) fn boundary(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<(), CpuError> {
        if cpu.regs.cs == 0xf000 && cpu.regs.ip() == 0xff40 {
            if let Some(saved) = self.callback.take() {
                cpu.regs = saved.registers;
                cpu.interrupts_enabled = saved.interrupts_enabled;
                cpu.interrupt_inhibit = saved.interrupt_inhibit;
                cpu.halted = false;
            }
        }
        self.refresh_cursor(memory);
        if self.callback.is_some()
            || !self.enabled
            || self.pending == 0
            || !cpu.interrupts_enabled
            || cpu.interrupt_inhibit != 0
        {
            return Ok(());
        }
        let shift = memory.read_u8(0x417);
        let modifiers = (u16::from(shift & 3 != 0) << 7)
            | (u16::from(shift & 4 != 0) << 8)
            | (u16::from(shift & 8 != 0) << 9);
        let handler = self
            .alternate
            .iter()
            .find(|h| {
                h.mask & 0x380 == modifiers
                    && h.mask & self.pending & 0x7f != 0
                    && (h.segment != 0 || h.offset != 0)
            })
            .copied()
            .unwrap_or(self.handler);
        if handler.mask == 0 || (handler.segment == 0 && handler.offset == 0) {
            self.pending = 0;
            return Ok(());
        }
        if handler.mask & self.pending == 0 {
            return Ok(());
        }
        let saved = Callback {
            registers: cpu.regs.clone(),
            interrupts_enabled: cpu.interrupts_enabled,
            interrupt_inhibit: cpu.interrupt_inhibit,
        };
        // The callback's RETF is decoded by the ordinary CPU. The return address
        // is recognised at the next interpreter boundary and restores caller state.
        cpu.try_push16(memory, 0xf000)?;
        cpu.try_push16(memory, 0xff40)?;
        cpu.regs.set_ax(std::mem::take(&mut self.pending));
        cpu.regs.set_bx(u16::from(self.buttons));
        cpu.regs.set_cx(self.position[0] as u16);
        cpu.regs.set_dx(self.position[1] as u16);
        cpu.regs.set_si(self.motion[0] as u16);
        cpu.regs.set_di(self.motion[1] as u16);
        cpu.regs.ds = 0xf000;
        cpu.regs.ds_cache = SegmentCache::real_mode(0xf000);
        cpu.regs.cs = handler.segment;
        cpu.regs.cs_cache = SegmentCache::real_mode(handler.segment);
        cpu.regs.set_ip(handler.offset);
        cpu.regs.flags.remove(Flags::DIRECTION);
        cpu.regs.flags.insert(Flags::INTERRUPT);
        cpu.interrupts_enabled = true;
        cpu.halted = false;
        self.callback = Some(saved);
        Ok(())
    }

    pub fn interrupt(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        self.cursor_dirty = true;
        match cpu.regs.ax() {
            0x0000 | 0x0021 => {
                self.restore_cursor(memory);
                let callback = self.callback.take();
                *self = Self::new();
                self.callback = callback;
                self.limits[1].1 = match memory.read_u8(0x0449) & 0x7f {
                    0x0f | 0x10 => 349,
                    0x11 | 0x12 => 479,
                    _ => 199,
                };
                cpu.regs.set_ax(0xffff);
                cpu.regs.set_bx(3);
            }
            0x0001 => self.visibility = self.visibility.saturating_add(1).min(0),
            0x0002 => self.visibility = self.visibility.saturating_sub(1),
            0x0003 => {
                cpu.regs.set_bx(u16::from(self.buttons));
                cpu.regs.set_cx(self.position[0] as u16);
                cpu.regs.set_dx(self.position[1] as u16);
            }
            0x0004 => {
                for (axis, position) in [cpu.regs.cx(), cpu.regs.dx()].into_iter().enumerate() {
                    let (min, max) = self.limits[axis];
                    self.position[axis] = (position as i16).clamp(min, max);
                }
                self.remainder = [0; 2];
            }
            function @ (0x0005 | 0x0006) => {
                let index = usize::from(cpu.regs.bx());
                cpu.regs.set_ax(u16::from(self.buttons));
                let (counts, positions) = if function == 5 {
                    (&mut self.presses, &self.press_position)
                } else {
                    (&mut self.releases, &self.release_position)
                };
                if index < 3 {
                    cpu.regs.set_bx(std::mem::take(&mut counts[index]));
                    cpu.regs.set_cx(positions[index][0] as u16);
                    cpu.regs.set_dx(positions[index][1] as u16);
                } else {
                    cpu.regs.set_bx(0);
                    cpu.regs.set_cx(0);
                    cpu.regs.set_dx(0);
                }
            }
            function @ (0x0007 | 0x0008) => {
                let axis = usize::from(function - 7);
                let a = cpu.regs.cx() as i16;
                let b = cpu.regs.dx() as i16;
                self.limits[axis] = (a.min(b), a.max(b));
                self.position[axis] = self.position[axis].clamp(a.min(b), a.max(b));
                self.remainder[axis] = 0;
            }
            0x0009 => {
                self.hotspot = [cpu.regs.bx() as i16, cpu.regs.cx() as i16];
                for row in 0..16 {
                    self.screen_mask[row] = memory
                        .read_u16_seg(cpu.regs.es, cpu.regs.dx().wrapping_add((row * 2) as u16));
                    self.cursor_mask[row] = memory.read_u16_seg(
                        cpu.regs.es,
                        cpu.regs.dx().wrapping_add((32 + row * 2) as u16),
                    );
                }
            }
            0x000c | 0x0014 => {
                let old = self.handler;
                self.handler = Handler {
                    mask: cpu.regs.cx() & 0x7f,
                    segment: cpu.regs.es,
                    offset: cpu.regs.dx(),
                };
                self.pending = 0;
                if cpu.regs.ax() == 0x14 {
                    cpu.regs.set_cx(old.mask);
                    cpu.regs.es = old.segment;
                    cpu.regs.es_cache = SegmentCache::real_mode(old.segment);
                    cpu.regs.set_dx(old.offset);
                }
            }
            0x0018 => {
                let mask = cpu.regs.cx();
                let existing = self
                    .alternate
                    .iter()
                    .position(|h| h.mask & 0x380 == mask & 0x380);
                let slot = existing.or_else(|| self.alternate.iter().position(|h| h.mask == 0));
                if let Some(index) = slot {
                    self.alternate[index] = Handler {
                        mask,
                        segment: cpu.regs.es,
                        offset: cpu.regs.dx(),
                    };
                    if mask == 0 || (cpu.regs.es == 0 && cpu.regs.dx() == 0) {
                        self.alternate[index] = Handler::default();
                    }
                    cpu.regs.set_ax(0x18);
                } else {
                    cpu.regs.set_ax(0xffff);
                }
            }
            0x0019 => {
                if let Some(handler) = self
                    .alternate
                    .iter()
                    .find(|h| h.mask != 0 && h.mask & 0x380 == cpu.regs.cx() & 0x380)
                {
                    cpu.regs.set_bx(handler.segment);
                    cpu.regs.set_dx(handler.offset);
                    cpu.regs.set_cx(handler.mask);
                } else {
                    cpu.regs.set_cx(0);
                    cpu.regs.set_bx(0);
                    cpu.regs.set_dx(0);
                }
            }
            0x001f => {
                self.enabled = false;
                cpu.regs.set_ax(0x1f);
            }
            0x0020 => self.enabled = true,
            0x000b => {
                cpu.regs.set_cx(std::mem::take(&mut self.motion[0]) as u16);
                cpu.regs.set_dx(std::mem::take(&mut self.motion[1]) as u16);
            }
            0x000f => {
                self.ratio = [cpu.regs.cx().max(1), cpu.regs.dx().max(1)];
                self.remainder = [0; 2];
            }
            0x0013 => self.sensitivity[2] = cpu.regs.dx(),
            0x001a => self.sensitivity = [cpu.regs.bx(), cpu.regs.cx(), cpu.regs.dx()],
            0x001b => {
                cpu.regs.set_bx(self.sensitivity[0]);
                cpu.regs.set_cx(self.sensitivity[1]);
                cpu.regs.set_dx(self.sensitivity[2]);
            }
            0x001d => self.page = cpu.regs.bx(),
            0x001e => cpu.regs.set_bx(self.page),
            0x0024 => {
                cpu.regs.set_bx(0x0803);
                cpu.regs.set_cx(0x0400);
            }
            0x0026 => {
                cpu.regs.set_bx(u16::from(self.visibility >= 0));
                cpu.regs.set_cx(self.limits[0].1 as u16);
                cpu.regs.set_dx(self.limits[1].1 as u16);
            }
            _ => {}
        }
        self.refresh_cursor(memory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(mouse: &mut Mouse, cpu: &mut Cpu, memory: &mut Memory, function: u16) {
        cpu.regs.set_ax(function);
        mouse.interrupt(cpu, memory);
    }

    #[test]
    fn reset_motion_and_destructive_counter_queries() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        call(&mut mouse, &mut cpu, &mut memory, 0);
        assert_eq!((cpu.regs.ax(), cpu.regs.bx()), (0xffff, 3));
        mouse.move_relative(12, 1);
        mouse.move_relative(0, 1);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (12, 1));
        call(&mut mouse, &mut cpu, &mut memory, 0xb);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (12, 2));
        call(&mut mouse, &mut cpu, &mut memory, 0xb);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (0, 0));
    }

    #[test]
    fn placing_maps_frame_pixels_onto_limits_and_counts_mickeys() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        mouse.place(160, 100, 320, 200);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (320, 100));
        call(&mut mouse, &mut cpu, &mut memory, 0xb);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (320, 200));
        mouse.place(1000, -5, 320, 200);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (639, 0));
    }

    #[test]
    fn button_transitions_keep_positions_and_clear_each_count() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        mouse.move_relative(20, 30);
        mouse.button(0, true);
        mouse.button(0, true);
        mouse.move_relative(10, 10);
        mouse.button(0, false);
        cpu.regs.set_bx(0);
        call(&mut mouse, &mut cpu, &mut memory, 5);
        assert_eq!(
            (cpu.regs.ax(), cpu.regs.bx(), cpu.regs.cx(), cpu.regs.dx()),
            (0, 1, 20, 15)
        );
        cpu.regs.set_bx(0);
        call(&mut mouse, &mut cpu, &mut memory, 5);
        assert_eq!(cpu.regs.bx(), 0);
        call(&mut mouse, &mut cpu, &mut memory, 6);
        assert_eq!((cpu.regs.bx(), cpu.regs.cx(), cpu.regs.dx()), (1, 30, 20));
    }

    #[test]
    fn mode_limits_reversed_ranges_and_extreme_motion_are_bounded() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        memory.write_u8(0x449, 0x12);
        call(&mut mouse, &mut cpu, &mut memory, 0);
        mouse.move_relative(i32::MAX, i32::MAX);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (639, 479));
        cpu.regs.set_cx(100);
        cpu.regs.set_dx(20);
        call(&mut mouse, &mut cpu, &mut memory, 7);
        mouse.move_relative(i32::MIN, i32::MIN);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (20, 0));
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(0);
        call(&mut mouse, &mut cpu, &mut memory, 0xf);
        mouse.move_relative(i32::MAX, i32::MAX);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (100, 479));
    }
    #[test]
    fn exchange_and_alternate_handlers_obey_int33_register_contracts() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        cpu.regs.es = 0x1234;
        cpu.regs.set_dx(0x5678);
        cpu.regs.set_cx(3);
        call(&mut mouse, &mut cpu, &mut memory, 0xc);
        cpu.regs.es = 0x4321;
        cpu.regs.set_dx(0x8765);
        cpu.regs.set_cx(4);
        call(&mut mouse, &mut cpu, &mut memory, 0x14);
        assert_eq!(
            (cpu.regs.es, cpu.regs.dx(), cpu.regs.cx()),
            (0x1234, 0x5678, 3)
        );
        cpu.regs.es = 0x2468;
        cpu.regs.set_dx(0x1357);
        cpu.regs.set_cx(0x81);
        call(&mut mouse, &mut cpu, &mut memory, 0x18);
        assert_eq!(cpu.regs.ax(), 0x18);
        cpu.regs.set_cx(0x80);
        call(&mut mouse, &mut cpu, &mut memory, 0x19);
        assert_eq!(
            (cpu.regs.bx(), cpu.regs.dx(), cpu.regs.cx()),
            (0x2468, 0x1357, 0x81)
        );
        memory.write_u8(0x417, 1);
        cpu.regs.set_sp(0x8000);
        cpu.regs.flags.insert(Flags::INTERRUPT);
        cpu.interrupts_enabled = true;
        mouse.move_relative(1, 0);
        cpu.halted = true;
        mouse.boundary(&mut cpu, &mut memory).unwrap();
        assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x2468, 0x1357));
        assert!(!cpu.halted);
        cpu.regs.cs = 0xf000;
        cpu.regs.set_ip(0xff40);
        mouse.boundary(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.halted);
    }

    #[test]
    fn ratios_preserve_signed_fractional_motion_and_ranges_clip_warps() {
        let (mut mouse, mut cpu, mut memory) = (Mouse::new(), Cpu::new(), Memory::new());
        cpu.regs.set_cx(16);
        cpu.regs.set_dx(32);
        call(&mut mouse, &mut cpu, &mut memory, 0xf);
        cpu.regs.set_cx(10);
        cpu.regs.set_dx(100);
        call(&mut mouse, &mut cpu, &mut memory, 7);
        cpu.regs.set_cx(20);
        cpu.regs.set_dx(80);
        call(&mut mouse, &mut cpu, &mut memory, 8);
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(500);
        call(&mut mouse, &mut cpu, &mut memory, 4);
        mouse.move_relative(1, -1);
        mouse.move_relative(1, -3);
        call(&mut mouse, &mut cpu, &mut memory, 3);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (11, 79));
        call(&mut mouse, &mut cpu, &mut memory, 0xb);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (2, (-4i16) as u16));
        call(&mut mouse, &mut cpu, &mut memory, 0xb);
        assert_eq!((cpu.regs.cx(), cpu.regs.dx()), (0, 0));
    }
}
