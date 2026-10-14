use super::*;
use crate::arena::{ArenaStore, ARENA_PHYS_END, ARENA_PHYS_START};

#[test]
fn arena_admission_moves_shared_bytes_and_excludes_parked_memory() {
    let (store, token) = ArenaStore::new().expect("session section");
    let mut a = Memory::new();
    let mut b = Memory::new();
    a.attach_arena_store(store.clone()).expect("attach A");
    b.attach_arena_store(store).expect("attach B");
    let page = ARENA_PHYS_START + 0x20_000;

    assert!(!a.ram.is_backed(page..page + 1));
    assert!(a.ram.ptr_for(page..page + 1).is_none());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.ram[page])).is_err());

    a.give_arena_admission(token);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.ram[0x5000_0000])).is_err());
    assert_eq!(
        usize::from(a.ram.has_arena_admission()) + usize::from(b.ram.has_arena_admission()),
        1
    );
    assert!(!a.ram.is_backed(page..page + 1));
    assert!(!a.ram.is_backed(page..page));
    a.commit_arena_host_range(page..page + 8192)
        .expect("commit shared pages");
    // Commitment is in host pages (16 KiB on Apple Silicon).
    let committed_end = page + 8192usize.next_multiple_of(crate::host_page_size());
    assert!(a.ram.is_backed(page..page));
    assert!(a.ram.is_backed(committed_end..committed_end));
    assert!(!a.ram.is_backed(committed_end + 1..committed_end + 1));
    assert!(a.ram.is_backed(ARENA_PHYS_END..ARENA_PHYS_END));
    a.ram[page] = 0x5A;
    a.write_u8(page + 4096, 0xA5);
    a.ram[0x2000] = 0x11;
    b.ram[0x2000] = 0x22;

    let token = a.take_arena_admission().expect("A held token");
    assert!(a.ram.ptr_for(page..page + 1).is_none());
    assert!(a.direct_host_ram_range(page, 1).is_none());
    b.give_arena_admission(token);
    assert_eq!(
        usize::from(a.ram.has_arena_admission()) + usize::from(b.ram.has_arena_admission()),
        1
    );
    assert_eq!(b.ram[page], 0x5A);
    assert!(b.direct_host_ram_range(page, 1).is_some());
    assert_eq!(b.read_u8(page + 4096), 0xA5);
    assert_eq!(a.ram[0x2000], 0x11);
    assert_eq!(b.ram[0x2000], 0x22);
    b.ram[page + 1] = 0xC3;
    assert!(b.ram.ptr_for(page..page + 2).is_some());

    let token = b.take_arena_admission().expect("B held token");
    a.give_arena_admission(token);
    assert_eq!(
        usize::from(a.ram.has_arena_admission()) + usize::from(b.ram.has_arena_admission()),
        1
    );
    assert_eq!(a.ram[page + 1], 0xC3);
    assert!(b.ram.ptr_for(page..page + 1).is_none());
    assert!(a
        .ram
        .ptr_for((ARENA_PHYS_START - 1)..ARENA_PHYS_START + 1)
        .is_none());
}

#[test]
fn arena_detach_restores_guard_and_releases_admission() {
    let (store, token) = ArenaStore::new().expect("session section");
    let retained_store = store.clone();
    let mut mem = Memory::new();
    mem.attach_arena_store(store).expect("attach");
    mem.give_arena_admission(token);
    let page = ARENA_PHYS_START + 0x30_000;
    mem.commit_arena_host_range(page..page + 4096)
        .expect("commit");
    mem.ram[page] = 0x7B;
    let token = mem.detach_arena_store().expect("token returned");
    assert!(mem.ram.ptr_for(page..page + 1).is_none());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mem.ram[page])).is_err());
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{
            VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_RESERVE,
        };
        let mut info = unsafe { std::mem::zeroed::<MEMORY_BASIC_INFORMATION>() };
        let arena_base = unsafe { mem.ram.host_base().add(ARENA_PHYS_START) };
        let size = unsafe {
            VirtualQuery(
                arena_base.cast(),
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        assert_ne!(size, 0);
        assert_eq!(info.State, MEM_RESERVE);
    }
    let mut remounted = Memory::new();
    remounted
        .attach_arena_store(retained_store)
        .expect("reattach retained section");
    remounted.give_arena_admission(token);
    assert_eq!(remounted.ram[page], 0x7B);
}

#[test]
fn arena_exec_and_smc_watch_cover_high_physical_pages() {
    let (store, token) = ArenaStore::new().expect("session section");
    let mut mem = Memory::new();
    mem.attach_arena_store(store).expect("attach");
    mem.give_arena_admission(token);
    let phys = ARENA_PHYS_START + 0x80_000;
    mem.commit_arena_host_range(phys..phys + 4096)
        .expect("commit");
    let page = phys >> INSTR_EXEC_PAGE_SHIFT;
    mem.mark_instr_exec_page(phys);
    assert!(mem.core.instr_exec_page_marked(page));
    mem.set_page_watched((phys >> 12) as u32, true);
    assert!(mem.smc_span_may_hold_code(phys, 1));
    mem.set_page_watched((phys >> 12) as u32, false);
    assert!(!mem.smc_span_may_hold_code(phys, 1));
}

#[test]
fn arena_wide_writes_cross_pages_and_survive_admission_move() {
    let (store, token) = ArenaStore::new().expect("session section");
    let (mut a, mut b) = (Memory::new(), Memory::new());
    a.attach_arena_store(store.clone()).expect("attach A");
    b.attach_arena_store(store).expect("attach B");
    a.give_arena_admission(token);
    let page = ARENA_PHYS_START + 0x50_000;
    a.commit_arena_host_range(page..page + 8192)
        .expect("commit adjacent pages");
    a.write_u32(page + 4093, 0x1234_5678);
    a.write_u16(page + 4095, 0xBEEF);
    let token = a.take_arena_admission().expect("A admission");
    b.give_arena_admission(token);
    assert_eq!(
        &b.ram[page + 4093..page + 4098],
        &[0x78, 0x56, 0xEF, 0xBE, 0x00]
    );
    b.write_u32(page + 4094, 0xA1B2_C3D4);
    let token = b.take_arena_admission().expect("B admission");
    a.give_arena_admission(token);
    assert_eq!(&a.ram[page + 4094..page + 4098], &[0xD4, 0xC3, 0xB2, 0xA1]);
}

#[cfg(windows)]
#[test]
fn windows_gap_remains_reserved_with_one_three_and_six_views() {
    use crate::{RamBacking, PRIVATE_WINDOW_BYTES};
    use windows_sys::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RESERVE,
    };

    fn state(at: *const u8) -> u32 {
        // SAFETY: VirtualQuery only inspects this process's address map.
        let mut info = unsafe { std::mem::zeroed::<MEMORY_BASIC_INFORMATION>() };
        let size = unsafe {
            VirtualQuery(
                at.cast(),
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        assert_ne!(size, 0);
        info.State
    }

    let (store, token) = ArenaStore::new().expect("session section");
    let mut token = Some(token);
    let mut views = Vec::new();
    for count in 1..=6 {
        let mut ram = RamBacking::zeroed(0x2_0000, 0x1_0000);
        ram.attach_arena(store.clone()).expect("attach view");
        if count == 1 {
            ram.give_admission(token.take().expect("initial admission"));
            ram.commit_arena((ARENA_PHYS_START + 0x10_000)..(ARENA_PHYS_START + 0x11_000))
                .expect("commit one shared page");
        }
        views.push(ram);
        if [1, 3, 6].contains(&count) {
            for view in &views {
                let base = view.host_base();
                let gap = unsafe { base.add(0x2_0000) };
                let arena_page = unsafe { base.add(ARENA_PHYS_START + 0x10_000) };
                assert_eq!(state(gap), MEM_RESERVE);
                assert_eq!(state(arena_page), MEM_COMMIT);
                assert_eq!(
                    state(unsafe { base.add(PRIVATE_WINDOW_BYTES - 4096) }),
                    MEM_RESERVE
                );
            }
        }
    }
}
