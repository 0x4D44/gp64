//! MDD-BUG-CRUCIBLE-00380: every INT 21h AH=3Dh open of a registered
//! virtual file used to move a fresh full copy of the payload into the
//! cost N copies. Lookups now hand out a shared `Arc`.

use super::Dos;
use std::sync::Arc;

#[test]
fn every_lookup_of_a_registered_virtual_file_shares_one_allocation() {
    let mut dos = Dos::new();
    let payload: Arc<[u8]> = Arc::from(vec![0x4Du8, 0x5A, 0x90, 0x00]);
    dos.register_virtual_file(r"C:\TESTDIR\MAIN.EXE", payload.clone());

    let first = dos
        .get_virtual_file_bytes(2, r"\TESTDIR\MAIN.EXE")
        .expect("registered path resolves");
    let second = dos
        .get_virtual_file_bytes(2, r"\TESTDIR\MAIN.EXE")
        .expect("a second open resolves too");

    assert!(
        Arc::ptr_eq(&first, &second),
        "two opens must see one allocation, not two copies"
    );
    assert!(
        Arc::ptr_eq(&first, &payload),
        "and it must be the allocation that was registered"
    );
    assert_eq!(first.as_ref(), [0x4D, 0x5A, 0x90, 0x00]);
}

/// Registering from an owned `Vec` still works — the one copy happens at
/// REGISTRATION, which is once per program, not once per open.
#[test]
fn registering_an_owned_vec_still_shares_it_across_opens() {
    let mut dos = Dos::new();
    dos.register_virtual_file(r"C:\GAME.EXE", vec![1u8, 2, 3]);

    let first = dos.get_virtual_file_bytes(2, r"\GAME.EXE").expect("open");
    let second = dos.get_virtual_file_bytes(2, r"\GAME.EXE").expect("reopen");

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.as_ref(), [1, 2, 3]);
}
