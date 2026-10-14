use gp64_rtc::Mc146818;

#[test]
fn fixed_epoch_uses_only_guest_elapsed_time() {
    // Also reject an incidental host-clock read whose result is discarded.
    let source = include_str!("../src/lib.rs");
    assert!(
        !source.contains("SystemTime"),
        "chip must never read the host clock"
    );

    let mut rtc = Mc146818::new(0);
    assert_eq!(rtc.diagnostic_snapshot().startup_epoch_secs, 0);
    assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 0));
    assert_eq!(rtc.get_rtc_date_bcd(), (0x19, 0x70, 0x01, 0x01));
    assert!(rtc.nvram()[0x0e..0x32].iter().all(|&byte| byte == 0));
    rtc.tick(1_999_999);
    assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 1));
    rtc.tick(1);
    assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 2));

    for register in [0x32, 0x37] {
        rtc.write_index(register);
        rtc.write_data(0xa5);
        assert_eq!(rtc.read_data(), 0xa5);
        assert_eq!(rtc.get_rtc_date_bcd(), (0x19, 0x70, 0x01, 0x01));
    }
    rtc.write_index(0x8a);
    assert_eq!(rtc.read_index(), 0x8a);
    assert_eq!(rtc.read_data(), 0xff);

    let rtc = Mc146818::new(946_684_799);
    assert_eq!(rtc.get_rtc_time_bcd(), (0x23, 0x59, 0x59));
    assert_eq!(rtc.get_rtc_date_bcd(), (0x19, 0x99, 0x12, 0x31));
}

fn write(rtc: &mut Mc146818, reg: u8, value: u8) {
    rtc.write_index(reg);
    rtc.write_data(value);
}

#[test]
fn divider_reset_stops_calendar_flags_and_uip() {
    for reset in [0x66, 0x76] {
        let mut rtc = Mc146818::new(86_399);
        rtc.advance_clock(999_800);
        write(&mut rtc, 0x0b, 0x72);
        for alarm in [1, 3, 5] {
            write(&mut rtc, alarm, 0xc0);
        }
        write(&mut rtc, 0x0a, reset);
        rtc.write_index(0x0c);
        rtc.read_data();
        let calendar = rtc.get_rtc_time_bcd();
        let date = rtc.get_rtc_date_bcd();
        for _ in 0..5 {
            assert!(!rtc.tick(1_000_000), "divider reset must not assert IRQ");
            assert_eq!(
                rtc.get_rtc_time_bcd(),
                calendar,
                "divider reset must freeze calendar"
            );
            assert_eq!(rtc.get_rtc_date_bcd(), date);
            assert_eq!(rtc.peek_pending_flags(), 0);
            rtc.write_index(0x0a);
            assert_eq!(rtc.read_data(), reset, "raw DV bits survive with UIP clear");
            assert_eq!(rtc.get_periodic_frequency_hz(), None);
        }
        assert_eq!(rtc.diagnostic_snapshot().emulated_us, 5_999_800);
    }
}

#[test]
fn divider_reset_release_restarts_half_second_phase() {
    for reset in [0x66, 0x76] {
        let mut rtc = Mc146818::new(0);
        rtc.advance_clock(123_456);
        write(&mut rtc, 0x0a, reset);
        rtc.advance_clock(3_234_567);
        write(&mut rtc, 0x0a, 0x26);
        assert_eq!(
            rtc.get_rtc_time_bcd(),
            (0, 0, 0),
            "release preserves frozen calendar"
        );
        rtc.advance_clock(499_755);
        rtc.write_index(0x0a);
        assert_eq!(rtc.read_data() & 0x80, 0);
        rtc.advance_clock(1);
        assert_eq!(rtc.read_data() & 0x80, 0x80);
        rtc.advance_clock(243);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 0));
        rtc.advance_clock(1);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 1));
        assert_eq!(rtc.peek_pending_flags() & 0x10, 0x10);
        rtc.advance_clock(999_999);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 1));
        rtc.advance_clock(1);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 2));
    }
}

#[test]
fn divider_reset_set_overlap_and_checkpoint_preserve_phase() {
    for reset_first in [false, true] {
        let mut rtc = Mc146818::new(0);
        rtc.advance_clock(234_567);
        if reset_first {
            write(&mut rtc, 0x0a, 0x66);
        }
        write(&mut rtc, 0x0b, 0xc2);
        if !reset_first {
            write(&mut rtc, 0x0a, 0x66);
        }
        write(&mut rtc, 0x00, 0x17);
        rtc.advance_clock(3_000_000);
        write(&mut rtc, 0x0a, 0x76);
        let state = rtc.save_state();
        let mut restored = Mc146818::new(0);
        restored.restore_state(state);
        write(&mut restored, 0x0b, 0x42);
        restored.advance_clock(2_000_000);
        assert_eq!(
            restored.get_rtc_time_bcd(),
            (0, 0, 0x17),
            "SET clear must not release divider freeze"
        );
        write(&mut restored, 0x0a, 0x26);
        restored.advance_clock(499_999);
        assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 0x17));
        restored.advance_clock(1);
        assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 0x18));
        // Release the divider with SET still asserted, then release SET midphase.
        write(&mut rtc, 0x0a, 0x26);
        assert!(rtc.tick(200_000), "SET must allow PF after divider release");
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 0x17));
        write(&mut rtc, 0x0b, 0x42);
        rtc.advance_clock(299_999);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 0x17));
        rtc.advance_clock(1);
        assert_eq!(rtc.get_rtc_time_bcd(), (0, 0, 0x18));
    }
}

#[test]
fn divider_reset_preserves_pending_flags_and_restored_release_phase() {
    let mut rtc = Mc146818::new(86_399);
    write(&mut rtc, 0x0b, 0x72);
    rtc.tick(1234);
    let pending = rtc.peek_pending_flags();
    assert_eq!(pending, 0xc0);
    write(&mut rtc, 0x0a, 0x66);
    rtc.tick(3_000_000);
    assert_eq!(rtc.peek_pending_flags(), pending);
    let weekday = rtc.diagnostic_snapshot().adjusted_day_of_week;
    assert_eq!(
        rtc.get_rtc_time_bcd(),
        (0x23, 0x59, 0x59),
        "reset holds calendar with pending flags"
    );
    write(&mut rtc, 0x0a, 0x26);
    rtc.advance_clock(100_000);
    let state = rtc.save_state();
    let mut restored = Mc146818::new(86_399);
    restored.restore_state(state);
    restored.advance_clock(399_999);
    assert_eq!(restored.get_rtc_time_bcd(), (0x23, 0x59, 0x59));
    restored.advance_clock(1);
    assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 0));
    assert_eq!(
        restored.diagnostic_snapshot().adjusted_day_of_week,
        weekday % 7 + 1
    );
    let mut legacy = Mc146818::new(0);
    legacy.advance_clock(1_250_000);
    let mut state = legacy.save_state();
    state.calendar_clock_us = None;
    restored = Mc146818::new(0);
    restored.restore_state(state);
    assert_eq!(restored.get_rtc_time_bcd(), legacy.get_rtc_time_bcd());
}
