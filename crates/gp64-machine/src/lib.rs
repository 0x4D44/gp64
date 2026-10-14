//! Headless real-mode PC. Host pacing and DOS services belong to its callers.
mod batch;
mod bus;
mod dosrun;
pub mod launcher;
pub mod modem;
pub mod mouse;
pub mod nullmodem;
mod policy;
mod timing;
use bus::Bus;
pub use dosrun::{DosMachine, DosRunExit};
use gp64_cpu::{Cpu, CpuError};
use gp64_hw::{system::SystemControlAction, GuestResetSource, IoBus};
use gp64_interp::RunResult;
use gp64_memory::Memory;
pub use policy::MachinePolicy;
use policy::Observer;
pub use timing::AudioSample;
use timing::Timeline;

pub const DEFAULT_CYCLES_PER_SECOND: u64 = 20_000_000;
/// Execution ends only at an instruction boundary. Budget exits may overshoot by one instruction.
#[derive(Debug)]
pub enum RunExit {
    BudgetUsed,
    Halted,
    Reset(GuestResetSource),
    CpuFault(CpuError),
    UnhandledException {
        vector: u8,
        error_code: u32,
        fault_addr: Option<u32>,
    },
    UnhandledInterrupt {
        vector: u8,
    },
    SoftwareInterrupt {
        vector: u8,
    },
    UnsupportedExecution(RunResult),
}
/// The U3b services dispatcher can service, deliver, or yield a software interrupt.
pub enum InterruptAction {
    Handled,
    Deliver,
    Yield,
}
/// Indexed presentation pixels and the VGA DAC palette (8-bit RGB components).
/// Indices identify presentation colours; equal DAC colours may share an index.
pub struct Frame {
    pub pixels: Vec<u8>,
    pub palette: [(u8, u8, u8); 256],
    pub width: usize,
    pub height: usize,
    pub completed: u64,
}
/// A frozen native VGA frame stamped at its emulated completion deadline.
pub struct CompletedFrame {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
    pub cycle: u64,
}
/// Owned CPU, RAM (including VGA planes), and the fixed PC hardware.
pub struct Machine {
    pub cpu: Cpu,
    pub memory: Memory,
    pub io: IoBus,
    clock: Timeline,
    io_fraction: u64,
    modem: Option<modem::TcpModem>,
    null_modem: Option<nullmodem::NullModem>,
    exception_defaults: Option<(u32, [Option<u32>; 32])>,
    capture_frames: Option<Vec<CompletedFrame>>,
}
impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}
impl Machine {
    pub fn new() -> Self {
        Self::with_rate(DEFAULT_CYCLES_PER_SECOND)
    }
    pub fn with_rate(cycles_per_second: u64) -> Self {
        assert!(cycles_per_second != 0);
        let mut io = IoBus::new();
        let clock = Timeline::new(cycles_per_second, &mut io);
        let mut memory = Memory::new();
        memory.apply_vga_pipeline(&io.vga.build_pipeline_state());
        Self {
            cpu: Cpu::new(),
            memory,
            io,
            clock,
            io_fraction: 0,
            modem: None,
            null_modem: None,
            exception_defaults: None,
            capture_frames: None,
        }
    }
    pub fn attach_modem(
        &mut self,
        connection: &modem::ModemConnection,
        com: u8,
    ) -> std::io::Result<Option<std::net::SocketAddr>> {
        self.attach_modem_with_clock(connection, com, modem::ModemClock::RealTime)
    }
    /// Inject the modem time source. Emulated peers must advance in slices below 10 ms.
    pub fn attach_modem_with_clock(
        &mut self,
        connection: &modem::ModemConnection,
        com: u8,
        clock: modem::ModemClock,
    ) -> std::io::Result<Option<std::net::SocketAddr>> {
        let (mut modem, address) =
            modem::TcpModem::attach_with_clock(connection, com, &mut self.io.serial, clock)?;
        modem.start_at(self.cycle());
        self.modem = Some(modem);
        Ok(address)
    }
    /// Attach a modem with no phone line; the returned handle starts one.
    /// The COM port stays fixed while this modem is attached.
    pub fn attach_modem_line(&mut self, com: u8) -> std::io::Result<modem::ModemLine> {
        self.attach_modem_line_with_clock(com, modem::ModemClock::RealTime)
    }
    pub fn attach_modem_line_with_clock(
        &mut self,
        com: u8,
        clock: modem::ModemClock,
    ) -> std::io::Result<modem::ModemLine> {
        let mut modem = modem::TcpModem::attach_line(com, &mut self.io.serial, clock)?;
        modem.start_at(self.cycle());
        let line = modem.line();
        self.modem = Some(modem);
        Ok(line)
    }
    /// Attach a null-modem cable with nothing at the far end; the returned
    /// handle connects it. The COM port stays fixed while it is attached.
    pub fn attach_null_modem(&mut self, com: u8) -> std::io::Result<modem::ModemLine> {
        let mut cable = nullmodem::NullModem::attach(com, &mut self.io.serial)?;
        cable.start_at(self.cycle());
        let line = cable.line();
        self.null_modem = Some(cable);
        Ok(line)
    }
    /// The attached modem's line control, if a modem is attached.
    pub fn modem_line(&self) -> Option<modem::ModemLine> {
        self.modem.as_ref().map(modem::TcpModem::line)
    }
    pub fn cycle(&self) -> u64 {
        self.cpu.cycles
    }
    /// Mark firmware vectors so only guest-installed exception handlers count as installed.
    pub fn record_firmware_exception_vectors(&mut self) {
        self.exception_defaults = Some((
            self.vector_table_base(),
            std::array::from_fn(|v| self.vector(v as u8).ok()),
        ));
    }
    pub fn cycles_per_second(&self) -> u64 {
        self.clock.rate
    }
    pub fn set_cycles_per_second(&mut self, rate: u64) {
        assert!(rate != 0);
        self.advance();
        self.clock.retime(&mut self.io, rate);
    }
    pub fn completed_frames(&self) -> u64 {
        self.clock.frames
    }
    /// Enable the lossless frame tap only while a recorder needs it.
    pub fn enable_frame_capture(&mut self, enabled: bool) {
        self.capture_frames = enabled.then(Vec::new);
    }
    pub fn take_completed_frames(&mut self) -> Vec<CompletedFrame> {
        self.capture_frames
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }
    pub fn take_audio(&mut self) -> Vec<AudioSample> {
        let mut audio = std::mem::take(&mut self.clock.audio);
        if let Some(modem) = &mut self.modem {
            modem.mix_speaker(&mut audio);
        }
        audio
    }
    /// Snapshot the current VGA image. Mode dimensions and palette come from the device state.
    pub fn frame(&mut self) -> Frame {
        self.io.vga.render(&mut self.memory);
        let mut palette = *self.io.vga.get_dac_entries();
        let (width, height) = self.io.vga.dimensions();
        let mut indices = std::collections::HashMap::new();
        for (index, &(r, g, b)) in palette.iter().enumerate() {
            indices
                .entry((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
                .or_insert(index as u8);
        }
        // Blanking is black even if no DAC entry is black. Use an otherwise unused
        // presentation index for colours emitted by the display gates.
        let mut used = [false; 256];
        for rgb in self.io.vga.framebuffer() {
            if let Some(&index) = indices.get(rgb) {
                used[usize::from(index)] = true;
            }
        }
        for &rgb in self.io.vga.framebuffer() {
            if let std::collections::hash_map::Entry::Vacant(entry) = indices.entry(rgb) {
                let index = used
                    .iter()
                    .position(|&in_use| !in_use)
                    .expect("VGA output fits an indexed palette");
                palette[index] = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
                entry.insert(index as u8);
                used[index] = true;
            }
        }
        let pixels = self
            .io
            .vga
            .framebuffer()
            .iter()
            .map(|rgb| indices[rgb])
            .collect();
        Frame {
            pixels,
            palette,
            width,
            height,
            completed: self.clock.frames,
        }
    }
    pub fn run_for(&mut self, cycles: u64) -> RunExit {
        self.run_until(self.cycle().saturating_add(cycles))
    }
    pub fn run_until(&mut self, cycle: u64) -> RunExit {
        self.run_until_with_hook(cycle, |_, _| InterruptAction::Deliver)
    }
    /// A hook sees each software INT before IVT delivery, after the instruction's IP advance.
    /// Return Handled after servicing it, Deliver for guest IVT delivery, or Yield to return its typed exit.
    pub fn run_until_with_hook(
        &mut self,
        end: u64,
        mut hook: impl FnMut(&mut Self, u8) -> InterruptAction,
    ) -> RunExit {
        self.run_until_with_boundary_hook(end, |machine, vector| {
            vector.map_or(InterruptAction::Handled, |vector| hook(machine, vector))
        })
    }
    /// The None event runs at a safe instruction boundary, before IRQ delivery.
    pub(crate) fn run_until_with_boundary_hook(
        &mut self,
        end: u64,
        mut hook: impl FnMut(&mut Self, Option<u8>) -> InterruptAction,
    ) -> RunExit {
        while self.cycle() < end {
            self.advance();
            if let Some(exit) = self.side_effects() {
                return exit;
            }
            if self.cpu.is_protected_mode() {
                return RunExit::UnsupportedExecution(RunResult::ModeChange { executed: 0 });
            }
            if matches!(hook(self, None), InterruptAction::Yield) {
                return RunExit::BudgetUsed;
            }
            if self.cpu.interrupts_enabled && self.cpu.interrupt_inhibit == 0 {
                if let Some(vector) = self.io.pic.get_interrupt() {
                    if !self.installed(vector) {
                        return RunExit::UnhandledInterrupt { vector };
                    }
                    self.cpu.halted = false;
                    if let Err(error) = self
                        .cpu
                        .interrupt_hardware_irq::<_, Observer>(&mut self.memory, vector)
                    {
                        return RunExit::CpuFault(error);
                    }
                }
            }
            let horizon = self.clock.next().unwrap_or(end).min(end);
            if self.cpu.halted {
                if self.clock.next().is_none() {
                    return RunExit::Halted;
                }
                self.cpu.cycles = horizon;
                continue;
            }
            let mut bus = Bus {
                devices: &mut self.io,
                clock: &mut self.clock,
                delay: 0,
                accessed: false,
                timestamp: self.cpu.cycles,
                fraction: &mut self.io_fraction,
            };
            let budget = horizon.saturating_sub(self.cpu.cycles).max(1);
            let result = gp64_interp::run_ext_charged_until_cycle_budget(
                &mut self.cpu,
                &mut self.memory,
                &mut bus,
                &mut MachinePolicy,
                budget.min(u64::from(u32::MAX)) as u32,
                true,
                budget,
            );
            self.cpu.cycles += u64::from(result.charged_cycles) + bus.delay;
            gp64_interp::sync_on_exit(&mut self.cpu);
            self.advance();
            match result.result {
                RunResult::SoftwareInterrupt { vector, .. } => {
                    match hook(self, Some(vector)) {
                        InterruptAction::Handled => continue,
                        InterruptAction::Yield => return RunExit::SoftwareInterrupt { vector },
                        InterruptAction::Deliver => {}
                    }
                    {
                        if !self.installed(vector) {
                            return RunExit::SoftwareInterrupt { vector };
                        }
                        if let Err(error) = self
                            .cpu
                            .interrupt_software::<_, Observer>(&mut self.memory, vector)
                        {
                            return RunExit::CpuFault(error);
                        }
                    }
                }
                RunResult::Exception {
                    vector,
                    error_code,
                    fault_addr,
                    ..
                } => {
                    if !self.installed(vector)
                        || self
                            .exception_defaults
                            .as_ref()
                            .is_some_and(|(base, defaults)| {
                                self.vector_table_base() == *base
                                    && usize::from(vector) < defaults.len()
                                    && defaults[usize::from(vector)].is_some_and(|default| {
                                        self.vector(vector).is_ok_and(|entry| entry == default)
                                    })
                            })
                    {
                        return RunExit::UnhandledException {
                            vector,
                            error_code,
                            fault_addr,
                        };
                    }
                    if let Some(address) = fault_addr {
                        self.cpu.regs.cr2 = address;
                    }
                    if let Err(error) = self.cpu.exception_with_error::<_, Observer>(
                        &mut self.memory,
                        vector,
                        error_code,
                    ) {
                        return RunExit::CpuFault(error);
                    }
                }
                RunResult::Halt { .. } => self.cpu.halted = true,
                RunResult::BudgetExhausted { .. }
                | RunResult::InterruptPollBoundary { .. }
                | RunResult::IoCompleted { .. }
                | RunResult::Iret { .. }
                | RunResult::Retf { .. }
                | RunResult::Invlpg { .. }
                | RunResult::ModeChange { .. } => {}
                other => return RunExit::UnsupportedExecution(other),
            }
        }
        self.advance();
        if let Some(exit) = self.side_effects() {
            return exit;
        }
        RunExit::BudgetUsed
    }
    fn vector_table_base(&self) -> u32 {
        if self.cpu.cpu_type.supports_286() {
            self.cpu.regs.idtr.base
        } else {
            0
        }
    }
    fn vector(&self, vector: u8) -> gp64_cpu::Result<u32> {
        self.cpu
            .real_mode_vector(&self.memory, vector)
            .map(|(ip, cs)| u32::from(ip) | (u32::from(cs) << 16))
    }
    fn installed(&self, vector: u8) -> bool {
        // A lookup error must reach delivery: the 386 can dispatch #DF instead.
        !matches!(self.vector(vector), Ok(0))
    }
    fn advance(&mut self) {
        if let Some(modem) = &mut self.modem {
            modem.pump(&mut self.io.serial, self.cpu.cycles, self.clock.rate);
        }
        if let Some(cable) = &mut self.null_modem {
            cable.pump(&mut self.io.serial, self.cpu.cycles, self.clock.rate);
        }
        // Services may change VGA state directly. Apply their pending timing and
        // memory-pipeline changes before the next guest instruction.
        self.clock.after_write(&mut self.io, 0, 0);
        if self.io.vga.pipeline_dirty() {
            self.memory
                .apply_vga_pipeline(&self.io.vga.build_pipeline_state());
            self.io.vga.clear_pipeline_dirty();
        }
        self.clock.advance(&mut self.io, self.cpu.cycles);
        for (cycle, line) in self.clock.render.drain(..) {
            if let Some(line) = line {
                self.io.vga.render_scanline(line, &mut self.memory);
            } else {
                self.io.vga.publish_completed_frame();
                if let Some(frames) = &mut self.capture_frames {
                    let (width, height) = self.io.vga.completed_dimensions();
                    frames.push(CompletedFrame {
                        pixels: self.io.vga.completed_framebuffer().to_vec(),
                        width,
                        height,
                        cycle,
                    });
                }
            }
        }
    }
    fn side_effects(&mut self) -> Option<RunExit> {
        if let Some(action) = self.io.system.take_pending_action() {
            match action {
                SystemControlAction::Reset(source) => return Some(RunExit::Reset(source)),
                SystemControlAction::A20Changed(enabled) => self.memory.set_a20(enabled),
            }
        }
        if let Some(enabled) = self.io.keyboard.take_pending_a20() {
            self.memory.set_a20(enabled);
        }
        self.io
            .keyboard
            .take_pending_guest_reset()
            .map(RunExit::Reset)
    }
}

/// Test seam for instruction oracles that supply their own fixture I/O bus.
/// Uses the machine's delivery observer with the same CPU interrupt dispatch.
#[doc(hidden)]
pub fn deliver_interrupt_for_test(machine: &mut Machine, vector: u8) -> gp64_cpu::Result<()> {
    machine
        .cpu
        .interrupt::<_, Observer>(&mut machine.memory, vector)
}
