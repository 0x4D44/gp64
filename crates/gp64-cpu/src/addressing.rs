//! Effective address calculation for x86 memory operands.
//!
//! Shared between the FPU module (in this crate) and the fast interpreter
//! (`gp64-interp`).

use crate::sse_decode::{SseAddrReg, SseMemory, SseSegment};
use crate::x87_decode::{X87AddressRegister, X87AddressSize, X87MemoryOperand, X87Segment};
use crate::{Cpu, CpuError, PageAccess, PagedMemory, Result};
#[cfg(feature = "iced-interop")]
use iced_x86::{Instruction, OpKind, Register};

/// A segment register, in this crate's own vocabulary.
///
/// Public since 2026.08.20 so a caller that already knows which segment it
/// means can say so directly, instead of translating into an
/// `iced_x86::Register` purely to have `check_segment_access` translate it
/// straight back (`ResolvedSegment::from_iced`). That round trip was the last
/// thing pulling iced into a guest-visible path outside the decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedSegment {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

impl ResolvedSegment {
    #[cfg(feature = "iced-interop")]
    fn from_iced(reg: Register) -> Self {
        match reg {
            Register::ES => Self::Es,
            Register::CS => Self::Cs,
            Register::SS => Self::Ss,
            Register::FS => Self::Fs,
            Register::GS => Self::Gs,
            _ => Self::Ds,
        }
    }

    fn from_x87(segment: X87Segment) -> Self {
        match segment {
            X87Segment::Es => Self::Es,
            X87Segment::Cs => Self::Cs,
            X87Segment::Ss => Self::Ss,
            X87Segment::Ds => Self::Ds,
            X87Segment::Fs => Self::Fs,
            X87Segment::Gs => Self::Gs,
        }
    }

    fn from_sse(segment: SseSegment) -> Self {
        match segment {
            SseSegment::Es => Self::Es,
            SseSegment::Cs => Self::Cs,
            SseSegment::Ss => Self::Ss,
            SseSegment::Ds => Self::Ds,
            SseSegment::Fs => Self::Fs,
            SseSegment::Gs => Self::Gs,
        }
    }

    fn selector(self, cpu: &Cpu) -> u16 {
        match self {
            Self::Es => cpu.regs.es,
            Self::Cs => cpu.regs.cs,
            Self::Ss => cpu.regs.ss,
            Self::Ds => cpu.regs.ds,
            Self::Fs => cpu.regs.fs,
            Self::Gs => cpu.regs.gs,
        }
    }

    fn cached_limit(self, cpu: &Cpu) -> u32 {
        match self {
            Self::Es => cpu.regs.es_cache.effective_limit(),
            Self::Cs => cpu.regs.cs_cache.effective_limit(),
            Self::Ss => cpu.regs.ss_cache.effective_limit(),
            Self::Ds => cpu.regs.ds_cache.effective_limit(),
            Self::Fs => cpu.regs.fs_cache.effective_limit(),
            Self::Gs => cpu.regs.gs_cache.effective_limit(),
        }
    }

    #[inline]
    fn cache(self, cpu: &Cpu) -> crate::registers::SegmentCache {
        match self {
            Self::Es => cpu.regs.es_cache,
            Self::Cs => cpu.regs.cs_cache,
            Self::Ss => cpu.regs.ss_cache,
            Self::Ds => cpu.regs.ds_cache,
            Self::Fs => cpu.regs.fs_cache,
            Self::Gs => cpu.regs.gs_cache,
        }
    }
}

#[cfg(feature = "iced-interop")]
/// Read a CPU register by its iced-x86 enum value.
#[inline]
pub fn read_register(cpu: &Cpu, reg: Register) -> u64 {
    match reg {
        Register::AL => cpu.regs.al() as u64,
        Register::AH => cpu.regs.ah() as u64,
        Register::AX => cpu.regs.ax() as u64,
        Register::EAX => cpu.regs.eax as u64,
        Register::BL => cpu.regs.bl() as u64,
        Register::BH => cpu.regs.bh() as u64,
        Register::BX => cpu.regs.bx() as u64,
        Register::EBX => cpu.regs.ebx as u64,
        Register::CL => cpu.regs.cl() as u64,
        Register::CH => cpu.regs.ch() as u64,
        Register::CX => cpu.regs.cx() as u64,
        Register::ECX => cpu.regs.ecx as u64,
        Register::DL => cpu.regs.dl() as u64,
        Register::DH => cpu.regs.dh() as u64,
        Register::DX => cpu.regs.dx() as u64,
        Register::EDX => cpu.regs.edx as u64,
        Register::SI => cpu.regs.si() as u64,
        Register::ESI => cpu.regs.esi as u64,
        Register::DI => cpu.regs.di() as u64,
        Register::EDI => cpu.regs.edi as u64,
        Register::BP => cpu.regs.bp() as u64,
        Register::EBP => cpu.regs.ebp as u64,
        Register::SP => cpu.regs.sp() as u64,
        Register::ESP => cpu.regs.esp as u64,
        Register::CS => cpu.regs.cs as u64,
        Register::DS => cpu.regs.ds as u64,
        Register::ES => cpu.regs.es as u64,
        Register::FS => cpu.regs.fs as u64,
        Register::GS => cpu.regs.gs as u64,
        Register::SS => cpu.regs.ss as u64,
        _ => 0,
    }
}

#[cfg(feature = "iced-interop")]
/// Detect whether the instruction uses 16-bit effective addressing.
#[inline]
pub fn is_16bit_addressing(_cpu: &Cpu, instr: &Instruction) -> bool {
    // Detect 16-bit effective address size directly from the decoded operands.
    // This must honor address-size overrides even in 32-bit code segments.
    (instr.memory_base() == Register::None || instr.memory_base().is_gpr16())
        && (instr.memory_index() == Register::None || instr.memory_index().is_gpr16())
        && instr.memory_displ_size() <= 2
}

/// Map a segment register to the appropriate fault type:
/// SS violations generate #SS, others generate #GP.
#[inline]
fn segment_access_fault(seg_reg: ResolvedSegment) -> CpuError {
    if seg_reg == ResolvedSegment::Ss {
        CpuError::StackFault
    } else {
        CpuError::GeneralProtectionFault
    }
}

#[cfg(feature = "iced-interop")]
/// Check segment access rights (present, read/write permissions).
pub fn check_segment_access(
    cpu: &Cpu,
    segment: u16,
    seg_reg: Register,
    access: PageAccess,
) -> Result<()> {
    check_segment_access_resolved(cpu, segment, ResolvedSegment::from_iced(seg_reg), access)
}

/// Resolve a 32-bit absolute offset through the live DS hidden cache.
///
/// This is the segment half of PM32 `moffs32` handling. It deliberately runs
/// the same full-span limit and access-right checks as decoded memory operands
/// before returning a linear address; paging remains the caller's next step.
pub fn calc_ds_moffs32_linear(
    cpu: &Cpu,
    offset: u32,
    access_size: u32,
    access: PageAccess,
) -> Result<u32> {
    calc_offset32_linear(cpu, ResolvedSegment::Ds, offset, access_size, access)
}

/// Resolve a 32-bit offset through the live ES hidden cache.
///
/// This is the segment half of PM32 string destinations. It preserves the
/// architectural rule that string stores always use ES, while leaving paging
/// to the caller.
pub fn calc_es_offset32_linear(
    cpu: &Cpu,
    offset: u32,
    access_size: u32,
    access: PageAccess,
) -> Result<u32> {
    calc_offset32_linear(cpu, ResolvedSegment::Es, offset, access_size, access)
}

/// Resolve a 32-bit offset through the live SS hidden cache.
///
/// This is the segment half of PM32 EBP/ESP-based addressing. It preserves
/// #SS fault classification while leaving paging to the caller.
pub fn calc_ss_offset32_linear(
    cpu: &Cpu,
    offset: u32,
    access_size: u32,
    access: PageAccess,
) -> Result<u32> {
    calc_offset32_linear(cpu, ResolvedSegment::Ss, offset, access_size, access)
}

/// Resolve a 32-bit offset through the live FS hidden cache.
///
/// Checks the hidden segment base and limit before paging a PM32 `FS:` operand.
pub fn calc_fs_offset32_linear(
    cpu: &Cpu,
    offset: u32,
    access_size: u32,
    access: PageAccess,
) -> Result<u32> {
    calc_offset32_linear(cpu, ResolvedSegment::Fs, offset, access_size, access)
}

fn calc_offset32_linear(
    cpu: &Cpu,
    seg_reg: ResolvedSegment,
    offset: u32,
    access_size: u32,
    access: PageAccess,
) -> Result<u32> {
    let segment = seg_reg.selector(cpu);
    check_operand_segment_limits(cpu, seg_reg, segment, offset, access_size)?;
    check_segment_access_resolved(cpu, segment, seg_reg, access)?;
    Ok(seg_reg.cache(cpu).base.wrapping_add(offset))
}

/// Check segment access rights (present, read/write permissions).
///
/// The native-typed entry point. `check_segment_access` is the same check for
/// callers that are holding an iced `Register` because they just decoded one.
pub fn check_segment_access_resolved(
    cpu: &Cpu,
    segment: u16,
    seg_reg: ResolvedSegment,
    access: PageAccess,
) -> Result<()> {
    if !cpu.is_protected_mode() {
        return Ok(());
    }

    let cache = seg_reg.cache(cpu);
    if !cache.present {
        tracing::debug!(
            target: "gp64::cpu::segment_fault",
            segment = format_args!("{:04X}", segment),
            seg_reg = ?seg_reg,
            access = ?access,
            cs = format_args!("{:04X}", cpu.regs.cs),
            ip = format_args!("{:04X}", cpu.regs.ip()),
            "Segment not present"
        );
        return Err(segment_access_fault(seg_reg));
    }

    if cache.is_code {
        if access.is_write() {
            tracing::debug!(
                target: "gp64::cpu::segment_fault",
                segment = format_args!("{:04X}", segment),
                seg_reg = ?seg_reg,
                access = ?access,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "Write to code segment"
            );
            return Err(segment_access_fault(seg_reg));
        }
        if !matches!(access, PageAccess::Fetch) && !cache.is_readable {
            tracing::debug!(
                target: "gp64::cpu::segment_fault",
                segment = format_args!("{:04X}", segment),
                seg_reg = ?seg_reg,
                access = ?access,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "Read from non-readable code segment"
            );
            return Err(segment_access_fault(seg_reg));
        }
    } else if access.is_write() && !cache.is_writable {
        tracing::debug!(
            target: "gp64::cpu::segment_fault",
            segment = format_args!("{:04X}", segment),
            seg_reg = ?seg_reg,
            access = ?access,
            is_writable = cache.is_writable,
            access_byte = format_args!("{:02X}", cache.access),
            cs = format_args!("{:04X}", cpu.regs.cs),
            ip = format_args!("{:04X}", cpu.regs.ip()),
            "Write to non-writable data segment"
        );
        return Err(segment_access_fault(seg_reg));
    }

    Ok(())
}

#[cfg(feature = "iced-interop")]
/// Calculate the effective linear address for a memory operand.
///
/// Handles base + index*scale + displacement, segment override, 16-bit wrapping,
/// segment limit checking, segment access checking, and paging translation.
pub fn calc_effective_address<M: PagedMemory>(
    cpu: &Cpu,
    instr: &Instruction,
    mem: &mut M,
    access: PageAccess,
) -> Result<usize> {
    let resolved = resolve_operand_ea(cpu, instr);
    calc_effective_address_resolved(
        cpu,
        mem,
        access,
        instr.memory_size().size() as u32,
        resolved,
    )
}

/// Calculate an x87 descriptor's effective linear address without constructing
/// an iced instruction.
pub fn calc_x87_effective_address<M: PagedMemory>(
    cpu: &Cpu,
    operand: &X87MemoryOperand,
    mem: &mut M,
    access: PageAccess,
) -> Result<usize> {
    calc_effective_address_resolved(
        cpu,
        mem,
        access,
        u32::from(operand.width_bytes),
        resolve_x87_operand_ea(cpu, operand),
    )
}

fn calc_effective_address_resolved<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    access: PageAccess,
    access_size: u32,
    // Address size no longer reaches the limit check: a single contiguous access
    // is bounded by the segment limit alone, whatever produced its start offset.
    (seg_reg, segment, addr, _is_16bit): (ResolvedSegment, u16, u32, bool),
) -> Result<usize> {
    check_operand_segment_limits(cpu, seg_reg, segment, addr, access_size)?;
    check_segment_access_resolved(cpu, segment, seg_reg, access)?;

    let linear = translate_operand_address_full_checked(cpu, mem, seg_reg, segment, addr, access)?;

    // Debug: trace memory accesses to understand segment usage
    let trace_ea = tracing::enabled!(target: "gp64::cpu::ea_video", tracing::Level::DEBUG);
    if trace_ea && linear >= 0x90000 {
        static EA_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let count = EA_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count < 50 {
            tracing::debug!(
                count,
                seg = format_args!("{:04X}", segment),
                off = format_args!("{:04X}", addr),
                linear = format_args!("{:05X}", linear),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "EA-HIGH: high address access"
            );
        }
    }

    Ok(linear)
}

#[cfg(feature = "iced-interop")]
/// Resolve a memory operand's `(seg_reg, segment_selector, effective_offset,
/// is_16bit)`. Pure address math (base + index*scale + displacement, segment
/// selection, 16-bit wrap); no limit checks, access checks, or translation.
fn resolve_operand_ea(cpu: &Cpu, instr: &Instruction) -> (ResolvedSegment, u16, u32, bool) {
    if matches!(instr.op0_kind(), OpKind::MemorySegDI | OpKind::MemoryESDI) {
        let seg_reg = ResolvedSegment::from_iced(instr.memory_segment());
        return (
            seg_reg,
            seg_reg.selector(cpu),
            u32::from(cpu.regs.di()),
            true,
        );
    }
    if matches!(instr.op0_kind(), OpKind::MemorySegEDI | OpKind::MemoryESEDI) {
        let seg_reg = ResolvedSegment::from_iced(instr.memory_segment());
        return (seg_reg, seg_reg.selector(cpu), cpu.regs.edi, false);
    }
    let mut addr: u32 = 0;

    // Base register. With SIB index=100b there is no index register, and
    // scale is ignored: the effective address is `[base + disp]`
    // (Intel SDM Vol 2A Table 2-3). This also holds on 386/486/Pentium.
    if instr.memory_base() != Register::None {
        let base = read_register(cpu, instr.memory_base()) as u32;
        addr = addr.wrapping_add(base);
    }

    // Index register with scale
    if instr.memory_index() != Register::None {
        let index = read_register(cpu, instr.memory_index()) as u32;
        let scale = instr.memory_index_scale();
        addr = addr.wrapping_add(index.wrapping_mul(scale));
    }

    // Displacement
    addr = addr.wrapping_add(instr.memory_displacement32());

    // In 16-bit addressing modes, effective offsets wrap at 16 bits.
    // iced-x86 exposes displacements as 32-bit values (often sign-extended), so we must
    // explicitly apply 16-bit wraparound when the instruction uses 16-bit address size.
    let is_16bit = is_16bit_addressing(cpu, instr);
    if is_16bit {
        addr &= 0xFFFF;
    }

    // Segment
    //
    // Use iced-x86's `memory_segment()` which already applies any segment override prefix and
    // selects the correct default (SS for BP/SP-based addressing, DS otherwise).
    let seg_reg = ResolvedSegment::from_iced(instr.memory_segment());
    let segment = seg_reg.selector(cpu);

    (seg_reg, segment, addr, is_16bit)
}

#[inline]
fn read_x87_address_register(cpu: &Cpu, reg: X87AddressRegister) -> u32 {
    match reg {
        X87AddressRegister::Bx => u32::from(cpu.regs.bx()),
        X87AddressRegister::Bp => u32::from(cpu.regs.bp()),
        X87AddressRegister::Si => u32::from(cpu.regs.si()),
        X87AddressRegister::Di => u32::from(cpu.regs.di()),
        X87AddressRegister::Eax => cpu.regs.eax,
        X87AddressRegister::Ecx => cpu.regs.ecx,
        X87AddressRegister::Edx => cpu.regs.edx,
        X87AddressRegister::Ebx => cpu.regs.ebx,
        X87AddressRegister::Esp => cpu.regs.esp,
        X87AddressRegister::Ebp => cpu.regs.ebp,
        X87AddressRegister::Esi => cpu.regs.esi,
        X87AddressRegister::Edi => cpu.regs.edi,
    }
}

fn resolve_x87_operand_ea(
    cpu: &Cpu,
    operand: &X87MemoryOperand,
) -> (ResolvedSegment, u16, u32, bool) {
    let mut addr = operand
        .base
        .map_or(0, |reg| read_x87_address_register(cpu, reg));
    if let Some(index) = operand.index {
        addr = addr.wrapping_add(
            read_x87_address_register(cpu, index).wrapping_mul(u32::from(operand.scale)),
        );
    }
    addr = addr.wrapping_add(operand.displacement as u32);
    let is_16bit = operand.address_size == X87AddressSize::Bits16;
    if is_16bit {
        addr &= 0xffff;
    }
    let seg_reg = ResolvedSegment::from_x87(operand.segment);
    (seg_reg, seg_reg.selector(cpu), addr, is_16bit)
}

#[inline]
fn read_sse_address_register(cpu: &Cpu, reg: SseAddrReg) -> u32 {
    match reg {
        SseAddrReg::Bx => u32::from(cpu.regs.bx()),
        SseAddrReg::Bp => u32::from(cpu.regs.bp()),
        SseAddrReg::Si => u32::from(cpu.regs.si()),
        SseAddrReg::Di => u32::from(cpu.regs.di()),
        SseAddrReg::Eax => cpu.regs.eax,
        SseAddrReg::Ecx => cpu.regs.ecx,
        SseAddrReg::Edx => cpu.regs.edx,
        SseAddrReg::Ebx => cpu.regs.ebx,
        SseAddrReg::Esp => cpu.regs.esp,
        SseAddrReg::Ebp => cpu.regs.ebp,
        SseAddrReg::Esi => cpu.regs.esi,
        SseAddrReg::Edi => cpu.regs.edi,
    }
}

/// Resolve an SSE descriptor's address expression, exactly as
/// [`resolve_x87_operand_ea`] does for x87.
///
/// MASKMOVQ/MASKMOVDQU need no special case: their implicit `DS:rDI` operand
/// is decoded as an ordinary descriptor with `DI`/`EDI` as its base, so the
/// offset is read from the guest register here, at execution time.
fn resolve_sse_operand_ea(cpu: &Cpu, memory: &SseMemory) -> (ResolvedSegment, u16, u32, bool) {
    let mut addr = memory
        .base
        .map_or(0, |reg| read_sse_address_register(cpu, reg));
    if let Some(index) = memory.index {
        addr = addr.wrapping_add(
            read_sse_address_register(cpu, index).wrapping_mul(u32::from(memory.scale)),
        );
    }
    addr = addr.wrapping_add(memory.disp as u32);
    let is_16bit = memory.addr16;
    if is_16bit {
        addr &= 0xffff;
    }
    let seg_reg = ResolvedSegment::from_sse(memory.segment);
    (seg_reg, seg_reg.selector(cpu), addr, is_16bit)
}

/// Calculate an SSE descriptor's effective linear address without constructing
/// an iced instruction.
///
/// The limit check uses the descriptor's per-encoding `width`, which is what
/// [`calc_effective_address`] takes from iced's `memory_size()`.
pub fn calc_sse_effective_address<M: PagedMemory>(
    cpu: &Cpu,
    memory: &SseMemory,
    mem: &mut M,
    access: PageAccess,
) -> Result<usize> {
    calc_effective_address_resolved(
        cpu,
        mem,
        access,
        u32::from(memory.width),
        resolve_sse_operand_ea(cpu, memory),
    )
}

/// Enforce segment limits for an `access_size`-byte operand starting at `addr`.
///
/// Extracted from `calc_effective_address` so the single-address and span-aware
/// paths apply identical fault behaviour to the *full* operand span (an x87
/// operand can be up to 108 bytes, so the last byte, not just the first, must be
/// inside the segment limit).
///
/// * Protected mode: check against the segment descriptor's limit (with
///   expand-down handling).
/// * Real mode or V86 on a 286+: check against the cached segment limit
///   (0xFFFF normally, larger in unreal mode), whatever the address size.
///   The address size decides where the *start* offset can land, never whether
///   the operand is allowed to run off the end of the segment.
/// * 8086/80186: no check — there are no segment limits, and a 16-bit operand
///   simply wraps at offset 0xFFFF.
fn check_operand_segment_limits(
    cpu: &Cpu,
    seg_reg: ResolvedSegment,
    segment: u16,
    addr: u32,
    access_size: u32,
) -> Result<()> {
    if cpu.is_protected_mode() {
        let cache = seg_reg.cache(cpu);
        let effective_limit = cache.effective_limit();
        let expand_down = !cache.is_code && (cache.access & 0x04) != 0;
        let max = if cache.big { 0xFFFF_FFFF } else { 0xFFFF };

        if access_size > 0 {
            // Compute the last-byte offset with overflow detection: an operand that
            // runs off the top of the address space cannot fit before the segment
            // end. saturating_add would cap it at 0xFFFF_FFFF and mask the overflow
            // (MDD-BUG-CRUCIBLE-00022). In 16-bit addressing the EA is <= 0xFFFF,
            // but the full operand must still fit within the protected-mode segment.
            let exceeds_limit = match addr.checked_add(access_size - 1) {
                None => true,
                Some(last_byte_offset) => {
                    if expand_down {
                        addr <= effective_limit || last_byte_offset > max
                    } else {
                        last_byte_offset > effective_limit
                    }
                }
            };

            if exceeds_limit {
                tracing::debug!(
                    target: "gp64::cpu::segment_fault",
                    segment = format_args!("{:04X}", segment),
                    seg_reg = ?seg_reg,
                    addr = format_args!("{:08X}", addr),
                    access_size,
                    effective_limit = format_args!("{:08X}", effective_limit),
                    expand_down,
                    max = format_args!("{:08X}", max),
                    cs = format_args!("{:04X}", cpu.regs.cs),
                    ip = format_args!("{:04X}", cpu.regs.ip()),
                    "Segment limit exceeded"
                );
                // SS segment violations generate #SS (vector 12), others generate #GP (vector 13)
                if seg_reg == ResolvedSegment::Ss {
                    return Err(CpuError::StackFault);
                }
                return Err(CpuError::GeneralProtectionFault);
            }
        }
    } else if access_size > 0 && cpu.cpu_type.supports_286() {
        // Real mode or V86 on a 286+.
        //
        // Check against the cached segment limit. In normal real mode this is
        // 0xFFFF (64KB). In "unreal mode" (big real mode), the cache retains
        // the larger limit set during protected mode.
        //
        // MDD-BUG-CRUCIBLE-00577: this was gated on `!is_16bit`, so a
        // 16-bit-addressed `MOV AX,[FFFFh]` skipped the check entirely and the
        // walk below wrapped its second byte to offset 0 — 8086 behaviour on a
        // CPU that must raise #GP(0) (or #SS through SS) instead. The fast
        // interpreter already pins the fault, so the two engines disagreed.
        // Address size bounds where the operand STARTS; it never licenses the
        // operand to run past the segment end.
        let seg_limit = seg_reg.cached_limit(cpu);
        // Overflow past 0xFFFF_FFFF always exceeds the limit (MDD-BUG-CRUCIBLE-00022).
        let exceeds = match addr.checked_add(access_size - 1) {
            None => true,
            Some(last_byte_offset) => last_byte_offset > seg_limit,
        };
        if exceeds {
            if seg_reg == ResolvedSegment::Ss {
                return Err(CpuError::StackFault);
            }
            return Err(CpuError::GeneralProtectionFault);
        }
    }

    Ok(())
}

/// Whether per-byte operand offsets wrap at 64 KB rather than advancing linearly.
///
/// Only an 8086/80186 wraps: it has no segment limits, so `MOV AX,[FFFFh]`
/// legitimately reads offset FFFFh then offset 0 of the same segment. From the
/// 286 on — real, V86, or protected mode — [`check_operand_segment_limits`] has
/// already proved the whole span fits inside the segment before any byte is
/// touched, so a span that reaches here must advance linearly: in normal real
/// mode nothing can cross FFFFh at all, and under a retained unreal-mode (or
/// big protected-mode) limit the next byte belongs at 10000h, not at 0.
#[inline]
fn operand_offsets_wrap(cpu: &Cpu, is_16bit: bool) -> bool {
    is_16bit && !cpu.is_protected_mode() && !cpu.cpu_type.supports_286()
}

/// Advance an operand offset by `delta` bytes, honoring 8086 64 KB segment wrap.
#[inline]
fn advance_operand_offset(offset: u32, delta: u32, wraps: bool) -> u32 {
    if wraps {
        offset.wrapping_add(delta) & 0xFFFF
    } else {
        offset.wrapping_add(delta)
    }
}

/// Translate one byte of an already-validated operand through the hidden cache
/// belonging to the decoded segment register.
///
/// In protected mode, visible selector equality does not imply hidden-cache
/// equality: software can load the same selector into two segment registers and
/// later retain different caches after descriptor-table changes. Selector-based
/// lookup would therefore choose whichever alias happens to be scanned first.
#[inline]
fn translate_operand_address_full_checked<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    seg_reg: ResolvedSegment,
    segment: u16,
    offset: u32,
    access: PageAccess,
) -> Result<usize> {
    if cpu.is_protected_mode() {
        let linear = seg_reg.cache(cpu).base.wrapping_add(offset);
        cpu.translate_linear_checked(mem, linear, access)
    } else {
        cpu.translate_address_full_checked(mem, segment, offset, access)
    }
}

/// Walk the physical page runs a `len`-byte operand at `segment:offset` occupies,
/// invoking `f(mem, phys, run_offset, run_len)` for each maximal run that stays
/// within a single 4 KiB physical page.
///
/// Each run is contiguous in host physical memory, so callers can read/write
/// `run_len` bytes from `phys` directly. Paging is translated per run start, so an
/// operand crossing a linear page boundary is served from the correct (possibly
/// non-adjacent) physical pages, and a not-present later page raises `#PF` at the
/// run that first touches it — instead of silently reading/writing whatever
/// physical bytes happened to follow the first page. Fixes MDD-BUG-CRUCIBLE-00021.
fn walk_operand_pages<M: PagedMemory, F>(
    cpu: &Cpu,
    mem: &mut M,
    seg_reg: ResolvedSegment,
    segment: u16,
    offset: u32,
    len: usize,
    is_16bit: bool,
    access: PageAccess,
    mut f: F,
) -> Result<()>
where
    F: FnMut(&mut M, usize, usize, usize) -> Result<()>,
{
    let wraps = operand_offsets_wrap(cpu, is_16bit);
    let mut done = 0usize;
    while done < len {
        let cur_off = advance_operand_offset(offset, done as u32, wraps);
        let phys =
            translate_operand_address_full_checked(cpu, mem, seg_reg, segment, cur_off, access)?;
        // Bytes remaining in this physical page. Paging preserves the low 12 bits,
        // so the linear-page boundary is at the same offset as the physical one.
        let page_left = 0x1000 - (phys & 0xFFF);
        let mut run = page_left.min(len - done);
        if wraps {
            // An 8086 operand wraps at offset 0x10000, which is a page boundary
            // only when the segment base is page aligned; end the run there so
            // the next one starts at the wrapped offset (MDD-BUG-TEM-00179).
            run = run.min(0x1_0000 - cur_off as usize);
        }
        f(mem, phys, done, run)?;
        done += run;
    }
    Ok(())
}

/// Translate and permission-check every lane of a scalar operand before any
/// physical-device callback can run (seam B, contract LLD §3.9). Per-lane
/// integer addresses survive page-walk backing changes, preserve unpaged
/// sub-page mappings, and follow page and 8086 offset crossings.
#[cold]
#[inline(never)]
fn prepare_scalar_operand<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    (seg_reg, segment, offset, is_16bit): (ResolvedSegment, u16, u32, bool),
    width: usize,
    access: PageAccess,
) -> Result<[usize; 8]> {
    debug_assert!(matches!(width, 1 | 2 | 4 | 8));
    let wraps = operand_offsets_wrap(cpu, is_16bit);
    let mut physical = [0usize; 8];
    for (lane, address) in physical.iter_mut().take(width).enumerate() {
        let offset = advance_operand_offset(offset, lane as u32, wraps);
        *address =
            translate_operand_address_full_checked(cpu, mem, seg_reg, segment, offset, access)?;
    }
    Ok(physical)
}

/// Read a scalar operand through [`PagedMemory::read_translated_scalar`].
#[cold]
#[inline(never)]
fn read_physical_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    resolved: (ResolvedSegment, u16, u32, bool),
    buf: &mut [u8],
    access: PageAccess,
) -> Result<()> {
    let physical = prepare_scalar_operand(cpu, mem, resolved, buf.len(), access)?;
    let value = mem.read_translated_scalar(&physical[..buf.len()]);
    buf.copy_from_slice(&value.to_le_bytes()[..buf.len()]);
    Ok(())
}

/// Write a scalar operand through [`PagedMemory::write_translated_scalar`].
#[cold]
#[inline(never)]
fn write_physical_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    resolved: (ResolvedSegment, u16, u32, bool),
    buf: &[u8],
) -> Result<()> {
    let physical = prepare_scalar_operand(cpu, mem, resolved, buf.len(), PageAccess::Write)?;
    let mut bytes = [0u8; 8];
    bytes[..buf.len()].copy_from_slice(buf);
    mem.write_translated_scalar(&physical[..buf.len()], u64::from_le_bytes(bytes));
    Ok(())
}

#[cfg(feature = "iced-interop")]
/// Read the current instruction's memory operand (`buf.len()` bytes) into `buf`,
/// translating every page the operand spans.
///
/// Unlike a single `calc_effective_address` + contiguous read, this delivers a
/// `#PF` (or reads from the correct physical page) when the operand crosses a
/// linear page boundary. Used by the x87 memory helpers, whose operands range
/// from 2 bytes (FLDCW) to 108 bytes (FRSTOR) and can straddle a page boundary.
pub fn read_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    instr: &Instruction,
    mem: &mut M,
    buf: &mut [u8],
) -> Result<()> {
    read_operand_bytes_resolved(cpu, mem, buf, resolve_operand_ea(cpu, instr))
}

#[cfg(feature = "iced-interop")]
/// Read a read-modify-write operand after validating every spanned page for writes.
///
/// RMW instructions report paging failures as write accesses even when the
/// comparison or arithmetic would ultimately preserve the old memory value.
/// Translating with [`PageAccess::Write`] also guarantees that no registers or
/// flags need to be published before a later page's write permission is known.
pub fn read_rmw_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    instr: &Instruction,
    mem: &mut M,
    buf: &mut [u8],
) -> Result<()> {
    read_rmw_operand_bytes_resolved(cpu, mem, buf, resolve_operand_ea(cpu, instr))
}

fn read_rmw_operand_bytes_resolved<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    buf: &mut [u8],
    (seg_reg, segment, offset, is_16bit): (ResolvedSegment, u16, u32, bool),
) -> Result<()> {
    check_operand_segment_limits(cpu, seg_reg, segment, offset, buf.len() as u32)?;
    check_segment_access_resolved(cpu, segment, seg_reg, PageAccess::Write)?;
    if M::SCALAR_DISPATCH && matches!(buf.len(), 1 | 2 | 4 | 8) {
        return read_physical_operand_bytes(
            cpu,
            mem,
            (seg_reg, segment, offset, is_16bit),
            buf,
            PageAccess::Write,
        );
    }
    walk_operand_pages(
        cpu,
        mem,
        seg_reg,
        segment,
        offset,
        buf.len(),
        is_16bit,
        PageAccess::Write,
        |mem, phys, run_off, run_len| {
            mem.read_run(phys, &mut buf[run_off..run_off + run_len]);
            Ok(())
        },
    )
}

/// Read an x87 descriptor's memory operand without constructing an iced
/// instruction. Address, segment, limit, paging, and fault ordering are shared
/// with the general operand path.
pub fn read_x87_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    operand: &X87MemoryOperand,
    mem: &mut M,
    buf: &mut [u8],
) -> Result<()> {
    read_operand_bytes_resolved(cpu, mem, buf, resolve_x87_operand_ea(cpu, operand))
}

/// Read an SSE descriptor's memory operand, sharing the address, segment,
/// limit, paging, and fault ordering of the general operand path.
pub fn read_sse_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    memory: &SseMemory,
    mem: &mut M,
    buf: &mut [u8],
) -> Result<()> {
    read_operand_bytes_resolved(cpu, mem, buf, resolve_sse_operand_ea(cpu, memory))
}

/// Read an SSE descriptor's memory operand as a read-modify-write, so a
/// masked-off MASKMOV* byte still faults as the write it architecturally is.
pub fn read_rmw_sse_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    memory: &SseMemory,
    mem: &mut M,
    buf: &mut [u8],
) -> Result<()> {
    read_rmw_operand_bytes_resolved(cpu, mem, buf, resolve_sse_operand_ea(cpu, memory))
}

fn read_operand_bytes_resolved<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    buf: &mut [u8],
    (seg_reg, segment, offset, is_16bit): (ResolvedSegment, u16, u32, bool),
) -> Result<()> {
    check_operand_segment_limits(cpu, seg_reg, segment, offset, buf.len() as u32)?;
    check_segment_access_resolved(cpu, segment, seg_reg, PageAccess::Read)?;
    if M::SCALAR_DISPATCH && matches!(buf.len(), 1 | 2 | 4 | 8) {
        return read_physical_operand_bytes(
            cpu,
            mem,
            (seg_reg, segment, offset, is_16bit),
            buf,
            PageAccess::Read,
        );
    }
    walk_operand_pages(
        cpu,
        mem,
        seg_reg,
        segment,
        offset,
        buf.len(),
        is_16bit,
        PageAccess::Read,
        |mem, phys, run_off, run_len| {
            mem.read_run(phys, &mut buf[run_off..run_off + run_len]);
            Ok(())
        },
    )
}

#[cfg(feature = "iced-interop")]
/// Write `buf` to the current instruction's memory operand, translating every page
/// the operand spans.
///
/// All spanned pages are translated (and write-checked) *before* any byte is
/// written, so a `#PF` on a later page leaves memory unmodified and the faulting
/// instruction is restartable — no partial side effects. Fixes MDD-BUG-CRUCIBLE-00021.
pub fn write_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    instr: &Instruction,
    mem: &mut M,
    buf: &[u8],
) -> Result<()> {
    write_operand_bytes_resolved(cpu, mem, buf, resolve_operand_ea(cpu, instr))
}

/// Write an x87 descriptor's memory operand after pre-translating every page,
/// preserving the existing all-or-nothing fault behavior.
pub fn write_x87_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    operand: &X87MemoryOperand,
    mem: &mut M,
    buf: &[u8],
) -> Result<()> {
    write_operand_bytes_resolved(cpu, mem, buf, resolve_x87_operand_ea(cpu, operand))
}

/// Write an SSE descriptor's memory operand, keeping the all-or-nothing fault
/// behaviour of the general operand path.
pub fn write_sse_operand_bytes<M: PagedMemory>(
    cpu: &Cpu,
    memory: &SseMemory,
    mem: &mut M,
    buf: &[u8],
) -> Result<()> {
    write_operand_bytes_resolved(cpu, mem, buf, resolve_sse_operand_ea(cpu, memory))
}

/// One resolved physical run of an operand, recorded by the fault-atomicity
/// pre-walk so the commit pass does not have to walk again.
///
/// These are INTEGERS, never a host pointer and never a borrowed slice. A
/// later page's walk can reach `write_u32_physical_ad`, which on a real
/// `Memory` can grow (and therefore reallocate) the RAM vector — a recorded
/// pointer would dangle, a recorded physical address cannot (A052 G5).
#[derive(Clone, Copy)]
struct OperandRun {
    physical: usize,
    buf_offset: usize,
    len: usize,
}

impl OperandRun {
    const EMPTY: Self = Self {
        physical: 0,
        buf_offset: 0,
        len: 0,
    };
}

/// The widest operand is FRSTOR/FNSAVE's 108 bytes, which spans at most two
/// 4 KiB pages; an 8086 64 KiB operand wrap can split one of those again.
/// Four is slack, and an operand that somehow needs more falls back to the
/// two-walk path rather than being truncated.
const MAX_OPERAND_RUNS: usize = 4;

/// A052 engagement census for the resolved-operand write funnel.
///
/// NON-test and always on: the keep/revert verdict is decided on a PGO
/// artifact, which carries no `cfg(test)`, so the evidence has to come out of
/// the shipped binary. Two relaxed atomic adds per operand, against a page
/// walk that costs hundreds of cycles.
///
/// This counts FUNNEL traffic only — how many operands took the recorded-run
/// replay and how many runs that was. Whether those runs were bulk-copied or
/// fell back to the per-byte dispatch is a different question, answered by
/// `Memory::span_write_census`. Reading engagement off this census alone
/// would be reading the funnel's throughput as if it were bulk hits.
pub mod operand_write_census {
    use gp64_types::span_write_census::{width_bucket, WIDTH_BUCKETS};
    use std::sync::atomic::{AtomicU64, Ordering};

    static OPERANDS: [AtomicU64; WIDTH_BUCKETS] = [const { AtomicU64::new(0) }; WIDTH_BUCKETS];
    static RUNS: AtomicU64 = AtomicU64::new(0);
    static FALLBACKS: [AtomicU64; WIDTH_BUCKETS] = [const { AtomicU64::new(0) }; WIDTH_BUCKETS];

    /// One operand committed by replaying `runs` recorded runs.
    #[inline]
    pub fn note_replay(len: usize, runs: usize) {
        OPERANDS[width_bucket(len)].fetch_add(1, Ordering::Relaxed);
        RUNS.fetch_add(runs as u64, Ordering::Relaxed);
    }

    /// One operand that overflowed the recorded-run array and took the
    /// two-walk fallback.
    #[inline]
    pub fn note_fallback(len: usize) {
        FALLBACKS[width_bucket(len)].fetch_add(1, Ordering::Relaxed);
    }

    /// `(operands by width, total runs replayed, fallbacks by width)`.
    pub fn snapshot() -> ([u64; WIDTH_BUCKETS], u64, [u64; WIDTH_BUCKETS]) {
        let mut operands = [0u64; WIDTH_BUCKETS];
        let mut fallbacks = [0u64; WIDTH_BUCKETS];
        for bucket in 0..WIDTH_BUCKETS {
            operands[bucket] = OPERANDS[bucket].load(Ordering::Relaxed);
            fallbacks[bucket] = FALLBACKS[bucket].load(Ordering::Relaxed);
        }
        (operands, RUNS.load(Ordering::Relaxed), fallbacks)
    }
}

fn write_operand_bytes_resolved<M: PagedMemory>(
    cpu: &Cpu,
    mem: &mut M,
    buf: &[u8],
    (seg_reg, segment, offset, is_16bit): (ResolvedSegment, u16, u32, bool),
) -> Result<()> {
    check_operand_segment_limits(cpu, seg_reg, segment, offset, buf.len() as u32)?;
    check_segment_access_resolved(cpu, segment, seg_reg, PageAccess::Write)?;
    if M::SCALAR_DISPATCH && matches!(buf.len(), 1 | 2 | 4 | 8) {
        return write_physical_operand_bytes(cpu, mem, (seg_reg, segment, offset, is_16bit), buf);
    }
    // Pass 1: pre-translate every touched page so a #PF aborts before any
    // write, recording each resolved run on the way through. Recording does
    // not change what pass 1 does — every page is still walked, so A/D bits
    // and #PF ordering are exactly as before.
    let mut runs = [OperandRun::EMPTY; MAX_OPERAND_RUNS];
    let mut recorded = 0usize;
    let mut overflowed = false;
    walk_operand_pages(
        cpu,
        mem,
        seg_reg,
        segment,
        offset,
        buf.len(),
        is_16bit,
        PageAccess::Write,
        |_mem, phys, run_off, run_len| {
            if recorded < MAX_OPERAND_RUNS {
                runs[recorded] = OperandRun {
                    physical: phys,
                    buf_offset: run_off,
                    len: run_len,
                };
                recorded += 1;
            } else {
                // Assert and fall back, never truncate. The walk deliberately
                // continues so every remaining page still faults in pass 1.
                overflowed = true;
            }
            Ok(())
        },
    )?;
    debug_assert!(
        !overflowed,
        "a {}-byte operand needed more than {MAX_OPERAND_RUNS} runs",
        buf.len()
    );
    if !overflowed {
        // Pass 2: replay the recorded runs. Nothing between the two passes can
        // stale them — pass 1 commits no bytes and fires no notifier, and the
        // walker's A/D write-backs only OR status bits 5/6 into a PTE, which
        // cannot move a frame.
        for run in &runs[..recorded] {
            mem.write_run(run.physical, &buf[run.buf_offset..run.buf_offset + run.len]);
        }
        operand_write_census::note_replay(buf.len(), recorded);
        return Ok(());
    }
    operand_write_census::note_fallback(buf.len());
    // Overflow fallback: re-walk to commit, exactly as the funnel used to.
    walk_operand_pages(
        cpu,
        mem,
        seg_reg,
        segment,
        offset,
        buf.len(),
        is_16bit,
        PageAccess::Write,
        |mem, phys, run_off, run_len| {
            for k in 0..run_len {
                mem.write_u8(phys + k, buf[run_off + k]);
            }
            Ok(())
        },
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::registers::SegmentCache;
    use crate::x87_decode::X87Operand;
    use gp64_memory::Memory;
    use iced_x86::{Decoder, DecoderOptions};

    // ---- Helper: decode one instruction from bytes ----

    #[cfg(feature = "iced-interop")]
    fn decode16(bytes: &[u8]) -> Instruction {
        let mut decoder = Decoder::with_ip(16, bytes, 0, DecoderOptions::NONE);
        decoder.decode()
    }

    #[cfg(feature = "iced-interop")]
    fn decode32(bytes: &[u8]) -> Instruction {
        let mut decoder = Decoder::with_ip(32, bytes, 0, DecoderOptions::NONE);
        decoder.decode()
    }

    // ---- Helper: simple PagedMemory impl for tests ----

    struct NullMem;
    impl gp64_types::MemoryAccess for NullMem {
        fn read_u8(&self, _: usize) -> u8 {
            0
        }
        fn read_u16(&self, _: usize) -> u16 {
            0
        }
        fn read_u32(&self, _: usize) -> u32 {
            0
        }
        fn write_u8(&mut self, _: usize, _: u8) {}
        fn write_u16(&mut self, _: usize, _: u16) {}
        fn write_u32(&mut self, _: usize, _: u32) {}
        fn read_u64(&self, _: usize) -> u64 {
            0
        }
        fn write_u64(&mut self, _: usize, _: u64) {}
    }
    impl PagedMemory for NullMem {
        fn map_linear_physical(&self, _: u32) -> Option<usize> {
            None
        }
        fn tlb_lookup(&self, _: u32) -> Option<(u32, bool, bool, bool)> {
            None
        }
        fn tlb_insert(&mut self, _: u32, _: u32, _: bool, _: bool, _: bool) {}
        fn read_u32_physical(&self, _: usize) -> u32 {
            0
        }
        fn read_u8_physical_checked(&self, _: usize) -> Option<u8> {
            None
        }
        fn write_u32_physical(&mut self, _: usize, _: u32) {}
        fn set_page_fault(&mut self, _: u32, _: u32) {}
        fn clear_page_fault(&mut self) {}
        fn page_fault_pending(&self) -> bool {
            false
        }
        fn page_fault_address(&self) -> u32 {
            0
        }
        fn page_fault_error_code(&self) -> u32 {
            0
        }
        #[cfg(feature = "iced-interop")]
        fn invalidate_tlb(&mut self) {}
        fn invalidate_tlb_page(&mut self, _: u32) {}
    }

    // ---- Helper: set up protected mode CPU with writable data segment ----

    fn cpu_protected_mode() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1; // PE=1
                           // Set up DS cache as writable data segment with 64KB limit
        let ds_cache = SegmentCache {
            base: 0x0001_0000,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: false,
        };
        cpu.regs.ds = 0x0010;
        cpu.regs.ds_cache = ds_cache;
        // CS cache (code, readable)
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x9B,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: true,
            is_conforming: false,
            is_readable: true,
            is_writable: false,
            granularity: false,
            big: false,
        };
        // SS cache (writable data)
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0x0002_0000,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: false,
        };
        cpu
    }

    // ========================================================================
    // read_register tests
    // ========================================================================

    #[test]
    fn test_read_register_8bit_low() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0x1234_5678;
        cpu.regs.ebx = 0xAABB_CCDD;
        cpu.regs.ecx = 0x1111_2233;
        cpu.regs.edx = 0x4455_6677;

        assert_eq!(read_register(&cpu, Register::AL), 0x78);
        assert_eq!(read_register(&cpu, Register::BL), 0xDD);
        assert_eq!(read_register(&cpu, Register::CL), 0x33);
        assert_eq!(read_register(&cpu, Register::DL), 0x77);
    }

    #[test]
    fn test_read_register_8bit_high() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0x1234_5678;
        cpu.regs.ebx = 0xAABB_CCDD;
        cpu.regs.ecx = 0x1111_2233;
        cpu.regs.edx = 0x4455_6677;

        assert_eq!(read_register(&cpu, Register::AH), 0x56);
        assert_eq!(read_register(&cpu, Register::BH), 0xCC);
        assert_eq!(read_register(&cpu, Register::CH), 0x22);
        assert_eq!(read_register(&cpu, Register::DH), 0x66);
    }

    #[test]
    fn test_read_register_16bit() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0x1234_5678;
        cpu.regs.ebx = 0xAABB_CCDD;
        cpu.regs.ecx = 0x1111_2233;
        cpu.regs.edx = 0x4455_6677;
        cpu.regs.esi = 0x8899_AABB;
        cpu.regs.edi = 0xCCDD_EEFF;
        cpu.regs.ebp = 0x1122_3344;
        cpu.regs.esp = 0x5566_7788;

        assert_eq!(read_register(&cpu, Register::AX), 0x5678);
        assert_eq!(read_register(&cpu, Register::BX), 0xCCDD);
        assert_eq!(read_register(&cpu, Register::CX), 0x2233);
        assert_eq!(read_register(&cpu, Register::DX), 0x6677);
        assert_eq!(read_register(&cpu, Register::SI), 0xAABB);
        assert_eq!(read_register(&cpu, Register::DI), 0xEEFF);
        assert_eq!(read_register(&cpu, Register::BP), 0x3344);
        assert_eq!(read_register(&cpu, Register::SP), 0x7788);
    }

    #[test]
    fn test_read_register_32bit() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xDEAD_BEEF;
        cpu.regs.ebx = 0xCAFE_BABE;
        cpu.regs.ecx = 0x1234_5678;
        cpu.regs.edx = 0x9ABC_DEF0;
        cpu.regs.esi = 0x1111_1111;
        cpu.regs.edi = 0x2222_2222;
        cpu.regs.ebp = 0x3333_3333;
        cpu.regs.esp = 0x4444_4444;

        assert_eq!(read_register(&cpu, Register::EAX), 0xDEAD_BEEF);
        assert_eq!(read_register(&cpu, Register::EBX), 0xCAFE_BABE);
        assert_eq!(read_register(&cpu, Register::ECX), 0x1234_5678);
        assert_eq!(read_register(&cpu, Register::EDX), 0x9ABC_DEF0);
        assert_eq!(read_register(&cpu, Register::ESI), 0x1111_1111);
        assert_eq!(read_register(&cpu, Register::EDI), 0x2222_2222);
        assert_eq!(read_register(&cpu, Register::EBP), 0x3333_3333);
        assert_eq!(read_register(&cpu, Register::ESP), 0x4444_4444);
    }

    #[test]
    fn test_read_register_segment() {
        let mut cpu = Cpu::new();
        cpu.regs.cs = 0x1000;
        cpu.regs.ds = 0x2000;
        cpu.regs.es = 0x3000;
        cpu.regs.fs = 0x4000;
        cpu.regs.gs = 0x5000;
        cpu.regs.ss = 0x6000;

        assert_eq!(read_register(&cpu, Register::CS), 0x1000);
        assert_eq!(read_register(&cpu, Register::DS), 0x2000);
        assert_eq!(read_register(&cpu, Register::ES), 0x3000);
        assert_eq!(read_register(&cpu, Register::FS), 0x4000);
        assert_eq!(read_register(&cpu, Register::GS), 0x5000);
        assert_eq!(read_register(&cpu, Register::SS), 0x6000);
    }

    #[test]
    fn test_read_register_unknown_returns_zero() {
        let cpu = Cpu::new();
        // CR0, MM0, XMM0 etc. are not handled — should return 0
        assert_eq!(read_register(&cpu, Register::CR0), 0);
    }

    // ========================================================================
    // is_16bit_addressing tests
    // ========================================================================

    #[test]
    fn test_is_16bit_addressing_16bit_mode() {
        let cpu = Cpu::new();
        // MOV AX, [BX+SI+0x10] — 16-bit addressing with BX+SI base/index
        let instr = decode16(&[0x8B, 0x40, 0x10]); // MOV AX, [BX+SI+10h]
        assert!(is_16bit_addressing(&cpu, &instr));
    }

    #[test]
    fn test_is_16bit_addressing_32bit_mode() {
        let cpu = Cpu::new();
        // MOV EAX, [EBX+0x10] — 32-bit addressing
        let instr = decode32(&[0x8B, 0x43, 0x10]);
        assert!(!is_16bit_addressing(&cpu, &instr));
    }

    #[test]
    fn test_is_16bit_addressing_direct_16bit() {
        let cpu = Cpu::new();
        // MOV AX, [0x1234] — 16-bit direct address (mod=00, rm=110)
        let instr = decode16(&[0xA1, 0x34, 0x12]);
        assert!(is_16bit_addressing(&cpu, &instr));
    }

    #[test]
    fn test_is_16bit_addressing_32bit_disp() {
        let cpu = Cpu::new();
        // MOV EAX, [0x12345678] — 32-bit direct address
        let instr = decode32(&[0xA1, 0x78, 0x56, 0x34, 0x12]);
        assert!(!is_16bit_addressing(&cpu, &instr));
    }

    // ========================================================================
    // segment_access_fault tests (via check_segment_access)
    // ========================================================================

    #[test]
    fn test_check_segment_access_real_mode_always_ok() {
        let cpu = Cpu::new(); // Real mode by default (CR0.PE=0)
                              // In real mode, any access should succeed
        assert!(check_segment_access(&cpu, 0x1234, Register::DS, PageAccess::Read).is_ok());
        assert!(check_segment_access(&cpu, 0x1234, Register::DS, PageAccess::Write).is_ok());
        assert!(check_segment_access(&cpu, 0x1234, Register::SS, PageAccess::Write).is_ok());
    }

    #[test]
    fn test_check_segment_access_not_present_gp() {
        let mut cpu = cpu_protected_mode();
        // Make DS not present
        cpu.regs.ds_cache.present = false;
        let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_check_segment_access_not_present_ss() {
        let mut cpu = cpu_protected_mode();
        // Make SS not present — should give StackFault
        cpu.regs.ss_cache.present = false;
        let result = check_segment_access(&cpu, cpu.regs.ss, Register::SS, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_check_segment_access_write_to_code_segment() {
        let cpu = cpu_protected_mode();
        // CS is a code segment — writing should fail
        let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Write);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_check_segment_access_read_non_readable_code() {
        let mut cpu = cpu_protected_mode();
        // Make CS non-readable (execute-only)
        cpu.regs.cs_cache.is_readable = false;
        let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_check_segment_access_fetch_non_readable_code_ok() {
        let mut cpu = cpu_protected_mode();
        // Execute-only code segment — Fetch should succeed even if not readable
        cpu.regs.cs_cache.is_readable = false;
        let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Fetch);
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_segment_access_write_non_writable_data() {
        let mut cpu = cpu_protected_mode();
        // Make DS read-only
        cpu.regs.ds_cache.is_writable = false;
        let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Write);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_check_segment_access_read_writable_data_ok() {
        let cpu = cpu_protected_mode();
        // Read from writable data segment should succeed
        let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_segment_access_write_writable_data_ok() {
        let cpu = cpu_protected_mode();
        let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Write);
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_segment_access_read_readable_code_ok() {
        let cpu = cpu_protected_mode();
        // CS is readable — read should succeed
        let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn ds_moffs32_uses_hidden_base_and_checks_the_full_span() {
        let cpu = cpu_protected_mode();

        assert_eq!(
            calc_ds_moffs32_linear(&cpu, 0x1234, 4, PageAccess::Read).unwrap(),
            0x0001_1234
        );
        assert!(matches!(
            calc_ds_moffs32_linear(&cpu, 0xFFFD, 4, PageAccess::Read),
            Err(CpuError::GeneralProtectionFault)
        ));
    }

    #[test]
    fn ds_moffs32_rejects_an_unusable_data_segment_before_paging() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.present = false;

        assert!(matches!(
            calc_ds_moffs32_linear(&cpu, 0x20, 4, PageAccess::Read),
            Err(CpuError::GeneralProtectionFault)
        ));
    }

    #[test]
    fn ss_offset32_uses_hidden_base_and_preserves_stack_faults() {
        let mut cpu = cpu_protected_mode();

        assert_eq!(
            calc_ss_offset32_linear(&cpu, 0x1234, 4, PageAccess::Read).unwrap(),
            0x0002_1234
        );
        cpu.regs.ss_cache.limit = 0x20;
        cpu.regs.ss_cache.cached_effective_limit = 0x20;
        assert!(matches!(
            calc_ss_offset32_linear(&cpu, 0x20, 4, PageAccess::Read),
            Err(CpuError::StackFault)
        ));
    }

    // ========================================================================
    // calc_effective_address tests — real mode
    // ========================================================================

    #[test]
    fn test_ea_real_mode_direct_address() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x1000;
        let mut mem = NullMem;
        // MOV AX, [0x0100] — DS:0x0100 → linear 0x10100
        let instr = decode16(&[0xA1, 0x00, 0x01]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x10100);
    }

    #[test]
    fn test_ea_real_mode_bx_base() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x2000;
        cpu.regs.ebx = 0x0050;
        let mut mem = NullMem;
        // MOV AL, [BX] — DS:BX → linear 0x20050
        let instr = decode16(&[0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x20050);
    }

    #[test]
    fn test_ea_real_mode_bx_plus_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x3000;
        cpu.regs.ebx = 0x0100;
        let mut mem = NullMem;
        // MOV AL, [BX+0x20] — DS:(BX+0x20) → linear 0x30120
        let instr = decode16(&[0x8A, 0x47, 0x20]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x30120);
    }

    #[test]
    fn test_ea_real_mode_bp_uses_ss() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x5000;
        cpu.regs.ebp = 0x0200;
        let mut mem = NullMem;
        // MOV AL, [BP+0x04] — SS:BP+4 → linear 0x50204
        let instr = decode16(&[0x8A, 0x46, 0x04]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x50204);
    }

    #[test]
    fn test_ea_real_mode_16bit_wrap() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0xFFF0;
        let mut mem = NullMem;
        // MOV AL, [BX+0x20] — offset wraps: (0xFFF0+0x20) & 0xFFFF = 0x0010
        let instr = decode16(&[0x8A, 0x47, 0x20]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0010);
    }

    #[test]
    fn test_ea_real_mode_es_segment() {
        let mut cpu = Cpu::new();
        cpu.regs.es = 0x4000;
        cpu.regs.edi = 0x0080;
        let mut mem = NullMem;
        // STOSB uses ES:DI — ES segment override: MOV AL, ES:[DI]
        // 26h = ES override prefix, 8A 05 = MOV AL, [DI]
        let instr = decode16(&[0x26, 0x8A, 0x05]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x40080);
    }

    // ========================================================================
    // calc_effective_address tests — 32-bit addressing
    // ========================================================================

    #[test]
    fn test_ea_32bit_base_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0000_1234;
        let mut mem = NullMem;
        // MOV AL, [EBX] — 32-bit addressing
        let instr = decode32(&[0x8A, 0x03]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x1234);
    }

    #[test]
    fn test_ea_32bit_base_plus_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ecx = 0x0000_5000;
        let mut mem = NullMem;
        // MOV AL, [ECX+0x10]
        let instr = decode32(&[0x8A, 0x41, 0x10]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x5010);
    }

    #[test]
    fn test_ea_32bit_base_plus_disp32() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0000_1000;
        let mut mem = NullMem;
        // MOV AL, [EAX+0x00002000]
        let instr = decode32(&[0x8A, 0x80, 0x00, 0x20, 0x00, 0x00]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x3000);
    }

    #[test]
    fn test_ea_32bit_sib_base_plus_index() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0000_1000;
        cpu.regs.ecx = 0x0000_0200;
        let mut mem = NullMem;
        // MOV AL, [EBX+ECX] — SIB byte: scale=1, index=ECX, base=EBX
        let instr = decode32(&[0x8A, 0x04, 0x0B]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x1200);
    }

    #[test]
    fn test_ea_32bit_sib_index_scaled() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0000_1000;
        cpu.regs.ecx = 0x0000_0040;
        let mut mem = NullMem;
        // MOV AL, [EAX+ECX*4] — SIB: scale=4, index=ECX, base=EAX
        let instr = decode32(&[0x8A, 0x04, 0x88]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x1100);
    }

    #[test]
    fn test_ea_32bit_sib_index_scale8() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.edx = 0x0000_2000;
        cpu.regs.esi = 0x0000_0010;
        let mut mem = NullMem;
        // MOV AL, [EDX+ESI*8] — SIB: scale=8, index=ESI, base=EDX
        let instr = decode32(&[0x8A, 0x04, 0xF2]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x2080);
    }

    #[test]
    fn test_ea_32bit_disp32_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;
        // MOV AL, [0x00004000] — mod=00, rm=101 → disp32 only
        let instr = decode32(&[0x8A, 0x05, 0x00, 0x40, 0x00, 0x00]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x4000);
    }

    #[test]
    fn test_ea_32bit_ebp_base_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.ebp = 0x0000_FF00;
        let mut mem = NullMem;
        // MOV AL, [EBP+8] — uses SS segment, EBP-based
        let instr = decode32(&[0x8A, 0x45, 0x08]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0xFF08);
    }

    #[test]
    fn test_ea_32bit_esp_base_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.esp = 0x0000_ABCD;
        let mut mem = NullMem;
        // MOV AL, [ESP+4] — SIB byte (mod=01, rm=100 → SIB with base=ESP, no index)
        let instr = decode32(&[0x8A, 0x44, 0x24, 0x04]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0xABD1);
    }

    // ========================================================================
    // calc_effective_address — segment override tests
    // ========================================================================

    #[test]
    fn test_ea_fs_segment_override() {
        let mut cpu = Cpu::new();
        cpu.regs.fs = 0x7000;
        cpu.regs.ebx = 0x0100;
        let mut mem = NullMem;
        // FS:[BX] — 64h prefix = FS override
        let instr = decode16(&[0x64, 0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x70100);
    }

    #[test]
    fn test_ea_gs_segment_override() {
        let mut cpu = Cpu::new();
        cpu.regs.gs = 0x8000;
        cpu.regs.ebx = 0x0200;
        let mut mem = NullMem;
        // GS:[BX] — 65h prefix = GS override
        let instr = decode16(&[0x65, 0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x80200);
    }

    #[test]
    fn test_ea_cs_segment_override() {
        let mut cpu = Cpu::new();
        cpu.regs.cs = 0x9000;
        cpu.regs.ebx = 0x0050;
        let mut mem = NullMem;
        // CS:[BX] — 2Eh prefix = CS override
        let instr = decode16(&[0x2E, 0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x90050);
    }

    #[test]
    fn test_ea_ss_segment_override() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0xA000;
        cpu.regs.ebx = 0x0010;
        let mut mem = NullMem;
        // SS:[BX] — 36h prefix = SS override
        let instr = decode16(&[0x36, 0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0xA0010);
    }

    #[test]
    fn test_ea_pm_es_override_uses_es_cache_when_selector_aliases_ds() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.es = cpu.regs.ds;
        cpu.regs.es_cache = cpu.regs.ds_cache;
        cpu.regs.es_cache.base = 0x0003_0000;
        cpu.regs.ebx = 0x0020;
        let mut mem = NullMem;
        // MOV AL, ES:[BX]. The visible selectors alias, but the hidden caches do not.
        let instr = decode16(&[0x26, 0x8A, 0x07]);

        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();

        assert_eq!(ea, 0x0003_0020);
    }

    #[test]
    fn test_ea_pm_es_override_uses_es_limit_when_selector_aliases_ds() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.es = cpu.regs.ds;
        cpu.regs.es_cache = cpu.regs.ds_cache;
        cpu.regs.es_cache.set_limit(0x000F);
        cpu.regs.ebx = 0x0010;
        let mut mem = NullMem;
        let instr = decode16(&[0x26, 0x8A, 0x07]); // MOV AL, ES:[BX]

        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);

        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_pm_es_override_uses_es_access_rights_when_selector_aliases_ds() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.es = cpu.regs.ds;
        cpu.regs.es_cache = cpu.regs.ds_cache;
        cpu.regs.es_cache.is_writable = false;
        cpu.regs.ebx = 0x0010;
        let mut mem = NullMem;
        let instr = decode16(&[0x26, 0x88, 0x07]); // MOV ES:[BX], AL

        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Write);

        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_operand_span_uses_es_cache_when_selector_aliases_ds() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.es = cpu.regs.ds;
        cpu.regs.es_cache = cpu.regs.ds_cache;
        cpu.regs.es_cache.base = 0x0003_0000;
        cpu.regs.ebx = 0x0020;
        let mut mem = Memory::new();
        mem.write_u32(0x0001_0020, 0x1111_1111);
        mem.write_u32(0x0003_0020, 0x3333_3333);
        let instr = decode16(&[0x26, 0x66, 0x8B, 0x07]); // MOV EAX, ES:[BX]
        let mut bytes = [0; 4];

        read_operand_bytes(&cpu, &instr, &mut mem, &mut bytes).unwrap();

        assert_eq!(bytes, 0x3333_3333u32.to_le_bytes());
    }

    // ========================================================================
    // calc_effective_address — protected mode tests
    // ========================================================================

    #[test]
    fn test_ea_protected_mode_within_limit() {
        let cpu = cpu_protected_mode();
        // DS base=0x10000, limit=0xFFFF. Access at offset 0x100 should succeed.
        let mut mem = NullMem;
        let instr = decode16(&[0xA1, 0x00, 0x01]); // MOV AX, [0x0100]
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        // linear = base(0x10000) + offset(0x100) = 0x10100
        assert_eq!(ea, 0x10100);
    }

    #[test]
    fn test_ea_protected_mode_limit_exceeded_gp() {
        let mut cpu = cpu_protected_mode();
        // DS limit=0x00FF — a small limit
        cpu.regs.ds_cache.set_limit(0x00FF);
        let mut mem = NullMem;
        // MOV AX, [0x0100] — offset 0x100 > limit 0xFF → #GP
        let instr = decode16(&[0xA1, 0x00, 0x01]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_protected_mode_ss_limit_exceeded_sf() {
        let mut cpu = cpu_protected_mode();
        // SS limit=0x00FF
        cpu.regs.ss_cache.set_limit(0x00FF);
        let mut mem = NullMem;
        // MOV AL, [BP+0x00] — BP-based → uses SS. BP=0x0100 > SS limit 0xFF
        cpu.regs.ebp = 0x0100;
        let instr = decode16(&[0x8A, 0x46, 0x00]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_ea_protected_mode_expand_down_in_range() {
        let mut cpu = cpu_protected_mode();
        // Expand-down segment: access byte bit 2 set, data segment
        cpu.regs.ds_cache.access = 0x97; // writable, expand-down
        cpu.regs.ds_cache.set_limit(0x0FFF); // lower bound
        cpu.regs.ds_cache.big = false;
        cpu.regs.ds_cache.is_code = false;
        let mut mem = NullMem;
        // Expand-down: valid range is (limit, max]. For big=false, max=0xFFFF.
        // Offset 0x1000 > 0x0FFF and <= 0xFFFF — should succeed.
        let instr = decode16(&[0xA1, 0x00, 0x10]); // MOV AX, [0x1000]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ea_protected_mode_expand_down_below_limit() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.access = 0x97;
        cpu.regs.ds_cache.set_limit(0x1000);
        cpu.regs.ds_cache.big = false;
        cpu.regs.ds_cache.is_code = false;
        let mut mem = NullMem;
        // Offset 0x0800 <= limit 0x1000 — should fail (#GP)
        let instr = decode16(&[0xA1, 0x00, 0x08]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_protected_mode_big_segment() {
        let mut cpu = cpu_protected_mode();
        // 32-bit segment with large limit (granularity enabled)
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true);
        cpu.regs.ds_cache.big = true;
        let mut mem = NullMem;
        // 32-bit addressing with offset 0x00100000
        let instr = decode32(&[0x8A, 0x05, 0x00, 0x00, 0x10, 0x00]); // MOV AL, [0x00100000]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ea_protected_mode_dword_at_top_of_address_space_faults() {
        // MDD-BUG-CRUCIBLE-00022: a dword access at EA 0xFFFF_FFFF runs off the top
        // of a 4GB segment and must #GP. The old saturating_add(access_size - 1)
        // capped the last-byte offset at 0xFFFF_FFFF, so it passed the limit check.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true);
        cpu.regs.ds_cache.big = true;
        cpu.regs.eax = 0xFFFF_FFFF;
        let mut mem = NullMem;
        // MOV ECX, [EAX] (8B 08): dword read at EA = EAX = 0xFFFFFFFF.
        let instr = decode32(&[0x8B, 0x08]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            matches!(result, Err(CpuError::GeneralProtectionFault)),
            "dword at EA 0xFFFFFFFF in a 4GB segment must #GP, got {result:?}"
        );
    }

    // ========================================================================
    // calc_effective_address — real mode 32-bit addressing limit check
    // ========================================================================

    #[test]
    fn test_ea_real_mode_32bit_within_64k() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0000_FF00;
        let mut mem = NullMem;
        // 32-bit addressing in real mode with 67h prefix
        // MOV CL, [EAX] in 16-bit mode → 67h prefix
        let instr = decode16(&[0x67, 0x8A, 0x08]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(ea.is_ok());
    }

    #[test]
    fn test_ea_real_mode_32bit_exceeds_64k_gp() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0001_0000;
        let mut mem = NullMem;
        // 32-bit addressing in real mode, offset 0x10000 > 0xFFFF → #GP
        let instr = decode16(&[0x67, 0x8A, 0x08]); // MOV CL, [EAX] with 67h prefix
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_real_mode_32bit_ss_exceeds_64k_sf() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.ebp = 0x0001_0000;
        let mut mem = NullMem;
        // 32-bit addressing with EBP base → SS segment, offset > 64K → #SS
        let instr = decode16(&[0x67, 0x8A, 0x45, 0x00]); // MOV AL, [EBP+0]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_ea_real_mode_32bit_dword_at_top_of_address_space_faults() {
        // MDD-BUG-CRUCIBLE-00022 on the real/unreal 32-bit-addressing path: a dword
        // at EA 0xFFFF_FFFF against a big (unreal) cached limit must #GP, not pass
        // via saturating_add.
        let mut cpu = Cpu::new(); // real mode
        cpu.regs.ds = 0x0000;
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true); // unreal-mode 4GB cached limit
        cpu.regs.eax = 0xFFFF_FFFF;
        let mut mem = NullMem;
        // 66h (opsize32) 67h (addrsize32) MOV ECX, [EAX]: dword read at EA 0xFFFFFFFF.
        let instr = decode16(&[0x66, 0x67, 0x8B, 0x08]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            matches!(result, Err(CpuError::GeneralProtectionFault)),
            "unreal-mode dword at EA 0xFFFFFFFF must #GP, got {result:?}"
        );
    }

    // ========================================================================
    // calc_effective_address — 16-bit addressing mode combinations
    // ========================================================================

    #[test]
    fn test_ea_16bit_bx_si() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0100;
        cpu.regs.esi = 0x0020;
        let mut mem = NullMem;
        // MOV AL, [BX+SI] — mod=00, rm=000
        let instr = decode16(&[0x8A, 0x00]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0120);
    }

    #[test]
    fn test_ea_16bit_bx_di() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0200;
        cpu.regs.edi = 0x0030;
        let mut mem = NullMem;
        // MOV AL, [BX+DI] — mod=00, rm=001
        let instr = decode16(&[0x8A, 0x01]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0230);
    }

    #[test]
    fn test_ea_16bit_bp_si() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.ebp = 0x0300;
        cpu.regs.esi = 0x0040;
        let mut mem = NullMem;
        // MOV AL, [BP+SI] — mod=00, rm=010 → uses SS
        let instr = decode16(&[0x8A, 0x02]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0340);
    }

    #[test]
    fn test_ea_16bit_bp_di() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.ebp = 0x0400;
        cpu.regs.edi = 0x0050;
        let mut mem = NullMem;
        // MOV AL, [BP+DI] — mod=00, rm=011 → uses SS
        let instr = decode16(&[0x8A, 0x03]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0450);
    }

    #[test]
    fn test_ea_16bit_si_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.esi = 0x0600;
        let mut mem = NullMem;
        // MOV AL, [SI] — mod=00, rm=100
        let instr = decode16(&[0x8A, 0x04]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0600);
    }

    #[test]
    fn test_ea_16bit_di_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.edi = 0x0700;
        let mut mem = NullMem;
        // MOV AL, [DI] — mod=00, rm=101
        let instr = decode16(&[0x8A, 0x05]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0700);
    }

    #[test]
    fn test_ea_16bit_direct_address_mod00_rm110() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;
        // MOV AL, [0x1234] — mod=00, rm=110 → 16-bit direct address
        let instr = decode16(&[0x8A, 0x06, 0x34, 0x12]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x1234);
    }

    #[test]
    fn test_ea_16bit_bx_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0800;
        let mut mem = NullMem;
        // MOV AL, [BX] — mod=00, rm=111
        let instr = decode16(&[0x8A, 0x07]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0800);
    }

    // ========================================================================
    // calc_effective_address — 16-bit with displacement
    // ========================================================================

    #[test]
    fn test_ea_16bit_bx_si_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0100;
        cpu.regs.esi = 0x0020;
        let mut mem = NullMem;
        // MOV AL, [BX+SI+0x10] — mod=01, rm=000
        let instr = decode16(&[0x8A, 0x40, 0x10]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0130);
    }

    #[test]
    fn test_ea_16bit_bx_si_disp16() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0100;
        cpu.regs.esi = 0x0020;
        let mut mem = NullMem;
        // MOV AL, [BX+SI+0x2000] — mod=10, rm=000
        let instr = decode16(&[0x8A, 0x80, 0x00, 0x20]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x2120);
    }

    #[test]
    fn test_ea_16bit_negative_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0100;
        let mut mem = NullMem;
        // MOV AL, [BX-4] — disp8 = 0xFC (sign-extended to -4), wraps to 0x00FC
        let instr = decode16(&[0x8A, 0x47, 0xFC]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x00FC);
    }

    // ========================================================================
    // calc_effective_address — protected mode segment access failures
    // ========================================================================

    #[test]
    fn test_ea_protected_mode_write_to_code_fails() {
        let cpu = cpu_protected_mode();
        // Override segment to CS, try writing
        let mut mem = NullMem;
        // CS:[0x0100] write — CS is code segment, should fail
        let instr = decode16(&[0x2E, 0x88, 0x06, 0x00, 0x01]); // MOV CS:[0x0100], AL
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Write);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_protected_mode_not_present_segment() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.present = false;
        let mut mem = NullMem;
        let instr = decode16(&[0xA1, 0x00, 0x01]); // MOV AX, [0x0100]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    // ========================================================================
    // calc_effective_address — 32-bit mod=00 rm=5 (disp32 no base)
    // ========================================================================

    #[test]
    fn test_ea_32bit_mod00_rm5_disp32_no_base() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;
        // MOV EAX, [0x00004000] — mod=00, rm=101 → disp32, no base register
        // Using a small displacement that fits within real-mode 64KB limit
        let instr = decode32(&[0x8B, 0x05, 0x00, 0x40, 0x00, 0x00]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x4000);
    }

    #[test]
    fn test_ea_32bit_mod00_rm5_disp32_large_protected_mode() {
        let mut cpu = cpu_protected_mode();
        // Use a flat 4GB segment in protected mode
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true);
        cpu.regs.ds_cache.big = true;
        cpu.regs.ds_cache.base = 0;
        let mut mem = NullMem;
        // MOV EAX, [0x12345678] — large disp32, needs protected mode with big limit
        let instr = decode32(&[0x8B, 0x05, 0x78, 0x56, 0x34, 0x12]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x12345678);
    }

    // ========================================================================
    // calc_effective_address — SIB no-index (ESP=4 in index field)
    // ========================================================================

    #[test]
    fn test_ea_32bit_sib_no_index_base_only() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0000_3000;
        let mut mem = NullMem;
        // MOV AL, [EBX] via SIB — SIB: scale=1, index=none(ESP), base=EBX
        // mod=00, rm=100 (SIB follows), SIB=0x23 (scale=0, index=4=none, base=3=EBX)
        let instr = decode32(&[0x8A, 0x04, 0x23]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x3000);
    }

    // ========================================================================
    // calc_effective_address — protected mode 16-bit addressing with 64K limit
    // ========================================================================

    #[test]
    fn test_ea_protected_mode_16bit_word_ending_at_64k_limit_succeeds() {
        let mut cpu = cpu_protected_mode();
        // DS limit = 0xFFFF (64KB).
        cpu.regs.ds_cache.set_limit(0xFFFF);
        cpu.regs.ebx = 0xFFFE;
        let mut mem = NullMem;
        // MOV AX, [BX] — 16-bit word access at BX=0xFFFE, reads bytes at 0xFFFE and 0xFFFF
        // The last byte offset equals the limit, so the operand fits.
        let instr = decode16(&[0x8B, 0x07]); // MOV AX, [BX]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ea_protected_mode_16bit_word_crossing_ds_limit_faults() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0xFFFF);
        cpu.regs.ebx = 0xFFFF;
        let mut mem = NullMem;
        let instr = decode16(&[0x8B, 0x07]); // MOV AX, [BX]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_protected_mode_16bit_word_crossing_ss_limit_faults() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ebp = 0xFFFF;
        let mut mem = NullMem;
        let instr = decode16(&[0x8B, 0x46, 0x00]); // MOV AX, [BP+0]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_ea_protected_mode_16bit_small_limit_exceeds() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x00FF); // only 256 bytes
        cpu.regs.ebx = 0x0100;
        let mut mem = NullMem;
        // MOV AL, [BX] at BX=0x100 exceeds limit 0xFF
        let instr = decode16(&[0x8A, 0x07]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    // ========================================================================
    // calc_effective_address — expand-down with big=true
    // ========================================================================

    #[test]
    fn test_ea_protected_mode_expand_down_big_above_max() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.access = 0x97;
        cpu.regs.ds_cache.set_limit(0x0000_0FFF);
        cpu.regs.ds_cache.big = true;
        cpu.regs.ds_cache.is_code = false;
        let mut mem = NullMem;
        // Expand-down, big=true: valid range is (0xFFF, 0xFFFFFFFF]
        // Offset 0x2000 > 0xFFF → should succeed
        let instr = decode32(&[0x8A, 0x05, 0x00, 0x20, 0x00, 0x00]); // MOV AL, [0x2000]
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    // ========================================================================
    // Negative disp8 sign extension in 16-bit mode
    // ========================================================================

    #[test]
    fn test_ea_16bit_bp_negative_disp8() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x1000;
        cpu.regs.ebp = 0x0010;
        let mut mem = NullMem;
        // MOV AL, [BP-2] — disp8=0xFE = -2
        // offset = (0x0010 + 0xFFFFFFFE) & 0xFFFF = 0x000E
        let instr = decode16(&[0x8A, 0x46, 0xFE]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x1000E);
    }

    // ========================================================================
    // calc_effective_address — zero displacement edge case
    // ========================================================================

    #[test]
    fn test_ea_zero_displacement() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ebx = 0x0500;
        let mut mem = NullMem;
        // MOV AL, [BX+0] — disp8=0
        let instr = decode16(&[0x8A, 0x47, 0x00]);
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x0500);
    }

    // ========================================================================
    // write access in real mode (just verifying it works)
    // ========================================================================

    #[test]
    fn test_ea_real_mode_write_access() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x2000;
        let mut mem = NullMem;
        let instr = decode16(&[0x88, 0x06, 0x34, 0x12]); // MOV [0x1234], AL
        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Write).unwrap();
        assert_eq!(ea, 0x21234);
    }

    // ========================================================================
    // calc_effective_address — PM segment limit checking
    // ========================================================================

    #[test]
    fn test_ea_pm_segment_limit_exceeded_ds() {
        let mut cpu = cpu_protected_mode();
        // DS has limit 0xFFFF. Access at offset 0xFFF0 with 32-byte size would exceed.
        // Use 32-bit addressing in protected mode to hit the limit check.
        cpu.regs.cs_cache.big = true;
        cpu.cs_bits = 32;
        cpu.regs.ds_cache.big = false;
        cpu.regs.ds_cache.set_limit(0x00FF); // Small limit
        let mut mem = NullMem;
        // MOV EAX, [0x100] — 32-bit addressing, offset 0x100 > limit 0xFF
        let instr = decode32(&[0xA1, 0x00, 0x01, 0x00, 0x00]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_pm_segment_limit_exceeded_ss() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ss_cache.set_limit(0x00FF); // Small limit
        cpu.regs.ebp = 0x0100; // BP=0x100, offset will exceed limit
        let mut mem = NullMem;
        // MOV AL, [BP+0x10] — SS-based, offset > limit
        let instr = decode16(&[0x8A, 0x46, 0x10]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        // SS violations produce StackFault
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_ea_pm_expand_down_within() {
        let mut cpu = cpu_protected_mode();
        // Set up expand-down data segment
        cpu.regs.ds_cache.access = 0x97; // data, expand-down, writable
        cpu.regs.ds_cache.set_limit(0x0FFF); // Expand-down: offset must be > limit
        cpu.regs.ds_cache.big = false; // max = 0xFFFF
        cpu.regs.ebx = 0x2000; // offset 0x2000 > limit 0x0FFF -> OK
        let mut mem = NullMem;
        // MOV AL, [BX] — 16-bit addressing
        let instr = decode16(&[0x8A, 0x07]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ea_pm_expand_down_violation() {
        let mut cpu = cpu_protected_mode();
        // Set up expand-down data segment
        cpu.regs.ds_cache.access = 0x97; // data, expand-down, writable
        cpu.regs.ds_cache.set_limit(0x0FFF); // Expand-down: offset must be > limit
        cpu.regs.ds_cache.big = false;
        cpu.regs.ebx = 0x0100; // offset 0x100 <= limit 0x0FFF -> violation
        let mut mem = NullMem;
        let instr = decode16(&[0x8A, 0x07]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    // ========================================================================
    // calc_effective_address — real mode 32-bit addressing limit check
    // ========================================================================

    #[test]
    fn test_ea_real_mode_32bit_addressing_within_limit() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0000_5000;
        let mut mem = NullMem;
        // 67h prefix in 16-bit code → 32-bit addressing in real mode
        // MOV AL, [EAX] with 67h prefix
        let instr = decode16(&[0x67, 0x8A, 0x00]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ea_real_mode_32bit_addressing_exceed_limit() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.eax = 0x0001_0000; // Exceeds 64KB limit
        let mut mem = NullMem;
        let instr = decode16(&[0x67, 0x8A, 0x00]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }

    #[test]
    fn test_ea_real_mode_32bit_addressing_ss_exceed_limit() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.ebp = 0x0001_0000; // Exceeds 64KB limit
        let mut mem = NullMem;
        // 67h prefix MOV AL, [EBP+0] — SS-based, 32-bit addressing
        let instr = decode16(&[0x67, 0x8A, 0x45, 0x00]);
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    // ========================================================================
    // check_segment_access — write to non-writable SS (StackFault)
    // ========================================================================

    #[test]
    fn test_check_segment_access_write_non_writable_ss() {
        let mut cpu = cpu_protected_mode();
        cpu.regs.ss_cache.is_writable = false;
        let result = check_segment_access(&cpu, cpu.regs.ss, Register::SS, PageAccess::Write);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_check_segment_access_write_code_ss() {
        let mut cpu = cpu_protected_mode();
        // Set SS cache to look like a code segment
        cpu.regs.ss_cache.is_code = true;
        let result = check_segment_access(&cpu, cpu.regs.ss, Register::SS, PageAccess::Write);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    #[test]
    fn test_check_segment_access_read_non_readable_code_ss() {
        let mut cpu = cpu_protected_mode();
        // Make SS look like non-readable code segment
        cpu.regs.ss_cache.is_code = true;
        cpu.regs.ss_cache.is_readable = false;
        let result = check_segment_access(&cpu, cpu.regs.ss, Register::SS, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }

    // ========================================================================
    // SIB no-index path: scale is ignored (Intel SDM Vol 2A Table 2-3)
    // ========================================================================

    #[test]
    fn test_ea_sib_no_index_ignores_scale() {
        // Per Intel SDM Vol 2A Table 2-3, when SIB.index = 100b the scale bits are
        // ignored; the effective address is just `[base + disp]`. An earlier "386
        // hardware quirk" branch in calc_effective_address multiplied the base by
        // scale in this case — that was mythical (cluster C1 in the 2026-04-23
        // fuzz triage). This test pins the correct behaviour.
        let mut mem = NullMem;

        // Sanity path: normal indexed SIB with non-trivial scale should still work.
        let mut cpu_idx = Cpu::new();
        cpu_idx.regs.ds = 0;
        cpu_idx.regs.eax = 0x100;
        cpu_idx.regs.ecx = 0x10;
        // SIB byte: scale=2 (01), index=ECX (001), base=EAX (000) = 01_001_000 = 0x48
        let instr_idx = decode32(&[0x8A, 0x04, 0x48]); // MOV AL, [EAX+ECX*2]
        let ea = calc_effective_address(&cpu_idx, &instr_idx, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(ea, 0x100 + 0x10 * 2);

        // Quirk path: SIB with no index (index=ESP) and scale>1 — scale must be
        // ignored and EA must equal the base alone.
        let mut cpu_base = Cpu::new();
        cpu_base.regs.ds = 0;
        cpu_base.regs.ebx = 0x100;
        // ModRM=0x04 (mod=00, rm=100=SIB), SIB=0x63 (scale=2, index=ESP=none, base=EBX)
        let instr_base = decode32(&[0x8A, 0x04, 0x63]); // MOV AL, [EBX] (scale ignored)
        assert_eq!(instr_base.memory_base(), Register::EBX);
        assert_eq!(instr_base.memory_index(), Register::None);
        let ea_base =
            calc_effective_address(&cpu_base, &instr_base, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(
            ea_base, 0x100,
            "SIB no-index: scale must be ignored, EA == base"
        );
    }

    // ========================================================================
    // Tests with tracing enabled to cover tracing code paths
    // ========================================================================

    fn with_trace_subscriber<F: FnOnce()>(f: F) {
        use tracing_subscriber::fmt;
        use tracing_subscriber::prelude::*;
        let subscriber = fmt::layer()
            .with_writer(std::io::sink)
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
        let _guard = tracing_subscriber::registry()
            .with(subscriber)
            .set_default();
        f();
    }

    #[test]
    fn test_check_segment_access_not_present_ds_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.ds_cache.present = false;
            let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }

    #[test]
    fn test_check_segment_access_not_present_ss_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.ss_cache.present = false;
            let result = check_segment_access(&cpu, cpu.regs.ss, Register::SS, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::StackFault)));
        });
    }

    #[test]
    fn test_check_segment_access_write_code_with_tracing() {
        with_trace_subscriber(|| {
            let cpu = cpu_protected_mode();
            let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Write);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }

    #[test]
    fn test_check_segment_access_read_non_readable_code_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.cs_cache.is_readable = false;
            let result = check_segment_access(&cpu, cpu.regs.cs, Register::CS, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }

    #[test]
    fn test_check_segment_access_write_non_writable_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.ds_cache.is_writable = false;
            let result = check_segment_access(&cpu, cpu.regs.ds, Register::DS, PageAccess::Write);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }

    #[test]
    fn test_ea_pm_limit_exceeded_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.ds_cache.set_limit(0x00FF);
            cpu.regs.ebx = 0x0100;
            let mut mem = NullMem;
            let instr = decode16(&[0x8A, 0x07]); // MOV AL, [BX]
            let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }

    #[test]
    fn test_ea_pm_limit_exceeded_ss_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = cpu_protected_mode();
            cpu.regs.ss_cache.set_limit(0x00FF);
            cpu.regs.ebp = 0x0100;
            let mut mem = NullMem;
            let instr = decode16(&[0x8A, 0x46, 0x10]); // MOV AL, [BP+0x10]
            let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::StackFault)));
        });
    }

    /// Test SIB no-index form: when SIB.index = 100b (no index) the scale bits are
    /// ignored per Intel SDM Vol 2A Table 2-3, regardless of scale>1 in the encoding.
    /// SIB byte: [Scale:2][Index:3][Base:3]
    /// Index=100 (ESP), Scale=01 (x2), Base=011 (EBX) => SIB = 0x63
    #[test]
    fn test_ea_32bit_sib_no_index_scale_ignored() {
        let mut cpu = Cpu::new();
        cpu.regs.ebx = 0x100;
        // 32-bit MOV AL, [SIB] with SIB=0x63
        // ModRM: 00_000_100 = 0x04 (mod=00, reg=AL, rm=100=SIB)
        // SIB:   01_100_011 = 0x63 (scale=2, index=ESP=none, base=EBX)
        let instr = decode32(&[0x8A, 0x04, 0x63]);
        // Verify iced-x86 decodes as expected: base=EBX, no index.
        assert_eq!(instr.memory_base(), Register::EBX);
        assert_eq!(instr.memory_index(), Register::None);
        // Regardless of what iced-x86 reports for scale, EA must equal the base.
        let mut mem = NullMem;
        let addr = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read).unwrap();
        assert_eq!(
            addr, 0x100,
            "SIB no-index: scale must be ignored, EA == base"
        );
    }

    // ========================================================================
    // Mutation-testing targeted tests for calc_effective_address
    // ========================================================================

    // --- L214-215: access_size boundary ---
    // access_size > 0 check and last_byte_offset = addr + (access_size - 1)
    // Need a test where the access exactly hits the segment limit.

    #[test]
    fn test_pm_segment_limit_exact_boundary() {
        // DS limit = 0xFF (256 bytes). Access 1 byte at offset 0xFF — exactly at limit.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0xFF);
        // MOV AL, [0xFF] — 32-bit: A0 FF 00 00 00
        let instr = decode32(&[0xA0, 0xFF, 0x00, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            result.is_ok(),
            "1-byte access at limit=0xFF, offset=0xFF should succeed"
        );
    }

    #[test]
    fn test_pm_segment_limit_one_past() {
        // DS limit = 0xFE. Access 1 byte at offset 0xFF — one past limit.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0xFE);
        let instr = decode32(&[0xA0, 0xFF, 0x00, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_err(), "1-byte access past limit should fault");
    }

    #[test]
    fn test_pm_segment_limit_multibyte_exact() {
        // DS limit = 0x101. Access 2 bytes at offset 0x100 — last byte at 0x101 = limit.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x101);
        // MOV AX, [0x100] — 66 A1 00 01 00 00
        let instr = decode32(&[0x66, 0xA1, 0x00, 0x01, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            result.is_ok(),
            "2-byte access where last byte = limit should succeed"
        );
    }

    #[test]
    fn test_pm_segment_limit_multibyte_one_past() {
        // DS limit = 0x100. Access 2 bytes at offset 0x100 — last byte at 0x101 > limit.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x100);
        let instr = decode32(&[0x66, 0xA1, 0x00, 0x01, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            result.is_err(),
            "2-byte access where last byte > limit should fault"
        );
    }

    // --- L222-223: expand-down segment limit check ---
    // expand_down: addr <= effective_limit || last_byte_offset > max

    #[test]
    fn test_pm_expand_down_within_range() {
        // Expand-down with limit=0x7FFF, big=false (max=0xFFFF).
        // Valid range: 0x8000..0xFFFF.
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x7FFF);
        cpu.regs.ds_cache.access = 0x97; // writable, expand-down (bit 2 set)
        cpu.regs.ds_cache.is_writable = true;
        // MOV AL, [0x8000]
        let instr = decode32(&[0xA0, 0x00, 0x80, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            result.is_ok(),
            "expand-down: addr=0x8000 with limit=0x7FFF should succeed"
        );
    }

    #[test]
    fn test_pm_expand_down_at_limit() {
        // addr=0x7FFF = limit → fault (addr <= limit)
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x7FFF);
        cpu.regs.ds_cache.access = 0x97;
        cpu.regs.ds_cache.is_writable = true;
        let instr = decode32(&[0xA0, 0xFF, 0x7F, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_err(), "expand-down: addr=limit should fault");
    }

    #[test]
    fn test_pm_expand_down_below_limit() {
        // addr=0x7FFE < limit → fault (addr <= limit)
        let mut cpu = cpu_protected_mode();
        cpu.regs.ds_cache.set_limit(0x7FFF);
        cpu.regs.ds_cache.access = 0x97;
        cpu.regs.ds_cache.is_writable = true;
        let instr = decode32(&[0xA0, 0xFE, 0x7F, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_err(), "expand-down: addr < limit should fault");
    }

    // --- L252-259: real-mode 32-bit addressing (67h prefix) limit check ---

    #[test]
    fn test_real_mode_32bit_addr_within_limit() {
        // Real mode, 67h prefix, access within 64KB limit
        let mut cpu = Cpu::new(); // real mode
                                  // MOV AL, [EAX] with 67h prefix: 67 8A 00
        cpu.regs.eax = 0x100;
        let instr = decode16(&[0x67, 0x8A, 0x00]); // 16-bit decoder with 67h
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok());
    }

    #[test]
    fn test_real_mode_32bit_addr_at_limit() {
        // Real mode, 32-bit addressing, offset exactly at 64KB limit
        let cpu = Cpu::new();
        // MOV AL, [disp32=0xFFFF]: 67 A0 FF FF 00 00
        let instr = decode16(&[0x67, 0xA0, 0xFF, 0xFF, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(result.is_ok(), "32-bit addr at limit=0xFFFF should succeed");
    }

    #[test]
    fn test_real_mode_32bit_addr_past_limit() {
        // Real mode, 32-bit addressing, offset past 64KB limit → #GP
        let cpu = Cpu::new();
        // MOV AL, [disp32=0x10000]: 67 A0 00 00 01 00
        let instr = decode16(&[0x67, 0xA0, 0x00, 0x00, 0x01, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            result.is_err(),
            "32-bit addr > 64KB limit in real mode should fault"
        );
    }

    // --- L273/276: check_segment_access ---
    // Write to code segment should fault; read from non-readable code segment should fault.

    #[test]
    fn test_write_to_code_segment_faults() {
        let cpu = cpu_protected_mode();
        // Use CS override: MOV [CS:0x00], AL → 2E A2 00 00 00 00
        let instr = decode32(&[0x2E, 0xA2, 0x00, 0x00, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Write);
        assert!(result.is_err(), "write to code segment should fault");
    }

    #[test]
    fn test_ss_segment_limit_gives_stack_fault() {
        // SS violation → StackFault (not GPF)
        let mut cpu = cpu_protected_mode();
        cpu.regs.ss_cache.set_limit(0x00FF);
        // MOV AL, [SS:BP+0x200] — using BP implies SS
        // In 32-bit: 36 8A 85 00 02 00 00 (SS: MOV AL, [EBP+0x200])
        let instr = decode32(&[0x36, 0x8A, 0x85, 0x00, 0x02, 0x00, 0x00]);
        let mut mem = NullMem;
        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);
        assert!(
            matches!(result, Err(CpuError::StackFault)),
            "SS limit violation should give StackFault, got {:?}",
            result
        );
    }

    #[test]
    fn portable_x87_addresses_match_iced_addressing() {
        fn compare(
            cpu: &Cpu,
            bitness: u32,
            code_bitness: crate::x87_decode::CodeBitness,
            bytes: &[u8],
        ) {
            let mut decoder = Decoder::with_ip(bitness, bytes, 0, DecoderOptions::NONE);
            let iced = decoder.decode();
            let decoded = crate::x87_decode::decode_x87(bytes, code_bitness).unwrap();
            let X87Operand::Memory(operand) = decoded.operand1 else {
                panic!("expected x87 memory operand for {bytes:02x?}");
            };
            let mut iced_mem = NullMem;
            let mut portable_mem = NullMem;
            let portable =
                calc_x87_effective_address(cpu, &operand, &mut portable_mem, PageAccess::Read)
                    .unwrap();
            let iced = calc_effective_address(cpu, &iced, &mut iced_mem, PageAccess::Read).unwrap();
            assert_eq!(portable, iced, "address mismatch for {bytes:02x?}");
        }

        let mut cpu = Cpu::new();
        cpu.regs.set_bp(0x0200);
        cpu.regs.set_si(0x0010);
        cpu.regs.ebx = 0x0200;
        cpu.regs.ecx = 3;
        cpu.regs.eax = 0x0100;
        cpu.regs.ss = 0x0100;
        cpu.regs.ds = 0x0200;
        cpu.regs.fs = 0x0300;

        compare(
            &cpu,
            16,
            crate::x87_decode::CodeBitness::Bits16,
            &[0xd9, 0x42, 0xfc],
        );
        compare(
            &cpu,
            32,
            crate::x87_decode::CodeBitness::Bits32,
            &[0xd9, 0x44, 0x8b, 0xf0],
        );
        compare(
            &cpu,
            32,
            crate::x87_decode::CodeBitness::Bits32,
            &[0x64, 0xd9, 0x40, 0x08],
        );
        compare(
            &cpu,
            32,
            crate::x87_decode::CodeBitness::Bits32,
            &[0x67, 0xd9, 0x46, 0xfe],
        );
    }

    #[test]
    fn portable_x87_ss_limit_fault_matches_iced_addressing() {
        let bytes = [0xd9, 0x85, 0x00, 0x02, 0x00, 0x00];
        let mut decoder = Decoder::with_ip(32, &bytes, 0, DecoderOptions::NONE);
        let iced = decoder.decode();
        let decoded =
            crate::x87_decode::decode_x87(&bytes, crate::x87_decode::CodeBitness::Bits32).unwrap();
        let X87Operand::Memory(operand) = decoded.operand1 else {
            panic!("expected x87 memory operand");
        };

        let mut cpu = cpu_protected_mode();
        cpu.regs.ebp = 0;
        cpu.regs.ss_cache.set_limit(0x00ff);
        let mut iced_mem = NullMem;
        let mut portable_mem = NullMem;
        assert!(matches!(
            calc_effective_address(&cpu, &iced, &mut iced_mem, PageAccess::Read),
            Err(CpuError::StackFault)
        ));
        assert!(matches!(
            calc_x87_effective_address(&cpu, &operand, &mut portable_mem, PageAccess::Read),
            Err(CpuError::StackFault)
        ));
    }

    // ========================================================================
    // MDD-BUG-CRUCIBLE-00577 — a 16-bit-addressed operand must be limit-checked
    // across its WHOLE span in real/V86 mode on a 286+, not silently wrapped at
    // offset 0xFFFF. The fast interpreter already faults here; this crate's
    // shared addressing (legacy engine + the production x87 helpers) did not.
    // ========================================================================

    /// Real mode with a 4 GiB cached DS limit — "unreal mode". `ds_cache` alone
    /// is retained; the visible selector stays 0 so linear == offset.
    fn cpu_unreal_mode() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true);
        cpu
    }

    #[test]
    fn real_mode_16bit_word_crossing_the_segment_top_faults() {
        let mut cpu = Cpu::new(); // 386 default, real mode, DS limit 0xFFFF
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;
        // MOV AX, [0xFFFF] — the second byte would land at offset 0x10000.
        let instr = decode16(&[0xA1, 0xFF, 0xFF]);

        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);

        assert!(
            matches!(result, Err(CpuError::GeneralProtectionFault)),
            "a 286+ must #GP on a word that does not fit below the segment limit, got {result:?}"
        );
    }

    #[test]
    fn real_mode_16bit_word_crossing_the_stack_segment_top_gives_stack_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        let mut mem = NullMem;
        // MOV AX, SS:[0xFFFF] — 0x36 forces the SS segment.
        let instr = decode16(&[0x36, 0xA1, 0xFF, 0xFF]);

        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);

        assert!(
            matches!(result, Err(CpuError::StackFault)),
            "an SS span violation is #SS, not #GP, got {result:?}"
        );
    }

    #[test]
    fn v86_16bit_word_crossing_the_segment_top_faults() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.set_v86_mode(true);
        cpu.regs.ds = 0x0000;
        cpu.regs.ds_cache.set_limit(0xFFFF);
        cpu.regs.ds_cache.set_granularity(false);
        let mut mem = NullMem;
        let instr = decode16(&[0xA1, 0xFF, 0xFF]); // MOV AX, [0xFFFF]

        let result = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);

        assert!(
            matches!(result, Err(CpuError::GeneralProtectionFault)),
            "V86 uses the same 64 KiB cached limit as real mode, got {result:?}"
        );
    }

    #[test]
    fn real_mode_16bit_word_ending_exactly_at_the_segment_top_is_legal() {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;
        // MOV AX, [0xFFFE] — last byte is offset 0xFFFF, still inside.
        let instr = decode16(&[0xA1, 0xFE, 0xFF]);

        let ea = calc_effective_address(&cpu, &instr, &mut mem, PageAccess::Read);

        assert!(ea.is_ok(), "a word ending at the limit must not fault");
    }

    #[test]
    fn unreal_mode_16bit_word_continues_past_ffff_instead_of_wrapping() {
        let cpu = cpu_unreal_mode();
        let mut mem = Memory::new();
        mem.write_u8(0x0000_FFFF, 0xAA);
        mem.write_u8(0x0001_0000, 0xBB); // the architecturally correct second byte
        mem.write_u8(0x0000_0000, 0xCC); // where a 64 KiB wrap would land
        let instr = decode16(&[0xA1, 0xFF, 0xFF]); // MOV AX, [0xFFFF]
        let mut bytes = [0u8; 2];

        read_operand_bytes(&cpu, &instr, &mut mem, &mut bytes).unwrap();

        assert_eq!(
            bytes,
            [0xAA, 0xBB],
            "a retained limit above 0xFFFF makes the span legal, so it must continue at 0x10000"
        );
    }

    #[test]
    fn pre_286_16bit_word_still_wraps_at_the_64k_boundary() {
        let mut cpu = Cpu::new();
        cpu.cpu_type = crate::CpuType::I8086;
        cpu.regs.ds = 0x0000;
        let mut mem = Memory::new();
        mem.write_u8(0x0000_FFFF, 0xAA);
        mem.write_u8(0x0001_0000, 0xBB);
        mem.write_u8(0x0000_0000, 0xCC);
        let instr = decode16(&[0xA1, 0xFF, 0xFF]); // MOV AX, [0xFFFF]
        let mut bytes = [0u8; 2];

        read_operand_bytes(&cpu, &instr, &mut mem, &mut bytes).unwrap();

        assert_eq!(
            bytes,
            [0xAA, 0xCC],
            "an 8086 has no segment limits, so the operand wraps to offset 0"
        );
    }

    /// MDD-BUG-TEM-00179: with a segment base that is not page aligned
    /// (DS = 0x0001, base 0x10), the 64 KiB wrap falls mid-page, and both the
    /// read and the write must still wrap there.
    #[test]
    fn pre_286_operand_wraps_mid_page_for_an_unaligned_segment() {
        let mut cpu = Cpu::new();
        cpu.cpu_type = crate::CpuType::I8086;
        cpu.regs.ds = 0x0001;
        let mut mem = Memory::new();
        mem.write_u8(0x0001_000F, 0xAA); // DS:FFFF
        mem.write_u8(0x0000_0010, 0xCC); // DS:0000, the wrapped second byte
        mem.write_u8(0x0001_0010, 0xEE); // where an unwrapped read runs on

        let mut bytes = [0u8; 2];
        read_operand_bytes(&cpu, &decode16(&[0xA1, 0xFF, 0xFF]), &mut mem, &mut bytes).unwrap();
        assert_eq!(bytes, [0xAA, 0xCC], "the read wraps to DS:0000");

        write_operand_bytes(
            &cpu,
            &decode16(&[0xA3, 0xFF, 0xFF]),
            &mut mem,
            &[0x11, 0x22],
        )
        .unwrap();
        assert_eq!(mem.read_u8(0x0001_000F), 0x11);
        assert_eq!(mem.read_u8(0x0000_0010), 0x22, "the write wraps to DS:0000");
        assert_eq!(
            mem.read_u8(0x0001_0010),
            0xEE,
            "nothing past DS:FFFF is written"
        );
    }

    // ---- A052: the write funnel commits from pass 1's recorded runs ----

    /// The 8086 write twin of `pre_286_16bit_word_still_wraps_at_the_64k_boundary`.
    /// A 16-bit operand at offset 0xFFFF is TWO recorded runs — one byte at
    /// 0xFFFF, one at 0x0000 — and the commit pass must replay both, in order,
    /// to the wrapped addresses rather than running on past 0xFFFF.
    #[test]
    fn pre_286_16bit_operand_write_still_wraps_at_the_64k_boundary() {
        let mut cpu = Cpu::new();
        cpu.cpu_type = crate::CpuType::I8086;
        cpu.regs.ds = 0x0000;
        let mut mem = Memory::new();
        mem.write_u8(0x0001_0000, 0xCC); // where an un-wrapped second byte lands
        let instr = decode16(&[0xA3, 0xFF, 0xFF]); // MOV [0xFFFF], AX

        write_operand_bytes(&cpu, &instr, &mut mem, &[0xAA, 0xBB]).unwrap();

        assert_eq!(
            mem.read_u8(0x0000_FFFF),
            0xAA,
            "first byte at the segment top"
        );
        assert_eq!(
            mem.read_u8(0x0000_0000),
            0xBB,
            "an 8086 has no segment limits, so the second byte wraps to offset 0"
        );
        assert_eq!(
            mem.read_u8(0x0001_0000),
            0xCC,
            "and nothing may be written past 0xFFFF"
        );
    }

    /// PM32 with paging OFF and a DPMI physical remap in play: two adjacent
    /// LINEAR pages deliberately map to non-adjacent PHYSICAL ones. The funnel
    /// re-resolves at every run start, so a store straddling the boundary must
    /// split — the panel's F3 shape, kept as regression armor.
    #[test]
    fn pm32_unpaged_dpmi_remapped_operand_write_splits_at_the_mapping_boundary() {
        const LINEAR: u32 = 0x0030_0000;
        const PHYS_A: u32 = 0x0002_0000;
        const PHYS_B: u32 = 0x0008_0000; // NOT adjacent to A

        let mut cpu = cpu_protected_mode();
        cpu.cs_bits = 32;
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ds_cache.set_granularity(true);
        cpu.regs.ds_cache.set_limit(0x000F_FFFF);
        cpu.regs.ds_cache.big = true;
        assert!(cpu.is_protected_mode() && (cpu.regs.cr0 & 0x8000_0000) == 0);

        let mut mem = Memory::new();
        assert!(mem.add_physical_mapping(LINEAR, PHYS_A, 0x1000));
        assert!(mem.add_physical_mapping(LINEAR + 0x1000, PHYS_B, 0x1000));
        mem.write_u8(PHYS_A as usize + 0x1000, 0xEE); // decoy after A

        cpu.regs.eax = LINEAR + 0xFFC;
        let instr = decode32(&[0x89, 0x18]); // MOV [eax], ebx (32-bit operand)
        write_operand_bytes(&cpu, &instr, &mut mem, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();

        assert_eq!(
            (0..4)
                .map(|i| mem.read_u8(PHYS_A as usize + 0xFFC + i))
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
            "the head must land in the first mapping"
        );
        assert_eq!(
            (0..4)
                .map(|i| mem.read_u8(PHYS_B as usize + i))
                .collect::<Vec<_>>(),
            vec![5, 6, 7, 8],
            "the tail must follow the SECOND mapping, not physical adjacency"
        );
        assert_eq!(
            mem.read_u8(PHYS_A as usize + 0x1000),
            0xEE,
            "the physically adjacent page must be untouched"
        );
    }

    /// A052 engagement, non-test counters: one helper-path FSTP m64 must show
    /// up in BOTH censuses — as funnel traffic AND as a bulk-arm outcome. The
    /// pair is the point (G6): funnel traffic alone proves only that the
    /// funnel ran, not that anything was bulk-copied.
    #[test]
    fn x87_m64_store_bumps_the_funnel_and_bulk_engagement_counters() {
        let (before_ops, before_runs, _) = operand_write_census::snapshot();
        let w8 = gp64_types::span_write_census::width_bucket(8);

        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        cpu.regs.set_bx(0x2000);
        cpu.fpu.control_word = 0x037F;
        cpu.fpu
            .push(crate::x87_ops::F80::from_f64(std::f64::consts::PI));
        let mut mem = Memory::new();

        let instr = crate::x87_decode::decode_x87(
            &[0xDD, 0x1F], // FSTP qword ptr [bx]
            crate::x87_decode::CodeBitness::Bits16,
        )
        .expect("FSTP qword ptr [bx]");
        crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem).expect("store must not fault");
        assert_eq!(mem.read_u64(0x2000), std::f64::consts::PI.to_bits());

        // Funnel side: a global census, so other tests in this binary can only
        // ADD to it — the claim is a lower bound, exact when run filtered.
        let (after_ops, after_runs, _) = operand_write_census::snapshot();
        assert!(
            after_ops[w8] > before_ops[w8],
            "the m64 store must be counted as funnel traffic"
        );
        assert!(
            after_runs > before_runs,
            "and must count the run it replayed"
        );

        // Outcome side: per-instance, so this one is exact. Plain low RAM, so
        // the span had to take the bulk arm.
        let (bulk, per_byte) = mem.span_write_census();
        assert_eq!(bulk[w8], 1, "one 8-byte span served by the bulk arm");
        assert_eq!(
            per_byte.iter().sum::<u64>(),
            0,
            "and nothing fell back to the per-byte leg"
        );
    }

    #[test]
    fn real_mode_x87_m32real_crossing_the_segment_top_faults() {
        let bytes = [0xD9, 0x06, 0xFE, 0xFF]; // FLD dword ptr [0xFFFE]
        let decoded =
            crate::x87_decode::decode_x87(&bytes, crate::x87_decode::CodeBitness::Bits16).unwrap();
        let X87Operand::Memory(operand) = decoded.operand1 else {
            panic!("expected x87 memory operand");
        };

        let mut cpu = Cpu::new();
        cpu.regs.ds = 0x0000;
        let mut mem = NullMem;

        let result = calc_x87_effective_address(&cpu, &operand, &mut mem, PageAccess::Read);

        assert!(
            matches!(result, Err(CpuError::GeneralProtectionFault)),
            "a 4-byte x87 operand at 0xFFFE runs off the segment, got {result:?}"
        );
    }
}
