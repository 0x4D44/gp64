use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn projection_outcome(result: Result<u64, Box<dyn std::any::Any + Send>>) -> String {
    match result {
        Ok(deadline) => format!("returned deadline {deadline}"),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            format!("panicked: {message}")
        }
    }
}

#[test]
fn timer_expiry_at_two_clocks() {
    for clock in [1_000_000, 20_000_000] {
        for (register, start, micros, status) in [(2, 1, 80, 0xC0), (3, 2, 320, 0xA0)] {
            let mut chip = Opl2::new_with_clock(clock);
            chip.write(0, register, 0xFE);
            chip.write(0, 4, start);
            let expiry = clock * micros * 2 / 1_000_000;
            assert_eq!(chip.read_status(expiry - 1), 0);
            assert_eq!(chip.read_status(expiry), status);
        }
    }
}

#[test]
fn repeated_advance_is_idempotent() {
    let mut chip = Opl2::new_with_clock(1_000_000);
    chip.write(0, 2, 0xFF);
    chip.write(0, 4, 1);
    for _ in 0..20 {
        chip.advance_to(79);
    }
    assert_eq!(chip.read_status(79), 0);
    assert_eq!(chip.read_status(80), 0xC0);
    let mut output = Vec::new();
    chip.render_until(80, &mut output);
    assert_eq!(output.len(), 3);
}

#[test]
fn masks_clear_flags_without_stopping_counters() {
    let mut chip = Opl2::new_with_clock(1_000_000);
    chip.write(0, 2, 0xFF);
    chip.write(0, 3, 0xFF);
    chip.write(0, 4, 3);
    assert_eq!(chip.read_status(320), 0xE0);
    chip.write(320, 4, 0x43);
    assert_eq!(chip.read_status(400), 0xA0);
    chip.write(400, 4, 0x63);
    assert_eq!(chip.read_status(700), 0);
    chip.write(700, 4, 3);
    assert_eq!(chip.read_status(719), 0);
    assert_eq!(chip.read_status(720), 0xC0);
    assert_eq!(chip.read_status(960), 0xE0);
}

#[test]
fn irq_reset_ignores_other_control_bits() {
    let mut chip = Opl2::new_with_clock(1_000_000);
    chip.write(0, 2, 0xFF);
    chip.write(0, 3, 0xFF);
    chip.write(0, 4, 3);
    assert_eq!(chip.read_status(320), 0xE0);
    chip.write(350, 4, 0xE0);
    assert_eq!(chip.read_status(350), 0);
    assert_eq!(chip.read_status(400), 0xC0);
    assert_eq!(chip.read_status(640), 0xE0);
}

#[test]
fn preset_reload_stop_and_restart() {
    for (register, start, step, status) in [(2, 1, 80, 0xC0), (3, 2, 320, 0xA0)] {
        let mut chip = Opl2::new_with_clock(1_000_000);
        chip.write(0, register, 0xFE);
        chip.write(0, 4, start);
        chip.write(step, register, 0xFC);
        chip.write(step, 4, start);
        assert_eq!(chip.read_status(2 * step - 1), 0);
        assert_eq!(chip.read_status(2 * step), status);
        chip.write(2 * step, 4, 0x80);
        assert_eq!(chip.read_status(6 * step - 1), 0);
        assert_eq!(chip.read_status(6 * step), status);
        chip.write(6 * step, 4, 0);
        chip.write(6 * step, 4, 0x80);
        assert_eq!(chip.read_status(10 * step), 0);
        chip.write(10 * step, register, 0xFC);
        chip.write(10 * step, 4, start);
        assert_eq!(chip.read_status(14 * step - 1), 0);
        assert_eq!(chip.read_status(14 * step), status);
    }
}

#[test]
fn status_low_bits_are_zero() {
    let mut chip = Opl2::new_with_clock(1_000_000);
    chip.write(0, 2, 0xFF);
    chip.write(0, 3, 0xFF);
    chip.write(0, 4, 3);
    for cycle in 0..1000 {
        assert_eq!(chip.read_port(cycle, 0) & 0x1F, 0);
    }
}

#[test]
fn detection_with_one_microsecond_in_loop() {
    for clock in [1_000_000, 20_000_000] {
        let io_cycle = clock / 1_000_000;
        let mut chip = Opl2::new_with_clock(clock);
        chip.write_port(0, 0, 4);
        chip.write_port(io_cycle, 1, 0x60);
        chip.write_port(2 * io_cycle, 1, 0x80);
        assert_eq!(chip.read_port(3 * io_cycle, 0), 0);
        chip.write_port(4 * io_cycle, 0, 2);
        chip.write_port(5 * io_cycle, 1, 0xFF);
        chip.write_port(6 * io_cycle, 0, 4);
        chip.write_port(7 * io_cycle, 1, 1);
        for delay in 1..80 {
            assert_eq!(chip.read_port((7 + delay) * io_cycle, 0), 0);
        }
        assert_eq!(chip.read_port(87 * io_cycle, 0), 0xC0);
        chip.write_port(88 * io_cycle, 1, 0x80);
        assert_eq!(chip.read_port(89 * io_cycle, 0), 0);
    }
}

#[test]
fn short_note_renders_in_its_sample_window() {
    let mut chip = Opl2::new_with_clock(u64::from(SAMPLE_RATE));
    for (register, value) in [
        (0x20, 0x21),
        (0x23, 0x21),
        (0x40, 0x3F),
        (0x60, 0xF0),
        (0x63, 0xF0),
        (0x83, 0x0F),
        (0xA0, 0),
        (0xB0, 0x12),
    ] {
        chip.write(0, register, value);
    }
    chip.write(100, 0xB0, 0x32);
    chip.write(300, 0xB0, 0x12);
    let mut output = Vec::new();
    chip.render_until(1024, &mut output);
    assert_eq!(output.len(), 1024);
    assert!(output[..100].iter().all(|&s| s == 0));
    assert!(output[100..300].iter().any(|&s| s != 0));
    // Release is not instantaneous; compare against an identically timed held key.
    let mut held = Opl2::new_with_clock(u64::from(SAMPLE_RATE));
    for (register, value) in [
        (0x20, 0x21),
        (0x23, 0x21),
        (0x40, 0x3F),
        (0x60, 0xF0),
        (0x63, 0xF0),
        (0x83, 0x0F),
        (0xA0, 0),
        (0xB0, 0x12),
    ] {
        held.write(0, register, value);
    }
    held.write(100, 0xB0, 0x32);
    let mut held_output = Vec::new();
    held.render_until(1024, &mut held_output);
    assert_eq!(output[..300], held_output[..300]);
    assert_ne!(output[300..], held_output[300..]);
    assert!(output[800..].iter().all(|&s| s == 0));
}

#[test]
fn fractional_samples_and_timer_ticks_do_not_drift() {
    for clock in [1_000_003, 20_000_003] {
        let mut chip = Opl2::new_with_clock(clock);
        chip.write(0, 2, 0xFF);
        chip.write(0, 4, 1);
        let expiry = (clock * 80).div_ceil(1_000_000);
        for cycle in 1..expiry {
            assert_eq!(chip.read_status(cycle), 0);
        }
        assert_eq!(chip.read_status(expiry), 0xC0);
        let mut output = Vec::new();
        let end = clock * 30 + 123;
        let mut cycle = expiry;
        while cycle < end {
            cycle = (cycle + 7919).min(end);
            chip.render_until(cycle, &mut output);
        }
        assert_eq!(
            output.len() as u128,
            u128::from(end) * u128::from(SAMPLE_RATE) / u128::from(clock)
        );
    }
}

#[test]
fn clock_change_preserves_fractional_phase() {
    let mut chip = Opl2::new_with_clock(1_000_000);
    chip.write(0, 2, 0xFF);
    chip.write(0, 4, 1);
    chip.set_clock(39, 20_000_000);
    assert_eq!(chip.next_sample_cycle(), 64);
    assert_eq!(chip.read_status(39 + 41 * 20 - 1), 0);
    assert_eq!(chip.read_status(39 + 41 * 20), 0xC0);
    let mut output = Vec::new();
    chip.render_until(39 + 961 * 20, &mut output);
    assert_eq!(output.len(), 49);
}

#[test]
fn sample_deadline_projection_checks_numerator_product() {
    for (clock, deadline) in [
        (1, 1),
        (u64::from(SAMPLE_RATE), 1),
        (1_000_000, 21),
        (20_000_000, 403),
        (u64::MAX, 371_042_402_319_366),
    ] {
        assert_eq!(Opl2::new_with_clock(clock).next_sample_cycle(), deadline);
    }

    let mut chip = Opl2::new_with_clock(1_u64 << 63);
    chip.advance_to(1);
    chip.set_clock(1, (1_u64 << 63) - 1);
    chip.advance_to(2);

    let outcome = projection_outcome(catch_unwind(AssertUnwindSafe(|| chip.next_sample_cycle())));
    assert_eq!(
        outcome, "panicked: OPL2 sample deadline numerator overflow",
        "an unrepresentable numerator projection must panic descriptively"
    );
}

#[test]
fn sample_deadline_projection_checks_denominator_product() {
    let denominator = u128::MAX / u128::from(SAMPLE_RATE) + 1;
    let mut chip = Opl2::new_with_clock(2);
    chip.audio_phase = Phase {
        numerator: denominator - 1,
        denominator,
    };

    let outcome = projection_outcome(catch_unwind(AssertUnwindSafe(|| chip.next_sample_cycle())));
    assert_eq!(
        outcome, "panicked: OPL2 sample deadline denominator overflow",
        "an unrepresentable denominator projection must panic descriptively"
    );
}

#[test]
#[should_panic(expected = "monotonic")]
fn backwards_cycle_is_rejected() {
    let mut chip = Opl2::new();
    chip.advance_to(10);
    chip.write(9, 2, 0);
}

#[test]
fn timestamped_eg_type_write_renders_old_audio_then_fades() {
    let mut chip = Opl2::new_with_clock(u64::from(SAMPLE_RATE));
    for (register, value) in [
        (0x20, 0x01),
        (0x23, 0x21),
        (0x40, 0x3F),
        (0x43, 0x00),
        (0x60, 0xF0),
        (0x63, 0xF0),
        (0x80, 0x0F),
        (0x83, 0x0F),
        (0xA0, 0x00),
        (0xB0, 0x32),
    ] {
        chip.write_port(0, 0, register);
        chip.write_port(0, 1, value);
    }

    let mut before_change = Vec::new();
    chip.render_until(4_096, &mut before_change);
    assert!(
        before_change.iter().any(|sample| *sample != 0),
        "the sustained note must render before the EG-TYPE write"
    );

    let mut unchanged = chip.clone();
    let mut expected_old_sample = Vec::new();
    unchanged.render_until(4_097, &mut expected_old_sample);
    assert_eq!(expected_old_sample.len(), 1);
    assert_ne!(
        expected_old_sample[0], 0,
        "the ordering sample must be audible"
    );

    chip.write_port(4_096, 0, 0x23);
    chip.write_port(4_097, 1, 0x01);
    let mut timestamped_old_audio = Vec::new();
    chip.drain_samples(&mut timestamped_old_audio);
    assert_eq!(
        timestamped_old_audio, expected_old_sample,
        "the data-port timestamp must render its sample before applying EG-TYPE"
    );

    let mut after_change = Vec::new();
    chip.render_until(12_289, &mut after_change);
    assert_eq!(after_change.len(), 8_192);
    let early_peak = after_change[..1_024]
        .iter()
        .map(|sample| i32::from(*sample).abs())
        .max()
        .unwrap_or(0);
    let terminal_tail = &after_change[after_change.len() - 1_024..];
    assert!(
        early_peak > 0,
        "the percussive fade must remain audible just after the timestamped EG-TYPE write"
    );
    assert!(
        terminal_tail.iter().all(|sample| *sample == 0),
        "the percussive fade must reach silence while KON stays set"
    );
}
