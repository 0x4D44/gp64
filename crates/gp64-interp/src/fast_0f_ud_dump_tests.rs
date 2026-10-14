use super::test_helpers::setup;
use super::*;
use std::path::{Path, PathBuf};
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn dump_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.display(), suffix))
}
fn dump_path(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "tmp/fast_0f_ud_dump_{name}_{}.bin",
        std::process::id()
    ))
}
fn cleanup_dump(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(dump_sidecar_path(path, ".txt"));
    let _ = fs::remove_file(dump_sidecar_path(path, ".ring.txt"));
}
fn reset_fast_0f_ud_dump_test_state() {
    *FAST_0F_UD_DUMP_TEST_OWNER_THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::thread::current().id());
    FAST_0F_UD_DUMP_TEST_THREAD_ACTIVE.with(|active| active.set(true));
    FAST_0F_UD_DUMPED.store(false, Ordering::SeqCst);
    INTERP_STEP_TRACE_STATE.clear_backlog_for_test();
}
fn assert_unhandled_0f_ud(result: RunResult, expected_executed: u32) {
    match result {
        RunResult::Exception {
            vector,
            error_code,
            fault_addr,
            executed,
        } => {
            assert_eq!(vector, 6);
            assert_eq!(error_code, 0);
            assert_eq!(fault_addr, None);
            assert_eq!(executed, expected_executed);
        }
        other => panic!("expected unhandled 0F #UD exception, got {other:?}"),
    }
}
#[test]
fn fast_0f_ud_dump_disabled_preserves_unhandled_0f_exception() {
    let _lock = env_lock();
    let path = dump_path("disabled_unhandled_0f");
    cleanup_dump(&path);
    reset_fast_0f_ud_dump_test_state();
    let (mut cpu, mut mem) = setup(&[0x0F, 0x0E, 0xCC]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_unhandled_0f_ud(result, 0);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert!(!path.exists());
    assert!(!dump_sidecar_path(&path, ".txt").exists());
    cleanup_dump(&path);
}
#[test]
fn fast_0f_ud_dump_enabled_writes_unhandled_0f_dump() {
    let _lock = env_lock();
    let path = dump_path("enabled_unhandled_0f");
    cleanup_dump(&path);
    reset_fast_0f_ud_dump_test_state();
    let (mut cpu, mut mem) = setup(&[0x0F, 0x0E, 0xCC]);
    mem.write_u16(6 * 4, 0x0118);
    mem.write_u16(6 * 4 + 2, 0xF000);
    cpu.regs.eax = 0x1234_5678;
    cpu.regs.ebx = 0x9ABC_DEF0;
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_unhandled_0f_ud(result, 0);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert!(!path.exists());
    assert!(!dump_sidecar_path(&path, ".txt").exists());
    assert!(!dump_sidecar_path(&path, ".ring.txt").exists());
    cleanup_dump(&path);
}
#[test]
fn fast_0f_ud_dump_ring_snapshots_unhandled_0f_backlog_without_draining() {
    let _lock = env_lock();
    let path = dump_path("ring_unhandled_0f");
    cleanup_dump(&path);
    reset_fast_0f_ud_dump_test_state();
    let (mut cpu, mut mem) = setup(&[0x90, 0x90, 0x0F, 0x0E, 0xCC]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert_unhandled_0f_ud(result, 2);
    assert_eq!(cpu.regs.eip, 0x0102);
    assert!(!path.exists());
    assert!(!dump_sidecar_path(&path, ".ring.txt").exists());
    assert!(!dump_sidecar_path(&path, ".txt").exists());
    cleanup_dump(&path);
}
