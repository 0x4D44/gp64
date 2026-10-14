//! Cold compatibility adapter for legacy engines, tests, and benchmarks.
//!
//! Production interpreter and dyncore x87 paths decode directly to
//! [`DecodedX87`]. This module exists only for callers that already own an iced
//! instruction and must not be used to construct a hot runtime path.

use iced_x86::{CodeSize, Instruction, Mnemonic, OpKind, Register};

use crate::x87_decode::{
    CodeBitness, DecodedX87, RepeatPrefix, X87AddressRegister, X87AddressSize,
    X87EnvironmentLayout, X87MemoryOperand, X87Operand, X87Operation, X87Prefixes, X87Segment,
};
use crate::{Cpu, PagedMemory};

#[must_use]
pub fn decode_instruction(instruction: &Instruction) -> DecodedX87 {
    let operation = operation(instruction.mnemonic());
    let mut operand1 = operand(instruction, 0);
    let mut operand2 = operand(instruction, 1);
    let width = instruction.memory_size().size() as u16;
    // Some legacy mutation tests deliberately replace the public operand kind
    // while retaining a memory-form Code and its decoded address fields. The
    // old semantic dispatcher still followed that memory form.
    if operand1 == X87Operand::None && width != 0 {
        operand1 = X87Operand::Memory(memory_operand(instruction));
    }
    if matches!(operation, X87Operation::Fcompp | X87Operation::Fucompp)
        && operand1 == X87Operand::None
    {
        operand1 = X87Operand::Stack(0);
        operand2 = X87Operand::Stack(1);
    }
    let environment_layout = match width {
        14 => X87EnvironmentLayout::Environment14,
        28 => X87EnvironmentLayout::Environment28,
        94 => X87EnvironmentLayout::State94,
        108 => X87EnvironmentLayout::State108,
        _ => X87EnvironmentLayout::None,
    };
    let operand_size = match instruction.code_size() {
        CodeSize::Code16 => CodeBitness::Bits16,
        _ => CodeBitness::Bits32,
    };
    DecodedX87 {
        operation: if instruction.is_invalid() {
            X87Operation::Invalid
        } else {
            operation
        },
        operand1,
        operand2,
        length: instruction.len() as u8,
        prefixes: X87Prefixes {
            lock: instruction.has_lock_prefix(),
            operand_size_override: false,
            address_size_override: false,
            segment_override: segment(instruction.segment_prefix()),
            repeat: if instruction.has_repne_prefix() {
                Some(RepeatPrefix::Repne)
            } else if instruction.has_rep_prefix() {
                Some(RepeatPrefix::Rep)
            } else {
                None
            },
        },
        operand_size,
        environment_layout,
    }
}

pub fn execute<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &Instruction,
    memory: &mut M,
) -> anyhow::Result<()> {
    crate::fpu::execute_decoded(cpu, &decode_instruction(instruction), memory)
}

fn operand(instruction: &Instruction, index: u32) -> X87Operand {
    if index >= instruction.op_count() {
        return X87Operand::None;
    }
    match instruction.op_kind(index) {
        OpKind::Register => match instruction.op_register(index) {
            Register::AX => X87Operand::Ax,
            register if register.is_st() => {
                X87Operand::Stack((register.number() - Register::ST0.number()) as u8)
            }
            _ => X87Operand::None,
        },
        OpKind::Memory => X87Operand::Memory(memory_operand(instruction)),
        _ => X87Operand::None,
    }
}

fn memory_operand(instruction: &Instruction) -> X87MemoryOperand {
    let base = address_register(instruction.memory_base());
    let index = address_register(instruction.memory_index());
    let address_size = if base.is_some_and(is_16_bit_register)
        || index.is_some_and(is_16_bit_register)
        || (base.is_none() && index.is_none() && instruction.memory_displ_size() <= 2)
    {
        X87AddressSize::Bits16
    } else {
        X87AddressSize::Bits32
    };
    let default_segment = if matches!(
        base,
        Some(X87AddressRegister::Bp | X87AddressRegister::Ebp | X87AddressRegister::Esp)
    ) {
        X87Segment::Ss
    } else {
        X87Segment::Ds
    };
    let exposed_displacement = instruction.memory_displacement32();
    let displacement_size = instruction.memory_displ_size() as u8;
    let displacement_raw = match displacement_size {
        1 => u32::from(exposed_displacement as u8),
        2 => u32::from(exposed_displacement as u16),
        _ => exposed_displacement,
    };
    let displacement = match displacement_size {
        1 => i32::from(displacement_raw as u8 as i8),
        2 if base.is_some() || index.is_some() => i32::from(displacement_raw as u16 as i16),
        _ => displacement_raw as i32,
    };
    X87MemoryOperand {
        address_size,
        base,
        index,
        scale: if index.is_some() {
            instruction.memory_index_scale() as u8
        } else {
            1
        },
        displacement,
        displacement_raw,
        displacement_size,
        default_segment,
        segment: segment(instruction.memory_segment()).unwrap_or(default_segment),
        width_bytes: instruction.memory_size().size() as u16,
    }
}

fn is_16_bit_register(register: X87AddressRegister) -> bool {
    matches!(
        register,
        X87AddressRegister::Bx
            | X87AddressRegister::Bp
            | X87AddressRegister::Si
            | X87AddressRegister::Di
    )
}

fn address_register(register: Register) -> Option<X87AddressRegister> {
    Some(match register {
        Register::BX => X87AddressRegister::Bx,
        Register::BP => X87AddressRegister::Bp,
        Register::SI => X87AddressRegister::Si,
        Register::DI => X87AddressRegister::Di,
        Register::EAX => X87AddressRegister::Eax,
        Register::ECX => X87AddressRegister::Ecx,
        Register::EDX => X87AddressRegister::Edx,
        Register::EBX => X87AddressRegister::Ebx,
        Register::ESP => X87AddressRegister::Esp,
        Register::EBP => X87AddressRegister::Ebp,
        Register::ESI => X87AddressRegister::Esi,
        Register::EDI => X87AddressRegister::Edi,
        _ => return None,
    })
}

fn segment(register: Register) -> Option<X87Segment> {
    Some(match register {
        Register::ES => X87Segment::Es,
        Register::CS => X87Segment::Cs,
        Register::SS => X87Segment::Ss,
        Register::DS => X87Segment::Ds,
        Register::FS => X87Segment::Fs,
        Register::GS => X87Segment::Gs,
        _ => return None,
    })
}

fn operation(mnemonic: Mnemonic) -> X87Operation {
    use X87Operation as Op;
    match mnemonic {
        Mnemonic::Fadd => Op::Fadd,
        Mnemonic::Faddp => Op::Faddp,
        Mnemonic::Fiadd => Op::Fiadd,
        Mnemonic::Fmul => Op::Fmul,
        Mnemonic::Fmulp => Op::Fmulp,
        Mnemonic::Fimul => Op::Fimul,
        Mnemonic::Fsub => Op::Fsub,
        Mnemonic::Fsubp => Op::Fsubp,
        Mnemonic::Fisub => Op::Fisub,
        Mnemonic::Fsubr => Op::Fsubr,
        Mnemonic::Fsubrp => Op::Fsubrp,
        Mnemonic::Fisubr => Op::Fisubr,
        Mnemonic::Fdiv => Op::Fdiv,
        Mnemonic::Fdivp => Op::Fdivp,
        Mnemonic::Fidiv => Op::Fidiv,
        Mnemonic::Fdivr => Op::Fdivr,
        Mnemonic::Fdivrp => Op::Fdivrp,
        Mnemonic::Fidivr => Op::Fidivr,
        Mnemonic::Fcom => Op::Fcom,
        Mnemonic::Fcomp => Op::Fcomp,
        Mnemonic::Fcompp => Op::Fcompp,
        Mnemonic::Ficom => Op::Ficom,
        Mnemonic::Ficomp => Op::Ficomp,
        Mnemonic::Fucom => Op::Fucom,
        Mnemonic::Fucomp => Op::Fucomp,
        Mnemonic::Fucompp => Op::Fucompp,
        Mnemonic::Fcomi => Op::Fcomi,
        Mnemonic::Fcomip => Op::Fcomip,
        Mnemonic::Fucomi => Op::Fucomi,
        Mnemonic::Fucomip => Op::Fucomip,
        Mnemonic::Fld => Op::Fld,
        Mnemonic::Fild => Op::Fild,
        Mnemonic::Fldz => Op::Fldz,
        Mnemonic::Fld1 => Op::Fld1,
        Mnemonic::Fldpi => Op::Fldpi,
        Mnemonic::Fldl2e => Op::Fldl2e,
        Mnemonic::Fldl2t => Op::Fldl2t,
        Mnemonic::Fldlg2 => Op::Fldlg2,
        Mnemonic::Fldln2 => Op::Fldln2,
        Mnemonic::Fst => Op::Fst,
        Mnemonic::Fstp => Op::Fstp,
        Mnemonic::Fstpnce => Op::Fstp,
        Mnemonic::Fist => Op::Fist,
        Mnemonic::Fistp => Op::Fistp,
        Mnemonic::Fisttp => Op::Fisttp,
        Mnemonic::Fxch => Op::Fxch,
        Mnemonic::Ffree => Op::Ffree,
        Mnemonic::Ffreep => Op::Ffreep,
        Mnemonic::Fbld => Op::Fbld,
        Mnemonic::Fbstp => Op::Fbstp,
        Mnemonic::Fcmovb => Op::Fcmovb,
        Mnemonic::Fcmovbe => Op::Fcmovbe,
        Mnemonic::Fcmove => Op::Fcmove,
        Mnemonic::Fcmovnb => Op::Fcmovnb,
        Mnemonic::Fcmovnbe => Op::Fcmovnbe,
        Mnemonic::Fcmovne => Op::Fcmovne,
        Mnemonic::Fcmovnu => Op::Fcmovnu,
        Mnemonic::Fcmovu => Op::Fcmovu,
        Mnemonic::Fldcw => Op::Fldcw,
        Mnemonic::Fnstcw | Mnemonic::Fstcw => Op::Fnstcw,
        Mnemonic::Fnstsw | Mnemonic::Fstsw => Op::Fnstsw,
        Mnemonic::Fldenv => Op::Fldenv,
        Mnemonic::Fnstenv | Mnemonic::Fstenv => Op::Fnstenv,
        Mnemonic::Frstor => Op::Frstor,
        Mnemonic::Fnsave | Mnemonic::Fsave => Op::Fnsave,
        Mnemonic::Fchs => Op::Fchs,
        Mnemonic::Fabs => Op::Fabs,
        Mnemonic::Ftst => Op::Ftst,
        Mnemonic::Fxam => Op::Fxam,
        Mnemonic::Fsin => Op::Fsin,
        Mnemonic::Fcos => Op::Fcos,
        Mnemonic::Fsincos => Op::Fsincos,
        Mnemonic::Fsqrt => Op::Fsqrt,
        Mnemonic::Fptan => Op::Fptan,
        Mnemonic::Fpatan => Op::Fpatan,
        Mnemonic::Fyl2x => Op::Fyl2x,
        Mnemonic::Fyl2xp1 => Op::Fyl2xp1,
        Mnemonic::F2xm1 => Op::F2xm1,
        Mnemonic::Fscale => Op::Fscale,
        Mnemonic::Fxtract => Op::Fxtract,
        Mnemonic::Fprem => Op::Fprem,
        Mnemonic::Fprem1 => Op::Fprem1,
        Mnemonic::Frndint => Op::Frndint,
        Mnemonic::Fdecstp => Op::Fdecstp,
        Mnemonic::Fincstp => Op::Fincstp,
        Mnemonic::Fnop | Mnemonic::Wait => Op::Fnop,
        Mnemonic::Fninit | Mnemonic::Finit => Op::Fninit,
        Mnemonic::Fnclex | Mnemonic::Fclex => Op::Fnclex,
        Mnemonic::Fneni | Mnemonic::Feni => Op::Fneni,
        Mnemonic::Fndisi | Mnemonic::Fdisi => Op::Fndisi,
        Mnemonic::Fnsetpm | Mnemonic::Fsetpm => Op::Fnsetpm,
        _ => Op::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    #[test]
    fn no_base_sib_with_ebp_index_keeps_ds_default() {
        let instruction =
            Decoder::with_ip(32, &[0xd9, 0x04, 0x6d, 0, 0, 0, 0], 0, DecoderOptions::NONE).decode();
        let decoded = decode_instruction(&instruction);
        let X87Operand::Memory(memory) = decoded.operand1 else {
            panic!("expected memory operand");
        };
        assert_eq!(memory.default_segment, X87Segment::Ds);
    }
}
