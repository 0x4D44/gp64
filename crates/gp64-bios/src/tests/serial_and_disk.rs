use super::*;

/// MDD-BUG-KILN-00278: an INT 14h timeout wait must not run as one
/// monolithic host loop.
///
/// The BDA timeout byte is guest-writable, so FFh asks for 16,711,425 polls
/// per phase. Before this fix a single dispatch ran every one of them as
/// synchronous host work at ZERO guest cost — the CPU, PIT, IRQs, input and
/// IPC could not run until it finished, which is a guest-triggerable stall
/// of the whole emulator.
///
/// Fails without the fix: the call returns `Iret` with the timeout already
/// published and `cpu.cycles` unmoved.
#[test]
fn int14_idle_wait_yields_within_one_poll_chunk_kiln278() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
    // No auto DSR/CTS from the null-modem simulation: the wait is idle.
    serial.ports[0].external_tx = true;
    memory.write_u8(0x47C, 0xFF); // COM1 timeout = FFh, the worst case

    cpu.regs.set_ah(0x02); // read: waits for DSR, then for DR
    cpu.regs.set_dx(0x0000);
    let cycles_before = cpu.cycles;

    let disp = bios
        .int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert!(
        matches!(disp, Int14Disp::Retry),
        "an idle wait must yield to the scheduler, not run to completion"
    );
    assert_eq!(
        cpu.cycles - cycles_before,
        u64::from(Bios::INT14_POLL_CHUNK) * Bios::INT14_CYCLES_PER_POLL,
        "one dispatch must spend exactly one poll chunk of host work"
    );
    assert_eq!(
        cpu.regs.ah() & 0x80,
        0,
        "a yielded wait must not publish a timeout result yet"
    );
}

/// The other half of MDD-BUG-KILN-00278: chunking must not lose the
/// guest-visible contract. Re-entered to completion, the wait still reaches
/// its BDA limit, still reports the timeout, costs the guest time along the
/// way, and leaves no progress behind for the next request to inherit.
#[test]
fn int14_chunked_wait_still_times_out_and_costs_guest_time_kiln278() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
    serial.ports[0].external_tx = true;
    memory.write_u8(0x47C, 0x01); // 65,535 polls for the phase-1 wait

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);
    let cycles_before = cpu.cycles;

    let dispatches = int14_to_completion(
        &mut bios,
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_, _| {},
    );

    assert!(
        dispatches > 1,
        "a 65,535-poll wait must span dispatches, not run monolithically"
    );
    assert_ne!(
        cpu.regs.ah() & 0x80,
        0,
        "the BIOS timeout must still fire once the limit is reached"
    );
    assert_eq!(
        cpu.cycles - cycles_before,
        0xFFFF * Bios::INT14_CYCLES_PER_POLL,
        "every poll of the wait must be charged to the guest exactly once"
    );
    assert!(
        bios.serial_wait.is_none(),
        "a completed wait must not leave poll progress behind"
    );
}

/// A ready device must still be served in ONE dispatch — chunking must not
/// add a re-entry to the common path (MDD-BUG-KILN-00278).
#[test]
fn int14_ready_device_still_completes_in_a_single_dispatch_kiln278() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
    serial.ports[0].update_msr_signals(true, false, false, false); // DSR
    serial.ports[0].receive_byte(0x42);

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);

    let disp = bios
        .int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert!(
        matches!(disp, Int14Disp::Iret),
        "a device that is already ready must not cost a re-entry"
    );
    assert_eq!(cpu.regs.al(), 0x42);
    assert_eq!(cpu.regs.ah() & 0x80, 0, "no timeout for a ready device");
}

/// MDD-BUG-KILN-00275: the BDA is guest-writable, so `0040:0000` can hold
/// any `u16`. INT 14h derives every UART register address by adding to that
/// base, so a base near `0xFFFF` overflowed — an abort in overflow-checked
/// development and test builds, and silent wrapping onto unrelated ports in
/// release. Sweep every base that cannot cover the 8-register window,
/// across every subfunction, and require a defined absent result from each.
#[test]
fn int14_rejects_bda_bases_that_cannot_cover_the_uart_window_kiln275() {
    for base in 0xFFF9u16..=0xFFFF {
        for ah in 0x00u8..=0x04 {
            let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
            // Poison COM1's BDA base and select it.
            memory.write_u16(0x400, base);
            cpu.regs.set_ah(ah);
            cpu.regs.set_al(0xE3);
            cpu.regs.set_dx(0x0000);

            bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
                .unwrap_or_else(|e| panic!("base {base:#06X} AH={ah:#04X} errored: {e:?}"));

            assert_eq!(
                cpu.regs.ah(),
                0x80,
                "base {base:#06X} AH={ah:#04X} must report the port absent"
            );
            assert!(
                !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                "base {base:#06X} AH={ah:#04X} must leave CF clear"
            );
        }
    }
}

/// The boundary from the other side: `0xFFF8` is the highest base that
/// still covers all eight UART registers, so it must NOT be rejected as
/// absent — otherwise the guard would be over-broad.
#[test]
fn int14_accepts_the_highest_base_that_covers_the_uart_window_kiln275() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
    memory.write_u16(0x400, 0xFFF8);
    cpu.regs.set_ah(0x03); // AH=03h: status only, no I/O programming
    cpu.regs.set_dx(0x0000);

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .expect("0xFFF8 covers the register window");

    assert_ne!(
        cpu.regs.ah(),
        0x80,
        "0xFFF8 covers the whole UART window and must not be reported absent"
    );
}

/// Zero keeps its existing meaning — the BDA's own "no port installed"
/// encoding — so the new bound cannot have changed it.
#[test]
fn int14_still_reports_a_zero_bda_base_as_absent_kiln275() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();
    memory.write_u16(0x400, 0);
    cpu.regs.set_ah(0x00);
    cpu.regs.set_dx(0x0000);

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert_eq!(cpu.regs.ah(), 0x80, "a zero base is an absent port");
}

#[test]
fn test_int14_bda_timeout_initialization() {
    let (_bios, _cpu, memory, _serial) = int14_test_setup();
    // init_ivt() is called inside int14_test_setup() via bios.init_ivt()
    assert_eq!(memory.read_u8(0x47C), 1, "COM1 timeout should be 1");
    assert_eq!(memory.read_u8(0x47D), 1, "COM2 timeout should be 1");
    assert_eq!(memory.read_u8(0x47E), 1, "COM3 timeout should be 1");
    assert_eq!(memory.read_u8(0x47F), 1, "COM4 timeout should be 1");
}

#[test]
fn test_int14_initialize() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=00h, AL=0xE3 = 9600 baud, no parity, 1 stop bit, 8 data bits
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0xE3);
    cpu.regs.set_dx(0x0000); // COM1
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Should return real LSR/MSR, not timeout.
    // THRE+TEMT should be set (0x60) after init.
    assert_ne!(
        cpu.regs.ah() & 0x60,
        0,
        "THRE+TEMT should be set after init"
    );
}

#[test]
fn test_int14_status() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=03h - Get status for COM1
    cpu.regs.set_ah(0x03);
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // LSR should have THRE+TEMT set (0x60).
    assert_ne!(cpu.regs.ah() & 0x60, 0, "LSR should have THRE+TEMT");
}

#[test]
fn test_int14_write_char() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Assert DSR + CTS so modem handshake passes
    serial.ports[0].update_msr_signals(true, true, false, false);

    // AH=01h - Write 'A' to COM1
    cpu.regs.set_ah(0x01);
    cpu.regs.set_al(0x41);
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // THRE should be set (byte accepted), no timeout
    assert_eq!(
        cpu.regs.ah() & 0x80,
        0,
        "Should not timeout writing to COM1"
    );
}

#[test]
fn test_int14_write_char_no_dsr() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Mark as external device so null-modem simulation doesn't override MSR
    serial.ports[0].external_tx = true;
    // CTS asserted but DSR not — isolates DSR as the missing signal
    serial.ports[0].update_msr_signals(false, true, false, false);

    cpu.regs.set_ah(0x01);
    cpu.regs.set_al(0x41);
    cpu.regs.set_dx(0x0000);
    int14_to_completion(
        &mut bios,
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_, _| {},
    );

    assert_ne!(cpu.regs.ah() & 0x80, 0, "Should timeout without DSR");
    assert_eq!(cpu.regs.ah(), 0x90, "Timeout with CTS set but DSR missing");
}

#[test]
fn test_int14_write_char_no_cts() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Mark as external device so null-modem simulation doesn't override MSR
    serial.ports[0].external_tx = true;
    // DSR asserted but CTS not
    serial.ports[0].update_msr_signals(true, false, false, false);

    cpu.regs.set_ah(0x01);
    cpu.regs.set_al(0x41);
    cpu.regs.set_dx(0x0000);
    int14_to_completion(
        &mut bios,
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_, _| {},
    );

    // Should timeout because CTS not asserted (both DSR+CTS required for TX)
    assert_ne!(cpu.regs.ah() & 0x80, 0, "Should timeout without CTS");
    assert_eq!(cpu.regs.ah(), 0xA0, "Timeout with DSR set but CTS missing");
}

#[test]
fn test_int14_invalid_port_returns_timeout() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // DX=4 → no such port in BDA → should return timeout.
    cpu.regs.set_ah(0x03);
    cpu.regs.set_dx(0x0004);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert_eq!(cpu.regs.ah(), 0x80, "Invalid port should return timeout");
}

#[test]
fn test_int14_read_char_timeout_when_empty() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Assert DSR so we reach the DR polling phase
    serial.ports[0].update_msr_signals(true, false, false, false);

    // AH=02h - Read from COM1 (nothing in RBR → should timeout on DR check)
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);
    int14_to_completion(
        &mut bios,
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_, _| {},
    );

    assert_ne!(cpu.regs.ah() & 0x80, 0, "Should timeout with no data");
    assert_eq!(cpu.regs.ah(), 0xE0, "Timeout should contain LSR status");
}

#[test]
fn test_int14_read_char_success() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Assert DSR so modem handshake passes (RX only needs DSR, not CTS)
    serial.ports[0].update_msr_signals(true, false, false, false);

    // Inject a byte into the UART RX buffer
    serial.ports[0].receive_byte(0x42); // 'B'

    // AH=02h - Read from COM1
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Should succeed: AL = received char, AH = LSR & 0x1E (error bits only)
    assert_eq!(cpu.regs.al(), 0x42, "Should read 'B'");
    assert_eq!(cpu.regs.ah() & 0x80, 0, "Should not timeout");
    // AH should only contain error bits (OE, PE, FE, BI) - bits 1-4
    // THRE/TEMT (bits 5-6) should NOT be present
    assert_eq!(
        cpu.regs.ah() & 0x60,
        0,
        "THRE/TEMT should not be in receive status"
    );
    assert_eq!(cpu.regs.ah(), 0x00, "No error bits for clean receive");
}

#[test]
fn test_int14_read_char_no_dsr() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Mark as external device so null-modem simulation doesn't override MSR
    serial.ports[0].external_tx = true;
    // Inject data but DON'T assert DSR
    serial.ports[0].receive_byte(0x42);

    // AH=02h - Read from COM1
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);
    int14_to_completion(
        &mut bios,
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_, _| {},
    );

    // Should timeout because DSR not asserted, even though data is ready
    assert_ne!(cpu.regs.ah() & 0x80, 0, "Should timeout without DSR");
    assert_eq!(cpu.regs.ah(), 0x80, "Timeout with no modem signals");
}

#[test]
fn test_int14_unhandled_function() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    cpu.regs.set_ah(0xFF);
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert_eq!(cpu.regs.ah(), 0x80);
}

#[test]
fn test_int14_status_clears_cf() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Set CF before calling INT 14h
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);

    // AH=03h - Get status for COM1
    cpu.regs.set_ah(0x03);
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // CF should be cleared
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be cleared after INT 14h"
    );
}

#[test]
fn test_int14_extended_init() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=04h: Extended init
    // CL=08 (19200 baud), CH=03 (8 data bits), BL=00 (1 stop), BH=00 (no parity), AL=01 (no break)
    cpu.regs.set_ah(0x04);
    cpu.regs.set_al(0x01); // no break
    cpu.regs.set_bh(0x00); // no parity
    cpu.regs.set_bl(0x00); // 1 stop bit
    cpu.regs.set_ch(0x03); // 8 data bits
    cpu.regs.set_cl(0x08); // 19200 baud
    cpu.regs.set_dx(0x0000); // COM1
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Should return LSR in AH (THRE+TEMT should be set) and MSR in AL
    assert_ne!(
        cpu.regs.ah() & 0x60,
        0,
        "THRE+TEMT should be set after init"
    );
    assert_eq!(cpu.regs.ah() & 0x80, 0, "Should not indicate timeout");

    // Verify the UART was configured: read back LCR
    // LCR should be 0x03 (8 data bits, 1 stop, no parity, no break)
    let lcr = serial.ports[0].read(0x3F8 + 3);
    assert_eq!(lcr, 0x03, "LCR should be 8N1 no break");
}

#[test]
fn test_int14_extended_init_parity() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=04h: 9600 baud, 7 data bits, even parity, 1 stop, no break
    cpu.regs.set_ah(0x04);
    cpu.regs.set_al(0x01); // no break
    cpu.regs.set_bh(0x02); // even parity
    cpu.regs.set_bl(0x00); // 1 stop bit
    cpu.regs.set_ch(0x02); // 7 data bits
    cpu.regs.set_cl(0x07); // 9600 baud
    cpu.regs.set_dx(0x0000); // COM1
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // LCR should be: bits 1-0=10 (7 data), bit 2=0 (1 stop), bits 5-3=011 (even parity)
    // = 0b00011010 = 0x1A
    let lcr = serial.ports[0].read(0x3F8 + 3);
    assert_eq!(lcr, 0x1A, "LCR should be 7E1 (even parity)");
}

#[test]
fn test_int14_invalid_port_clears_cf() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Set CF before calling with invalid port
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);

    cpu.regs.set_ah(0x03);
    cpu.regs.set_dx(0x0004); // Invalid port
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be cleared even for invalid port"
    );
}

#[test]
fn test_int14_extended_init_break() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=04h: 9600 baud, 8N1, break enabled (AL=0x00)
    cpu.regs.set_ah(0x04);
    cpu.regs.set_al(0x00); // break enabled
    cpu.regs.set_bh(0x00); // no parity
    cpu.regs.set_bl(0x00); // 1 stop bit
    cpu.regs.set_ch(0x03); // 8 data bits
    cpu.regs.set_cl(0x07); // 9600 baud
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // LCR should be 0x43 (8N1 + break bit 6)
    let lcr = serial.ports[0].read(0x3F8 + 3);
    assert_eq!(lcr, 0x43, "LCR should have break bit set");
}

#[test]
fn test_int14_extended_init_stick_parity() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=04h: 9600 baud, 8 data, stick odd parity, 2 stop bits, no break
    cpu.regs.set_ah(0x04);
    cpu.regs.set_al(0x01); // no break
    cpu.regs.set_bh(0x03); // stick odd parity
    cpu.regs.set_bl(0x01); // 2 stop bits
    cpu.regs.set_ch(0x03); // 8 data bits
    cpu.regs.set_cl(0x07); // 9600 baud
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // LCR: bits 1-0=11 (8 data), bit 2=1 (2 stop), bits 5-3=101 (stick odd)
    // = 0b00101111 = 0x2F
    let lcr = serial.ports[0].read(0x3F8 + 3);
    assert_eq!(lcr, 0x2F, "LCR should be 8 data, 2 stop, stick odd parity");
}

#[test]
fn test_int14_extended_init_invalid_baud() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // AH=04h: invalid baud (CL=0xFF), 8N1, no break
    cpu.regs.set_ah(0x04);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_ch(0x03);
    cpu.regs.set_cl(0xFF); // invalid
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Should not timeout (falls back to 9600 baud)
    assert_eq!(
        cpu.regs.ah() & 0x80,
        0,
        "Should not timeout with invalid baud"
    );
}

#[test]
fn test_int14_ah01_serial_tick_receives_correct_port_index() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Assert DSR + CTS so the modem handshake passes and we reach THRE polling.
    serial.ports[1].update_msr_signals(true, true, false, false);

    cpu.regs.set_ah(0x01);
    cpu.regs.set_al(b'X');
    cpu.regs.set_dx(0x0001); // COM2 → port index 1

    let mut observed_indices: Vec<usize> = Vec::new();
    bios.int14(
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_serial, port_idx| {
            observed_indices.push(port_idx);
        },
    )
    .unwrap();

    // The tick closure should have been called with port index 1 (COM2).
    assert!(
        !observed_indices.is_empty(),
        "serial_tick should have been invoked at least once"
    );
    assert!(
        observed_indices.iter().all(|&idx| idx == 1),
        "All serial_tick calls should target port index 1 (COM2), got: {:?}",
        observed_indices
    );
}

#[test]
fn test_int14_ah02_serial_tick_receives_correct_port_index() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Assert DSR so we reach the DR polling phase.
    serial.ports[0].update_msr_signals(true, false, false, false);

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000); // COM1 → port index 0

    let mut observed_indices: Vec<usize> = Vec::new();
    bios.int14(
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |_serial, port_idx| {
            observed_indices.push(port_idx);
        },
    )
    .unwrap();

    assert!(
        !observed_indices.is_empty(),
        "serial_tick should have been invoked at least once during AH=02h"
    );
    assert!(
        observed_indices.iter().all(|&idx| idx == 0),
        "All serial_tick calls should target port index 0 (COM1), got: {:?}",
        observed_indices
    );
}

#[test]
fn test_int14_ah01_polling_advances_uart_timing() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Initialize COM1 at 9600 baud so char_cycles is non-trivial.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0xE3); // 9600 baud, 8N1
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Record the UART clock before the TX call.
    let cycle_before = serial.ports[0].current_cycle();

    // Assert DSR + CTS so the modem handshake passes.
    serial.ports[0].update_msr_signals(true, true, false, false);

    // AH=01h: transmit byte — the tick closure advances UART timing.
    cpu.regs.set_ah(0x01);
    cpu.regs.set_al(b'A');
    cpu.regs.set_dx(0x0000);
    bios.int14(
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |serial, port_idx| {
            if port_idx < serial.ports.len() {
                let quantum = serial.ports[port_idx].char_cycles().max(1);
                serial.ports[port_idx].advance_cycles(quantum);
            }
        },
    )
    .unwrap();

    let cycle_after = serial.ports[0].current_cycle();
    assert!(
        cycle_after > cycle_before,
        "UART timing should advance during AH=01h polling: before={}, after={}",
        cycle_before,
        cycle_after
    );
}

#[test]
fn test_int14_ah02_polling_advances_uart_timing() {
    let (mut bios, mut cpu, mut memory, mut serial) = int14_test_setup();

    // Initialize COM1 at 9600 baud.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0xE3); // 9600 baud, 8N1
    cpu.regs.set_dx(0x0000);
    bios.int14(&mut cpu, &mut memory, &mut serial, &mut |_, _| {})
        .unwrap();

    // Assert DSR so we reach the DR polling phase.
    serial.ports[0].update_msr_signals(true, false, false, false);

    let cycle_before = serial.ports[0].current_cycle();

    // AH=02h: receive byte — will timeout (no data), but the tick
    // closure should advance UART timing during polling.
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0000);
    bios.int14(
        &mut cpu,
        &mut memory,
        &mut serial,
        &mut |serial, port_idx| {
            if port_idx < serial.ports.len() {
                let quantum = serial.ports[port_idx].char_cycles().max(1);
                serial.ports[port_idx].advance_cycles(quantum);
            }
        },
    )
    .unwrap();

    let cycle_after = serial.ports[0].current_cycle();
    assert!(
        cycle_after > cycle_before,
        "UART timing should advance during AH=02h polling: before={}, after={}",
        cycle_before,
        cycle_after
    );
}

#[test]
fn test_int17_print_char() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=00h - Print character
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x41); // 'A'
    cpu.regs.set_dx(0x0000); // LPT1
    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01); // Timeout
}

#[test]
fn test_int17_init_printer() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=01h - Initialize printer port
    cpu.regs.set_ah(0x01);
    cpu.regs.set_dx(0x0000);
    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01);
}

#[test]
fn test_int17_get_status() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=02h - Get printer status
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0x0001); // LPT2
    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01);
}

#[test]
fn test_int17_unhandled_function() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=FFh - Unhandled function
    cpu.regs.set_ah(0xFF);
    cpu.regs.set_dx(0x0000);
    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01); // Still returns timeout
}

#[test]
fn test_copy_memory_block_same_src_dst() {
    let mut memory = Memory::new();
    // Write some data
    memory.write_u32(0x1000, 0xDEADBEEF);
    // Copy with same src and dst - should be a no-op
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1000, 4);
    assert_eq!(memory.read_u32(0x1000), 0xDEADBEEF);
}

#[test]
fn test_copy_memory_block_zero_length() {
    let mut memory = Memory::new();
    memory.write_u32(0x1000, 0xDEADBEEF);
    memory.write_u32(0x2000, 0x00000000);
    // Copy with zero length - should be a no-op
    Bios::copy_memory_block(&mut memory, 0x1000, 0x2000, 0);
    assert_eq!(memory.read_u32(0x2000), 0x00000000);
}

#[test]
fn test_copy_memory_block_non_overlapping_forward() {
    let mut memory = Memory::new();
    // Write 8 bytes of data
    memory.write_u32(0x1000, 0xAABBCCDD);
    memory.write_u32(0x1004, 0x11223344);
    // Copy to non-overlapping destination
    Bios::copy_memory_block(&mut memory, 0x1000, 0x2000, 8);
    assert_eq!(memory.read_u32(0x2000), 0xAABBCCDD);
    assert_eq!(memory.read_u32(0x2004), 0x11223344);
}

#[test]
fn test_copy_memory_block_non_overlapping_with_remainder() {
    let mut memory = Memory::new();
    // Write 5 bytes (4 + 1 remainder)
    memory.write_u32(0x1000, 0xAABBCCDD);
    memory.write_u8(0x1004, 0xEE);
    Bios::copy_memory_block(&mut memory, 0x1000, 0x2000, 5);
    assert_eq!(memory.read_u32(0x2000), 0xAABBCCDD);
    assert_eq!(memory.read_u8(0x2004), 0xEE);
}

#[test]
fn test_copy_memory_block_overlapping_backward() {
    let mut memory = Memory::new();
    // Write data at 0x1000..0x1008
    memory.write_u32(0x1000, 0xAABBCCDD);
    memory.write_u32(0x1004, 0x11223344);
    // Copy overlapping: src=0x1000, dst=0x1004, len=8
    // src < dst && src + len > dst => overlapping backward copy
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1004, 8);
    // After backward copy, dst should have the original src data
    assert_eq!(memory.read_u32(0x1004), 0xAABBCCDD);
    assert_eq!(memory.read_u32(0x1008), 0x11223344);
}

#[test]
fn test_copy_memory_block_overlapping_backward_with_remainder() {
    let mut memory = Memory::new();
    // Write 5 bytes
    memory.write_u32(0x1000, 0xAABBCCDD);
    memory.write_u8(0x1004, 0xEE);
    // Overlapping copy: src < dst, src+len > dst
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1002, 5);
    assert_eq!(memory.read_u8(0x1002), 0xDD);
    assert_eq!(memory.read_u8(0x1003), 0xCC);
    assert_eq!(memory.read_u8(0x1004), 0xBB);
    assert_eq!(memory.read_u8(0x1005), 0xAA);
    assert_eq!(memory.read_u8(0x1006), 0xEE);
}

#[test]
fn test_copy_memory_block_non_overlapping_dst_before_src() {
    let mut memory = Memory::new();
    // dst < src (no overlap flag), should use forward copy
    memory.write_u32(0x2000, 0xDEADBEEF);
    memory.write_u32(0x2004, 0xCAFEBABE);
    Bios::copy_memory_block(&mut memory, 0x2000, 0x1000, 8);
    assert_eq!(memory.read_u32(0x1000), 0xDEADBEEF);
    assert_eq!(memory.read_u32(0x1004), 0xCAFEBABE);
}

#[test]
fn test_int15_87_invalid_count() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AH=87h with word count > 0x8000
    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(0x8001); // Invalid: > 0x8000
    cpu.regs.es = 0x0050;
    cpu.regs.set_si(0x0000);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x02); // Invalid count
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_87_zero_count() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AH=87h with zero word count
    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(0x0000); // Zero count = no-op success
    cpu.regs.es = 0x0050;
    cpu.regs.set_si(0x0000);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00); // Success
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_87_successful_copy() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Write source data at address 0x10000 (above 64K)
    memory.write_u32(0x10000, 0xDEADBEEF);
    memory.write_u32(0x10004, 0xCAFEBABE);

    // Set up GDT at ES:SI = 0050:0000 = linear 0x500
    let gdt_addr = 0x500;
    cpu.regs.es = 0x0050;
    cpu.regs.set_si(0x0000);
    setup_int15_87_gdt(&mut memory, gdt_addr, 0x10000, 0x20000);

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(4); // 4 words = 8 bytes

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00); // Success
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(memory.read_u32(0x20000), 0xDEADBEEF);
    assert_eq!(memory.read_u32(0x20004), 0xCAFEBABE);
}

#[test]
fn int15_87_opens_the_a20_gate_for_the_transfer_and_restores_the_callers() {
    // MDD-BUG-TEM-00066. AH=87h validates the descriptor bases RAW against
    // `ram_size`, but copies through `Memory::read_u32`/`write_u32`, which
    // apply `mask_address` - and with A20 closed that clears bit 20. So the
    // call validated 0x0010_0000 and read 0x0000_0000, handing the guest the
    // IVT and BDA instead of extended memory, and reporting success. With
    // the bases reversed it wrote 64 KiB OVER the IVT, BDA and DOS kernel.
    //
    // A20 is CLOSED in the default HLE DOS state, and AH=87h exists exactly
    // for programs with no XMS driver - which is exactly when it is closed.
    // A real AT BIOS opens the gate for the move and restores it after, as
    // the equivalent XMS 0Bh already does here.
    //
    // Every other AH=87h test uses sub-1 MiB bases, where the mask is a
    // no-op, and `Memory::new` starts with A20 open - which is why the
    // suite could not see this.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Seed both addresses the masked and unmasked reads resolve to, while
    // the gate is still open.
    memory.write_u32(0x0010_0000, 0xDEAD_BEEF);
    memory.write_u32(0x0010_0004, 0xCAFE_BABE);
    memory.write_u32(0x0000_0000, 0x1111_1111);
    memory.write_u32(0x0000_0004, 0x2222_2222);

    // The caller's gate: closed, as a real-mode DOS program's is.
    memory.set_a20(false);
    assert_eq!(
        memory.read_u32(0x0010_0000),
        0x1111_1111,
        "with the gate closed 0x00100000 aliases the IVT - the premise"
    );

    let gdt_addr = 0x500;
    cpu.regs.es = 0x0050;
    cpu.regs.set_si(0x0000);
    setup_int15_87_gdt(&mut memory, gdt_addr, 0x0010_0000, 0x0002_0000);
    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(4); // 4 words = 8 bytes

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        (memory.read_u32(0x0002_0000), memory.read_u32(0x0002_0004)),
        (0xDEAD_BEEF, 0xCAFE_BABE),
        "the move must copy the address it validated - extended memory at              0x00100000 - not the IVT a closed A20 aliases it onto"
    );
    assert!(
        !memory.a20_enabled(),
        "the caller's closed gate must be restored, as a real AT BIOS does"
    );
}

#[test]
fn test_int15_87_out_of_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ram_size = memory.ram_size() as u32;
    // Set up GDT with destination beyond RAM
    let gdt_addr = 0x500;
    cpu.regs.es = 0x0050;
    cpu.regs.set_si(0x0000);
    setup_int15_87_gdt(&mut memory, gdt_addr, 0x10000, ram_size);

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(4); // 4 words = 8 bytes

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x02); // Memory error
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_event_wait_already_active() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First, set an event wait
    cpu.regs.set_ah(0x83);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x0001);
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x0050;
    cpu.regs.set_bx(0x0000);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(bios.event_wait_active);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Try to set another event wait while one is active
    cpu.regs.set_ah(0x83);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x0002);
    cpu.regs.set_dx(0x0000);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    // Should fail with CF=1
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_event_wait_invalid_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AH=83h, AL=0xFF - invalid sub-function
    cpu.regs.set_ah(0x83);
    cpu.regs.set_al(0xFF);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86); // Function not supported
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_ebda_not_present() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.es = 0x1234;
    cpu.regs.set_ah(0xC1);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.es, 0x1234);
    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn hle_memory_interfaces_consistently_report_no_ebda() {
    const SMAP: u32 = 0x534D_4150;

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    assert_eq!(memory.read_u16(0x40E), 0, "BDA must not point to an EBDA");

    bios.int12(&mut cpu).unwrap();
    assert_eq!(cpu.regs.ax(), 640);
    assert_eq!(cpu.regs.ax(), memory.read_u16(0x413));

    cpu.regs.set_ah(0xC0);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let config_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
    assert_eq!(
        memory.read_u8(config_addr + 5) & 0x04,
        0,
        "system configuration feature byte must say no EBDA"
    );

    cpu.regs.eax = 0xE820;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = E820Entry::SIZE as u32;
    cpu.regs.edx = SMAP;
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    let map_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    assert_eq!(memory.read_u64(map_addr), 0);
    assert_eq!(memory.read_u64(map_addr + 8), 0x000A_0000);
    assert_eq!(memory.read_u32(map_addr + 16), 1);
    assert_eq!(cpu.regs.ebx, 1);

    cpu.regs.eax = 0xE820;
    cpu.regs.ecx = E820Entry::SIZE as u32;
    cpu.regs.edx = SMAP;
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(memory.read_u64(map_addr), 0x000A_0000);
    assert_eq!(memory.read_u64(map_addr + 8), 0x0006_0000);
    assert_eq!(memory.read_u32(map_addr + 16), 2);

    cpu.regs.set_ax(0xC100);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e881_extended_memory_size() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ram_bytes = memory.ram_size() as u64;

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81); // E881h sub-function
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Calculate expected values
    let extended = ram_bytes.saturating_sub(0x0010_0000);
    let expected_1_to_16_kb = (extended.min(0x00F0_0000) / 1024) as u32;
    let expected_above_16_blocks = (extended.saturating_sub(0x00F0_0000) / 0x0001_0000) as u32;

    assert_eq!(cpu.regs.eax, expected_1_to_16_kb);
    assert_eq!(cpu.regs.ebx, expected_above_16_blocks);
    assert_eq!(cpu.regs.ecx, expected_1_to_16_kb);
    assert_eq!(cpu.regs.edx, expected_above_16_blocks);
}

#[test]
fn test_int15_e8_unsupported_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0xFF); // Not 0x01, 0x81, or 0x20
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86); // Function not supported
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e820_buffer_too_small() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x20);
    cpu.regs.edx = 0x534D_4150; // 'SMAP'
    cpu.regs.ebx = 0; // First entry
    cpu.regs.ecx = 10; // Buffer too small (needs 20)
    cpu.regs.es = 0x0050;
    cpu.regs.set_di(0x0000);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e820_invalid_continuation() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x20);
    cpu.regs.edx = 0x534D_4150; // 'SMAP'
    cpu.regs.ebx = 100; // Invalid continuation value (beyond entry count)
    cpu.regs.ecx = 20; // Adequate buffer
    cpu.regs.es = 0x0050;
    cpu.regs.set_di(0x0000);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_tick_event_wait_inactive() {
    let mut bios = Bios::new();
    let mut memory = Memory::new();

    // Event wait is not active by default
    assert!(!bios.event_wait_active);
    // Should return immediately (no-op)
    bios.tick_event_wait(1000, &mut memory);
    assert!(!bios.event_wait_active);
}

#[test]
fn test_tick_event_wait_expired() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up event wait: 10000 microseconds, flag at linear 0x500
    cpu.regs.set_ah(0x83);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x0000); // CX:DX = 0x00002710 = 10000 us
    cpu.regs.set_dx(0x2710);
    cpu.regs.es = 0x0050;
    cpu.regs.set_bx(0x0000);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(bios.event_wait_active);

    // Flag byte should have bit 7 cleared
    let flag_addr = Memory::seg_offset_to_linear(0x0050, 0x0000);
    assert_eq!(memory.read_u8(flag_addr) & 0x80, 0x00);

    // Tick with enough time to expire
    bios.tick_event_wait(10000, &mut memory);

    // Event should be inactive and flag bit 7 set
    assert!(!bios.event_wait_active);
    assert_ne!(memory.read_u8(flag_addr) & 0x80, 0x00);
}

#[test]
fn test_tick_event_wait_not_expired() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up event wait: 10000 microseconds
    cpu.regs.set_ah(0x83);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0x2710); // 10000 us
    cpu.regs.es = 0x0050;
    cpu.regs.set_bx(0x0000);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(bios.event_wait_active);

    // Tick with not enough time
    bios.tick_event_wait(5000, &mut memory);

    // Event should still be active
    assert!(bios.event_wait_active);
    assert_eq!(bios.event_wait_remaining_us, 5000);

    let flag_addr = Memory::seg_offset_to_linear(0x0050, 0x0000);
    assert_eq!(memory.read_u8(flag_addr) & 0x80, 0x00);
}

#[test]
fn test_generate_e820_map_no_extended_memory() {
    let bios = Bios::new();
    // Default memory is > 1MB, so we test with VBE active/inactive
    let memory = Memory::new();
    let (entries, count) = bios.generate_e820_map(&memory);

    // Default RAM is > 1MB, so base, video/ROM, and extended memory.
    assert_eq!(
        count,
        3,
        "default RAM {} should produce 3 E820 entries",
        memory.ram_size()
    );
    // Entry 0: base memory
    assert_eq!(entries[0].base, 0);
    assert_eq!(entries[0].length, 0x000A_0000);
    // Entry 1: Video/ROM
    assert_eq!(entries[1].base, 0x000A_0000);
    assert_eq!(entries[1].length, 0x0006_0000);
    // Entry 2: Extended memory
    assert_eq!(entries[2].base, 0x0010_0000);
}

#[test]
fn test_parse_descriptor_base() {
    let mut memory = Memory::new();
    let desc_addr = 0x1000;

    // Set up a descriptor with base = 0xAB123456
    // base_lo at +2 = 0x3456
    // base_mid at +4 = 0x12
    // base_hi at +7 = 0xAB
    memory.write_u16(desc_addr + 2, 0x3456);
    memory.write_u8(desc_addr + 4, 0x12);
    memory.write_u8(desc_addr + 7, 0xAB);

    let base = Bios::parse_descriptor_base(&memory, desc_addr);
    assert_eq!(base, 0xAB12_3456);
}

#[test]
fn test_load_font_to_plane2() {
    let mut memory = Memory::new();

    // Create a small font (8 bytes per char, 256 chars)
    let char_height = 8;
    let mut font = vec![0u8; 256 * char_height];
    // Set char 'A' (65) to a recognizable pattern
    let a_offset = 65 * char_height;
    font[a_offset] = 0x18; // ..##....
    font[a_offset + 1] = 0x3C; // .####...
    font[a_offset + 2] = 0x66; // .##..##.
    font[a_offset + 3] = 0x7E; // .######.
    font[a_offset + 4] = 0x66; // .##..##.
    font[a_offset + 5] = 0x66; // .##..##.
    font[a_offset + 6] = 0x66; // .##..##.
    font[a_offset + 7] = 0x00; // ........

    Bios::load_font_to_plane2_block(&mut memory, &font, char_height, 0, 0, 256);

    // Check that char 'A' was loaded at slot 65 (offset 65*32 in plane 2)
    let plane2 = memory.plane_mut(2);
    let plane2_offset = 65 * 32;
    assert_eq!(plane2[plane2_offset], 0x18);
    assert_eq!(plane2[plane2_offset + 1], 0x3C);
    assert_eq!(plane2[plane2_offset + 2], 0x66);
    assert_eq!(plane2[plane2_offset + 7], 0x00);
    // Bytes beyond char_height should be unwritten (0)
    assert_eq!(plane2[plane2_offset + 8], 0x00);
}

#[test]
fn test_e820_entry_write_to_memory() {
    let mut memory = Memory::new();
    let entry = E820Entry::new(
        0x0000_1000_0000_0000,
        0x0000_0000_FFFF_0000,
        E820Type::AddressRangeACPI,
    );

    entry.write_to_memory(&mut memory, 0x0050, 0x0000);

    let addr = 0x500usize;
    assert_eq!(memory.read_u64(addr), 0x0000_1000_0000_0000);
    assert_eq!(memory.read_u64(addr + 8), 0x0000_0000_FFFF_0000);
    assert_eq!(
        memory.read_u32(addr + 16),
        E820Type::AddressRangeACPI as u32
    );
}

#[test]
fn test_disk_address_packet_read_from_memory() {
    let mut memory = Memory::new();
    let addr = 0x1000;

    memory.write_u8(addr, 0x10); // size
                                 // +1 is reserved
    memory.write_u16(addr + 2, 0x0008); // sector_count
    memory.write_u16(addr + 4, 0x0200); // buffer_offset
    memory.write_u16(addr + 6, 0x0050); // buffer_segment
    memory.write_u64(addr + 8, 0x0000_0000_0000_0100); // lba

    let dap = DiskAddressPacket::read_from_memory(&memory, addr);
    assert_eq!(dap.size, 0x10);
    assert_eq!(dap.sector_count, 0x0008);
    assert_eq!(dap.buffer_offset, 0x0200);
    assert_eq!(dap.buffer_segment, 0x0050);
    assert_eq!(dap.lba, 0x100);
}

#[test]
fn test_int16_read_key_with_data() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    // Enqueue a key in BDA
    bda::kbd_enqueue(&mut memory, 0x1E, 0x61); // 'a'

    // Set up an IRET frame on the stack
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    let sp = cpu.regs.sp();
    memory.write_u16_seg(cpu.regs.ss, sp, 0x1234); // return IP
    memory.write_u16_seg(cpu.regs.ss, sp.wrapping_add(2), 0x0000); // return CS
    memory.write_u16_seg(cpu.regs.ss, sp.wrapping_add(4), 0x0200); // flags with IF

    cpu.regs.set_ah(0x00); // Read keyboard

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x1E);
    assert_eq!(cpu.regs.al(), 0x61);
}

#[test]
fn test_int16_read_key_extended_e0_replacement() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    // Enqueue a key with ASCII 0xE0 (extended key indicator)
    bda::kbd_enqueue(&mut memory, 0x48, 0xE0); // Up arrow

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x1234);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0x0000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    cpu.regs.set_ah(0x00); // AH=00h replaces 0xE0 with 0x00

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x48);
    assert_eq!(cpu.regs.al(), 0x00); // 0xE0 replaced with 0
}

#[test]
fn test_int16_read_key_extended_no_replacement() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    bda::kbd_enqueue(&mut memory, 0x48, 0xE0);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x1234);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0x0000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    cpu.regs.set_ah(0x10); // AH=10h keeps 0xE0

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x48);
    assert_eq!(cpu.regs.al(), 0xE0); // Preserved
}

#[test]
fn test_int16_read_key_empty_loopback() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    // No keys in buffer

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    let sp = cpu.regs.sp();
    memory.write_u16_seg(cpu.regs.ss, sp, 0x1234); // return IP
    memory.write_u16_seg(cpu.regs.ss, sp.wrapping_add(2), 0x0000); // CS
    memory.write_u16_seg(cpu.regs.ss, sp.wrapping_add(4), 0x0000); // flags (IF=0)

    cpu.regs.set_ah(0x00);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // Should return Stub redirect to assembler wait loop
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_WAIT_00),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
    // Stack should be untouched (no IP-2 modification)
    let ip_on_stack = memory.read_u16_seg(cpu.regs.ss, sp);
    assert_eq!(ip_on_stack, 0x1234);
}

#[test]
fn test_int16_peek_with_data() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    bda::kbd_enqueue(&mut memory, 0x1E, 0x61);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x1234);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0x0000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    cpu.regs.set_ah(0x01); // Peek keyboard

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // AH=01h always redirects to assembler peek stub
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_01),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

#[test]
fn test_int16_peek_e0_replacement_ah01() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    bda::kbd_enqueue(&mut memory, 0x48, 0xE0);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x1234);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0x0000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    cpu.regs.set_ah(0x01); // AH=01h — always redirects to peek stub

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // E0 replacement now handled by the assembler stub, not HLE
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_01),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

#[test]
fn test_int16_peek_empty() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x1234);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0x0000);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    cpu.regs.set_ah(0x01);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // AH=01h always redirects to assembler peek stub (even when empty)
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_01),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

#[test]
fn test_int16_get_shift_flags_basic() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);
    bda::kbd_set_shift_flags(&mut memory, 0x42);

    cpu.regs.set_ah(0x02);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x42);
}

#[test]
fn test_int16_get_extended_shift_flags() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x12);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
}

#[test]
fn test_int16_store_keycode() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x1E); // scancode
    cpu.regs.set_cl(0x61); // ascii

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // success

    // Verify the key was enqueued
    let peek = bda::kbd_peek(&memory);
    assert!(peek.is_some());
    assert_eq!(peek.unwrap(), (0x1E, 0x61));
}

#[test]
fn test_int16_typematic_reset_to_defaults() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // First set non-default values
    keyboard.set_typematic(3, 31);
    assert_eq!(keyboard.get_typematic_delay(), 3);
    assert_eq!(keyboard.get_typematic_rate(), 31);

    // AL=00h: Reset to defaults
    bios.init_ivt(&mut memory, false);
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x00);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(keyboard.get_typematic_delay(), 1);
    assert_eq!(keyboard.get_typematic_rate(), 0x0B);
}

#[test]
fn test_int16_typematic_disable() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // AL=04h: Disable typematic
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x04);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert!(!keyboard.is_typematic_enabled());
}

#[test]
fn test_int16_typematic_disable_then_reset() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Disable
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x04);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    assert!(!keyboard.is_typematic_enabled());

    // Reset to defaults should re-enable
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x00);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    assert!(keyboard.is_typematic_enabled());
    assert_eq!(keyboard.get_typematic_delay(), 1);
    assert_eq!(keyboard.get_typematic_rate(), 0x0B);
}

#[test]
fn test_int16_typematic_get_current() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Set specific values first
    keyboard.set_typematic(2, 20);

    // AL=06h: Get current
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x06);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();

    assert_eq!(cpu.regs.bh(), 2); // delay
    assert_eq!(cpu.regs.bl(), 20); // rate
}

#[test]
fn test_int16_typematic_unknown_subfunction() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Unknown subfunction (PCjr AL=01h) should be a no-op
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x01);
    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // Defaults unchanged (power-on defaults: delay=1, rate=0x0B)
    assert_eq!(keyboard.get_typematic_delay(), 1);
    assert_eq!(keyboard.get_typematic_rate(), 0x0B);
}

#[test]
fn test_int16_typematic_set_with_masking() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // AL=05h with out-of-range values
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(0xFF); // Should mask to 3
    cpu.regs.set_bl(0xFF); // Should mask to 31

    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    assert_eq!(keyboard.get_typematic_delay(), 3);
    assert_eq!(keyboard.get_typematic_rate(), 31);
}

#[test]
fn test_int16_enhanced_write_to_buffer() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x13);
    cpu.regs.set_ch(0x48); // scancode
    cpu.regs.set_cl(0x00); // ascii

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int16_unhandled_ah_returns_ok() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    cpu.regs.set_ah(0xFF);
    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int16_unhandled_ah_f1_sets_carry() {
    // INT 16h AH=F1h is a Compaq 386+ BIOS function: "READ CURRENT CPU SPEED"
    // style CPU speed reporting. On non-Compaq BIOSes, AH=F1 is unrecognized and
    // falls through to IRET. We return CF=1 as a "not supported" signal.
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    cpu.regs.set_ah(0xF1);
    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Unhandled INT 16h AH=F1h should set carry flag"
    );
}

#[test]
fn test_int10_set_palette_cga_background() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    // Set CGA video mode in BDA (mode 4)
    memory.write_u8(0x449, 0x04);

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00); // background color
    cpu.regs.set_bl(0x05); // color value

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_palette_cga_palette1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    memory.write_u8(0x449, 0x04); // CGA mode

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x01); // set palette
    cpu.regs.set_bl(0x01); // palette 1

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_palette_bda_update_cga_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    memory.write_u8(0x449, 0x04); // CGA mode <=0x07

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x03);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // BDA 0x466 should be updated
    let color_sel = memory.read_u8(0x466);
    assert_ne!(color_sel, 0xFF);
}

#[test]
fn test_int10_set_palette_bda_no_update_high_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    memory.write_u8(0x449, 0x13); // Mode 13h > 0x07

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x03);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_int10_set_border_color_via_ah10() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x01); // set border color
    cpu.regs.set_bh(0x05);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x11], 0x05);
}

#[test]
fn test_int10_set_all_palette_registers_and_border() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let table_seg = 0x3000u16;
    let table_off = 0x0000u16;
    let table_addr = Memory::seg_offset_to_linear(table_seg, table_off);
    for i in 0..16 {
        memory.write_u8(table_addr + i, (i + 1) as u8);
    }
    memory.write_u8(table_addr + 16, 0x42); // border

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02);
    cpu.regs.es = table_seg;
    cpu.regs.set_dx(table_off);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x11], 0x42);
}

#[test]
fn test_int10_select_display_page_graphics() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 13h (graphics mode)
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Now select page
    cpu.regs.set_ah(0x05);
    cpu.regs.set_al(0x01);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x462), 0x01);
}

#[test]
fn test_grayscale_triplet_clamping() {
    // Full white should clamp to 0x3F
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert_eq!(r, 0x3F);
    assert_eq!(g, 0x3F);
    assert_eq!(b, 0x3F);
}

#[test]
fn test_set_dcc_index() {
    let mut memory = Memory::new();
    // DCC_ENTRIES[0] is typically a known value; try setting to it
    assert!(Bios::set_dcc_index(&mut memory, 0x0000)); // Should find 0 in table
}

#[test]
fn test_toggle_blinking_enable() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    Bios::toggle_blinking(&mut memory, &mut vga, 1);
    let attr = vga.get_attribute_regs();
    assert_ne!(attr[0x10] & 0x08, 0);
    assert_ne!(memory.read_u8(0x465) & 0x20, 0);
}

#[test]
fn test_toggle_blinking_disable() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // First enable, then disable
    Bios::toggle_blinking(&mut memory, &mut vga, 1);
    Bios::toggle_blinking(&mut memory, &mut vga, 0);
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x08, 0);
    assert_eq!(memory.read_u8(0x465) & 0x20, 0);
}

#[test]
fn test_toggle_blinking_invalid() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // state > 1 should be a no-op
    Bios::toggle_blinking(&mut memory, &mut vga, 2);
}

#[test]
fn test_select_dac_page_mode_enable() {
    let mut vga = Vga::new();

    // function=0, mode=1 -> set P5,P4 enable (bit 7 of attr 0x10)
    Bios::select_dac_page(&mut vga, 0, 1);
    let attr = vga.get_attribute_regs();
    assert_ne!(attr[0x10] & 0x80, 0);
}

#[test]
fn test_select_dac_page_mode_disable() {
    let mut vga = Vga::new();

    Bios::select_dac_page(&mut vga, 0, 1); // enable
    Bios::select_dac_page(&mut vga, 0, 0); // disable
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x80, 0);
}

#[test]
fn test_select_dac_page_set_page_4color() {
    let mut vga = Vga::new();

    // With P5P4 disabled (bit 7 = 0), function=1 shifts page by 2
    Bios::select_dac_page(&mut vga, 0, 0); // disable P5P4
    Bios::select_dac_page(&mut vga, 1, 2);
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x14] & 0x0F, (2 << 2) & 0x0F);
}

#[test]
fn test_select_dac_page_set_page_16color() {
    let mut vga = Vga::new();

    // With P5P4 enabled (bit 7 = 1), function=1 keeps page as-is
    Bios::select_dac_page(&mut vga, 0, 1); // enable P5P4
    Bios::select_dac_page(&mut vga, 1, 3);
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x14] & 0x0F, 3);
}

#[test]
fn test_get_dac_page() {
    let vga = Vga::new();
    let (mode, page) = Bios::get_dac_page(&vga);
    // Default state: mode is 0 or 1
    assert!(mode <= 1);
    let _ = page; // Just exercise the path
}

#[test]
fn test_ega_ril_write_read_register_crtc() {
    let mut vga = Vga::new();

    // Write to CRTC register 0 (dx=0x00)
    Bios::ega_ril_write_register(&mut vga, 0, 0x42, 0x00);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x00);
    assert_eq!(val, 0x42);
}

#[test]
fn test_ega_ril_write_read_register_sequencer() {
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 1, 0x33, 0x08);
    let val = Bios::ega_ril_read_register(&vga, 1, 0x08);
    assert_eq!(val, 0x33);
}

#[test]
fn test_ega_ril_write_read_register_graphics() {
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 0, 0x55, 0x10);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x10);
    assert_eq!(val, 0x55);
}

#[test]
fn test_ega_ril_write_read_register_attribute() {
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 0, 0x77, 0x18);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x18);
    assert_eq!(val, 0x77);
}

#[test]
fn test_ega_ril_write_read_misc_output() {
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 0, 0xAB, 0x20); // misc output
    let val = Bios::ega_ril_read_register(&vga, 0, 0x20);
    assert_eq!(val, 0xAB);
}

#[test]
fn test_ega_ril_write_read_feature_control() {
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 0, 0x0A, 0x28); // feature control
    let val = Bios::ega_ril_read_register(&vga, 0, 0x28);
    assert_eq!(val, 0x0A);
}

#[test]
fn test_ega_ril_invalid_group() {
    let vga = Vga::new();
    let val = Bios::ega_ril_read_register(&vga, 0, 0xFF);
    assert_eq!(val, 0);
}

#[test]
fn test_ega_ril_out_of_range_register() {
    let vga = Vga::new();
    // CRTC has 25 regs; reg 30 is out of range
    let val = Bios::ega_ril_read_register(&vga, 30, 0x00);
    assert_eq!(val, 0);
}

#[test]
fn test_ega_ril_read_range() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set some CRTC regs
    Bios::ega_ril_write_register(&mut vga, 0, 0x10, 0x00);
    Bios::ega_ril_write_register(&mut vga, 1, 0x20, 0x00);

    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);

    Bios::ega_ril_read_range(&cpu, &mut memory, &vga, 0, 2, 0x00);

    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    assert_eq!(memory.read_u8(buf_addr), 0x10);
    assert_eq!(memory.read_u8(buf_addr + 1), 0x20);
}

#[test]
fn test_ega_ril_read_range_invalid_group() {
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let vga = Vga::new();

    // Invalid dx should be a no-op
    Bios::ega_ril_read_range(&cpu, &mut memory, &vga, 0, 2, 0xFF);
}

#[test]
fn test_ega_ril_read_range_misc_output() {
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let vga = Vga::new();

    // dx=0x20 is misc output (regs=0, should return early)
    Bios::ega_ril_read_range(&cpu, &mut memory, &vga, 0, 1, 0x20);
}

#[test]
fn test_ega_ril_write_range() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    memory.write_u8(buf_addr, 0xAA);
    memory.write_u8(buf_addr + 1, 0xBB);

    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);

    Bios::ega_ril_write_range(&cpu, &memory, &mut vga, 0, 2, 0x00);

    let val0 = Bios::ega_ril_read_register(&vga, 0, 0x00);
    let val1 = Bios::ega_ril_read_register(&vga, 1, 0x00);
    assert_eq!(val0, 0xAA);
    assert_eq!(val1, 0xBB);
}

#[test]
fn test_ega_ril_write_range_invalid_group() {
    let cpu = Cpu::new();
    let memory = Memory::new();
    let mut vga = Vga::new();

    Bios::ega_ril_write_range(&cpu, &memory, &mut vga, 0, 2, 0xFF);
}

#[test]
fn test_ega_ril_write_range_misc_output() {
    let cpu = Cpu::new();
    let memory = Memory::new();
    let mut vga = Vga::new();

    Bios::ega_ril_write_range(&cpu, &memory, &mut vga, 0, 1, 0x20);
}

#[test]
fn test_ega_ril_read_register_set() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    Bios::ega_ril_write_register(&mut vga, 2, 0x99, 0x00); // CRTC reg 2

    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    // Entry: dx=0x0000 (CRTC), reg=2, value=?
    memory.write_u16(buf_addr, 0x0000);
    memory.write_u8(buf_addr + 2, 0x02);
    memory.write_u8(buf_addr + 3, 0x00);

    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);

    Bios::ega_ril_read_register_set(&cpu, &mut memory, &vga, 1);

    assert_eq!(memory.read_u8(buf_addr + 3), 0x99);
}

#[test]
fn test_ega_ril_write_register_set() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    memory.write_u16(buf_addr, 0x0000); // CRTC
    memory.write_u8(buf_addr + 2, 0x03);
    memory.write_u8(buf_addr + 3, 0xCC);

    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);

    Bios::ega_ril_write_register_set(&cpu, &memory, &mut vga, 1);

    let val = Bios::ega_ril_read_register(&vga, 3, 0x00);
    assert_eq!(val, 0xCC);
}

#[test]
fn test_ega_ril_define_defaults() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    for i in 0..25 {
        memory.write_u8(buf_addr + i, (i + 1) as u8);
    }

    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);
    cpu.regs.set_dx(0x0000); // CRTC

    Bios::ega_ril_define_defaults(&cpu, &memory, &mut vga);
}

#[test]
fn test_ega_ril_revert_defaults() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // First define defaults
    let buf_seg = 0x3000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    for i in 0..25 {
        memory.write_u8(buf_addr + i, 0x11);
    }
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);
    cpu.regs.set_dx(0x0000); // CRTC
    Bios::ega_ril_define_defaults(&cpu, &memory, &mut vga);

    // Now revert
    Bios::ega_ril_revert_defaults(&mut vga, 0x0000);

    let val = Bios::ega_ril_read_register(&vga, 0, 0x00);
    assert_eq!(val, 0x11);
}

#[test]
fn test_ega_ril_revert_defaults_no_saved() {
    let mut vga = Vga::new();
    // No defaults saved -> should be no-op
    Bios::ega_ril_revert_defaults(&mut vga, 0x0000);
}

#[test]
fn test_ega_ril_revert_defaults_invalid_group() {
    let mut vga = Vga::new();
    Bios::ega_ril_revert_defaults(&mut vga, 0xFF);
}

#[test]
fn test_int15_wait_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0); // 0 microseconds

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert_eq!(result.unwrap().0, Some(0));
    assert_eq!(cpu.regs.ah(), 0x00);
}

#[test]
fn test_int15_wait_nonzero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let old_cycles = cpu.cycles;
    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(100); // 100 microseconds

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert_eq!(result.unwrap().0, Some(100));
    assert_eq!(cpu.cycles, old_cycles);
}

#[test]
fn test_int15_memory_move_too_large() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(0x8001); // > 0x8000

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_memory_move_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(0); // zero count

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_memory_move_out_of_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up GDT with source pointing beyond RAM
    let gdt_seg = 0x2000u16;
    let gdt_off = 0x0000u16;
    let gdt_addr = Memory::seg_offset_to_linear(gdt_seg, gdt_off);

    // Source descriptor at +0x10: base = 0xFFFF_0000 (way beyond RAM)
    memory.write_u16(gdt_addr + 0x10 + 2, 0x0000); // base_lo
    memory.write_u8(gdt_addr + 0x10 + 4, 0xFF); // base_mid
    memory.write_u8(gdt_addr + 0x10 + 7, 0xFF); // base_hi
                                                // Dest descriptor at +0x18: base = 0x0010_0000 (1MB, in range)
    memory.write_u16(gdt_addr + 0x18 + 2, 0x0000);
    memory.write_u8(gdt_addr + 0x18 + 4, 0x10);
    memory.write_u8(gdt_addr + 0x18 + 7, 0x00);

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(1); // 1 word
    cpu.regs.es = gdt_seg;
    cpu.regs.set_si(gdt_off);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e801_small_memory_default() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to ~1.1MB to test small memory E801 behavior
    memory.resize_ram(gp64_memory::TOTAL_SIZE);
    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0); // No memory above 16MB
}

#[test]
fn test_int15_e881_small_memory() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to ~1.1MB to test small memory E881 behavior
    memory.resize_ram(gp64_memory::TOTAL_SIZE);
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, 0);
}

#[test]
fn test_int10_video_state_get_size_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x00); // get size
    cpu.regs.set_cx(0x00); // no flags => 0 blocks

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int10_video_state_save_and_restore() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set a known video mode first
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Get buffer size
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x07);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
    let blocks = cpu.regs.bx();
    assert!(blocks > 0);

    // Save state
    let buf_seg = 0x4000u16;
    let buf_off = 0x0000u16;
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // save
    cpu.regs.set_cx(0x07);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);

    // Restore state
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(buf_off);
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // restore
    cpu.regs.set_cx(0x07);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
}

#[test]
fn test_int10_12h_subfunction_31_enable() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(0x01); // Enable

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_ne!(memory.read_u8(0x489) & 0x08, 0);
}

#[test]
fn test_int10_12h_subfunction_31_disable() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(0x00); // Disable

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_eq!(memory.read_u8(0x489) & 0x08, 0);
}

#[test]
fn test_int10_12h_subfunction_33_enable_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0x00); // Enable grayscale summing (bit cleared)

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_ne!(memory.read_u8(0x489) & 0x02, 0);
}

#[test]
fn test_int10_12h_subfunction_33_disable_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0x01); // Disable grayscale summing

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_eq!(memory.read_u8(0x489) & 0x02, 0);
}

#[test]
fn test_int10_font_user_char_height_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=11h AL=21h: user font with CX=char height
    let font_seg = 0x3000u16;
    let font_off = 0x0000u16;
    cpu.regs.es = font_seg;
    cpu.regs.set_bp(font_off);
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x21);
    cpu.regs.set_cx(0); // char_height = 0 -> should default to 8

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_int10_write_string_wraps() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set text mode
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Write a string at the edge of the screen to trigger wrapping
    let str_seg = 0x3000u16;
    let str_off = 0x0000u16;
    let str_addr = Memory::seg_offset_to_linear(str_seg, str_off);
    for i in 0..85 {
        memory.write_u8(str_addr + i, b'A');
    }

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x00); // mode 0 (cursor not updated)
    cpu.regs.set_bh(0x00); // page
    cpu.regs.set_bl(0x07); // attribute
    cpu.regs.set_cx(85); // count
    cpu.regs.set_dh(0x00); // row
    cpu.regs.set_dl(0x00); // col = 0, 85 chars > 80 cols -> wraps
    cpu.regs.es = str_seg;
    cpu.regs.set_bp(str_off);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_tick_event_wait_expired_coverage() {
    let mut bios = Bios::new();
    let mut memory = Memory::new();

    bios.event_wait_active = true;
    bios.event_wait_remaining_us = 100;
    bios.event_wait_flag_addr = 0x5000;
    memory.write_u8(0x5000, 0x00);

    bios.tick_event_wait(200, &mut memory);
    assert!(!bios.event_wait_active);
    assert_ne!(memory.read_u8(0x5000) & 0x80, 0);
}

#[test]
fn test_tick_event_wait_not_expired_coverage() {
    let mut bios = Bios::new();
    let mut memory = Memory::new();

    bios.event_wait_active = true;
    bios.event_wait_remaining_us = 1000;
    bios.event_wait_flag_addr = 0x5000;

    bios.tick_event_wait(100, &mut memory);
    assert!(bios.event_wait_active);
    assert_eq!(bios.event_wait_remaining_us, 900);
}

#[test]
fn test_tick_event_wait_inactive_coverage() {
    let mut bios = Bios::new();
    let mut memory = Memory::new();

    bios.event_wait_active = false;
    bios.tick_event_wait(100, &mut memory);
}

#[test]
fn test_update_text_metrics_zero_height() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    Bios::update_text_metrics_for_font(&mut memory, &mut vga, 0, true);
    // Should be no-op
}

#[test]
fn test_update_text_metrics_no_adjust() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    Bios::update_text_metrics_for_font(&mut memory, &mut vga, 16, false);
    assert_eq!(memory.read_u8(0x485), 16);
}

#[test]
fn test_update_text_metrics_adjust_text_mode() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set text mode 3
    memory.write_u8(0x449, 0x03);

    Bios::update_text_metrics_for_font(&mut memory, &mut vga, 16, true);
    assert_eq!(memory.read_u8(0x485), 16);
}

#[test]
fn test_update_text_metrics_adjust_graphics_mode() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set graphics mode 13h - should NOT adjust rows
    memory.write_u8(0x449, 0x13);

    let old_rows = memory.read_u8(0x484);
    Bios::update_text_metrics_for_font(&mut memory, &mut vga, 16, true);
    // In graphics mode, rows should not be adjusted
    assert_eq!(memory.read_u8(0x484), old_rows);
}

#[test]
fn test_int10_functionality_state_misc_flags() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up various flags to exercise all branches in functionality state
    memory.write_u8(0x489, 0x0E); // grayscale enabled, some flags
    memory.write_u16(0x463, 0x3B4); // mono CRTC
    memory.write_u8(0x487, 0x01); // cursor emulation

    // Set attr reg 0x10 bit 3
    let mut attr = vga.get_attribute_regs();
    attr[0x10] |= 0x08;
    vga.set_attribute_regs(attr);

    let buf_seg = 0x5000u16;
    let buf_off = 0x0000u16;
    cpu.regs.es = buf_seg;
    cpu.regs.set_di(buf_off);
    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0x0000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1B);

    // Check misc flags at buffer + 0x2D
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    let misc = memory.read_u8(buf_addr + 0x2D);
    // Should have bits: 0x02 (grayscale) | 0x04 (mono) | 0x08 (flag 0x489 bit 3) | 0x10 (0x487 bit 0) | 0x20 (attr bit 3)
    assert_ne!(misc & 0x02, 0); // grayscale
    assert_ne!(misc & 0x04, 0); // mono CRTC
    assert_ne!(misc & 0x10, 0); // cursor emulation
    assert_ne!(misc & 0x20, 0); // attr blinking
}

#[test]
fn test_font_blocks_from_spec() {
    let (p, s) = Bios::font_blocks_from_spec(0x00);
    assert_eq!(p, 0);
    assert_eq!(s, 0);

    let (p, s) = Bios::font_blocks_from_spec(0x31); // bits: 0b00110001
    assert_eq!(p, 0x01 | 0x04); // bit 4 set -> primary |= 4
    assert_eq!(s, 0x04); // bit 5 set -> secondary |= 4
}

#[test]
fn test_cga_mode_control_from_table_valid() {
    let result = Bios::cga_mode_control_from_table(0x03);
    assert!(result.is_some());
}

#[test]
fn test_cga_mode_control_from_table_out_of_range() {
    let result = Bios::cga_mode_control_from_table(0x08);
    assert!(result.is_none());
}

#[test]
fn test_load_font_to_plane2_block_all() {
    let mut memory = Memory::new();
    let font = vec![0xAA; 256 * 16]; // 256 chars, 16 high
    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 0, 0, 256);

    let plane2 = memory.plane_mut(2);
    // Check first char
    assert_eq!(plane2[0], 0xAA);
    // Check padding (row 16-31 should be 0)
    assert_eq!(plane2[16], 0);
}

#[test]
fn test_load_user_font_to_plane2_edge() {
    let mut memory = Memory::new();
    let src_addr = 0x5000;
    for i in 0..16 {
        memory.write_u8(src_addr + i, 0xBB);
    }

    Bios::load_user_font_to_plane2(&mut memory, src_addr, 1, 16, 0, 0);

    let plane2 = memory.plane_mut(2);
    assert_eq!(plane2[0], 0xBB);
}
/// Helper: set up BDA COM port entries and return a Bios + Memory + SerialPorts
/// ready for INT 14h testing.
fn int14_test_setup() -> (Bios, Cpu, Memory, gp64_hw::serial::SerialPorts) {
    let bios = Bios::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let serial = gp64_hw::serial::SerialPorts::new();

    // Write COM port base addresses to BDA.
    bios.init_ivt(&mut memory, false);

    (bios, cpu, memory, serial)
}

/// Helper to set up GDT for INT 15h AH=87h
fn setup_int15_87_gdt(memory: &mut Memory, gdt_addr: usize, src_base: u32, dst_base: u32) {
    // Source descriptor at GDT+0x10
    memory.write_u16(gdt_addr + 0x10 + 2, (src_base & 0xFFFF) as u16); // base_lo
    memory.write_u8(gdt_addr + 0x10 + 4, ((src_base >> 16) & 0xFF) as u8); // base_mid
    memory.write_u8(gdt_addr + 0x10 + 7, ((src_base >> 24) & 0xFF) as u8); // base_hi

    // Destination descriptor at GDT+0x18
    memory.write_u16(gdt_addr + 0x18 + 2, (dst_base & 0xFFFF) as u16);
    memory.write_u8(gdt_addr + 0x18 + 4, ((dst_base >> 16) & 0xFF) as u8);
    memory.write_u8(gdt_addr + 0x18 + 7, ((dst_base >> 24) & 0xFF) as u8);
}
