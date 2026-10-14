use super::*;

#[test]
fn physical_state_swap_keeps_private_ram_and_moves_device_state() {
    let mut first = Memory::new();
    let mut second = Memory::new();

    first.write_u32(0x2000, 0x1111_2222);
    second.write_u32(0x2000, 0x3333_4444);

    first.video_memory_mut()[0] = 0xA1;
    first.set_plane_mask(0x05);
    first.init_ems_pool(2).unwrap();
    first.write_ems_backing_byte(1, 7, 0xB1).unwrap();
    first
        .claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    first.set_ems_frame_page(0, Some(1)).unwrap();
    first.set_a20(false);
    first.init_lfb(64);
    first.lfb_mut()[3] = 0xC1;
    first.add_mmio_region(0x6000_0000, 16);
    first.mmio_slice_mut(0x6000_0000).unwrap()[4] = 0xD1;
    first.mark_rom_page(0xC0000 >> 12);
    first.ram[0xC0000] = 0xE1;
    first.set_option_rom_backing(&[0xF1; 8]);
    first.configure_option_rom_decoder(0xD0000, true);
    first.set_high_mem_alias_enabled(true);
    first.set_firmware_mp_table_suppression_enabled(true);
    first.set_allow_rom_writes(true);
    first.set_last_cr3(0x1111_0000);

    second.video_memory_mut()[0] = 0xA2;
    second.set_plane_mask(0x0A);
    second.init_ems_pool(2).unwrap();
    second.write_ems_backing_byte(0, 7, 0xB2).unwrap();
    second
        .claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    second.set_ems_frame_page(0, Some(0)).unwrap();
    second.set_a20(true);
    second.init_lfb(64);
    second.lfb_mut()[3] = 0xC2;
    second.add_mmio_region(0x6000_0000, 16);
    second.mmio_slice_mut(0x6000_0000).unwrap()[4] = 0xD2;
    second.ram[0xC0000] = 0xE2;
    second.set_option_rom_backing(&[0xF2; 8]);
    second.configure_option_rom_decoder(0xD1000, true);
    second.set_last_cr3(0x2222_0000);

    let first_policy_epoch = first.memory_policy_epoch();
    let second_policy_epoch = second.memory_policy_epoch();
    first.swap_physical_state(&mut second);

    assert_eq!(first.read_u32(0x2000), 0x1111_2222);
    assert_eq!(second.read_u32(0x2000), 0x3333_4444);

    assert_eq!(second.video_memory()[0], 0xA1);
    assert_eq!(second.vga_planes()[0], 0xA1);
    assert_eq!(second.vga.plane_mask, 0x05);
    assert_eq!(first.video_memory()[0], 0xA2);
    assert_eq!(first.vga_planes()[0], 0xA2);
    assert_eq!(first.vga.plane_mask, 0x0A);

    assert_eq!(second.read_ems_backing_byte(1, 7).unwrap(), 0xB1);
    assert_eq!(second.ems_frame_map_snapshot()[0], Some(1));
    assert_eq!(first.read_ems_backing_byte(0, 7).unwrap(), 0xB2);
    assert_eq!(first.ems_frame_map_snapshot()[0], Some(0));
    assert!(!second.a20_enabled());
    assert!(first.a20_enabled());

    assert_eq!(second.lfb()[3], 0xC1);
    assert_eq!(first.lfb()[3], 0xC2);
    assert_eq!(second.mmio_slice(0x6000_0000).unwrap()[4], 0xD1);
    assert_eq!(first.mmio_slice(0x6000_0000).unwrap()[4], 0xD2);

    assert!(second.is_rom_addr(0xC0000));
    assert_eq!(second.ram[0xC0000], 0xE1);
    assert!(!first.is_rom_addr(0xC0000));
    assert_eq!(first.ram[0xC0000], 0xE2);
    assert_eq!(second.option_rom_backing(), &[0xF1; 8]);
    assert_eq!(second.option_rom_decoder().base, 0xD0000);
    assert!(second.high_mem_alias_enabled());
    assert!(second.firmware_mp_table_suppression_enabled());
    assert!(second.allow_rom_writes());
    assert_eq!(second.last_cr3(), 0);
    assert_eq!(first.last_cr3(), 0);

    assert!(first.memory_policy_epoch() > first_policy_epoch);
    assert!(second.memory_policy_epoch() > second_policy_epoch);

    first.swap_physical_state(&mut second);
    assert_eq!(first.video_memory()[0], 0xA1);
    assert_eq!(first.read_ems_backing_byte(1, 7).unwrap(), 0xB1);
    assert_eq!(first.lfb()[3], 0xC1);
    assert_eq!(first.mmio_slice(0x6000_0000).unwrap()[4], 0xD1);
    assert!(first.is_rom_addr(0xC0000));
    assert_eq!(first.ram[0xC0000], 0xE1);
}
