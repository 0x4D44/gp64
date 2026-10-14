//! `RamBacking` oracles (HLD §10 Oracle 2, U2 rows): Vec-equivalent resize
//! behaviour, sparse indexing properties, and the guarded gap.

use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Reference model: the `Vec<u8>` behaviour `RamBacking` must reproduce.
fn model(capacity: usize, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; capacity];
    v.truncate(len);
    v
}

/// Every backed byte, extent by extent.
fn contents(ram: &RamBacking) -> Vec<u8> {
    ram.backed_extents().flat_map(|e| ram[e].to_vec()).collect()
}

fn fill_backed(ram: &mut RamBacking, value: u8) {
    let extents: Vec<_> = ram.backed_extents().collect();
    for e in extents {
        ram[e].fill(value);
    }
}

fn panics(f: impl FnOnce()) -> bool {
    catch_unwind(AssertUnwindSafe(f)).is_err()
}

#[test]
fn construction_matches_vec_length_capacity_and_zero_fill() {
    let ram = RamBacking::zeroed(0x40_0000, 0x10_0010);
    let reference = model(0x40_0000, 0x10_0010);
    assert_eq!(ram.len(), reference.len());
    assert_eq!(ram.capacity(), reference.capacity());
    assert!(contents(&ram).iter().all(|&b| b == 0));
}

#[test]
fn resize_sequence_matches_vec_including_zero_fill_on_regrowth() {
    let mut ram = RamBacking::zeroed(0x2_0000, 0x1_0000);
    let mut reference = model(0x2_0000, 0x1_0000);
    let base = ram.host_base();
    // Dirty the whole RAM, shrink, regrow: Vec re-zeroes the regrown tail.
    fill_backed(&mut ram, 0xA5);
    reference.fill(0xA5);
    for (len, value) in [
        (0x8000, 0),
        (0x1_8000, 0),
        (0x1_0000, 0),
        (0x1_4000, 0x3C),
        (0x0100, 0),
        (0x2_0000, 0),
        // Over-capacity growth: capacity doubles and the address stays.
        (0x2_0001, 0),
        (0x9_0000, 0),
    ] {
        ram.resize(len, value);
        reference.resize(len, value);
        assert_eq!(ram.len(), reference.len(), "len after resize to {len:#x}");
        assert_eq!(
            ram.capacity(),
            reference.capacity(),
            "capacity after resize to {len:#x}"
        );
        assert!(
            contents(&ram) == reference,
            "contents after resize to {len:#x}"
        );
        fill_backed(&mut ram, 0x5A);
        reference.fill(0x5A);
    }
    if ram.stable_reservation_base().is_some() {
        assert_eq!(ram.host_base(), base, "the reservation never moves");
    }
}

#[cfg(target_pointer_width = "64")]
#[test]
fn growth_to_max_ram_keeps_the_address_and_writes_land() {
    let mut ram = RamBacking::zeroed(0x100_0000, 0x10_0000);
    let base = ram
        .stable_reservation_base()
        .expect("stable on 64-bit hosts");
    assert_eq!(base, ram.host_base());
    let top = crate::MAX_RAM_SIZE + crate::FETCH_PADDING;
    ram.resize(top, 0);
    assert_eq!(ram.host_base(), base);
    ram[top - 1] = 0x77;
    ram[0x10_0000] = 0x66;
    assert_eq!(ram[top - 1], 0x77);
    assert_eq!(ram[0x10_0000], 0x66);
    assert_eq!(ram[top - 2], 0);
}

/// Offsets that matter: both edges of private RAM, both gap edges, the arena
/// start and end, the reservation end, and overflow values.
fn probe_points(ram: &RamBacking) -> Vec<usize> {
    let len = ram.len();
    let reservation = ram.reservation_len();
    let mut points = vec![
        0,
        1,
        len / 2,
        len - 1,
        len,
        len + 1,
        len + 0x1000,
        ram.capacity(),
        PRIVATE_WINDOW_BYTES - 1,
        PRIVATE_WINDOW_BYTES,
        PRIVATE_WINDOW_BYTES + 1,
        reservation - 1,
        reservation,
        reservation + 1,
        usize::MAX - 1,
        usize::MAX,
    ];
    // Deterministic pseudo-random offsets across the reservation.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..24 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let span = (reservation as u64).saturating_add(0x100);
        points.push((x % span) as usize);
        // Near the private end too, where off-by-ones live.
        points.push(len.saturating_sub(0x40) + (x % 0x80) as usize);
    }
    points
}

/// Oracle "sparse indexing properties": for arbitrary ranges, a reference is
/// built only when the whole range is backed, and it covers exactly the range.
#[test]
fn sparse_indexing_builds_a_reference_only_for_wholly_backed_ranges() {
    let ram = RamBacking::zeroed(0x2_0000, 0x1_0010);
    let len = ram.len();
    let base = ram.host_base() as usize;
    let points = probe_points(&ram);
    for &a in &points {
        // Single byte.
        assert_eq!(ram.get(a).is_some(), a < len, "ram.get({a:#x})");
        if let Some(byte) = ram.get(a) {
            assert_eq!(byte as *const u8 as usize, base + a);
        }
        // `a..`: to the end of the extent holding `a`, never past it.
        match ram.get(a..) {
            Some(tail) => {
                assert!(a <= len, "ram.get({a:#x}..) built a reference");
                assert_eq!(tail.as_ptr() as usize, base + a);
                assert_eq!(tail.len(), len - a);
            }
            None => assert!(a > len, "ram.get({a:#x}..) refused a backed start"),
        }
        for &b in &points {
            let whole = a <= b && b <= len;
            match ram.get(a..b) {
                Some(s) => {
                    assert!(whole, "ram.get({a:#x}..{b:#x}) built a reference");
                    assert_eq!(s.as_ptr() as usize, base + a);
                    assert_eq!(s.len(), b - a);
                }
                None => assert!(!whole, "ram.get({a:#x}..{b:#x}) refused a backed range"),
            }
            assert_eq!(ram.is_backed(a..b), whole);
            assert_eq!(ram.ptr_for(a..b).is_some(), whole);
            let inclusive = b < usize::MAX && a <= b + 1 && b < len;
            match ram.get(a..=b) {
                Some(s) => {
                    assert!(inclusive, "ram.get({a:#x}..={b:#x}) built a reference");
                    assert_eq!(s.len(), b + 1 - a);
                }
                None => assert!(!inclusive, "ram.get({a:#x}..={b:#x}) refused"),
            }
        }
        assert_eq!(ram.get(..a).is_some(), a <= len, "ram.get(..{a:#x})");
        assert_eq!(ram.get(..=a).is_some(), a < len, "ram.get(..={a:#x})");
    }
}

/// Oracle "range spanning backed extents validates as a whole": a range that
/// starts backed and ends in the gap is refused outright — indexing panics
/// rather than yielding the backed prefix.
#[test]
fn indexing_across_the_private_end_panics_as_a_whole() {
    let mut ram = RamBacking::zeroed(0x2_0000, 0x1_0010);
    let len = ram.len();
    ram[len - 1] = 0x42;
    assert_eq!(ram[len - 1], 0x42);
    assert_eq!(ram[len - 4..len], [0, 0, 0, 0x42]);
    assert!(ram[len..len].is_empty());
    assert!(ram[len..].is_empty());
    for (what, f) in [
        (
            "byte at private end",
            Box::new(|r: &mut RamBacking| {
                let _ = r[r.len()];
            }) as Box<dyn Fn(&mut RamBacking)>,
        ),
        (
            "range across private end",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                let _ = &r[l - 4..l + 4];
            }),
        ),
        (
            "mut range across private end",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                r[l - 1..l + 1].fill(1);
            }),
        ),
        (
            "inclusive range to private end",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                let _ = &r[l - 4..=l];
            }),
        ),
        (
            "first gap byte",
            Box::new(|r: &mut RamBacking| {
                let _ = r[r.len() + 1];
            }),
        ),
        (
            "last gap byte",
            Box::new(|r: &mut RamBacking| {
                let _ = r[PRIVATE_WINDOW_BYTES - 1];
            }),
        ),
        (
            "arena start",
            Box::new(|r: &mut RamBacking| {
                let _ = r[PRIVATE_WINDOW_BYTES];
            }),
        ),
        (
            "reservation end",
            Box::new(|r: &mut RamBacking| {
                let e = r.reservation_len();
                let _ = &r[e - 1..e];
            }),
        ),
        (
            "inverted range",
            Box::new(|r: &mut RamBacking| {
                let (start, end) = (8, std::hint::black_box(4));
                let _ = &r[start..end];
            }),
        ),
        (
            "inclusive overflow",
            Box::new(|r: &mut RamBacking| {
                let _ = &r[usize::MAX - 1..=usize::MAX];
            }),
        ),
        (
            "tail from the gap",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                let _ = &r[l + 1..];
            }),
        ),
        (
            "copy_within into the gap",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                r.copy_within(0..8, l - 4);
            }),
        ),
        (
            "copy_within from the gap",
            Box::new(|r: &mut RamBacking| {
                let l = r.len();
                r.copy_within(l - 4..l + 4, 0);
            }),
        ),
    ] {
        assert!(panics(|| f(&mut ram)), "{what} did not panic");
    }
    // The refused operations changed nothing.
    assert_eq!(ram[len - 1], 0x42);
    assert_eq!(ram[0..8], [0; 8]);
}

#[test]
fn copy_within_matches_slice_semantics_inside_backed_ram() {
    let mut ram = RamBacking::zeroed(0x1000, 0x1000);
    let mut reference = model(0x1000, 0x1000);
    for (i, b) in reference.iter_mut().enumerate() {
        *b = i as u8;
    }
    ram[0..0x1000].copy_from_slice(&reference);
    for (src, dest) in [(0..0x100, 0x80), (0x80..0x180, 0), (0xF00..0x1000, 0xF00)] {
        ram.copy_within(src.clone(), dest);
        reference.copy_within(src, dest);
        assert_eq!(ram[0..0x1000], reference[..]);
    }
}

#[test]
fn backed_extents_cover_exactly_the_private_ram() {
    let mut ram = RamBacking::zeroed(0x2_0000, 0x1_0010);
    assert_eq!(ram.backed_extents().collect::<Vec<_>>(), vec![0..0x1_0010]);
    ram.resize(0x3_0000, 0);
    assert_eq!(ram.backed_extents().collect::<Vec<_>>(), vec![0..0x3_0000]);
    ram.resize(0, 0);
    assert_eq!(ram.backed_extents().count(), 0);
}

/// HLD §5.2 premise for the session's retain-until-teardown commit policy.
#[cfg(windows)]
#[test]
fn sec_reserve_section_pages_cannot_be_decommitted_with_virtual_free() {
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualAlloc, VirtualFree,
        FILE_MAP_WRITE, MEM_COMMIT, MEM_DECOMMIT, PAGE_READWRITE, SEC_RESERVE,
    };

    const SIZE: usize = 0x1_0000;
    // SAFETY: a pagefile section, mapped only for this test. The view and
    // handle are released after the decommit probe.
    unsafe {
        let section = CreateFileMappingW(
            -1isize as _,
            std::ptr::null(),
            PAGE_READWRITE | SEC_RESERVE,
            0,
            SIZE as u32,
            std::ptr::null(),
        );
        assert!(
            !section.is_null(),
            "CreateFileMappingW: {}",
            std::io::Error::last_os_error()
        );
        let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE);
        assert!(
            !view.Value.is_null(),
            "MapViewOfFile: {}",
            std::io::Error::last_os_error()
        );
        let committed = VirtualAlloc(view.Value, 4096, MEM_COMMIT, PAGE_READWRITE);
        assert_eq!(
            committed,
            view.Value,
            "VirtualAlloc: {}",
            std::io::Error::last_os_error()
        );
        let decommitted = VirtualFree(view.Value, 4096, MEM_DECOMMIT);
        assert_eq!(
            decommitted, 0,
            "SEC_RESERVE section pages unexpectedly support VirtualFree(MEM_DECOMMIT)"
        );
        assert_ne!(UnmapViewOfFile(view), 0);
        windows_sys::Win32::Foundation::CloseHandle(section);
    }
}

/// Oracle "guarded gap" (host half): the host reservation above private RAM is
/// reserved and inaccessible, and on Windows the arena range is still a
/// placeholder U3 can replace with a section view (HLD §5.2).
#[cfg(all(windows, target_pointer_width = "64"))]
#[test]
fn windows_gap_stays_reserved_and_the_arena_range_is_a_placeholder() {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc2, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_REPLACE_PLACEHOLDER,
        MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE,
    };

    fn query(addr: *const u8) -> MEMORY_BASIC_INFORMATION {
        // SAFETY: VirtualQuery only reads the address-space map.
        unsafe {
            let mut info = std::mem::zeroed::<MEMORY_BASIC_INFORMATION>();
            let n = VirtualQuery(
                addr.cast(),
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            assert_ne!(n, 0, "VirtualQuery({addr:p}) failed");
            info
        }
    }

    let mut ram = RamBacking::zeroed(0x20_0000, 0x10_0010);
    let base = ram.host_base();
    let check = |ram: &RamBacking| {
        let committed_end = ram.capacity().div_ceil(4096) * 4096;
        // Private RAM: committed read/write, from the base.
        let head = query(base);
        assert_eq!(head.State, MEM_COMMIT);
        assert_eq!(head.Protect, PAGE_READWRITE);
        assert_eq!(head.AllocationBase, base.cast());
        assert_eq!(head.RegionSize, committed_end);
        // The gap: one reserved, uncommitted region up to the arena.
        // SAFETY: address arithmetic inside our own reservation.
        let gap = query(unsafe { base.add(committed_end) });
        assert_eq!(
            gap.State, MEM_RESERVE,
            "gap must be reserved, not committed"
        );
        assert_eq!(gap.AllocationBase, base.cast());
        assert_eq!(gap.AllocationProtect, PAGE_NOACCESS);
        assert_eq!(gap.RegionSize, PRIVATE_WINDOW_BYTES - committed_end);
        // The arena range: its own allocation (the placeholder), reserved.
        // SAFETY: as above.
        let arena_base = unsafe { base.add(PRIVATE_WINDOW_BYTES) };
        let arena = query(arena_base);
        assert_eq!(arena.State, MEM_RESERVE);
        assert_eq!(arena.AllocationBase, arena_base.cast());
        assert_eq!(
            arena.RegionSize,
            RAM_RESERVATION_BYTES - PRIVATE_WINDOW_BYTES
        );
    };
    check(&ram);
    // Growth commits more of the private window and nothing beyond it.
    ram.resize(0x50_0000, 0);
    check(&ram);
    ram[0x4F_FFFF] = 1;
    check(&ram);

    // The arena range really is a placeholder: MEM_REPLACE_PLACEHOLDER accepts
    // exactly that extent (U3 replaces it with MapViewOfFile3 the same way).
    // Replacing it with an ordinary reservation keeps Drop's release valid.
    // SAFETY: replaces the placeholder this backing owns.
    let replaced = unsafe {
        VirtualAlloc2(
            std::ptr::null_mut(),
            base.add(PRIVATE_WINDOW_BYTES).cast_const().cast(),
            RAM_RESERVATION_BYTES - PRIVATE_WINDOW_BYTES,
            MEM_RESERVE | MEM_REPLACE_PLACEHOLDER,
            PAGE_NOACCESS,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(
        replaced,
        // SAFETY: address arithmetic inside our own reservation.
        unsafe { base.add(PRIVATE_WINDOW_BYTES) }.cast(),
        "the arena range is not a replaceable placeholder: {}",
        std::io::Error::last_os_error()
    );
}

/// `MDEM_TEST_CHILD` id of the gap fault-injection child below (test-only).
#[cfg(target_pointer_width = "64")]
const GAP_PROBE_CHILD_ID: &str = "ram_gap_probe";

/// Child half of the fault-injection oracle: run as the re-exec child, a store
/// through a raw pointer one page into the gap must kill the process. Otherwise
/// it is a no-op, so an ordinary test run passes it.
#[cfg(target_pointer_width = "64")]
#[test]
fn gap_probe_child() {
    if !std::env::var("MDEM_TEST_CHILD").is_ok_and(|id| id == GAP_PROBE_CHILD_ID) {
        return;
    }
    let ram = RamBacking::zeroed(0x10_0000, 0x10_0000);
    let gap = ram.capacity().div_ceil(host_page_size()) * host_page_size() + host_page_size();
    assert!(!ram.is_backed(gap..gap + 1));
    // SAFETY: deliberately NOT safe — the point is that the host faults.
    unsafe { std::ptr::write_volatile(ram.host_base().add(gap), 0xAA) };
    // Reaching here means the gap was writable.
    println!("GAP-WRITE-SURVIVED");
    std::process::exit(0);
}

/// Oracle "guarded gap" (fault injection): a raw store into the gap faults in a
/// subprocess — the host guard exists, so an unproven pointer into the gap is a
/// detected emulator bug, never a silent write.
#[cfg(target_pointer_width = "64")]
#[test]
fn a_raw_store_into_the_gap_faults_the_host() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "ram_backing::tests::gap_probe_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MDEM_TEST_CHILD", GAP_PROBE_CHILD_ID)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the gap probe child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("GAP-WRITE-SURVIVED"),
        "a store into the gap succeeded: {stdout}"
    );
    assert!(
        stdout.contains("gap_probe_child"),
        "the child did not run the probe: {stdout}"
    );
    assert!(!out.status.success(), "the child exited cleanly: {stdout}");
    #[cfg(windows)]
    assert_eq!(
        out.status.code().map(|c| c as u32),
        Some(0xC000_0005),
        "expected STATUS_ACCESS_VIOLATION"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            matches!(out.status.signal(), Some(libc::SIGSEGV | libc::SIGBUS)),
            "expected SIGSEGV/SIGBUS, got {:?}",
            out.status
        );
    }
}
