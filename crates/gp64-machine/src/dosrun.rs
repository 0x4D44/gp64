//! DOS launch and service integration over the cycle-driven PC.
use crate::{
    batch::{BatchAction, BatchRunner},
    mouse::Mouse,
    timing::Timeline,
    InterruptAction, Machine, RunExit,
};
use anyhow::{bail, Context, Result};
use gp64_bios::{Bios, Int16Disp};
use gp64_cpu::{Cpu, Flags, Registers, SegmentCache};
use gp64_dos::{filesystem::HostDrive, Dos, Int21Disposition, XmsDispatch};
use gp64_hw::{bda, keyboard::Keyboard};
use gp64_memory::Memory;
use gp64_types::bios_layout::{DOS_IDLE_STUB_END, DOS_IDLE_STUB_START};
use std::{io::Read, path::Path};

/// Resolve DOS program names consistently for launches and batch CALL.
pub(crate) fn resolve_program(
    dos: &mut Dos,
    memory: &Memory,
    program: &str,
) -> Result<(String, Box<dyn gp64_dos::filesystem::VirtualFile>)> {
    let mut candidates = if Path::new(program).extension().is_some() {
        vec![program.to_string()]
    } else {
        ["COM", "EXE", "BAT"]
            .map(|ext| format!("{program}.{ext}"))
            .to_vec()
    };
    if !program.contains(['\\', '/', ':']) {
        if let Some(path) = dos.get_path_from_env(memory) {
            let names = candidates.clone();
            for directory in path.split(';').filter(|d| !d.is_empty()) {
                candidates.extend(names.iter().map(|name| format!("{directory}\\{name}")));
            }
        }
    }
    for name in candidates {
        let resolved = dos.path_resolver.resolve(&name);
        let drive = dos
            .drives
            .get_mut(&resolved.drive)
            .context("unmounted drive")?;
        if let Ok(file) = drive.open_file(&resolved.path, false, false, false) {
            return Ok((name, file));
        }
    }
    bail!("program not found: {program}")
}

// A retained BIOS wait parks here with the caller's IF. Real instructions keep
// an ISR's IRET safe even when the interpreter does not exit on the return.
const BIOS_WAIT_STUB_START: u16 = DOS_IDLE_STUB_END;
const BIOS_WAIT_STUB: [u8; 3] = [0xf4, 0xeb, 0xfd];
const _: () = {
    assert!(BIOS_WAIT_STUB_START == DOS_IDLE_STUB_END);
    assert!(BIOS_WAIT_STUB_START + (BIOS_WAIT_STUB.len() as u16) < 0xd000);
};
struct BiosWait {
    deadline: u64,
    ss: u16,
    sp: u16,
}
impl BiosWait {
    fn parked(&self, cpu: &Cpu) -> bool {
        cpu.regs.cs == 0xf000
            && (BIOS_WAIT_STUB_START..=BIOS_WAIT_STUB_START + 1).contains(&cpu.regs.ip())
            && cpu.regs.ss == self.ss
            && cpu.regs.sp() == self.sp
    }
}

const DOS_IDLE_STUB: [u8; 4] = [0xfb, 0xf4, 0xeb, 0xfd];
const _: () = {
    assert!(DOS_IDLE_STUB_START < DOS_IDLE_STUB_END);
    assert!(DOS_IDLE_STUB.len() == (DOS_IDLE_STUB_END - DOS_IDLE_STUB_START) as usize);
};

// The keyboard wait stub ends at F8F9 (exclusive). These two private traps
// occupy the unused tail of the reserved F800..F900 assembler stub range.
const CTRL_C_RETURN: u16 = 0xf8fa;
const CTRL_C_DEFAULT: u16 = 0xf8fc;
const _: () = {
    assert!(Bios::INT21_KBD_WAIT_RETURN < CTRL_C_RETURN);
    assert!(CTRL_C_RETURN + 2 <= CTRL_C_DEFAULT);
    assert!(CTRL_C_DEFAULT + 2 < gp64_types::bios_layout::STUB_CODE_RANGE_END);
};
#[derive(Default)]
struct KeyboardSequence {
    extended: bool,
    pause_remaining: u8,
    // Physical Ctrl/Alt: left bits 0/1, right bits 2/3.
    modifiers: u8,
}

struct CtrlCContinuation {
    psp: u16,
    registers: Registers, // SS:SP identifies the retained INT21 frame.
}

#[derive(Debug)]
pub enum DosRunExit {
    Running,
    Exited(u8),
    Machine(RunExit),
}

/// Consume the timeline's microseconds relative to the successful service start.
struct EventWaitClock {
    cursor_us: u64,
    anchor_remainder: u128,
    anchor_rate: u64,
}
impl EventWaitClock {
    fn new() -> Self {
        Self {
            cursor_us: 0,
            anchor_remainder: 0,
            anchor_rate: 1,
        }
    }
    fn start(&mut self, clock: &Timeline) {
        (self.cursor_us, self.anchor_remainder, self.anchor_rate) = clock.guest_time();
    }
    fn sync(&mut self, clock: &Timeline, bios: &mut Bios, memory: &mut Memory) {
        let (whole_us, remainder, rate) = clock.guest_time();
        // floor(current time - start fraction), including after CPU retiming.
        let below_start_phase =
            remainder * u128::from(self.anchor_rate) < self.anchor_remainder * u128::from(rate);
        let aligned_us = whole_us.saturating_sub(u64::from(below_start_phase));
        bios.tick_event_wait(aligned_us.saturating_sub(self.cursor_us), memory);
        self.cursor_us = self.cursor_us.max(aligned_us);
    }
}

/// A writable folder-backed DOS session. Only guest DOS calls write to the mount.
pub struct DosMachine {
    pub machine: Machine,
    pub dos: Dos,
    pub bios: Bios,
    mouse: Mouse,
    batch: Option<BatchRunner>,
    batch_wait_active: bool,
    root_psp: Option<u16>,
    exit: Option<u8>,
    vectors: [(u16, u16); 256],
    keyboard_sequence: KeyboardSequence,
    programs_started: Vec<String>,
    ctrl_c: Vec<CtrlCContinuation>,
    event_wait_clock: EventWaitClock,
    bios_waits: Vec<BiosWait>,
}

impl DosMachine {
    pub fn launch(folder: &Path, program: &str, args: &str) -> Result<Self> {
        let folder = folder.canonicalize().context("mount C:")?;
        if !folder.is_dir() {
            bail!("C: mount is not a directory");
        }
        let mut machine = Machine::new();
        let bios = Bios::new();
        bios.init_ivt(&mut machine.memory, false);
        // Exceptions without guest handlers must remain observable to the caller.
        for vector in (0..8).chain([0x31, 0x67]) {
            machine.memory.write_u32(vector * 4, 0);
        }
        machine.memory.write_u16(0x41a, 0x1e);
        machine.memory.write_u16(0x41c, 0x1e);
        machine
            .memory
            .write_u32(0x46c, gp64_dos::boot_time_bda_ticks());
        // BIOS IRQ0: preserve AX/DS, advance BDA ticks, chain INT1C, send EOI.
        install(
            &mut machine.memory,
            0xf000,
            Bios::BIOS_INT8_ISR_OFF,
            &[
                0x50, 0x1e, 0xcd, 0xfc, 0xcd, 0x1c, 0xb0, 0x20, 0xe6, 0x20, 0x1f, 0x58, 0xcf,
            ],
        );
        // IRQ1 uses the controller and the PIC; INT15 permits guest interception.
        install(
            &mut machine.memory,
            Bios::KBD_IRQ1_STUB_SEG,
            Bios::KBD_IRQ1_STUB_OFF,
            &[
                0x60, 0xe4, 0x60, 0xb4, 0x4f, 0xf9, 0xcd, 0x15, 0x73, 0x02, 0xcd, 0xfe, 0xb0, 0x20,
                0xe6, 0x20, 0x61, 0xcf,
            ],
        );
        // XMS entry is a real far-call stub, with a typed software service trap.
        install(&mut machine.memory, 0xf000, 0xff30, &[0xcd, 0xfd, 0xcb]);
        // Default IVT entries also work when a guest chains through a saved vector.
        for vector in [0x10u8, 0x11, 0x12, 0x15, 0x16, 0x1a, 0x20, 0x21, 0x2f, 0x33] {
            let address = usize::from(vector) * 4;
            let off = machine.memory.read_u16(address);
            let seg = machine.memory.read_u16(address + 2);
            install(&mut machine.memory, seg, off, &[0xcd, 0xfb]);
        }
        install(
            &mut machine.memory,
            0xf000,
            DOS_IDLE_STUB_START,
            &DOS_IDLE_STUB,
        );
        install(
            &mut machine.memory,
            0xf000,
            BIOS_WAIT_STUB_START,
            &BIOS_WAIT_STUB,
        );
        install(&mut machine.memory, 0xf000, CTRL_C_RETURN, &[0xcd, 0xfa]);
        install(&mut machine.memory, 0xf000, CTRL_C_DEFAULT, &[0xcd, 0xfa]);
        machine.memory.write_u16(0x23 * 4, CTRL_C_DEFAULT);
        machine.memory.write_u16(0x23 * 4 + 2, 0xf000);
        let mut dos = Dos::new();
        dos.drives.insert(2, Box::new(HostDrive::new(folder)));
        dos.init_environment(
            &mut machine.memory,
            &gp64_types::SoundEnvConfig {
                sound_blaster: Some(gp64_types::SoundBlasterEnv {
                    port: 0x220,
                    irq: 7,
                    dma: 1,
                    hdma: 0,
                    sb_type: 3,
                    emu8000_port: 0,
                    mpu_port: 0,
                }),
                gravis_ultrasound: None,
            },
        );
        dos.disable_umb(&mut machine.memory);
        dos.enable_stdout_capture();
        // A small shell PSP owns the environment; child images use DOS allocation.
        let shell = dos
            .dos_mem
            .alloc(&mut machine.memory, 8, 0x10)
            .map_err(|e| anyhow::anyhow!("shell PSP allocation: {e:?}"))?;
        dos.create_psp(
            &mut machine.memory,
            shell,
            shell + 0x10,
            dos.env_segment(),
            None,
            &[],
        );
        dos.set_current_psp(shell);
        machine.record_firmware_exception_vectors();
        let vectors = std::array::from_fn(|v| {
            (
                machine.memory.read_u16(v * 4),
                machine.memory.read_u16(v * 4 + 2),
            )
        });
        let mut pc = Self {
            machine,
            dos,
            bios,
            mouse: Mouse::new(),
            batch: None,
            batch_wait_active: false,
            root_psp: None,
            exit: None,
            vectors,
            keyboard_sequence: KeyboardSequence::default(),
            programs_started: Vec::new(),
            ctrl_c: Vec::new(),
            event_wait_clock: EventWaitClock::new(),
            bios_waits: Vec::new(),
        };
        pc.start(program, args)?;
        Ok(pc)
    }

    /// Successfully loaded executable names, in launch order, without arguments or paths.
    pub fn programs_started(&self) -> &[String] {
        &self.programs_started
    }

    pub fn take_console_output(&mut self) -> Vec<u8> {
        let bytes = self.dos.take_stdout_capture();
        self.dos.enable_stdout_capture();
        bytes
    }
    pub fn key_down(&mut self, scancode: u8) {
        self.key(scancode, true);
    }
    pub fn key_up(&mut self, scancode: u8) {
        self.key(scancode, false);
    }
    fn key(&mut self, scancode: u8, down: bool) {
        let byte = if down { scancode } else { scancode | 0x80 };
        self.machine.io.keyboard.shared_state().inject_scancode(
            &[byte],
            usize::from(scancode),
            down,
        );
        self.machine
            .io
            .keyboard
            .stage_output(&mut self.machine.io.pic);
    }
    pub fn mouse_move(&mut self, dx: i32, dy: i32) {
        self.mouse.move_relative(dx, dy);
        self.mouse.refresh_cursor(&mut self.machine.memory);
    }
    pub fn mouse_button(&mut self, n: u8, down: bool) {
        self.mouse.button(n, down);
    }

    fn read_program(&mut self, program: &str) -> Result<(String, Vec<u8>)> {
        let (name, file) = resolve_program(&mut self.dos, &self.machine.memory, program)?;
        let mut data = Vec::new();
        file.take(16 * 1024 * 1024 + 1).read_to_end(&mut data)?;
        if data.len() > 16 * 1024 * 1024 {
            bail!("program exceeds 16 MiB: {name}");
        }
        Ok((name, data))
    }
    fn start(&mut self, program: &str, args: &str) -> Result<()> {
        let (name, data) = self.read_program(program)?;
        if name.to_ascii_uppercase().ends_with(".BAT") {
            if data.len() as u64 > gp64_dos::shell::MAX_BATCH_FILE_BYTES {
                bail!("batch file too large");
            }
            self.batch = Some(BatchRunner::new(
                &gp64_dos::shell::decode_dos_text(&data),
                &name,
                args,
            ));
            return Ok(());
        }
        let parent = self.dos.current_psp();
        let env = self.dos.env_segment();
        self.dos
            .set_env_program_name(&mut self.machine.memory, &format!("C:\\{name}"));
        // Staging tail is below the MCB chain and outside DOS kernel structures.
        let tail = if args.is_empty() {
            String::new()
        } else {
            format!(" {args}")
        };
        let bytes = gp64_dos::shell::encode_dos_text(&tail);
        let len = bytes.len().min(126);
        self.machine.memory.write_u8(0x6000, len as u8);
        for (i, &b) in bytes[..len].iter().enumerate() {
            self.machine.memory.write_u8(0x6001 + i, b);
        }
        self.machine.memory.write_u8(0x6001 + len, 13);
        if data.starts_with(b"MZ") {
            self.dos.exec_load_exe(
                &mut self.machine.cpu,
                &mut self.machine.memory,
                &data,
                env,
                Some(0x6000),
                None,
                None,
                parent,
            )?;
        } else if name.to_ascii_uppercase().ends_with(".COM") {
            self.dos.exec_load_com(
                &mut self.machine.cpu,
                &mut self.machine.memory,
                &data,
                env,
                Some(0x6000),
                None,
                None,
                parent,
            )?;
        } else {
            bail!("unsupported executable format: {name}");
        }
        if self.machine.cpu.regs.flags.contains(Flags::CARRY) {
            bail!(
                "DOS loader failed for {name}: {}",
                self.machine.cpu.regs.ax()
            );
        }
        let argv = args
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        self.dos.parse_default_fcbs_from_args(
            &mut self.machine.memory,
            self.dos.current_psp(),
            &argv,
        );
        self.programs_started.push(executable_name(&name));
        self.root_psp = Some(self.dos.current_psp());
        self.machine.cpu.halted = false;
        Ok(())
    }

    pub fn run_for(&mut self, cycles: u64) -> Result<DosRunExit> {
        if let Some(code) = self.exit {
            return Ok(DosRunExit::Exited(code));
        }
        let end = self.machine.cycle().saturating_add(cycles);
        loop {
            if self.root_psp.is_none() {
                // Polling the batch can dequeue a key or launch a command. An
                // IRQ may have sent EOI without returning its saved frame yet.
                let regs = &self.machine.cpu.regs;
                if self.batch_wait_active
                    && !(regs.cs == 0xf000
                        && (DOS_IDLE_STUB_START..=DOS_IDLE_STUB_START + 2).contains(&regs.ip())
                        && regs.ss == 0
                        && regs.sp() == 0x6f00)
                {
                    return self.run_machine(end);
                }
                if let Some(mut batch) = self.batch.take() {
                    let action = batch.next(&mut self.dos, &mut self.machine)?;
                    self.batch = Some(batch);
                    match action {
                        BatchAction::Continue => return Ok(DosRunExit::Running),
                        BatchAction::Waiting => {
                            if !self.batch_wait_active {
                                self.machine.cpu.regs.cs = 0xf000;
                                self.machine.cpu.regs.cs_cache = SegmentCache::real_mode(0xf000);
                                self.machine.cpu.regs.set_ip(DOS_IDLE_STUB_START);
                                self.machine.cpu.regs.ss = 0;
                                self.machine.cpu.regs.ss_cache = SegmentCache::real_mode(0);
                                self.machine.cpu.regs.set_sp(0x6f00);
                                self.machine.cpu.halted = false;
                                self.batch_wait_active = true;
                            }
                            let result = self.run_machine(end)?;
                            return Ok(result);
                        }
                        BatchAction::Finished(code) => {
                            self.batch_wait_active = false;
                            self.exit = Some(code);
                            return Ok(DosRunExit::Exited(code));
                        }
                        BatchAction::Launch { program, args } => {
                            self.batch_wait_active = false;
                            if let Err(error) = self.start(&program, &args) {
                                if let Some(batch) = self.batch.as_mut() {
                                    batch.restore_all_redirects(&mut self.dos);
                                }
                                return Err(error);
                            }
                        }
                    }
                }
            }
            let result = self.run_machine(end)?;
            if let DosRunExit::Exited(code) = result {
                if let Some(batch) = self.batch.as_mut() {
                    batch.program_exited(code);
                    self.exit = None;
                    if self.machine.cycle() >= end {
                        return Ok(DosRunExit::Running);
                    }
                    continue;
                }
            }
            return Ok(result);
        }
    }
    fn run_machine(&mut self, end: u64) -> Result<DosRunExit> {
        let Self {
            machine,
            dos,
            bios,
            mouse,
            root_psp,
            keyboard_sequence,
            vectors,
            programs_started,
            ctrl_c,
            event_wait_clock,
            bios_waits,
            ..
        } = self;
        let mut termination = None;
        let mut failure = None;
        let result = loop {
            let horizon = bios_waits
                .last()
                .filter(|wait| wait.deadline > machine.cycle())
                .map_or(end, |wait| end.min(wait.deadline));
            let mut wait_boundary_changed = false;
            let result = machine.run_until_with_boundary_hook(horizon, |machine, vector| {
                event_wait_clock.sync(&machine.clock, bios, &mut machine.memory);
                let Some(vector) = vector else {
                    if let Some(wait) = bios_waits.last().filter(|wait| wait.parked(&machine.cpu)) {
                        if machine.cycle() >= wait.deadline {
                            machine.cpu.halted = false;
                            if let Err(error) = machine.cpu.iret(&mut machine.memory) {
                                failure = Some(error.into());
                                return InterruptAction::Yield;
                            }
                            bios_waits.pop();
                            wait_boundary_changed = true;
                            return InterruptAction::Yield;
                        } else {
                            machine.cpu.halted = true;
                        }
                    }
                    if let Err(error) = mouse.boundary(&mut machine.cpu, &mut machine.memory) {
                        failure = Some(error.into());
                        return InterruptAction::Yield;
                    }
                    return InterruptAction::Handled;
                };
                let exec = if (vector == 0x21
                    || (vector == 0xfb
                        && machine.cpu.regs.cs == vectors[0x21].1
                        && machine.cpu.regs.ip() == vectors[0x21].0.wrapping_add(2)))
                    && machine.cpu.regs.ax() == 0x4b00
                {
                    let base =
                        Memory::seg_offset_to_linear(machine.cpu.regs.ds, machine.cpu.regs.dx());
                    let bytes: Vec<_> = (0..260)
                        .map(|i| machine.memory.read_u8(base + i))
                        .take_while(|&b| b != 0)
                        .collect();
                    Some((dos.current_psp(), gp64_dos::shell::decode_dos_text(&bytes)))
                } else {
                    None
                };
                let result = dispatch(
                    machine,
                    dos,
                    bios,
                    mouse,
                    *root_psp,
                    keyboard_sequence,
                    vectors,
                    ctrl_c,
                    event_wait_clock,
                    bios_waits,
                    vector,
                );
                if let Some((parent, name)) = exec {
                    if result.is_ok() && dos.current_psp() != parent {
                        programs_started.push(executable_name(&name));
                    }
                }
                match result {
                    Ok(Service::Wait) => {
                        wait_boundary_changed = true;
                        InterruptAction::Yield
                    }
                    Ok(Service::Handled) => InterruptAction::Handled,
                    Ok(Service::Deliver) => InterruptAction::Deliver,
                    Ok(Service::Exit(code)) => {
                        termination = Some(code);
                        InterruptAction::Yield
                    }
                    Err(e) => {
                        failure = Some(e);
                        InterruptAction::Yield
                    }
                }
            });
            if failure.is_some() || termination.is_some() {
                break result;
            }
            if wait_boundary_changed || (matches!(result, RunExit::BudgetUsed) && horizon < end) {
                continue;
            }
            break result;
        };
        // A halted final horizon advances the clock without another hook.
        event_wait_clock.sync(&machine.clock, bios, &mut machine.memory);
        if let Some(error) = failure {
            return Err(error);
        }
        if let Some(code) = termination {
            self.root_psp = None;
            self.exit = Some(code);
            return Ok(DosRunExit::Exited(code));
        }
        Ok(match result {
            RunExit::BudgetUsed => DosRunExit::Running,
            other => DosRunExit::Machine(other),
        })
    }
}
fn executable_name(path: &str) -> String {
    path.rsplit(['\\', '/', ':'])
        .next()
        .unwrap_or(path)
        .to_ascii_uppercase()
}
fn install(memory: &mut Memory, seg: u16, off: u16, bytes: &[u8]) {
    let base = Memory::seg_offset_to_linear(seg, off);
    for (i, &b) in bytes.iter().enumerate() {
        if seg == 0xf000 {
            memory.write_rom(base + i, b);
        } else {
            memory.write_u8(base + i, b);
        }
    }
}
enum Service {
    Handled,
    Wait,
    Deliver,
    Exit(u8),
}
fn dispatch(
    machine: &mut Machine,
    dos: &mut Dos,
    bios: &mut Bios,
    mouse: &mut Mouse,
    root: Option<u16>,
    sequence: &mut KeyboardSequence,
    vectors: &[(u16, u16); 256],
    ctrl_c: &mut Vec<CtrlCContinuation>,
    event_wait_clock: &mut EventWaitClock,
    bios_waits: &mut Vec<BiosWait>,
    vector: u8,
) -> Result<Service> {
    let Machine {
        cpu, memory, io, ..
    } = machine;
    if vector == 0xfa && cpu.regs.cs == 0xf000 {
        if cpu.regs.ip() == CTRL_C_DEFAULT + 2 {
            return terminate_ctrl_c(cpu, memory, dos, root, ctrl_c);
        }
        if cpu.regs.ip() == CTRL_C_RETURN + 2 {
            let continuation = ctrl_c.last().context("INT23 return without continuation")?;
            let saved = &continuation.registers;
            if continuation.psp != dos.current_psp() || cpu.regs.ss != saved.ss {
                bail!("INT23 returned with a different process or stack segment");
            }
            let far_return = cpu.regs.sp() == saved.sp().wrapping_sub(2);
            if !far_return && cpu.regs.sp() != saved.sp() {
                bail!("INT23 returned with an unbalanced stack");
            }
            if far_return && cpu.regs.flags.contains(Flags::CARRY) {
                return terminate_ctrl_c(cpu, memory, dos, root, ctrl_c);
            }
            cpu.regs = ctrl_c.pop().expect("validated continuation").registers;
            cpu.interrupts_enabled = cpu.regs.flags.contains(Flags::INTERRUPT);
            let registers = cpu.regs.clone();
            let disposition = dos.int21(cpu, memory, &mut io.vga, &mut io.keyboard, &mut io.pic)?;
            complete_int21(cpu, memory, dos, ctrl_c, registers, disposition)?;
            return Ok(Service::Handled);
        }
    }
    // The installed keyboard stub owns a retained DOS entry while waiting.
    // Release it at the actual ROM INT instruction boundary, even when a
    // guest INT 21h hook will receive the interrupt and chain to DOS later.
    if vector == 0x21 && cpu.regs.cs == 0xf000 && cpu.regs.ip() == Bios::INT21_KBD_WAIT_RETURN {
        Dos::leave_dos(memory);
    }
    // Private BIOS service traps are reachable only from our generated stubs.
    if vector == 0xfc && cpu.regs.cs == 0xf000 && cpu.regs.ip() == Bios::BIOS_INT8_ISR_OFF + 4 {
        let ticks = memory.read_u32(0x46c).wrapping_add(1);
        if ticks >= gp64_types::bda::BIOS_TICKS_PER_DAY {
            memory.write_u32(0x46c, 0);
            memory.write_u8(0x470, memory.read_u8(0x470).wrapping_add(1));
        } else {
            memory.write_u32(0x46c, ticks);
        }
        return Ok(Service::Handled);
    }
    if vector == 0xfe
        && cpu.regs.cs == Bios::KBD_IRQ1_STUB_SEG
        && cpu.regs.ip() == Bios::KBD_IRQ1_STUB_OFF + 12
    {
        keyboard_byte(memory, cpu.regs.al(), sequence);
        return Ok(Service::Handled);
    }
    if vector == 0xfd && cpu.regs.cs == 0xf000 && cpu.regs.ip() == 0xff32 {
        dos.xms.dispatch_with_umb(cpu, memory, &mut dos.dos_mem);
        return Ok(Service::Handled);
    }
    let from_stub = vector == 0xfb
        && vectors.iter().enumerate().any(|(v, &(off, seg))| {
            matches!(
                v,
                0x10 | 0x11 | 0x12 | 0x15 | 0x16 | 0x1a | 0x20 | 0x21 | 0x2f | 0x33
            ) && cpu.regs.cs == seg
                && cpu.regs.ip() == off.wrapping_add(2)
        });
    let vector = if from_stub {
        vectors
            .iter()
            .position(|&(off, seg)| cpu.regs.cs == seg && cpu.regs.ip() == off.wrapping_add(2))
            .expect("validated service stub") as u8
    } else {
        vector
    };
    if !from_stub
        && !cpu
            .real_mode_vector(memory, vector)
            .is_ok_and(|entry| entry == vectors[usize::from(vector)])
    {
        return Ok(Service::Deliver);
    }
    let exit_code = match vector {
        0x20 => Some(0),
        0x21 if cpu.regs.ah() == 0x4c => Some(cpu.regs.al()),
        0x21 if cpu.regs.ah() == 0 => Some(0),
        _ => None,
    };
    if let Some(code) = exit_code {
        let exiting_psp = dos.current_psp();
        ctrl_c.retain(|entry| entry.psp != exiting_psp);
        if root == Some(dos.current_psp()) {
            finish_root_process(dos, memory, code, 0);
            return Ok(Service::Exit(code));
        }
    }
    if !matches!(
        vector,
        0x10 | 0x11 | 0x12 | 0x15 | 0x16 | 0x1a | 0x20 | 0x21 | 0x2f | 0x33
    ) {
        return Ok(Service::Deliver);
    }
    // Copied services expect the real-mode INT frame (EXEC saves it for its parent).
    if !from_stub {
        cpu.try_push16(memory, cpu.regs.flags.bits())?;
        cpu.try_push16(memory, cpu.regs.cs)?;
        cpu.try_push16(memory, cpu.regs.ip())?;
    } else {
        // Recover the caller's IF before a service edits the result flags.
        let saved = memory.read_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
        cpu.regs.flags = Flags::from_bits_truncate(saved);
    }
    let mut no_iret = false;
    match vector {
        0x10 => {
            if cpu.regs.ah() == 0 {
                mouse.video_mode_change(memory);
            }
            bios.int10(cpu, memory, &mut io.vga)?;
        }
        0x11 => bios.int11(cpu)?,
        0x12 => bios.int12(cpu)?,
        0x15 => {
            let starts_event_wait = cpu.regs.ax() == 0x8300;
            let (wait, disp) = bios.int15(cpu, memory, Some(&mut io.pic))?;
            if starts_event_wait && !cpu.regs.flags.contains(Flags::CARRY) {
                event_wait_clock.start(&machine.clock);
            }
            if let Some(micros) = wait {
                let elapsed = (u64::from(micros) * machine.clock.rate).div_ceil(1_000_000);
                if elapsed != 0 {
                    // Retain the original INT frame and its service result flags.
                    // IRQs push their own frames beneath it; only this SS:SP may
                    // complete the wait, after any installed handler returns.
                    memory.write_u16_seg(
                        cpu.regs.ss,
                        cpu.regs.sp().wrapping_add(4),
                        cpu.regs.flags.bits(),
                    );
                    bios_waits.push(BiosWait {
                        deadline: cpu.cycles.saturating_add(elapsed),
                        ss: cpu.regs.ss,
                        sp: cpu.regs.sp(),
                    });
                    cpu.regs.cs = 0xf000;
                    cpu.regs.cs_cache = SegmentCache::real_mode(0xf000);
                    cpu.regs.set_ip(BIOS_WAIT_STUB_START);
                    cpu.interrupts_enabled = cpu.regs.flags.contains(Flags::INTERRUPT);
                    cpu.halted = true;
                    return Ok(Service::Wait);
                }
            }
            if !matches!(disp, gp64_bios::Int15Disp::Iret) {
                no_iret = true;
            }
        }
        0x16 => {
            if let Int16Disp::Stub(off) = bios.int16(cpu, memory, &mut io.keyboard)? {
                cpu.regs.cs = 0xf000;
                cpu.regs.cs_cache = SegmentCache::real_mode(0xf000);
                cpu.regs.set_ip(off);
                no_iret = true;
            }
        }
        0x1a => bios.int1a(cpu, memory, &mut io.cmos, &io.pit, None)?,
        0x20 => {
            dos.terminate_process(cpu, memory, 0, 0)?;
            no_iret = true;
        }
        0x21 => {
            let registers = cpu.regs.clone();
            let disposition = dos.int21(cpu, memory, &mut io.vga, &mut io.keyboard, &mut io.pic)?;
            complete_int21(cpu, memory, dos, ctrl_c, registers, disposition)?;
            no_iret = true; // complete_int21 owns the service and handler frames.
        }
        0x2f => {
            if cpu.regs.ax() == 0x1687 {
                cpu.regs.set_ax(1);
            } else if !dos.xms.int2f_check(cpu) {
                dos.int2f(cpu, memory, false)?;
            }
        }
        0x33 => mouse.interrupt(cpu, memory),
        _ => unreachable!(),
    }
    if !no_iret {
        memory.write_u16_seg(
            cpu.regs.ss,
            cpu.regs.sp().wrapping_add(4),
            cpu.regs.flags.bits(),
        );
        cpu.iret(memory)?;
    }
    Ok(Service::Handled)
}
fn complete_int21(
    cpu: &mut Cpu,
    memory: &mut Memory,
    dos: &Dos,
    ctrl_c: &mut Vec<CtrlCContinuation>,
    registers: Registers,
    disposition: Int21Disposition,
) -> Result<()> {
    match disposition {
        Int21Disposition::Iret => {
            memory.write_u16_seg(
                cpu.regs.ss,
                cpu.regs.sp().wrapping_add(4),
                cpu.regs.flags.bits(),
            );
            cpu.iret(memory)?;
        }
        Int21Disposition::NoIret => (),
        Int21Disposition::CtrlC => {
            // trigger_ctrl_c installed the INT23 vector and pushed its frame.
            // Its return belongs to this gate, never the HLE's CS:IP.
            let handler_cs = cpu.regs.cs;
            let handler_ip = cpu.regs.ip();
            let handler_sp = cpu.regs.sp();
            let handler_flags = cpu.regs.flags;
            memory.write_u16_seg(cpu.regs.ss, handler_sp, CTRL_C_RETURN);
            memory.write_u16_seg(cpu.regs.ss, handler_sp.wrapping_add(2), 0xf000);
            cpu.regs = registers.clone();
            cpu.regs.cs = handler_cs;
            cpu.regs.cs_cache = SegmentCache::real_mode(handler_cs);
            cpu.regs.set_ip(handler_ip);
            cpu.regs.set_sp(handler_sp);
            cpu.regs.flags = handler_flags;
            cpu.interrupts_enabled = false;
            ctrl_c.push(CtrlCContinuation {
                psp: dos.current_psp(),
                registers,
            });
        }
        Int21Disposition::KbdWaitStub => {
            cpu.regs.cs = 0xf000;
            cpu.regs.cs_cache = SegmentCache::real_mode(0xf000);
            cpu.regs.set_ip(Bios::INT21_KBD_WAIT);
        }
        Int21Disposition::CriticalError { .. } => bail!("DOS critical I/O error"),
    }
    Ok(())
}
fn finish_root_process(dos: &mut Dos, memory: &mut Memory, code: u8, termination_type: u8) {
    let psp = dos.current_psp();
    let parent = memory.read_u16(usize::from(psp) * 16 + 0x16);
    dos.set_last_exit_code(code);
    dos.set_last_termination_type(termination_type);
    dos.file_table.close_process_files(psp);
    dos.restore_psp_vectors(memory, psp);
    dos.dos_mem.free_all_for_psp(memory, psp);
    dos.set_current_psp(parent);
}
fn terminate_ctrl_c(
    cpu: &mut Cpu,
    memory: &mut Memory,
    dos: &mut Dos,
    root: Option<u16>,
    ctrl_c: &mut Vec<CtrlCContinuation>,
) -> Result<Service> {
    let psp = dos.current_psp();
    ctrl_c.retain(|entry| entry.psp != psp);
    if root == Some(psp) {
        finish_root_process(dos, memory, 0, 1);
        Ok(Service::Exit(0))
    } else {
        dos.terminate_process(cpu, memory, 0, 1)?;
        Ok(Service::Handled)
    }
}
fn keyboard_byte(memory: &mut Memory, byte: u8, sequence: &mut KeyboardSequence) {
    // Pause's E1 1D 45 E1 9D C5 sequence contains fake Ctrl/NumLock bytes.
    // Consume them together, without disturbing real held keys or the ring.
    if sequence.pause_remaining != 0 {
        sequence.pause_remaining -= 1;
        return;
    }
    if byte == 0xe1 {
        sequence.extended = false;
        sequence.pause_remaining = 5;
        return;
    }
    if byte == 0xe0 {
        sequence.extended = true;
        return;
    }
    let extended = std::mem::take(&mut sequence.extended);
    let scan = byte & 0x7f;
    let released = byte & 0x80 != 0;
    // E0 Shift transitions are synthetic wrappers for enhanced keys.
    if extended && matches!(scan, 0x2a | 0x36) {
        return;
    }
    let mut flags = bda::kbd_get_shift_flags(memory);
    if matches!(scan, 0x1d | 0x38) {
        let left_bit = if scan == 0x1d { 1 } else { 2 };
        let physical_bit = if extended { left_bit << 2 } else { left_bit };
        if released {
            sequence.modifiers &= !physical_bit;
        } else {
            sequence.modifiers |= physical_bit;
        }
        let aggregate_bit = left_bit << 2;
        if sequence.modifiers & (left_bit | aggregate_bit) == 0 {
            flags &= !aggregate_bit;
        } else {
            flags |= aggregate_bit;
        }
        bda::kbd_set_shift_flags(memory, flags);
        let left = bda::kbd_get_extended_shift_flags(memory);
        bda::kbd_set_extended_shift_flags(memory, (left & !3) | (sequence.modifiers & 3));
        let right = bda::kbd_get_keyboard_flags(memory);
        bda::kbd_set_keyboard_flags(memory, (right & !12) | (sequence.modifiers & 12));
        return;
    }
    let lock = match scan {
        0x3a => 0x40,
        0x45 => 0x20,
        0x46 => 0x10,
        _ => 0,
    };
    if lock != 0 {
        let mut down = bda::kbd_get_extended_shift_flags(memory);
        if released {
            down &= !lock;
        } else {
            if down & lock == 0 {
                flags ^= lock;
            }
            down |= lock;
        }
        bda::kbd_set_shift_flags(memory, flags);
        bda::kbd_set_extended_shift_flags(memory, down);
        return;
    }
    let bit = match scan {
        0x2a => 1,
        0x36 => 2,
        _ => 0,
    };
    if bit != 0 {
        if released {
            flags &= !bit;
        } else {
            flags |= bit;
        }
        bda::kbd_set_shift_flags(memory, flags);
    } else if !released {
        bda::kbd_set_shift_flags(memory, flags);
        let ascii = Keyboard::ascii_for_scancode_set1_extended(scan, extended, flags);
        bda::kbd_enqueue(memory, scan, ascii);
    }
}

#[cfg(test)]
mod wait_tests {
    use super::*;

    // Entirely synthetic real-mode guests; no DOS files or host mount.
    fn guest(wait: bool, chained: bool, interrupts: bool, micros: u32) -> DosMachine {
        let mut machine = Machine::new();
        let bios = Bios::new();
        bios.init_ivt(&mut machine.memory, false);
        machine.set_cycles_per_second(1_000_000);
        install(
            &mut machine.memory,
            0xf000,
            BIOS_WAIT_STUB_START,
            &BIOS_WAIT_STUB,
        );
        install(
            &mut machine.memory,
            0xf000,
            Bios::BIOS_INT8_ISR_OFF,
            &[
                0x50, 0x1e, 0xcd, 0xfc, 0xcd, 0x1c, 0xb0, 0x20, 0xe6, 0x20, 0x1f, 0x58, 0xcf,
            ],
        );
        let vectors = std::array::from_fn(|v| {
            (
                machine.memory.read_u16(v * 4),
                machine.memory.read_u16(v * 4 + 2),
            )
        });
        let (irq_off, irq_seg) = vectors[8];
        let [ilo, ihi] = irq_off.to_le_bytes();
        let [islo, ishi] = irq_seg.to_le_bytes();
        install(
            &mut machine.memory,
            0x2200,
            0,
            &[
                0x50, 0x1e, 0x31, 0xc0, 0x8e, 0xd8, 0xff, 0x06, 0x02, 0x05, 0x1f, 0x58, 0xea, ilo,
                ihi, islo, ishi,
            ],
        );
        machine.memory.write_u16(8 * 4, 0);
        machine.memory.write_u16(8 * 4 + 2, 0x2200);
        let (off, seg) = vectors[0x15];
        install(&mut machine.memory, seg, off, &[0xcd, 0xfb]);
        // INT1C increments a private RAM counter, preserving AX and DS.
        install(
            &mut machine.memory,
            0x2000,
            0,
            &[
                0x50, 0x1e, 0x31, 0xc0, 0x8e, 0xd8, 0xff, 0x06, 0x00, 0x05, 0x1f, 0x58, 0xcf,
            ],
        );
        machine.memory.write_u16(0x1c * 4, 0);
        machine.memory.write_u16(0x1c * 4 + 2, 0x2000);
        if chained {
            // The installed INT15 handler tail-chains through the saved ROM vector.
            let [lo, hi] = off.to_le_bytes();
            let [slo, shi] = seg.to_le_bytes();
            install(&mut machine.memory, 0x2100, 0, &[0xea, lo, hi, slo, shi]);
            machine.memory.write_u16(0x15 * 4, 0);
            machine.memory.write_u16(0x15 * 4 + 2, 0x2100);
        }
        install(
            &mut machine.memory,
            0x1000,
            0,
            if wait {
                &[0xcd, 0x15, 0xf4, 0xeb, 0xfd]
            } else {
                &[0xf4, 0xeb, 0xfd]
            },
        );
        let regs = &mut machine.cpu.regs;
        regs.cs = 0x1000;
        regs.cs_cache = SegmentCache::real_mode(0x1000);
        regs.set_ip(0);
        regs.ss = 0x3000;
        regs.ss_cache = SegmentCache::real_mode(0x3000);
        regs.set_sp(0x8000);
        regs.set_ax(0x865a);
        regs.set_cx((micros >> 16) as u16);
        regs.set_dx(micros as u16);
        regs.set_bx(0x4321);
        regs.flags = Flags::RESERVED1 | Flags::CARRY | Flags::DIRECTION;
        regs.flags.set(Flags::INTERRUPT, interrupts);
        machine.cpu.interrupts_enabled = interrupts;
        DosMachine {
            machine,
            dos: Dos::new(),
            bios,
            mouse: Mouse::new(),
            batch: None,
            batch_wait_active: false,
            root_psp: Some(0),
            exit: None,
            vectors,
            keyboard_sequence: KeyboardSequence::default(),
            programs_started: Vec::new(),
            ctrl_c: Vec::new(),
            event_wait_clock: EventWaitClock::new(),
            bios_waits: Vec::new(),
        }
    }
    fn advance(pc: &mut DosMachine, end: u64, slice: u64) {
        while pc.machine.cycle() < end {
            assert!(matches!(
                pc.run_for(slice.min(end - pc.machine.cycle())).unwrap(),
                DosRunExit::Running
            ));
        }
    }
    fn cadence(chained: bool, slice: u64) {
        let mut waiting = guest(true, chained, true, 1_000_000);
        let mut halted = guest(false, false, true, 1_000_000);
        let end = waiting.machine.cycles_per_second() + 10_000;
        let mut saw_live_irq = false;
        while waiting.machine.cycle() < end {
            let next = waiting.machine.cycle().saturating_add(slice).min(end);
            advance(&mut waiting, next, slice);
            saw_live_irq |= waiting.machine.io.pic.diagnostic_snapshot().master.isr != 0;
        }
        if chained {
            assert!(
                saw_live_irq,
                "tiny slices must exercise retained unfinished IRQ frames"
            );
        }
        advance(&mut halted, end, slice);
        assert!(halted.machine.memory.read_u32(0x46c) >= 18);
        assert_eq!(
            waiting.machine.memory.read_u32(0x46c),
            halted.machine.memory.read_u32(0x46c),
            "BIOS wait must deliver every elapsed timer tick"
        );
        assert_eq!(
            waiting.machine.memory.read_u16(0x500),
            halted.machine.memory.read_u16(0x500),
            "installed INT1C callbacks must retain halted cadence"
        );
        assert_eq!(
            waiting.machine.memory.read_u16(0x502),
            halted.machine.memory.read_u16(0x502),
            "guest-installed IRQ0 handler must run at each timer edge"
        );
        assert_eq!(
            waiting.machine.cpu.regs.sp(),
            0x8000,
            "all service/IRQ frames must return"
        );
        assert_eq!(waiting.machine.cpu.regs.cs, 0x1000);
        assert_eq!(waiting.machine.cpu.regs.ax(), 0x005a);
        assert_eq!(waiting.machine.cpu.regs.bx(), 0x4321);
        assert!(waiting
            .machine
            .cpu
            .regs
            .flags
            .contains(Flags::DIRECTION | Flags::INTERRUPT));
        assert!(!waiting.machine.cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(waiting.machine.io.pic.diagnostic_snapshot().master.isr, 0);
    }
    #[test]
    fn bios_wait_direct_keeps_timer_cadence() {
        cadence(false, 25_000_000);
    }
    #[test]
    fn bios_wait_chained_keeps_timer_cadence_across_tiny_slices() {
        cadence(true, 7);
    }
    #[test]
    fn bios_wait_preserves_context_without_iret_exit() {
        let mut pc = guest(true, false, true, 1_000_000);
        pc.machine.cpu.far_return_watch = false;
        // INT1C returns with IF already set, without an IRET boundary exit.
        install(&mut pc.machine.memory, 0x2000, 12, &[0xfb, 0xcf]);
        install(
            &mut pc.machine.memory,
            0xf000,
            Bios::BIOS_INT8_ISR_OFF + 12,
            &[0xfb, 0xcf],
        );
        advance(&mut pc, 1_010_000, 731);
        assert_eq!(
            pc.machine.memory.read_u16(0x500),
            18,
            "STI/IRET callbacks must keep timer cadence"
        );
        assert_eq!(pc.machine.cpu.regs.sp(), 0x8000);
        assert_eq!(pc.machine.cpu.regs.ax(), 0x005a);
        assert_eq!(pc.machine.io.pic.diagnostic_snapshot().master.isr, 0);
    }
    #[test]
    fn bios_wait_nested_callback_retains_both_frames() {
        let mut pc = guest(true, false, true, 1_000_000);
        install(
            &mut pc.machine.memory,
            0x2000,
            0,
            &[
                0x50, 0x51, 0x52, 0x1e, 0xb4, 0x86, 0x31, 0xc9, 0xba, 0xe8, 0x03, 0xcd, 0x15, 0x31,
                0xc0, 0x8e, 0xd8, 0xff, 0x06, 0x00, 0x05, 0x1f, 0x5a, 0x59, 0x58, 0xcf,
            ],
        );
        advance(&mut pc, 100_000, 7);
        assert!(
            pc.machine.memory.read_u16(0x500) > 0,
            "nested AH86 must execute during the retained outer wait"
        );
        let outer_deadline = pc
            .bios_waits
            .first()
            .expect("outer wait is retained")
            .deadline;
        while pc.machine.cpu.regs.sp() != 0x8000 && pc.machine.cycle() < outer_deadline + 100 {
            assert!(matches!(pc.run_for(7).unwrap(), DosRunExit::Running));
        }
        assert!(
            pc.machine.cycle() <= outer_deadline + 32,
            "nested completion must recompute the parent deadline"
        );
        assert!(pc.bios_waits.is_empty());
        advance(&mut pc, 1_010_000, 7);
        assert_eq!(pc.machine.cpu.regs.sp(), 0x8000);
        assert_eq!(pc.machine.cpu.regs.cs, 0x1000);
        assert_eq!(pc.machine.cpu.regs.ax(), 0x005a);
        assert_eq!(pc.machine.io.pic.diagnostic_snapshot().master.isr, 0);
    }
    #[test]
    fn bios_wait_disabled_if_respects_slice_and_zero_return_flags() {
        let mut pc = guest(true, false, false, 12_345);
        let initial_flags = pc.machine.cpu.regs.flags;
        advance(&mut pc, 100, 100);
        assert!(
            pc.machine.cycle() < 12_345,
            "a BIOS wait must retain its caller across run_for budgets"
        );
        advance(&mut pc, 12_400, 17);
        assert_eq!(pc.machine.cpu.regs.sp(), 0x8000);
        assert_eq!(pc.machine.cpu.regs.ax(), 0x005a);
        assert_eq!(pc.machine.cpu.regs.flags, initial_flags - Flags::CARRY);
        assert_eq!(pc.machine.memory.read_u32(0x46c), 0);
        let mut zero = guest(true, false, true, 0);
        advance(&mut zero, 100, 100);
        assert_eq!(zero.machine.cpu.regs.sp(), 0x8000);
        assert_eq!(zero.machine.cpu.regs.ax(), 0x005a);
        assert!(zero
            .machine
            .cpu
            .regs
            .flags
            .contains(Flags::INTERRUPT | Flags::DIRECTION));
        assert!(!zero.machine.cpu.regs.flags.contains(Flags::CARRY));
    }
}
