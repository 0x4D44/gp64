use gp64_pit::Pit;

#[test]
fn machine_neutral_controller_starts_stopped_until_programmed() {
    let pit = Pit::new_unprogrammed(1_000_000, 33_000_000);

    for channel in 0..3 {
        assert!(!pit.get_loaded(channel));
        assert!(!pit.get_counting(channel));
    }
    assert_eq!(pit.pit_hz(), 1_000_000);
    assert_eq!(pit.emulated_ips(), 33_000_000);
}
