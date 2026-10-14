use super::*;
use std::path::PathBuf;

#[test]
fn subst_create_maps_an_unmounted_letter_and_delete_removes_it() {
    let mut dos = Dos::new();
    assert!(!dos.drives.contains_key(&0), "A: must start unmounted");

    dos.subst_create(0, PathBuf::from(".")).expect("subst a:");
    assert!(dos.drives.contains_key(&0));
    assert!(dos.subst_drives.contains(&0));

    dos.subst_delete(0).expect("subst a: /d");
    assert!(!dos.drives.contains_key(&0));
    assert!(!dos.subst_drives.contains(&0));
}

#[test]
fn subst_create_refuses_mounted_letters_double_subst_and_bad_index() {
    let mut dos = Dos::new();
    // C: (index 2) is a real mount made by Dos::new.
    assert_eq!(
        dos.subst_create(2, PathBuf::from(".")).unwrap_err(),
        "Invalid parameter - C:"
    );

    dos.subst_create(0, PathBuf::from(".")).expect("subst a:");
    assert_eq!(
        dos.subst_create(0, PathBuf::from(".")).unwrap_err(),
        "Drive already SUBSTed - A:"
    );

    assert!(dos.subst_create(26, PathBuf::from(".")).is_err());
}

#[test]
fn subst_delete_refuses_real_mounts_and_the_current_drive() {
    let mut dos = Dos::new();
    // C: is a real mount, not a SUBST mapping — the launcher idiom
    // `subst c: /d` must fail without unmounting anything.
    assert_eq!(dos.subst_delete(2).unwrap_err(), "Invalid parameter - C:");
    assert!(
        dos.drives.contains_key(&2),
        "a real mount must survive subst /d"
    );

    // The current drive cannot be deleted even when SUBSTed.
    dos.subst_create(0, PathBuf::from(".")).expect("subst a:");
    dos.path_resolver.set_current_drive(0);
    assert!(dos.subst_delete(0).is_err());
    assert!(dos.drives.contains_key(&0));

    dos.path_resolver.set_current_drive(2);
    dos.subst_delete(0).expect("subst a: /d");
}

#[test]
fn subst_mappings_lists_created_mappings_with_host_roots() {
    let mut dos = Dos::new();
    assert!(dos.subst_mappings().is_empty());

    dos.subst_create(3, std::env::temp_dir()).expect("subst d:");
    let maps = dos.subst_mappings();
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].0, 3);
}
