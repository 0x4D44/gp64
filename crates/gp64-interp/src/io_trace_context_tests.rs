use super::*;

#[derive(Default)]
struct ContextCaptureIo {
    last: Option<IoTraceContext>,
}

impl IoPort for ContextCaptureIo {
    fn read_u8(&mut self, _: u16) -> u8 {
        0xFF
    }

    fn read_u16(&mut self, _: u16) -> u16 {
        0xFFFF
    }

    fn read_u32(&mut self, _: u16) -> u32 {
        0xFFFF_FFFF
    }

    fn write_u8(&mut self, _: u16, _: u8) {}
    fn write_u16(&mut self, _: u16, _: u16) {}
    fn write_u32(&mut self, _: u16, _: u32) {}

    fn read_u8_ctx(&mut self, _: u16, ctx: IoTraceContext) -> u8 {
        self.last = Some(ctx);
        0xFF
    }

    fn write_u32_ctx(&mut self, _: u16, _: u32, ctx: IoTraceContext) {
        self.last = Some(ctx);
    }
}

#[test]
fn interp_io_trace_context_propagates_real_mode() {
    let cpu = Cpu::new();
    let mut io = ContextCaptureIo::default();

    let _ = io_read_u8(&cpu, &mut io, 0, 0x22, 0x1234, 0x00005678);

    assert_eq!(
        io.last,
        Some(IoTraceContext {
            cs: 0x1234,
            eip: 0x00005678,
            mode: IoCpuMode::Real,
            cpl: None,
            cr3: None,
        })
    );
}

#[test]
fn interp_io_trace_context_propagates_protected_mode() {
    let mut cpu = Cpu::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cr3 = 0x0012_3000;
    cpu.regs.set_cpl(0);
    let mut io = ContextCaptureIo::default();

    io_write_u32(&cpu, &mut io, 0, 0x23, 0x0008, 0x80401234, 0xAABB_CCDD);

    assert_eq!(
        io.last,
        Some(IoTraceContext {
            cs: 0x0008,
            eip: 0x80401234,
            mode: IoCpuMode::Protected,
            cpl: Some(0),
            cr3: Some(0x0012_3000),
        })
    );
}

#[test]
fn interp_io_trace_context_propagates_v86_mode() {
    let mut cpu = Cpu::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cr3 = 0x0009_9000;
    cpu.regs.set_v86_mode(true);
    let mut io = ContextCaptureIo::default();

    let _ = io_read_u8(&cpu, &mut io, 0, 0x22, 0xF000, 0x0000FFF0);

    assert_eq!(
        io.last,
        Some(IoTraceContext {
            cs: 0xF000,
            eip: 0x0000FFF0,
            mode: IoCpuMode::V86,
            cpl: Some(3),
            cr3: Some(0x0009_9000),
        })
    );
}
