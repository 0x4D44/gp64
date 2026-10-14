use super::test_helpers::setup;
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/// Mock I/O port that records writes and returns configured read values.
struct MockIo {
    /// Queue of u8 values returned by read_u8, in order.
    read_u8_queue: RefCell<VecDeque<u8>>,
    /// Queue of u16 values returned by read_u16, in order.
    read_u16_queue: RefCell<VecDeque<u16>>,
    /// Queue of u32 values returned by read_u32, in order.
    read_u32_queue: RefCell<VecDeque<u32>>,
    /// Recorded (port, value) pairs for write_u8 calls.
    written_u8: RefCell<Vec<(u16, u8)>>,
    /// Recorded (port, value) pairs for write_u16 calls.
    written_u16: RefCell<Vec<(u16, u16)>>,
    /// Recorded (port, value) pairs for write_u32 calls.
    written_u32: RefCell<Vec<(u16, u32)>>,
    /// Recorded port values for read_u8 calls.
    read_u8_ports: RefCell<Vec<u16>>,
    cpu_cycles: u64,
    read_u8_cycles: RefCell<Vec<u64>>,
    io_access_cycles: RefCell<Vec<u64>>,
    io_access_widths: RefCell<Vec<u8>>,
    io_delay_per_read: Cell<u64>,
    io_delay_total: Cell<u64>,
    break_batch: Cell<bool>,
}

impl MockIo {
    fn new() -> Self {
        Self {
            read_u8_queue: RefCell::new(VecDeque::new()),
            read_u16_queue: RefCell::new(VecDeque::new()),
            read_u32_queue: RefCell::new(VecDeque::new()),
            written_u8: RefCell::new(Vec::new()),
            written_u16: RefCell::new(Vec::new()),
            written_u32: RefCell::new(Vec::new()),
            read_u8_ports: RefCell::new(Vec::new()),
            cpu_cycles: 0,
            read_u8_cycles: RefCell::new(Vec::new()),
            io_access_cycles: RefCell::new(Vec::new()),
            io_access_widths: RefCell::new(Vec::new()),
            io_delay_per_read: Cell::new(0),
            io_delay_total: Cell::new(0),
            break_batch: Cell::new(false),
        }
    }

    fn push_read_u8(&self, val: u8) {
        self.read_u8_queue.borrow_mut().push_back(val);
    }

    fn push_read_u16(&self, val: u16) {
        self.read_u16_queue.borrow_mut().push_back(val);
    }

    fn push_read_u32(&self, val: u32) {
        self.read_u32_queue.borrow_mut().push_back(val);
    }

    fn set_break_batch(&self, value: bool) {
        self.break_batch.set(value);
    }

    fn set_io_delay_per_read(&self, cycles: u64) {
        self.io_delay_per_read.set(cycles);
    }

    fn total_io_delay(&self) -> u64 {
        self.io_delay_total.get()
    }

    fn record_io_access(&self, width: u8) {
        self.io_access_cycles.borrow_mut().push(self.cpu_cycles);
        self.io_access_widths.borrow_mut().push(width);
    }
}

impl IoPort for MockIo {
    fn read_u8(&mut self, port: u16) -> u8 {
        self.read_u8_ports.borrow_mut().push(port);
        self.read_u8_cycles.borrow_mut().push(self.cpu_cycles);
        self.record_io_access(1);
        self.io_delay_total.set(
            self.io_delay_total
                .get()
                .saturating_add(self.io_delay_per_read.get()),
        );
        self.read_u8_queue.borrow_mut().pop_front().unwrap_or(0xFF)
    }
    fn read_u16(&mut self, _port: u16) -> u16 {
        self.record_io_access(2);
        self.io_delay_total.set(
            self.io_delay_total
                .get()
                .saturating_add(self.io_delay_per_read.get()),
        );
        self.read_u16_queue
            .borrow_mut()
            .pop_front()
            .unwrap_or(0xFFFF)
    }
    fn read_u32(&mut self, _port: u16) -> u32 {
        self.record_io_access(4);
        self.io_delay_total.set(
            self.io_delay_total
                .get()
                .saturating_add(self.io_delay_per_read.get()),
        );
        self.read_u32_queue
            .borrow_mut()
            .pop_front()
            .unwrap_or(0xFFFF_FFFF)
    }
    fn write_u8(&mut self, port: u16, value: u8) {
        self.record_io_access(1);
        self.written_u8.borrow_mut().push((port, value));
    }
    fn write_u16(&mut self, port: u16, value: u16) {
        self.record_io_access(2);
        self.written_u16.borrow_mut().push((port, value));
    }
    fn write_u32(&mut self, port: u16, value: u32) {
        self.record_io_access(4);
        self.written_u32.borrow_mut().push((port, value));
    }
    fn set_cpu_cycles(&mut self, cycles: u64) {
        self.cpu_cycles = cycles;
    }

    fn should_break_batch(&self) -> bool {
        self.break_batch.get()
    }

    fn io_delay_total(&self) -> u64 {
        self.io_delay_total.get()
    }
}

struct TimedLatchIo {
    cpu_cycles: u64,
    pending_lsb: Option<u8>,
    reload: u16,
    programmed_at: u64,
    latched: Option<u16>,
    read_state: u8,
}

impl TimedLatchIo {
    fn new() -> Self {
        Self {
            cpu_cycles: 0,
            pending_lsb: None,
            reload: 0,
            programmed_at: 0,
            latched: None,
            read_state: 0,
        }
    }

    fn current_counter(&self) -> u16 {
        let elapsed = self.cpu_cycles.saturating_sub(self.programmed_at);
        self.reload.saturating_sub(elapsed as u16)
    }
}

impl IoPort for TimedLatchIo {
    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x40 => {
                let value = self.latched.unwrap_or_else(|| self.current_counter());
                let byte = if self.read_state == 0 {
                    value as u8
                } else {
                    (value >> 8) as u8
                };
                if self.read_state == 0 {
                    self.read_state = 1;
                } else {
                    self.read_state = 0;
                    self.latched = None;
                }
                byte
            }
            _ => 0xFF,
        }
    }

    fn read_u16(&mut self, port: u16) -> u16 {
        u16::from_le_bytes([self.read_u8(port), self.read_u8(port)])
    }

    fn read_u32(&mut self, port: u16) -> u32 {
        u32::from(self.read_u16(port)) | (u32::from(self.read_u16(port)) << 16)
    }

    fn write_u8(&mut self, port: u16, value: u8) {
        match port {
            0x43 => match value {
                0x34 => {
                    self.pending_lsb = None;
                    self.read_state = 0;
                }
                0x00 => {
                    self.latched = Some(self.current_counter());
                    self.read_state = 0;
                }
                _ => {}
            },
            0x40 => {
                if let Some(lsb) = self.pending_lsb.take() {
                    self.reload = u16::from_le_bytes([lsb, value]);
                    self.programmed_at = self.cpu_cycles;
                } else {
                    self.pending_lsb = Some(value);
                }
            }
            _ => {}
        }
    }

    fn write_u16(&mut self, port: u16, value: u16) {
        self.write_u8(port, value as u8);
        self.write_u8(port.wrapping_add(1), (value >> 8) as u8);
    }

    fn write_u32(&mut self, port: u16, value: u32) {
        self.write_u16(port, value as u16);
        self.write_u16(port.wrapping_add(2), (value >> 16) as u16);
    }

    fn set_cpu_cycles(&mut self, cycles: u64) {
        self.cpu_cycles = cycles;
    }
}

// ====================================================================
// IN AL, imm8 (0xE4)
// ====================================================================

#[test]
fn test_in_al_imm8() {
    // E4 42 => IN AL, 0x42
    let (mut cpu, mut mem) = setup(&[0xE4, 0x42]);
    let mut io = MockIo::new();
    io.push_read_u8(0xAB);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 0xAB);
    assert_eq!(cpu.regs.eip, 0x0102);
    assert_eq!(io.read_u8_ports.borrow()[0], 0x42);
}

// ====================================================================
// IN AX, imm8 (0xE5) — 16-bit
// ====================================================================

#[test]
fn test_in_ax_imm8() {
    // E5 80 => IN AX, 0x80
    let (mut cpu, mut mem) = setup(&[0xE5, 0x80]);
    let mut io = MockIo::new();
    io.push_read_u16(0xBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0xBEEF);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// IN EAX, imm8 (0x66 0xE5) — 32-bit with operand size prefix
// ====================================================================

#[test]
fn test_in_eax_imm8_32bit() {
    // 66 E5 80 => IN EAX, 0x80 (operand size override in 16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x66, 0xE5, 0x80]);
    let mut io = MockIo::new();
    io.push_read_u32(0xDEADBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
    assert_eq!(cpu.regs.eip, 0x0103);
}

// ====================================================================
// OUT imm8, AL (0xE6)
// ====================================================================

#[test]
fn test_out_imm8_al() {
    // E6 61 => OUT 0x61, AL
    let (mut cpu, mut mem) = setup(&[0xE6, 0x61]);
    cpu.regs.set_al(0x7F);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x61, 0x7F));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// OUT imm8, AX (0xE7)
// ====================================================================

#[test]
fn test_out_imm8_ax() {
    // E7 43 => OUT 0x43, AX
    let (mut cpu, mut mem) = setup(&[0xE7, 0x43]);
    cpu.regs.set_ax(0x1234);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x43, 0x1234));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// OUT imm8, EAX (0x66 0xE7) — 32-bit
// ====================================================================

#[test]
fn test_out_imm8_eax_32bit() {
    // 66 E7 43 => OUT 0x43, EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0xE7, 0x43]);
    cpu.regs.eax = 0xCAFEBABE;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x43, 0xCAFEBABE));
    assert_eq!(cpu.regs.eip, 0x0103);
}

// ====================================================================
// IN AL, DX (0xEC)
// ====================================================================

#[test]
fn test_in_al_dx() {
    // EC => IN AL, DX
    let (mut cpu, mut mem) = setup(&[0xEC]);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    io.push_read_u8(0x55);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 0x55);
    assert_eq!(cpu.regs.eip, 0x0101);
    assert_eq!(io.read_u8_ports.borrow()[0], 0x03F8);
}

// ====================================================================
// IN AX, DX (0xED) — 16-bit
// ====================================================================

#[test]
fn test_in_ax_dx() {
    // ED => IN AX, DX
    let (mut cpu, mut mem) = setup(&[0xED]);
    cpu.regs.set_dx(0x01F0);
    let mut io = MockIo::new();
    io.push_read_u16(0xABCD);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0xABCD);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// IN EAX, DX (0x66 0xED) — 32-bit
// ====================================================================

#[test]
fn test_in_eax_dx_32bit() {
    // 66 ED => IN EAX, DX
    let (mut cpu, mut mem) = setup(&[0x66, 0xED]);
    cpu.regs.set_dx(0x01F0);
    let mut io = MockIo::new();
    io.push_read_u32(0x12345678);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0x12345678);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// OUT DX, AL (0xEE)
// ====================================================================

#[test]
fn test_out_dx_al() {
    // EE => OUT DX, AL
    let (mut cpu, mut mem) = setup(&[0xEE]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.set_al(0x0D);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x03F8, 0x0D));
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// OUT DX, AX (0xEF) — 16-bit
// ====================================================================

#[test]
fn test_out_dx_ax() {
    // EF => OUT DX, AX
    let (mut cpu, mut mem) = setup(&[0xEF]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.set_ax(0xFACE);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01F0, 0xFACE));
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// OUT DX, EAX (0x66 0xEF) — 32-bit
// ====================================================================

#[test]
fn test_out_dx_eax_32bit() {
    // 66 EF => OUT DX, EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0xEF]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.eax = 0xDEADC0DE;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x01F0, 0xDEADC0DE));
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_out_imm8_al_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0xE6, 0x80, 0xF4]);
    cpu.regs.set_al(0x7E);
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x80, 0x7E));
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_out_imm8_ax_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0xE7, 0x81, 0xF4]);
    cpu.regs.set_ax(0xA11C);
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x81, 0xA11C));
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_out_imm8_eax_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0x66, 0xE7, 0x82, 0xF4]);
    cpu.regs.eax = 0xC0DE_32E7;
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x82, 0xC0DE_32E7));
    assert_eq!(cpu.regs.eip, 0x0103);
}

#[test]
fn test_in_ax_dx_breaks_batch_after_read() {
    let (mut cpu, mut mem) = setup(&[0xED, 0xF4]);
    cpu.regs.set_dx(0x0330);
    let mut io = MockIo::new();
    io.push_read_u16(0xD15C);
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0xD15C);
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_al_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0xEE, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.set_al(0x05);
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x01CF, 0x05));
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_ax_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0xEF, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.set_ax(0x0001);
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01CF, 0x0001));
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_eax_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0x66, 0xEF, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.eax = 0xC0DE_66EF;
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x01CF, 0xC0DE_66EF));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// OUT DX, AL to VGA port — inline sync, no batch break
// ====================================================================

#[test]
fn test_out_dx_al_vga_port_no_batch_break() {
    // OUT DX, AL where DX=0x3CF (Graphics Controller data register).
    // VGA I/O syncs pipeline state inline — batch continues running.
    // MockIo's default vga_pipeline_dirty() returns false, so no sync
    // actually occurs, but the batch must NOT break with IoCompleted.
    let (mut cpu, mut mem) = setup(&[0xEE]);
    cpu.regs.set_dx(0x03CF); // GC data register
    cpu.regs.set_al(0x01); // e.g., Write Mode 1
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "VGA port OUT should NOT break the batch, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x03CF, 0x01));
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_ax_vga_port_no_batch_break() {
    // OUT DX, AX where DX=0x3C4 (Sequencer index+data via word write).
    // Same as above — inline sync, no batch break.
    let (mut cpu, mut mem) = setup(&[0xEF]);
    cpu.regs.set_dx(0x03C4); // Sequencer index register
    cpu.regs.set_ax(0x0F02); // Map Mask = 0x0F, index 2
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "VGA port OUT should NOT break the batch, got {:?}",
        result
    );
    assert_eq!(io.written_u16.borrow()[0], (0x03C4, 0x0F02));
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_al_non_vga_port_no_batch_break() {
    // OUT DX, AL where DX=0x20 (PIC) — no VGA sync, no batch break.
    let (mut cpu, mut mem) = setup(&[0xEE]);
    cpu.regs.set_dx(0x0020); // PIC command register
    cpu.regs.set_al(0x20); // EOI
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "non-VGA OUT should not break the batch, got {:?}",
        result
    );
}

// ====================================================================
// OUT imm8 (0xE6) — VGA pipeline sync
// ====================================================================

/// Mock I/O that reports VGA pipeline dirty and returns a custom state.
struct VgaPipelineMockIo {
    inner: MockIo,
    pipeline_state: std::cell::Cell<Option<gp64_types::VgaPipelineState>>,
}

impl VgaPipelineMockIo {
    fn new(state: gp64_types::VgaPipelineState) -> Self {
        Self {
            inner: MockIo::new(),
            pipeline_state: std::cell::Cell::new(Some(state)),
        }
    }
}

impl IoPort for VgaPipelineMockIo {
    fn read_u8(&mut self, port: u16) -> u8 {
        self.inner.read_u8(port)
    }
    fn read_u16(&mut self, port: u16) -> u16 {
        self.inner.read_u16(port)
    }
    fn read_u32(&mut self, port: u16) -> u32 {
        self.inner.read_u32(port)
    }
    fn write_u8(&mut self, port: u16, value: u8) {
        self.inner.write_u8(port, value);
    }
    fn write_u16(&mut self, port: u16, value: u16) {
        self.inner.write_u16(port, value);
    }
    fn write_u32(&mut self, port: u16, value: u32) {
        self.inner.write_u32(port, value);
    }
    fn vga_pipeline_dirty(&self) -> bool {
        self.pipeline_state.get().is_some()
    }
    fn take_vga_pipeline_state(&mut self) -> gp64_types::VgaPipelineState {
        self.pipeline_state.take().unwrap_or_default()
    }
}

#[test]
fn test_out_imm8_al_vga_pipeline_no_sync_low_port() {
    // OUT imm8, AL can only address ports 0x00-0xFF.  VGA registers live
    // at 0x3B0-0x3DF, so the VGA pipeline sync path can never fire for
    // this opcode in practice.  Verify the instruction still executes
    // correctly and does NOT touch pipeline state even when dirty.
    let (mut cpu, mut mem) = setup(&[0xE6, 0xC5]); // OUT 0xC5, AL
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    cpu.regs.set_al(0x42);

    let pipeline = gp64_types::VgaPipelineState {
        video_mode: 0x13,
        chain4: false,
        ..Default::default()
    };
    let mut io = VgaPipelineMockIo::new(pipeline);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "immediate OUT should not break batch, got {:?}",
        result
    );
    // Port 0xC5 is outside VGA range, so pipeline must NOT be applied.
    assert!(
        mem.vga.chain4,
        "chain4 must remain true — port 0xC5 is outside VGA range"
    );
    assert_eq!(io.inner.written_u8.borrow()[0], (0xC5, 0x42));
}

#[test]
fn test_out_imm8_ax_vga_pipeline_no_sync_low_port() {
    // OUT imm8, AX — same port-range limitation as above.
    let (mut cpu, mut mem) = setup(&[0xE7, 0xC4]); // OUT 0xC4, AX
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    cpu.regs.set_ax(0x0604);

    let pipeline = gp64_types::VgaPipelineState {
        video_mode: 0x13,
        chain4: false,
        ..Default::default()
    };
    let mut io = VgaPipelineMockIo::new(pipeline);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "immediate OUT should not break batch, got {:?}",
        result
    );
    assert!(
        mem.vga.chain4,
        "chain4 must remain true — port 0xC4 is outside VGA range"
    );
    assert_eq!(io.inner.written_u16.borrow()[0], (0xC4, 0x0604));
}

#[test]
fn test_out_dx_al_vga_pipeline_sync_applies() {
    // OUT DX, AL (0xEE) where DX=0x3C5 — VGA Sequencer data register.
    // Start in Mode 13h with chain-4 enabled.  The mock IO signals
    // pipeline dirty and returns chain4=false.  After the instruction,
    // Memory must reflect the updated pipeline (is_planar_mode() == true).
    let (mut cpu, mut mem) = setup(&[0xEE]); // OUT DX, AL
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    cpu.regs.set_dx(0x03C5);
    cpu.regs.set_al(0x06);

    let mut pipeline = gp64_types::VgaPipelineState {
        video_mode: 0x13,
        chain4: false,   // chain-4 disabled
        is_mode_x: true, // confirmed Mode X (CR9/CR17 reprogrammed)
        plane_mask: 0x0F,
        ..Default::default()
    };
    pipeline.memory_map_select = mem.vga.memory_map_select;

    let mut io = VgaPipelineMockIo::new(pipeline);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "VGA port OUT should not break batch, got {:?}",
        result
    );
    assert!(
        mem.is_planar_mode(),
        "Memory should be in planar mode after VGA pipeline sync"
    );
    assert!(
        !mem.vga.chain4,
        "chain4 should be false after pipeline sync"
    );
    assert_eq!(io.inner.written_u8.borrow()[0], (0x3C5, 0x06));
}

// ====================================================================
// INSB (0x6C) — no REP
// ====================================================================

#[test]
fn test_io_timing_uses_current_cycle_for_latched_reads() {
    let mut code = vec![
        0xBA, 0x43, 0x00, // MOV DX, 0x43
        0xB0, 0x34, // MOV AL, 0x34
        0xEE, // OUT DX, AL
        0xBA, 0x40, 0x00, // MOV DX, 0x40
        0xB0, 0x00, // MOV AL, 0x00
        0xEE, // OUT DX, AL
        0xB0, 0x80, // MOV AL, 0x80
        0xEE, // OUT DX, AL
    ];
    code.extend(std::iter::repeat_n(0x90, 100));
    code.extend([
        0xBA, 0x43, 0x00, // MOV DX, 0x43
        0xB0, 0x00, // MOV AL, 0x00
        0xEE, // OUT DX, AL (latch)
        0xBA, 0x40, 0x00, // MOV DX, 0x40
        0xEC, // IN AL, DX
        0x88, 0xC3, // MOV BL, AL
        0xEC, // IN AL, DX
        0x88, 0xC7, // MOV BH, AL
    ]);

    let (mut cpu, mut mem) = setup(&code);
    let mut io = TimedLatchIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 116);

    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 116 }
    ));
    assert!(
        cpu.regs.bx() < 0x8000,
        "latched counter should reflect elapsed instructions, got {:04X}",
        cpu.regs.bx()
    );
}

#[test]
fn test_insb() {
    // 6C => INSB
    let (mut cpu, mut mem) = setup(&[0x6C]);
    cpu.regs.set_dx(0x03DA); // port
    cpu.regs.es = 0x0020; // ES segment
    cpu.regs.set_di(0x0010); // DI
    let mut io = MockIo::new();
    io.push_read_u8(0x42);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // Byte should be stored at ES:DI = 0x0020*16 + 0x0010 = 0x0210
    assert_eq!(mem.ram[0x0210], 0x42);
    // DI should have advanced by 1 (DF=0)
    assert_eq!(cpu.regs.di(), 0x0011);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// INSB with DF=1 (decrement)
// ====================================================================

#[test]
fn test_insb_direction_flag() {
    let (mut cpu, mut mem) = setup(&[0x6C]);
    cpu.regs.set_dx(0x03DA);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = MockIo::new();
    io.push_read_u8(0x99);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0210], 0x99);
    // DI should have decremented by 1
    assert_eq!(cpu.regs.di(), 0x000F);
}

// ====================================================================
// INSW (0x6D) — no REP, 16-bit
// ====================================================================

#[test]
fn test_insw() {
    // 6D => INSW
    let (mut cpu, mut mem) = setup(&[0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    let mut io = MockIo::new();
    io.push_read_u16(0xBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // Word at ES:DI = 0x0210
    let val = u16::from_le_bytes([mem.ram[0x0210], mem.ram[0x0211]]);
    assert_eq!(val, 0xBEEF);
    // DI advanced by 2
    assert_eq!(cpu.regs.di(), 0x0012);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// INSD (0x66 0x6D) — 32-bit operand size
// ====================================================================

#[test]
fn test_insd() {
    // 66 6D => INSD
    let (mut cpu, mut mem) = setup(&[0x66, 0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    let mut io = MockIo::new();
    io.push_read_u32(0xDEADBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let val = u32::from_le_bytes([
        mem.ram[0x0210],
        mem.ram[0x0211],
        mem.ram[0x0212],
        mem.ram[0x0213],
    ]);
    assert_eq!(val, 0xDEADBEEF);
    // DI advanced by 4
    assert_eq!(cpu.regs.di(), 0x0014);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// OUTSB (0x6E) — no REP
// ====================================================================

#[test]
fn test_outsb() {
    // 6E => OUTSB
    let (mut cpu, mut mem) = setup(&[0x6E]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    // Place data at DS:SI = 0x0030*16 + 0x0020 = 0x0320
    mem.ram[0x0320] = 0x7E;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x03F8, 0x7E));
    // SI advanced by 1
    assert_eq!(cpu.regs.si(), 0x0021);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// OUTSW (0x6F) — no REP, 16-bit
// ====================================================================

#[test]
fn test_outsw() {
    // 6F => OUTSW
    let (mut cpu, mut mem) = setup(&[0x6F]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    // Place word at DS:SI = 0x0320
    mem.ram[0x0320] = 0xEF;
    mem.ram[0x0321] = 0xBE;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01F0, 0xBEEF));
    // SI advanced by 2
    assert_eq!(cpu.regs.si(), 0x0022);
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_outsw_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0x6F, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    mem.ram[0x0320] = 0x05;
    mem.ram[0x0321] = 0x00;
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01CF, 0x0005));
    assert_eq!(cpu.regs.si(), 0x0022);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ====================================================================
// OUTSD (0x66 0x6F) — 32-bit operand size
// ====================================================================

#[test]
fn test_outsd() {
    // 66 6F => OUTSD
    let (mut cpu, mut mem) = setup(&[0x66, 0x6F]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    // Place dword at DS:SI = 0x0320
    let bytes = 0xCAFEBABEu32.to_le_bytes();
    mem.ram[0x0320] = bytes[0];
    mem.ram[0x0321] = bytes[1];
    mem.ram[0x0322] = bytes[2];
    mem.ram[0x0323] = bytes[3];
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x01F0, 0xCAFEBABE));
    // SI advanced by 4
    assert_eq!(cpu.regs.si(), 0x0024);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_outsd_breaks_batch_after_write() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x6F, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    let bytes = 0xB16B_006F_u32.to_le_bytes();
    mem.ram[0x0320] = bytes[0];
    mem.ram[0x0321] = bytes[1];
    mem.ram[0x0322] = bytes[2];
    mem.ram[0x0323] = bytes[3];
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x01CF, 0xB16B_006F));
    assert_eq!(cpu.regs.si(), 0x0024);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ====================================================================
// REP INSB (0xF3 0x6C) — multiple iterations
// ====================================================================

#[test]
fn test_rep_insb() {
    // F3 6C => REP INSB; execute one iteration per call
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6C]);
    cpu.regs.set_dx(0x03DA);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(3);
    let mut io = MockIo::new();

    // Iteration 1: CX=3 -> CX=2, DI advances, IP stays at start
    io.push_read_u8(0xAA);
    let r1 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r1, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0210], 0xAA);
    assert_eq!(cpu.regs.di(), 0x0011);
    assert_eq!(cpu.regs.cx(), 2);
    assert_eq!(cpu.regs.eip, 0x0100); // IP not advanced (more iterations)

    // Iteration 2: CX=2 -> CX=1
    io.push_read_u8(0xBB);
    let r2 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r2, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0211], 0xBB);
    assert_eq!(cpu.regs.di(), 0x0012);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.eip, 0x0100); // still at start

    // Iteration 3: CX=1 -> CX=0, IP advances past instruction
    io.push_read_u8(0xCC);
    let r3 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r3, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0212], 0xCC);
    assert_eq!(cpu.regs.di(), 0x0013);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.eip, 0x0102); // IP advanced past F3 6C
}

// ====================================================================
// REP INSB with CX=0 — should skip, no I/O
// ====================================================================

#[test]
fn test_rep_insb_cx_zero() {
    // F3 6C with CX=0: skip entirely
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6C]);
    cpu.regs.set_dx(0x03DA);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // CX=0 means no I/O, falls through to normal epilogue
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.di(), 0x0010); // DI unchanged
    assert_eq!(cpu.regs.eip, 0x0102); // IP advanced past the instruction
}

#[test]
fn test_rep_insb_cx_zero_skips_es_limit_check() {
    // MDD-BUG-ANVIL-00006: REP INS with CX==0 performs no operation and must
    // NOT run the I/O-permission or ES:DI segment-limit checks. Force a would-be
    // #GP by giving ES a zero cached limit with DI beyond it; with CX==0 the
    // instruction must be skipped, leaving CX/DI untouched and reading no port.
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6C]); // REP INSB
    cpu.regs.es_cache.limit = 0;
    cpu.regs.es_cache.granularity = false;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.set_dx(0x03DA);
    cpu.regs.set_di(0x0100); // beyond the ES limit of 0
    cpu.regs.set_cx(0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "REP INSB with CX==0 must skip without faulting, got {result:?}"
    );
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.di(), 0x0100, "DI unchanged (no iteration ran)");
    assert_eq!(cpu.regs.eip, 0x0102, "IP advanced past the instruction");
    assert!(
        io.read_u8_ports.borrow().is_empty(),
        "no port read when CX==0"
    );
}

#[test]
fn test_rep_outsb_cx_zero_skips_ds_limit_check() {
    // MDD-BUG-ANVIL-00006 (OUTS arm): REP OUTS with CX==0 must NOT run the
    // I/O-permission or DS:SI segment-limit checks. DS has a zero cached limit
    // with SI beyond it; CX==0 must skip without faulting or writing a port.
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6E]); // REP OUTSB
    cpu.regs.ds_cache.limit = 0;
    cpu.regs.ds_cache.granularity = false;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.set_dx(0x03F8);
    cpu.regs.set_si(0x0100); // beyond the DS limit of 0
    cpu.regs.set_cx(0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "REP OUTSB with CX==0 must skip without faulting, got {result:?}"
    );
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.si(), 0x0100, "SI unchanged (no iteration ran)");
    assert_eq!(cpu.regs.eip, 0x0102, "IP advanced past the instruction");
    assert!(
        io.written_u8.borrow().is_empty(),
        "no port write when CX==0"
    );
}

#[test]
fn lock_out_dx_al_raises_ud_without_io() {
    // MDD-BUG-ANVIL-00004: LOCK OUT DX,AL (F0 EE) is an invalid LOCK, so it must
    // #UD BEFORE any I/O. Previously OUT performed the port write (and could
    // return early) before the epilogue LOCK check ran, leaking the I/O.
    let (mut cpu, mut mem) = setup(&[0xF0, 0xEE]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.eax = 0x5A; // AL = 0x5A
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK OUT must raise #UD; got {result:?}"
    );
    assert!(
        io.written_u8.borrow().is_empty(),
        "no port write may occur before the #UD"
    );
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "#UD must be reported at the original instruction IP"
    );
}

// ====================================================================
// REP OUTSB (0xF3 0x6E) — multiple iterations
// ====================================================================

#[test]
fn test_rep_outsb() {
    // F3 6E => REP OUTSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6E]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    cpu.regs.set_cx(2);
    // Place data at DS:SI
    mem.ram[0x0320] = 0x41; // 'A'
    mem.ram[0x0321] = 0x42; // 'B'
    let mut io = MockIo::new();

    // Iteration 1
    let r1 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r1, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x03F8, 0x41));
    assert_eq!(cpu.regs.si(), 0x0021);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.eip, 0x0100); // more iterations

    // Iteration 2 (last)
    let r2 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r2, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[1], (0x03F8, 0x42));
    assert_eq!(cpu.regs.si(), 0x0022);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.eip, 0x0102); // IP advanced
}

// ====================================================================
// REP OUTSB with CX=0 — should skip
// ====================================================================

#[test]
fn test_rep_outsb_cx_zero() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6E]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    cpu.regs.set_cx(0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.si(), 0x0020); // SI unchanged
}

// ====================================================================
// REP INSW (0xF3 0x6D) — 16-bit word I/O with REP
// ====================================================================

#[test]
fn test_rep_insw() {
    // F3 6D => REP INSW
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(2);
    let mut io = MockIo::new();

    // Iteration 1
    io.push_read_u16(0x1234);
    let r1 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r1, RunResult::BudgetExhausted { executed: 1 }));
    let v1 = u16::from_le_bytes([mem.ram[0x0210], mem.ram[0x0211]]);
    assert_eq!(v1, 0x1234);
    assert_eq!(cpu.regs.di(), 0x0012);
    assert_eq!(cpu.regs.cx(), 1);

    // Iteration 2 (last)
    io.push_read_u16(0x5678);
    let r2 = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(r2, RunResult::BudgetExhausted { executed: 1 }));
    let v2 = u16::from_le_bytes([mem.ram[0x0212], mem.ram[0x0213]]);
    assert_eq!(v2, 0x5678);
    assert_eq!(cpu.regs.di(), 0x0014);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_cycle_budget_counts_rep_insw_io_delay() {
    // Dense PIO loops must return to the scheduler once charged work plus
    // accumulated I/O delay consumes the current event quantum.
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(10);
    let mut io = MockIo::new();
    io.set_io_delay_per_read(5);
    for value in 0x1000..0x100A {
        io.push_read_u16(value);
    }

    // CRUCIBLE-00329: a 12-cycle ceiling buys exactly ONE REP element,
    // because a committed element now costs the 12 cycles the constant
    // declares. This assertion previously read `executed: 2` /
    // `charged_cycles: 2`, which pinned the defect — two elements retiring
    // for two cycles between them.
    let result =
        run_ext_charged_until_cycle_budget(&mut cpu, &mut mem, &mut io, &mut NoExec, 10, false, 12);

    assert!(
        matches!(result.result, RunResult::BudgetExhausted { executed: 1 }),
        "one 12-cycle element fills a 12-cycle quantum: {:?}",
        result.result
    );
    assert_eq!(result.charged_cycles, REP_INS_OUTS_ELEMENT_CHARGED_CYCLES);
    assert_eq!(io.total_io_delay(), 5);
    assert_eq!(cpu.regs.cx(), 9, "the remainder stays restartable");
    assert_eq!(cpu.regs.di(), 0x0012);
    assert_eq!(cpu.regs.eip, 0x0100, "EIP still points at the REP prefix");
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0210], mem.ram[0x0211]]),
        0x1000
    );
}

/// CRUCIBLE-00329 — string I/O must cost what the scalar I/O it repeats
/// costs. Before the fix every 6C-6F exit charged the default 1, so device
/// time and scheduler accounting lagged `IN`/`OUT` by ~10x.
#[test]
fn scalar_ins_is_charged_like_scalar_in() {
    // INSW (no REP)
    let (mut cpu, mut mem) = setup(&[0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    let mut io = MockIo::new();
    io.push_read_u16(0x1234);

    let result =
        run_ext_charged_until_cycle_budget(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false, 64);

    assert_eq!(
        result.charged_cycles, SCALAR_IO_CHARGED_CYCLES,
        "a non-REP INS is one scalar I/O"
    );
    assert_eq!(cpu.regs.di(), 0x0012);
}

#[test]
fn scalar_outs_is_charged_like_scalar_out() {
    // OUTSW (no REP)
    let (mut cpu, mut mem) = setup(&[0x6F]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.set_si(0x0010);
    let mut io = MockIo::new();

    let result =
        run_ext_charged_until_cycle_budget(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false, 64);

    assert_eq!(
        result.charged_cycles, SCALAR_IO_CHARGED_CYCLES,
        "a non-REP OUTS is one scalar I/O"
    );
    assert_eq!(cpu.regs.si(), 0x0012);
}

#[test]
fn rep_ins_charges_twelve_cycles_for_each_committed_element() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(3);
    let mut io = MockIo::new();
    for value in 0x2000..0x2003 {
        io.push_read_u16(value);
    }

    // Budget exactly the three REP retirements: a generous INSTRUCTION
    // budget would run on into the zero padding after the REP and add one
    // cycle per garbage instruction to the total.
    let result =
        run_ext_charged_until_cycle_budget(&mut cpu, &mut mem, &mut io, &mut NoExec, 3, false, 512);

    assert_eq!(cpu.regs.cx(), 0, "all three elements committed");
    assert_eq!(
        result.charged_cycles,
        3 * REP_INS_OUTS_ELEMENT_CHARGED_CYCLES,
        "each committed element costs its declared charge"
    );
}

#[test]
fn rep_ins_with_zero_count_performs_no_io_and_is_not_charged_as_one() {
    // CX == 0 does no I/O at all, so it must not be billed a scalar access.
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6D]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.es = 0x0020;
    cpu.regs.set_di(0x0010);
    cpu.regs.set_cx(0);
    let mut io = MockIo::new();

    let result =
        run_ext_charged_until_cycle_budget(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false, 64);

    assert_eq!(cpu.regs.di(), 0x0010, "no element was transferred");
    assert_eq!(
        result.charged_cycles, NORMAL_INSTRUCTION_CHARGED_CYCLES,
        "a zero-count REP is a decode and a test, not an I/O access"
    );
}

// ====================================================================
// REP OUTSW (0xF3 0x6F) — 16-bit word I/O with REP
// ====================================================================

#[test]
fn test_rep_outsw() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6F]);
    cpu.regs.set_dx(0x01F0);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    cpu.regs.set_cx(1);
    // Place word at DS:SI = 0x0320
    mem.ram[0x0320] = 0xCD;
    mem.ram[0x0321] = 0xAB;
    let mut io = MockIo::new();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01F0, 0xABCD));
    assert_eq!(cpu.regs.si(), 0x0022);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_rep_outsw_breaks_batch_after_committed_element() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6F, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    cpu.regs.set_cx(2);
    mem.ram[0x0320] = 0x05;
    mem.ram[0x0321] = 0x00;
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u16.borrow()[0], (0x01CF, 0x0005));
    assert_eq!(cpu.regs.si(), 0x0022);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_rep_outsd_breaks_batch_after_committed_element() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x66, 0x6F, 0xF4]);
    cpu.regs.set_dx(0x01CF);
    cpu.regs.ds = 0x0030;
    cpu.regs.set_si(0x0020);
    cpu.regs.set_cx(2);
    let bytes = 0xB16B_F36F_u32.to_le_bytes();
    mem.ram[0x0320] = bytes[0];
    mem.ram[0x0321] = bytes[1];
    mem.ram[0x0322] = bytes[2];
    mem.ram[0x0323] = bytes[3];
    let mut io = MockIo::new();
    io.set_break_batch(true);

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(io.written_u32.borrow()[0], (0x01CF, 0xB16B_F36F));
    assert_eq!(cpu.regs.si(), 0x0024);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_charged_cycles_rep_stosb_preserves_executed_count() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(4);
    cpu.regs.set_di(0x0200);

    let mut io = MockIo::new();
    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 4, false);

    assert!(matches!(
        result.result,
        RunResult::BudgetExhausted { executed: 4 }
    ));
    assert_eq!(result.charged_cycles, 16);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn vga_write_delay_charges_each_committed_aperture_byte() {
    let (mut cpu, mut mem) = setup(&[0xA3, 0x00, 0x00]);
    cpu.regs.ds = 0xA000;
    cpu.regs.set_ax(0x5AA5);
    let result = run_ext_charged_until_cycle_budget_with_vga_delay(
        &mut cpu,
        &mut mem,
        &mut (),
        &mut NoExec,
        1,
        false,
        1_000,
        48,
    );

    assert_eq!(result.charged_cycles, 97);
    assert_eq!(mem.vga_bus_write_count(), 2);
}

#[test]
fn vga_write_delay_stops_before_the_next_instruction() {
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x00, 0xA2, 0x01, 0x00]);
    cpu.regs.ds = 0xA000;
    cpu.regs.set_al(0x5A);
    let result = run_ext_charged_until_cycle_budget_with_vga_delay(
        &mut cpu,
        &mut mem,
        &mut (),
        &mut NoExec,
        2,
        false,
        49,
        48,
    );

    assert!(matches!(
        result.result,
        RunResult::BudgetExhausted { executed: 1 }
    ));
    assert_eq!(result.charged_cycles, 49);
    assert_eq!(cpu.regs.eip, 0x0103);
    assert_eq!(mem.vga_bus_write_count(), 1);
}

// ====================================================================
// MDD-BUG-CRUCIBLE-00330 — a bulk REP chunk must fit the caller's
// remaining charged-cycle quantum.
//
// REP MOVS/STOS pick their chunk from CX and the INSTRUCTION budget. That
// is fine for the one-cycle-per-element string forms, but these cost
// REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES each, so an instruction budget let a
// single REP overshoot the next device event by ~that factor.
// ====================================================================

#[test]
fn rep_stos_bulk_chunk_stops_at_the_charged_cycle_ceiling() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(100);
    cpu.regs.set_di(0x0200);
    let mut io = MockIo::new();

    // A one-element quantum. The instruction budget is deliberately huge:
    // before the fix it, not the cycle ceiling, chose the chunk.
    let result = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES as u64,
    );

    assert_eq!(cpu.regs.cx(), 99, "exactly one element may commit");
    assert_eq!(cpu.regs.di(), 0x0201);
    assert_eq!(
        result.charged_cycles, REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES,
        "and it is charged its declared cost"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "the remainder stays restartable");
    assert_eq!(mem.ram[0x0200], 0x5A);
    assert_eq!(
        mem.ram[0x0201], 0x00,
        "the second element must NOT be written"
    );
}

#[test]
fn rep_stos_resumes_decoded_bytes_after_overwriting_itself() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAB, 0xF4]);
    cpu.regs.set_ax(0xF4F4);
    cpu.regs.set_cx(2);
    cpu.regs.set_di(0x0100);
    let mut io = MockIo::new();

    let first = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES as u64,
    );
    assert!(matches!(first.result, RunResult::BudgetExhausted { .. }));
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(&mem.ram[0x0100..0x0102], &[0xF4, 0xF4]);

    let second = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES as u64,
    );
    assert!(matches!(second.result, RunResult::BudgetExhausted { .. }));
    assert_eq!(cpu.regs.cx(), 0, "the latched REP must finish");
    assert_eq!(cpu.regs.di(), 0x0104);
    assert_eq!(cpu.regs.eip, 0x0102);

    let third = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(third, RunResult::Halt { .. }));
}

#[test]
fn rep_movs_bulk_chunk_stops_at_the_charged_cycle_ceiling() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    cpu.regs.set_cx(100);
    cpu.regs.set_si(0x0300);
    cpu.regs.set_di(0x0400);
    for i in 0..8 {
        mem.ram[0x0300 + i] = 0xE0 + i as u8;
    }
    let mut io = MockIo::new();

    let result = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES as u64,
    );

    assert_eq!(cpu.regs.cx(), 99, "exactly one element may commit");
    assert_eq!(cpu.regs.si(), 0x0301);
    assert_eq!(cpu.regs.di(), 0x0401);
    assert_eq!(result.charged_cycles, REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES);
    assert_eq!(mem.ram[0x0400], 0xE0);
    assert_eq!(
        mem.ram[0x0401], 0x00,
        "the second element must NOT be copied"
    );
}

#[test]
fn rep_stos_bulk_chunk_scales_with_the_quantum() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(100);
    cpu.regs.set_di(0x0200);
    let mut io = MockIo::new();

    // Room for exactly five elements.
    let result = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        5 * REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES as u64,
    );

    assert_eq!(cpu.regs.cx(), 95, "five elements fit the quantum");
    assert_eq!(
        result.charged_cycles,
        5 * REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES
    );
    assert_eq!(mem.ram[0x0204], 0x5A);
    assert_eq!(mem.ram[0x0205], 0x00);
}

#[test]
fn rep_bulk_always_makes_forward_progress_on_a_spent_quantum() {
    // A quantum too small for even one element must still commit one,
    // otherwise a spent budget would spin without progress.
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(100);
    cpu.regs.set_di(0x0200);
    let mut io = MockIo::new();

    let result = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        4096,
        false,
        1,
    );

    assert_eq!(cpu.regs.cx(), 99, "one element, never zero");
    assert_eq!(result.charged_cycles, REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES);
}

#[test]
fn rep_bulk_is_unclamped_when_the_caller_sets_no_cycle_budget() {
    // The clamp must not degrade the unbudgeted bulk path: with no cycle
    // budget the whole REP still completes in one go.
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(64);
    cpu.regs.set_di(0x0200);
    let mut io = MockIo::new();

    // Budget exactly the 64 REP retirements: a larger INSTRUCTION budget
    // runs on into the zero padding after the REP and bills a cycle per
    // garbage instruction.
    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 64, false);

    assert_eq!(cpu.regs.cx(), 0, "the entire REP retires");
    assert_eq!(
        result.charged_cycles,
        64 * REP_MOVS_STOS_ELEMENT_CHARGED_CYCLES
    );
    assert_eq!(mem.ram[0x023F], 0x5A);
}

#[test]
fn test_hlt_charged_cycles_match_retired_count() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = MockIo::new();

    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false);

    assert!(matches!(&result.result, RunResult::Halt { executed: 1 }));
    assert_eq!(result.charged_cycles, 1);
    assert_eq!(result.result.legacy_cycle_count(), 1);
}

#[test]
fn test_nop_hlt_charged_cycles_match_retired_count() {
    let (mut cpu, mut mem) = setup(&[0x90, 0xF4]);
    let mut io = MockIo::new();

    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 2, false);

    assert!(matches!(&result.result, RunResult::Halt { executed: 2 }));
    assert_eq!(result.charged_cycles, 2);
    assert_eq!(result.result.legacy_cycle_count(), 2);
}

#[test]
fn test_charged_cycles_rep_then_int3_adds_interrupt_charge() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA, 0xCC]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(4);
    cpu.regs.set_di(0x0200);

    let mut io = MockIo::new();
    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 5, false);

    assert!(matches!(
        result.result,
        RunResult::SoftwareInterrupt {
            vector: 3,
            executed: 5
        }
    ));
    assert_eq!(result.charged_cycles, 17);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_in_batch_io_cycles_use_charged_elapsed_cycles() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA, 0xE4, 0x42]);
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(4);
    cpu.regs.set_di(0x0200);

    let mut io = MockIo::new();
    io.push_read_u8(0xAB);
    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 5, false);

    assert!(matches!(
        result.result,
        RunResult::BudgetExhausted { executed: 5 }
    ));
    assert_eq!(result.charged_cycles, 26);
    assert_eq!(cpu.regs.al(), 0xAB);
    assert_eq!(io.read_u8_cycles.borrow().as_slice(), &[16]);
}

#[test]
fn test_charged_cycles_scalar_out_preserves_executed_count() {
    let (mut cpu, mut mem) = setup(&[0xEE]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.set_al(0x7E);

    let mut io = MockIo::new();
    let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false);

    assert!(matches!(
        result.result,
        RunResult::BudgetExhausted { executed: 1 }
    ));
    assert_eq!(result.charged_cycles, 10);
    assert_eq!(io.written_u8.borrow()[0], (0x03F8, 0x7E));
}

fn string_io_charged_cycles_case(is_ins: bool, width: usize, addr32: bool, rep: bool) {
    const PORT: u16 = 0x1234;
    const IO_BASE: usize = 0x0204;
    const OUTS_BASE: usize = 0x0400;
    const INPUT_VALUES: [u32; 2] = [0x1234_ABCD, 0x7654_3210];
    const REP_ELEMENTS: usize = 2;
    const IO_DELAY: u64 = 5;

    let io_count = if rep { REP_ELEMENTS } else { 1 };
    let opcode = match (is_ins, width) {
        (true, 1) => 0x6C,
        (true, 2 | 4) => 0x6D,
        (false, 1) => 0x6E,
        (false, 2 | 4) => 0x6F,
        _ => panic!("unsupported string I/O width {width}"),
    };

    // REP INS/OUTS gets its count from the register that REP STOSB just
    // consumed, so load the target count after STOSB within the same batch.
    let mut code = vec![0xF3, 0xAA];
    if rep {
        if addr32 {
            code.extend_from_slice(&[0x66, 0xB9, 0x02, 0x00, 0x00, 0x00]); // MOV ECX, 2
        } else {
            code.extend_from_slice(&[0xB9, 0x02, 0x00]); // MOV CX, 2
        }
    }
    if addr32 {
        code.push(0x67);
    }
    if rep {
        code.push(0xF3);
    }
    if width == 4 {
        code.push(0x66);
    }
    code.push(opcode);

    let (mut cpu, mut mem) = setup(&code);
    cpu.cycles = 1_000;
    cpu.regs.set_al(0x5A);
    cpu.regs.set_cx(4);
    cpu.regs.set_di(0x0200);
    cpu.regs.set_si(OUTS_BASE as u16);

    let mut io = MockIo::new();
    io.set_io_delay_per_read(IO_DELAY);
    cpu.regs.set_dx(PORT);

    for (index, value) in INPUT_VALUES.iter().copied().take(io_count).enumerate() {
        if is_ins {
            match width {
                1 => io.push_read_u8(value as u8),
                2 => io.push_read_u16(value as u16),
                4 => io.push_read_u32(value),
                _ => unreachable!(),
            }
        } else {
            let start = OUTS_BASE + index * width;
            mem.ram[start..start + width].copy_from_slice(&value.to_le_bytes()[..width]);
        }
    }

    let expected_io_charge = if rep {
        REP_INS_OUTS_ELEMENT_CHARGED_CYCLES * io_count as u32
    } else {
        SCALAR_IO_CHARGED_CYCLES
    };
    let count_load_retired = if rep { 1 } else { 0 };
    let expected_retired = 4 + count_load_retired + io_count as u32;
    let result = run_ext_charged(
        &mut cpu,
        &mut mem,
        &mut io,
        &mut NoExec,
        expected_retired,
        false,
    );

    assert!(matches!(
        result.result,
        RunResult::BudgetExhausted { executed } if executed == expected_retired
    ));
    assert_eq!(
        result.charged_cycles,
        16 + count_load_retired + expected_io_charge,
        "the four STOSB retirements, REP count load, and each string I/O transfer keep their charges"
    );
    assert_eq!(
        io.io_access_widths.borrow().as_slice(),
        vec![width as u8; io_count].as_slice(),
        "each transfer uses the selected byte, word, or dword path"
    );

    let io_start_cycle = 1_016 + u64::from(count_load_retired);
    let expected_cycles = if rep {
        let second_access_delay = if is_ins { IO_DELAY } else { 0 };
        vec![
            io_start_cycle,
            io_start_cycle + u64::from(REP_INS_OUTS_ELEMENT_CHARGED_CYCLES) + second_access_delay,
        ]
    } else {
        vec![io_start_cycle]
    };
    assert_eq!(
        io.io_access_cycles.borrow().as_slice(),
        expected_cycles.as_slice(),
        "I/O timestamps include the 1,000-cycle base, REP element charges, and prior read delay"
    );
    assert_eq!(
        io.total_io_delay(),
        if is_ins {
            IO_DELAY * io_count as u64
        } else {
            0
        },
        "read delay accumulates separately from guest charged cycles"
    );

    if is_ins {
        for (index, value) in INPUT_VALUES.iter().copied().take(io_count).enumerate() {
            let start = IO_BASE + index * width;
            assert_eq!(
                &mem.ram[start..start + width],
                &value.to_le_bytes()[..width],
                "INS transfer {index} writes the expected value"
            );
        }
        let final_di = IO_BASE + io_count * width;
        assert_eq!(
            if addr32 {
                cpu.regs.edi
            } else {
                u32::from(cpu.regs.di())
            },
            final_di as u32,
            "INS advances the selected address-size index"
        );
    } else {
        let expected_values = INPUT_VALUES
            .iter()
            .copied()
            .take(io_count)
            .map(|value| match width {
                1 => (PORT, value as u8 as u32),
                2 => (PORT, value as u16 as u32),
                4 => (PORT, value),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let writes = match width {
            1 => io
                .written_u8
                .borrow()
                .iter()
                .map(|(port, value)| (*port, u32::from(*value)))
                .collect::<Vec<_>>(),
            2 => io
                .written_u16
                .borrow()
                .iter()
                .map(|(port, value)| (*port, u32::from(*value)))
                .collect::<Vec<_>>(),
            4 => io.written_u32.borrow().clone(),
            _ => unreachable!(),
        };
        assert_eq!(writes, expected_values, "OUTS transfers expected values");
        let final_si = OUTS_BASE + io_count * width;
        assert_eq!(
            if addr32 {
                cpu.regs.esi
            } else {
                u32::from(cpu.regs.si())
            },
            final_si as u32,
            "OUTS advances the selected address-size index"
        );
    }
}

macro_rules! string_io_charged_cycles_test {
    ($name:ident, $is_ins:expr, $width:literal, $addr32:expr, $rep:expr) => {
        #[test]
        fn $name() {
            string_io_charged_cycles_case($is_ins, $width, $addr32, $rep);
        }
    };
}

string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_byte_addr16_scalar,
    true,
    1,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_byte_addr16_rep,
    true,
    1,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_byte_addr32_scalar,
    true,
    1,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_byte_addr32_rep,
    true,
    1,
    true,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_word_addr16_scalar,
    true,
    2,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_word_addr16_rep,
    true,
    2,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_word_addr32_scalar,
    true,
    2,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_word_addr32_rep,
    true,
    2,
    true,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_dword_addr16_scalar,
    true,
    4,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_dword_addr16_rep,
    true,
    4,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_dword_addr32_scalar,
    true,
    4,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_ins_dword_addr32_rep,
    true,
    4,
    true,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_byte_addr16_scalar,
    false,
    1,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_byte_addr16_rep,
    false,
    1,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_byte_addr32_scalar,
    false,
    1,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_byte_addr32_rep,
    false,
    1,
    true,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_word_addr16_scalar,
    false,
    2,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_word_addr16_rep,
    false,
    2,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_word_addr32_scalar,
    false,
    2,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_word_addr32_rep,
    false,
    2,
    true,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_dword_addr16_scalar,
    false,
    4,
    false,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_dword_addr16_rep,
    false,
    4,
    false,
    true
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_dword_addr32_scalar,
    false,
    4,
    true,
    false
);
string_io_charged_cycles_test!(
    test_string_io_charged_cycles_outs_dword_addr32_rep,
    false,
    4,
    true,
    true
);

// ====================================================================
// OUTSB with segment override (0x26 0x6E) — ES segment
// ====================================================================

#[test]
fn test_outsb_segment_override() {
    // 26 6E => ES: OUTSB (reads from ES:[SI] instead of DS:[SI])
    let (mut cpu, mut mem) = setup(&[0x26, 0x6E]);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.es = 0x0040; // ES segment
    cpu.regs.ds = 0x0030; // DS segment (not used due to override)
    cpu.regs.set_si(0x0010);
    // Place data at ES:SI = 0x0040*16 + 0x0010 = 0x0410
    mem.ram[0x0410] = 0xDD;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x03F8, 0xDD));
}

// ====================================================================
// IN AL preserves upper bits of EAX
// ====================================================================

#[test]
fn test_in_al_preserves_eax_upper() {
    let (mut cpu, mut mem) = setup(&[0xE4, 0x60]);
    cpu.regs.eax = 0xDEAD_BE00;
    let mut io = MockIo::new();
    io.push_read_u8(0x42);
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // AL should be 0x42, upper bytes preserved
    assert_eq!(cpu.regs.eax, 0xDEAD_BE42);
}

// ====================================================================
// IN AX preserves upper 16 bits of EAX
// ====================================================================

#[test]
fn test_in_ax_preserves_eax_upper() {
    let (mut cpu, mut mem) = setup(&[0xE5, 0x60]);
    cpu.regs.eax = 0xDEAD_0000;
    let mut io = MockIo::new();
    io.push_read_u16(0xBEEF);
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // AX should be 0xBEEF, upper 16 bits preserved
    assert_eq!(cpu.regs.eax, 0xDEAD_BEEF);
}

// ========================================================================
// Trap Flag (TF) — INT 1 debug exception after instruction
// ========================================================================

#[test]
fn test_trap_flag_raises_int1_after_instruction() {
    // NOP with TF set — should execute NOP then return Exception vector 1.
    let (mut cpu, mut mem) = setup(&[0x90]); // NOP
    cpu.regs.flags.insert(Flags::TRAP);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 1,
            error_code: 0,
            fault_addr: None,
            executed: 1,
        } => {}
        other => panic!("expected Exception(vector=1), got {:?}", other),
    }
    // IP should have advanced past the NOP
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_trap_flag_not_set_no_exception() {
    // NOP without TF — should complete normally.
    let (mut cpu, mut mem) = setup(&[0x90]); // NOP
    cpu.regs.flags.remove(Flags::TRAP);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_trap_flag_fires_after_alu_instruction() {
    // ADD AX, 1 with TF set
    let (mut cpu, mut mem) = setup(&[0x05, 0x01, 0x00]); // ADD AX, 0x0001
    cpu.regs.eax = 0x0042;
    cpu.regs.flags.insert(Flags::TRAP);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 1,
            error_code: 0,
            executed: 1,
            ..
        } => {}
        other => panic!("expected Exception(vector=1), got {:?}", other),
    }
    // ADD should have executed
    assert_eq!(cpu.regs.eax as u16, 0x0043);
    assert_eq!(cpu.regs.eip, 0x0103);
}

// Helper: place a 16-bit value on a real-mode stack at 0000:0400 for POPF.
fn arm_real_mode_stack(cpu: &mut Cpu, mem: &mut Memory, flags_word: u16) {
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.set_sp(0x0400);
    cpu.regs.esp = 0x0400;
    let [lo, hi] = flags_word.to_le_bytes();
    mem.ram[0x0400] = lo;
    mem.ram[0x0401] = hi;
}

#[test]
fn test_popf_setting_tf_defers_single_step_one_instruction() {
    // Intel SDM 17.3.1.4: a POPF/POPFD/IRET that *sets* TF (0->1) is NOT
    // itself single-stepped — the #DB is taken after the FOLLOWING
    // instruction. After `popf` sets TF, the following instruction
    // is the first trapped.
    // Regression for the spurious-trap bug (interpreter single-stepped the
    // arming popf, yielding one extra #DB). Code: POPF; NOP; NOP.
    let (mut cpu, mut mem) = setup(&[0x9D, 0x90, 0x90]);
    cpu.regs.flags.remove(Flags::TRAP); // TF clear *before* the popf
    arm_real_mode_stack(&mut cpu, &mut mem, (Flags::TRAP | Flags::RESERVED1).bits());
    let mut io = ();
    // Budget 2: POPF (no #DB) then NOP (#DB). A spurious trap on the popf
    // itself would return executed=1.
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    match result {
        RunResult::Exception {
            vector: 1,
            executed: 2,
            ..
        } => {}
        other => panic!(
            "expected #DB after the instruction FOLLOWING popf (executed=2), got {:?}",
            other
        ),
    }
    // popf (0x100->0x101) + first NOP (0x101->0x102): #DB return IP past the NOP.
    assert_eq!(cpu.regs.eip, 0x0102);
    assert!(
        cpu.regs.flags.contains(Flags::TRAP),
        "popf should have set TF"
    );
}

#[test]
fn test_popf_with_tf_already_set_still_single_steps() {
    // Guard: the deferral applies ONLY to a 0->1 TF transition. If TF was
    // already set when the popf ran, the popf IS single-stepped (executed=1).
    // Code: POPF; NOP.
    let (mut cpu, mut mem) = setup(&[0x9D, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP); // TF already set before the popf
    arm_real_mode_stack(&mut cpu, &mut mem, (Flags::TRAP | Flags::RESERVED1).bits());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    match result {
        RunResult::Exception {
            vector: 1,
            executed: 1,
            ..
        } => {}
        other => panic!(
            "popf with TF already set must single-step itself (executed=1), got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.eip, 0x0101); // #DB taken right after the popf
}

#[test]
fn test_popf_clearing_tf_single_steps_using_entry_tf() {
    // TF at instruction entry still requests a #DB after POPF retires, even
    // when its popped FLAGS image clears TF. The following NOPs also prove
    // that returning the trap does not leave a second pending trap behind.
    let (mut cpu, mut mem) = setup(&[0x9D, 0x90, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    let popped_flags = Flags::RESERVED1 | Flags::DIRECTION;
    arm_real_mode_stack(&mut cpu, &mut mem, popped_flags.bits());
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 1,
                error_code: 0,
                executed: 1,
                ..
            }
        ),
        "POPF with entry TF set must single-step itself after clearing TF, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0101);
    assert_eq!(cpu.regs.sp(), 0x0402);
    assert!(!cpu.regs.flags.contains(Flags::TRAP));
    assert!(cpu.regs.flags.contains(Flags::DIRECTION));

    let resumed = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(resumed, RunResult::BudgetExhausted { executed: 2 }),
        "resuming after the acknowledged #DB must not deliver another trap, got {resumed:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0103);
}

#[test]
fn test_popfd_clearing_tf_single_steps_using_entry_tf() {
    // Operand-size override selects POPFD in real mode. A 32-bit popped image
    // with TF clear must not erase the #DB pending from instruction entry.
    let (mut cpu, mut mem) = setup(&[0x66, 0x9D, 0x90, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    let popped_flags = Flags::RESERVED1 | Flags::DIRECTION;
    arm_real_mode_stack(&mut cpu, &mut mem, popped_flags.bits());
    mem.ram[0x0402] = 0;
    mem.ram[0x0403] = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 1,
                error_code: 0,
                executed: 1,
                ..
            }
        ),
        "POPFD with entry TF set must single-step itself after clearing TF, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0102);
    assert_eq!(cpu.regs.sp(), 0x0404);
    assert!(!cpu.regs.flags.contains(Flags::TRAP));
    assert!(cpu.regs.flags.contains(Flags::DIRECTION));

    let resumed = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(resumed, RunResult::BudgetExhausted { executed: 2 }),
        "resuming after the acknowledged #DB must not deliver another trap, got {resumed:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0104);
}

// ========================================================================
// ARPL (0x63) — adjust RPL field
// ========================================================================

#[test]
fn test_arpl_ud_in_real_mode() {
    // ARPL in real mode should raise #UD (vector 6).
    // 0x63 0xC0 = ARPL AX, AX (register form)
    let (mut cpu, mut mem) = setup(&[0x63, 0xC0]);
    // Real mode: cr0 PE=0 (default)
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            executed: 0,
            ..
        } => {}
        other => panic!("expected #UD in real mode, got {:?}", other),
    }
    // IP should be rewound to snap_ip
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_arpl_register_zf_set() {
    // Protected mode: ARPL AX, CX where RPL(AX) < RPL(CX) → adjust, ZF=1.
    // 0x63 0xC8 = ARPL AX, CX  (modrm: mod=11, reg=CX(001), rm=AX(000))
    let (mut cpu, mut mem) = setup(&[0x63, 0xC8]);
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.eax = 0x0008; // Selector with RPL=0
    cpu.regs.ecx = 0x000B; // Selector with RPL=3
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "got {:?}",
        result
    );
    // AX should have RPL adjusted from 0 to 3
    assert_eq!(cpu.regs.eax as u16, 0x000B);
    // ZF should be set
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_arpl_register_zf_clear() {
    // Protected mode: ARPL AX, CX where RPL(AX) >= RPL(CX) → no change, ZF=0.
    // 0x63 0xC8 = ARPL AX, CX
    let (mut cpu, mut mem) = setup(&[0x63, 0xC8]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.eax = 0x000B; // RPL=3
    cpu.regs.ecx = 0x0009; // RPL=1
    cpu.regs.flags.insert(Flags::ZERO); // Pre-set ZF — should be cleared
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "got {:?}",
        result
    );
    // AX should be unchanged
    assert_eq!(cpu.regs.eax as u16, 0x000B);
    // ZF should be cleared
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_arpl_memory_zf_set() {
    // Protected mode: ARPL [mem], CX where RPL(mem) < RPL(CX) → adjust memory, ZF=1.
    // 0x63 0x0E 0x00 0x02 = ARPL [0x0200], CX  (modrm: mod=00, reg=CX(001), rm=110=disp16)
    let (mut cpu, mut mem) = setup(&[0x63, 0x0E, 0x00, 0x02]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    // Write selector with RPL=0 at address 0x0200
    mem.ram[0x0200] = 0x08; // 0x0008 = selector with RPL=0
    mem.ram[0x0201] = 0x00;
    cpu.regs.ecx = 0x000B; // RPL=3
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "got {:?}",
        result
    );
    // Memory at 0x0200 should have RPL adjusted to 3
    let stored = u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]);
    assert_eq!(stored, 0x000B);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_arpl_memory_zf_clear() {
    // Protected mode: ARPL [mem], CX where RPL(mem) >= RPL(CX) → no change, ZF=0.
    // 0x63 0x0E 0x00 0x02 = ARPL [0x0200], CX
    let (mut cpu, mut mem) = setup(&[0x63, 0x0E, 0x00, 0x02]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    // Write selector with RPL=3 at address 0x0200
    mem.ram[0x0200] = 0x0B; // 0x000B = selector with RPL=3
    mem.ram[0x0201] = 0x00;
    cpu.regs.ecx = 0x0009; // RPL=1
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "got {:?}",
        result
    );
    // Memory should be unchanged
    let stored = u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]);
    assert_eq!(stored, 0x000B);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_arpl_preserves_upper_selector_bits() {
    // ARPL should only modify the low 2 bits of the destination, preserving rest.
    // 0x63 0xC8 = ARPL AX, CX
    let (mut cpu, mut mem) = setup(&[0x63, 0xC8]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.eax = 0x1234; // Selector 0x1234: index=0x246, TI=1, RPL=0
    cpu.regs.ecx = 0x0003; // RPL=3
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "got {:?}",
        result
    );
    // AX should be 0x1237 (upper bits preserved, RPL adjusted to 3)
    assert_eq!(cpu.regs.eax as u16, 0x1237);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// IOPL checking — protected mode I/O permission
// ========================================================================

/// Set up a protected-mode CPU with CS at base 0, CPL=3, IOPL as specified.
/// Code is placed at linear address 0x0100.
fn setup_pm(code: &[u8], iopl: u8) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cs = 0x001B; // RPL=3
    cpu.regs.cs_cache.base = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.es_cache.base = 0;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.big = false;
    cpu.regs.cpl = 3;
    cpu.regs.set_iopl(iopl);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.cs_bits = 16;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

/// Set up a TSS with an I/O permission bitmap at the given base address.
/// The bitmap starts at offset `io_map_offset` within the TSS.
/// All ports are denied by default (bitmap filled with 0xFF).
fn setup_tss(mem: &mut Memory, cpu: &mut Cpu, tss_base: u32, tss_limit: u32, io_map_offset: u16) {
    cpu.regs.tss_base = tss_base;
    cpu.regs.tss_limit = tss_limit;
    // Write I/O map base at TSS+0x66
    let addr = (tss_base + 0x66) as usize;
    mem.ram[addr] = io_map_offset as u8;
    mem.ram[addr + 1] = (io_map_offset >> 8) as u8;
    // Fill bitmap area with 0xFF (all denied) up to TSS limit
    let bitmap_start = (tss_base as usize) + (io_map_offset as usize);
    let bitmap_end = (tss_base as usize) + (tss_limit as usize) + 1;
    for i in bitmap_start..bitmap_end.min(mem.ram.len()) {
        mem.ram[i] = 0xFF;
    }
}

/// Allow a specific port range in the TSS I/O permission bitmap.
fn allow_port(mem: &mut Memory, tss_base: u32, io_map_offset: u16, port: u16, size: u8) {
    for i in 0..size {
        let p = port + i as u16;
        let byte_off = (p / 8) as usize;
        let bit_off = p % 8;
        let addr = (tss_base as usize) + (io_map_offset as usize) + byte_off;
        mem.ram[addr] &= !(1 << bit_off); // Clear bit = allow
    }
}

// --- CPL <= IOPL: I/O allowed without bitmap ---

#[test]
fn test_iopl_cpl_le_iopl_allows_in_al_imm8() {
    // CPL=3, IOPL=3: I/O allowed
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 3);
    let mut io = MockIo::new();
    io.push_read_u8(0xAB);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 0xAB);
}

#[test]
fn test_iopl_cpl_le_iopl_allows_out_imm8_al() {
    let (mut cpu, mut mem) = setup_pm(&[0xE6, 0x61], 3);
    cpu.regs.set_al(0x7F);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(io.written_u8.borrow()[0], (0x61, 0x7F));
}

// --- CPL > IOPL, no TSS bitmap: #GP(0) ---

#[test]
fn test_iopl_gp_in_al_imm8_no_bitmap() {
    // CPL=3, IOPL=0, no TSS configured → #GP(0)
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
    // EIP should point to the faulting instruction
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_iopl_gp_out_imm8_al_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0xE6, 0x61], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_iopl_gp_in_ax_imm8_no_bitmap() {
    // E5 80 = IN AX, 0x80
    let (mut cpu, mut mem) = setup_pm(&[0xE5, 0x80], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_out_imm8_ax_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0xE7, 0x43], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_in_al_dx_no_bitmap() {
    // EC = IN AL, DX
    let (mut cpu, mut mem) = setup_pm(&[0xEC], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_in_ax_dx_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0xED], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_out_dx_al_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0xEE], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_out_dx_ax_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0xEF], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- INS/OUTS: CPL > IOPL, no bitmap → #GP(0) ---

#[test]
fn test_iopl_gp_insb_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0x6C], 0);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.es_cache.base = 0;
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_insw_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0x6D], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_outsb_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0x6E], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_gp_outsw_no_bitmap() {
    let (mut cpu, mut mem) = setup_pm(&[0x6F], 0);
    cpu.regs.set_dx(0x03F8);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- TSS bitmap allows specific port → I/O succeeds ---

#[test]
fn test_iopl_bitmap_allows_in_al_imm8() {
    // CPL=3, IOPL=0, but TSS bitmap allows port 0x42
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1; // Enough for port 0x42
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    io.push_read_u8(0xCD);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.al(), 0xCD);
}

#[test]
fn test_iopl_bitmap_denies_in_al_imm8() {
    // CPL=3, IOPL=0, TSS bitmap has port 0x42 denied (bit set)
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    // Don't allow_port — all bits are 0xFF (denied)
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_bitmap_allows_out_dx_al() {
    // OUT DX, AL (0xEE) with port from DX=0x0060, bitmap allows it
    let (mut cpu, mut mem) = setup_pm(&[0xEE], 0);
    cpu.regs.set_dx(0x0060);
    cpu.regs.set_al(0x55);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x60 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x0060, 1);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x0060, 0x55));
}

#[test]
fn test_iopl_bitmap_allows_in_ax_imm8_word() {
    // IN AX, 0x40 (0xE5 0x40) — needs 2 consecutive port bits allowed
    let (mut cpu, mut mem) = setup_pm(&[0xE5, 0x40], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x41 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x40, 2);
    let mut io = MockIo::new();
    io.push_read_u16(0xBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ax(), 0xBEEF);
}

#[test]
fn test_iopl_bitmap_partial_deny_word_io() {
    // IN AX, 0x40 — port 0x40 allowed but port 0x41 denied
    let (mut cpu, mut mem) = setup_pm(&[0xE5, 0x40], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x41 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x40, 1); // Only port 0x40
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- Byte boundary straddling ---

#[test]
fn test_iopl_bitmap_straddles_byte_boundary() {
    // Port 7 with size 2 straddles bytes (bit 7 in byte 0, bit 0 in byte 1)
    let (mut cpu, mut mem) = setup_pm(&[0xE5, 0x07], 0); // IN AX, 0x07
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + 2; // Need at least 2 bytes of bitmap
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 7, 2);
    let mut io = MockIo::new();
    io.push_read_u16(0x1234);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
}

// --- TSS too small (no bitmap) ---

#[test]
fn test_iopl_tss_too_small_for_bitmap() {
    // TSS limit < 0x67 → can't even read I/O map base → #GP
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 0);
    cpu.regs.tss_base = 0x5000;
    cpu.regs.tss_limit = 0x60; // Too small
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- Port outside bitmap range → denied ---

#[test]
fn test_iopl_port_outside_bitmap_range() {
    // Bitmap only covers ports 0-63, try port 0x80
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x80], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + 7; // Only 8 bytes = ports 0-63
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- Real mode: I/O always allowed regardless of IOPL ---

#[test]
fn test_iopl_real_mode_always_allows() {
    // Real mode (CR0.PE=0), IOPL=0 — should still allow I/O
    let (mut cpu, mut mem) = setup(&[0xE4, 0x42]);
    cpu.regs.set_iopl(0);
    let mut io = MockIo::new();
    io.push_read_u8(0xAB);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 0xAB);
}

// --- V86 mode: always check bitmap ---

#[test]
fn test_iopl_v86_mode_checks_bitmap_even_with_iopl3() {
    // V86 mode with IOPL=3 but no TSS → should #GP
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 3);
    cpu.regs.set_v86_mode(true); // VM flag
                                 // No TSS configured (tss_limit=0) → bitmap check fails
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

#[test]
fn test_iopl_v86_mode_allows_with_bitmap() {
    // V86 mode with bitmap allowing port
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 3);
    cpu.regs.set_v86_mode(true);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    io.push_read_u8(0xCD);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.al(), 0xCD);
}

#[test]
fn test_iopl_v86_bitmap_allows_out_dx_ax_through_pse_page() {
    // VGA BIOS executes OUT DX,AX. The CPU's implicit-supervisor IOPM
    // fetch must honour CR4.PSE just like an ordinary page-table walk.
    let (mut cpu, mut mem) = setup_pm(&[0xEF], 3);
    cpu.regs.set_v86_mode(true);
    cpu.regs.set_dx(0x01CE);
    cpu.regs.set_ax(0x0004);

    let page_directory = 0x1000u32;
    let tss_linear = 0x8040_5000u32;
    let tss_physical = 0x0040_5000usize;
    let io_map_off = 0x68u16;
    let byte_offset = (0x01CEu16 / 8) as usize;
    let tss_limit = io_map_off as u32 + byte_offset as u32;

    cpu.regs.cr3 = page_directory;
    cpu.regs.cr0 |= 0x8000_0000; // PG
    cpu.regs.cr4 |= 1 << 4; // PSE
    cpu.regs.tss_base = tss_linear;
    cpu.regs.tss_limit = tss_limit;

    // Identity-map the code page and map 0x80400000 to physical 4 MiB,
    // both as present/read-write/user 4 MiB pages.
    mem.write_u32_physical(page_directory as usize, 0x0000_0087);
    let tss_pde = page_directory as usize + (((tss_linear >> 22) as usize) * 4);
    mem.write_u32_physical(tss_pde, 0x0040_0087);

    mem.ram[tss_physical + 0x66] = io_map_off as u8;
    mem.ram[tss_physical + 0x67] = (io_map_off >> 8) as u8;
    let bitmap_start = tss_physical + io_map_off as usize;
    let bitmap_end = tss_physical + tss_limit as usize + 1;
    mem.ram[bitmap_start..bitmap_end].fill(0xFF);
    let bitmap_byte = bitmap_start + byte_offset;
    mem.ram[bitmap_byte] &= !((1 << (0x01CE % 8)) | (1 << (0x01CF % 8)));

    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 OUT DX,AX through a PSE-mapped IOPM must succeed, got {result:?}",
    );
    assert_eq!(io.written_u16.borrow().as_slice(), &[(0x01CE, 0x0004)]);
}

// --- INS/OUTS with bitmap ---

#[test]
fn test_iopl_bitmap_allows_insb() {
    // INSB (0x6C) with bitmap allowing the port
    let (mut cpu, mut mem) = setup_pm(&[0x6C], 0);
    cpu.regs.set_dx(0x0060);
    cpu.regs.es_cache.base = 0;
    cpu.regs.set_di(0x2000);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x60 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x0060, 1);
    let mut io = MockIo::new();
    io.push_read_u8(0x77);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x2000], 0x77);
}

#[test]
fn test_iopl_bitmap_allows_outsb() {
    // OUTSB (0x6E) with bitmap allowing the port
    let (mut cpu, mut mem) = setup_pm(&[0x6E], 0);
    cpu.regs.set_dx(0x0060);
    cpu.regs.ds_cache.base = 0;
    cpu.regs.set_si(0x2000);
    mem.ram[0x2000] = 0x88;
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x60 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x0060, 1);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x0060, 0x88));
}

// --- REP INSB: IOPL check fires before first iteration ---

#[test]
fn test_iopl_gp_rep_insb_no_bitmap() {
    // REP INSB (0xF3 0x6C) with CX>0, CPL > IOPL, no bitmap → #GP
    let (mut cpu, mut mem) = setup_pm(&[0xF3, 0x6C], 0);
    cpu.regs.set_dx(0x03F8);
    cpu.regs.es_cache.base = 0;
    cpu.regs.set_di(0x2000);
    cpu.regs.set_cx(5);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
    // CX should be unchanged (no iterations executed)
    assert_eq!(cpu.regs.cx(), 5);
}

// --- IOPL=2: CPL=2 allowed, CPL=3 denied ---

#[test]
fn test_iopl_2_cpl2_allowed() {
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 2);
    cpu.regs.cpl = 2;
    let mut io = MockIo::new();
    io.push_read_u8(0x99);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_iopl_2_cpl3_denied() {
    let (mut cpu, mut mem) = setup_pm(&[0xE4, 0x42], 2);
    cpu.regs.cpl = 3;
    // No TSS → denied
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

// --- 32-bit operand size: dword I/O needs 4 bitmap bits ---

#[test]
fn test_iopl_bitmap_allows_in_eax_imm8_dword() {
    // 66 E5 10 = IN EAX, 0x10 (op size override → 32-bit)
    let (mut cpu, mut mem) = setup_pm(&[0x66, 0xE5, 0x10], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x13 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x10, 4); // Allow ports 0x10-0x13
    let mut io = MockIo::new();
    io.push_read_u32(0xDEADBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
}

#[test]
fn test_iopl_bitmap_partial_deny_dword_io() {
    // IN EAX, 0x10 — only ports 0x10-0x12 allowed, 0x13 denied
    let (mut cpu, mut mem) = setup_pm(&[0x66, 0xE5, 0x10], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x13 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x10, 3); // Only 3 of 4
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP(0), got {:?}", other),
    }
}

/// MDD-BUG-FLU-03249: a near RET (C3), RET imm16 (C2), CALL near indirect
/// (FF /2) and JMP near indirect (FF /4) that retire through the
/// `branch_absolute!` fast path charge their cycle inline, so a port access
/// later in the same run sees its own start time, not one early by the
/// number of such branches. Each loop below retires one of the four forms
/// per iteration; every instruction before the IN charges one cycle, so the
/// IN must see the count of instructions retired before it.
#[test]
fn test_in_after_fast_path_branches_sees_its_start_time() {
    const ORG: u16 = 0x0100;
    const ITERATIONS: u16 = 3;
    // (name, instructions per iteration, loop body after DEC CX).
    type Body = fn(&mut Vec<u8>, u16) -> u16;
    let forms: [(&str, u64, Body); 4] = [
        ("RET C3", 4, |code, l2| {
            // PUSH L2; RET
            code.push(0x68);
            code.extend_from_slice(&l2.to_le_bytes());
            code.push(0xC3);
            4
        }),
        ("RET imm16 C2", 5, |code, l2| {
            // PUSH 0; PUSH L2; RET 2
            code.extend_from_slice(&[0x6A, 0x00, 0x68]);
            code.extend_from_slice(&l2.to_le_bytes());
            code.extend_from_slice(&[0xC2, 0x02, 0x00]);
            8
        }),
        ("CALL near indirect FF /2", 5, |code, _| {
            // CALL BX (BX = the far stub: POP DX; JMP L2)
            code.extend_from_slice(&[0xFF, 0xD3]);
            2
        }),
        ("JMP near indirect FF /4", 3, |code, _| {
            // JMP BX (BX = L2)
            code.extend_from_slice(&[0xFF, 0xE3]);
            2
        }),
    ];
    for (name, per_iteration, body) in forms {
        // MOV CX, n; MOV BX, target; top: DEC CX; body; L2: JNZ top;
        // IN AL, 0x42; HLT; ... stub at ORG + 0x80: POP DX; JMP L2.
        let mut code = vec![0xB9];
        code.extend_from_slice(&ITERATIONS.to_le_bytes());
        code.push(0xBB);
        let bx_at = code.len();
        code.extend_from_slice(&[0, 0]);
        let top = code.len();
        code.push(0x49);
        let body_len = body(&mut Vec::new(), 0);
        let l2 = ORG + (code.len() as u16) + body_len;
        body(&mut code, l2);
        assert_eq!(ORG + code.len() as u16, l2);
        let rel = top as isize - (code.len() as isize + 2);
        code.extend_from_slice(&[0x75, rel as i8 as u8]);
        code.extend_from_slice(&[0xE4, 0x42, 0xF4]);
        let stub = 0x80;
        let bx = if name.starts_with("CALL") {
            ORG + stub as u16
        } else {
            l2
        };
        code[bx_at..bx_at + 2].copy_from_slice(&bx.to_le_bytes());
        code.resize(stub, 0x90);
        code.push(0x5A);
        let rel = (l2 - ORG) as isize - (code.len() as isize + 2);
        code.extend_from_slice(&[0xEB, rel as i8 as u8]);

        let (mut cpu, mut mem) = setup(&code);
        cpu.regs.ss = 0;
        cpu.regs.set_sp(0x8000);
        let mut io = MockIo::new();
        io.push_read_u8(0xAB);
        let result = run_ext_charged(&mut cpu, &mut mem, &mut io, &mut NoExec, 200, false);

        let before_in = 2 + u64::from(ITERATIONS) * per_iteration;
        assert!(
            matches!(result.result, RunResult::Halt { .. }),
            "{name}: {:?}",
            result.result
        );
        assert_eq!(cpu.regs.al(), 0xAB, "{name}: the IN ran");
        assert_eq!(
            io.read_u8_cycles.borrow().as_slice(),
            &[before_in],
            "{name}: the IN is stamped at its own start"
        );
        assert_eq!(
            u64::from(result.charged_cycles),
            before_in + u64::from(SCALAR_IO_CHARGED_CYCLES) + 1,
            "{name}: each instruction charged once (IN 10, HLT 1)"
        );
    }
}

fn assert_tf_debug_boundary(result: RunResult, executed: u32) {
    assert!(
        matches!(result, RunResult::Exception {
        vector: 1, error_code: 0, fault_addr: None, executed: actual,
    } if actual == executed),
        "expected #DB after {executed} retirements, got {result:?}"
    );
}

#[test]
fn test_tf_rep_memory_string_boundaries() {
    // Independently assembled real-mode strings, including bulk MOVS/STOS.
    for opcode in [0xAC, 0xAD, 0xAA, 0xAB, 0xA4, 0xA5, 0xA6, 0xA7, 0xAE, 0xAF] {
        for count in [2u16, 1, 0] {
            for reverse in [false, true] {
                for early_compare in [false, true] {
                    let (mut cpu, mut mem) = setup(&[0xF3, opcode, 0x90]);
                    cpu.regs.ds = 0;
                    cpu.regs.ds_cache.base = 0;
                    cpu.regs.es = 0;
                    cpu.regs.es_cache.base = 0;
                    cpu.regs.set_si(0x0200);
                    cpu.regs.set_di(0x0300);
                    cpu.regs.set_cx(count);
                    cpu.regs.set_ax(0x4242);
                    cpu.regs.flags.insert(Flags::TRAP | Flags::CARRY);
                    cpu.regs.flags.set(Flags::DIRECTION, reverse);
                    mem.ram[0x0200..0x0202].fill(0x42);
                    mem.ram[0x0300..0x0302].fill(if early_compare { 0x43 } else { 0x42 });
                    let comparing = matches!(opcode, 0xA6 | 0xA7 | 0xAE | 0xAF);
                    let step = if opcode & 1 == 0 { 1u16 } else { 2 };
                    let result = run(&mut cpu, &mut mem, &mut (), 16);
                    assert_tf_debug_boundary(result, 1);
                    assert_eq!(
                        cpu.regs.cx(),
                        count.saturating_sub(1),
                        "opcode {opcode:02X}"
                    );
                    let completed = count <= 1 || (comparing && early_compare);
                    assert_eq!(cpu.regs.eip, if completed { 0x0102 } else { 0x0100 });
                    let advance = |index: u16| {
                        if count == 0 {
                            index
                        } else if reverse {
                            index.wrapping_sub(step)
                        } else {
                            index.wrapping_add(step)
                        }
                    };
                    assert_eq!(
                        cpu.regs.si(),
                        if matches!(opcode, 0xAC | 0xAD | 0xA4..=0xA7) {
                            advance(0x0200)
                        } else {
                            0x0200
                        }
                    );
                    assert_eq!(
                        cpu.regs.di(),
                        if matches!(opcode, 0xAA | 0xAB | 0xA4..=0xA7 | 0xAE | 0xAF) {
                            advance(0x0300)
                        } else {
                            0x0300
                        }
                    );
                    if comparing && count > 0 {
                        assert_eq!(cpu.regs.flags.contains(Flags::ZERO), !early_compare);
                        assert_eq!(cpu.regs.flags.contains(Flags::CARRY), early_compare);
                    } else {
                        assert!(cpu.regs.flags.contains(Flags::CARRY));
                    }
                    // Resume an interrupted REP with TF still armed.
                    if !completed {
                        let result = run(&mut cpu, &mut mem, &mut (), 16);
                        assert_tf_debug_boundary(result, 1);
                        assert_eq!(cpu.regs.cx(), 0);
                        assert_eq!(cpu.regs.eip, 0x0102);
                    }
                }
            }
        }
    }
}

#[test]
fn test_tf_scalar_and_rep_io_boundaries() {
    for batch_break in [true, false] {
        for opcode in [0xEE, 0xEF, 0xE6, 0xE7, 0xEC, 0xED, 0xE4, 0xE5] {
            let code = if opcode < 0xE8 {
                vec![opcode, 0x20, 0x90]
            } else {
                vec![opcode, 0x90]
            };
            let (mut cpu, mut mem) = setup(&code);
            cpu.regs.flags.insert(Flags::TRAP);
            cpu.regs.set_dx(0x20);
            let mut io = MockIo::new();
            io.set_break_batch(batch_break);
            assert_tf_debug_boundary(run(&mut cpu, &mut mem, &mut io, 16), 1);
            assert_eq!(cpu.regs.eip, if opcode < 0xE8 { 0x0102 } else { 0x0101 });
        }
        for opcode in 0x6C..=0x6F {
            for count in [2u16, 1, 0] {
                let (mut cpu, mut mem) = setup(&[0xF3, opcode, 0x90]);
                cpu.regs.flags.insert(Flags::TRAP);
                cpu.regs.set_dx(0x20);
                cpu.regs.ds_cache.base = 0;
                cpu.regs.es_cache.base = 0;
                cpu.regs.set_si(0x0200);
                cpu.regs.set_di(0x0300);
                cpu.regs.set_cx(count);
                let mut io = MockIo::new();
                io.set_break_batch(batch_break);
                assert_tf_debug_boundary(run(&mut cpu, &mut mem, &mut io, 16), 1);
                assert_eq!(cpu.regs.cx(), count.saturating_sub(1));
                assert_eq!(cpu.regs.eip, if count == 2 { 0x0100 } else { 0x0102 });
                let step = if count == 0 {
                    0
                } else if opcode & 1 == 0 {
                    1
                } else {
                    2
                };
                assert_eq!(
                    cpu.regs.si(),
                    0x0200 + if opcode >= 0x6E { step } else { 0 }
                );
                assert_eq!(cpu.regs.di(), 0x0300 + if opcode < 0x6E { step } else { 0 });
                assert_eq!(
                    io.read_u8_ports.borrow().len(),
                    usize::from(opcode == 0x6C && count > 0)
                );
                assert_eq!(
                    io.written_u8.borrow().len(),
                    usize::from(opcode == 0x6E && count > 0)
                );
            }
        }
    }
}

#[test]
fn test_tf_newly_set_before_rep() {
    let (mut cpu, mut mem) = setup(&[0x9D, 0xF3, 0xAC, 0x90]);
    arm_real_mode_stack(&mut cpu, &mut mem, (Flags::TRAP | Flags::RESERVED1).bits());
    cpu.regs.ds_cache.base = 0;
    cpu.regs.set_si(0x0200);
    cpu.regs.set_cx(2);
    mem.ram[0x0200] = 0x42;
    assert_tf_debug_boundary(run(&mut cpu, &mut mem, &mut (), 16), 2);
    assert_eq!(cpu.regs.eip, 0x0101);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.si(), 0x0201);
}

#[test]
fn test_tf_ss_load_suppresses_own_trap_before_rep() {
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0, 0xF3, 0xAC, 0x90]);
    cpu.regs.set_ax(0);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.regs.ds_cache.base = 0;
    cpu.regs.set_si(0x0200);
    cpu.regs.set_cx(2);
    let first = run(&mut cpu, &mut mem, &mut (), 16);
    assert!(
        matches!(first, RunResult::InterruptPollBoundary { executed: 1 }),
        "{first:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0102);
    assert_tf_debug_boundary(run(&mut cpu, &mut mem, &mut (), 16), 1);
    assert_eq!(cpu.regs.eip, 0x0102);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.si(), 0x0201);
}
