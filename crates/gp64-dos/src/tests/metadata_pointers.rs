use super::*;

fn metadata_fixture() -> (Dos, gp64_cpu::Cpu, Memory) {
    let mut dos = Dos::new();
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(4096, 2048)));
    dos.path_resolver.set_current_drive(2);
    (dos, gp64_cpu::Cpu::new(), Memory::new())
}

fn metadata_call(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, ah: u8) -> usize {
    cpu.regs.set_dl(3);
    pm16_int21(dos, cpu, memory, u16::from(ah) << 8);
    let offset = if ah == 0x63 {
        cpu.regs.si()
    } else {
        cpu.regs.bx()
    };
    if ah == 0x63 {
        assert_eq!(cpu.regs.al(), 0);
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    } else {
        assert_eq!(
            (cpu.regs.al(), cpu.regs.cx(), cpu.regs.dx()),
            (8, 512, 0xFFFF)
        );
    }
    cpu.regs.ds_cache.base as usize + usize::from(offset)
}

#[test]
fn metadata_pointers_preserve_guest_segments_and_remain_stable() {
    let (mut dos, mut cpu, mut memory) = metadata_fixture();
    let mut original_pointers = None;
    for segment in [0x2300, 0x3400] {
        let base = Memory::seg_offset_to_linear(segment, 0);
        for offset in 0..0x240 {
            memory.write_u8(base + offset, 0xA5);
        }
        dos.set_dta(segment, 0x180);
        let media = metadata_call(&mut dos, &mut cpu, &mut memory, 0x1B);
        let explicit_media = metadata_call(&mut dos, &mut cpu, &mut memory, 0x1C);
        let dbcs = metadata_call(&mut dos, &mut cpu, &mut memory, 0x63);
        for offset in 0..0x240 {
            assert_eq!(
                memory.read_u8(base + offset),
                0xA5,
                "guest canary at {offset:#x}"
            );
        }
        assert_eq!(media, explicit_media);
        assert_eq!(
            memory.read_u8(media),
            0xF8,
            "DBCS request preserves media byte"
        );
        assert_eq!(memory.read_u16(dbcs), 0);
        let sft_end = usize::from(SECSFT_SEG) * 16 + 6 + usize::from(DOS_FILES - 5) * 59;
        assert!(media >= sft_end && dbcs >= sft_end);
        assert!(media < usize::from(crate::memory::DEFAULT_FIRST_MCB_SEG) * 16);
        assert!(dbcs < usize::from(crate::memory::DEFAULT_FIRST_MCB_SEG) * 16);
        if let Some(pointers) = original_pointers {
            assert_eq!(
                (media, dbcs),
                pointers,
                "metadata pointers survive DTA relocation"
            );
        }
        original_pointers = Some((media, dbcs));
        metadata_call(&mut dos, &mut cpu, &mut memory, 0x1B);
        assert_eq!(
            memory.read_u16(dbcs),
            0,
            "media request preserves DBCS terminator"
        );
    }
}

#[test]
fn metadata_pointers_return_loadable_dpmi_data_selectors() {
    let (mut dos, mut cpu, mut memory) = metadata_fixture();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    let mut original_pointers = None;
    for dta_segment in [0x2800, 0x3900] {
        let dta_selector = dos
            .dpmi
            .segment_to_descriptor(&mut cpu, &mut memory, dta_segment)
            .unwrap();
        let base = usize::from(dta_segment) * 16;
        dos.set_dta_pm(dta_selector, 0x180, base as u32);
        for offset in 0..0x240 {
            memory.write_u8(base + offset, 0xA5);
        }
        let mut pointers = Vec::new();
        for ah in [0x1B, 0x1C, 0x63] {
            let pointer = metadata_call(&mut dos, &mut cpu, &mut memory, ah);
            assert_ne!(
                cpu.regs.ds, dta_selector,
                "metadata DS must leave guest DTA selector"
            );
            let descriptor = cpu
                .read_descriptor(&memory, cpu.regs.ds)
                .expect("metadata DS is loadable");
            assert_eq!(descriptor.base, cpu.regs.ds_cache.base);
            assert!(pointer < usize::from(crate::memory::DEFAULT_FIRST_MCB_SEG) * 16);
            assert_eq!(memory.read_u8(pointer), if ah == 0x63 { 0 } else { 0xF8 });
            pointers.push((cpu.regs.ds, pointer));
            for offset in 0..0x240 {
                assert_eq!(
                    memory.read_u8(base + offset),
                    0xA5,
                    "PM guest canary at {offset:#x}"
                );
            }
        }
        assert_eq!(memory.read_u8(pointers[0].1), 0xF8);
        if let Some(original) = &original_pointers {
            assert_eq!(
                &pointers, original,
                "PM metadata selectors and pointers remain stable"
            );
        }
        original_pointers = Some(pointers);
    }
}

#[test]
fn metadata_pointers_kernel_init_restores_only_metadata_bytes() {
    let (mut dos, mut cpu, mut memory) = metadata_fixture();
    let media = metadata_call(&mut dos, &mut cpu, &mut memory, 0x1B);
    let dbcs = metadata_call(&mut dos, &mut cpu, &mut memory, 0x63);
    for address in media - 1..media + 16 {
        memory.write_u8(address, 0xA5);
    }
    dos.init_kernel_structures(&mut memory);
    assert_eq!(memory.read_u8(media), 0xF8, "kernel init restores media ID");
    assert_eq!(
        memory.read_u16(dbcs),
        0,
        "kernel init restores DBCS terminator"
    );
    assert_eq!(
        memory.read_u8(media - 1),
        0xA5,
        "SFT paragraph padding survives"
    );
    for address in dbcs + 2..media + 16 {
        assert_eq!(
            memory.read_u8(address),
            0xA5,
            "metadata paragraph padding survives"
        );
    }
}

#[test]
fn metadata_pointers_simulated_real_mode_returns_real_segment() {
    let (mut dos, mut cpu, mut memory) = metadata_fixture();
    let real_media = metadata_call(&mut dos, &mut cpu, &mut memory, 0x1B);
    let real_segment = cpu.regs.ds;
    let real_dbcs = metadata_call(&mut dos, &mut cpu, &mut memory, 0x63);
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    dos.set_simulated_real_mode_hle(true);
    for ah in [0x1B, 0x1C, 0x63] {
        let pointer = metadata_call(&mut dos, &mut cpu, &mut memory, ah);
        assert_eq!(
            cpu.regs.ds, real_segment,
            "simulated RM must return a real segment"
        );
        assert_eq!(pointer, if ah == 0x63 { real_dbcs } else { real_media });
    }
}
