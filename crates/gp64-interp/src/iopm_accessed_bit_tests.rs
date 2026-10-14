use super::*;
use gp64_memory::Memory;

const ACCESSED: u32 = 1 << 5;
const DIRTY: u32 = 1 << 6;

const PD_ADDR: usize = 0x10000;
const PT_ADDR: usize = 0x11000;
const TSS_BASE: u32 = 0x20000;
/// TSS offset of the I/O-map base field, and the value we store there.
const IO_MAP_BASE: u32 = 0x68;
/// The port under test. `0x60 / 8 == 12`, so its bitmap byte is at
/// `TSS_BASE + IO_MAP_BASE + 12`, on the same page as the TSS header.
const PORT: u16 = 0x60;

/// Linear page holding both the TSS header and the bitmap byte.
const TSS_PAGE: usize = (TSS_BASE as usize) >> 12;
/// The PTE the walk must mark accessed, and a neighbour it must not touch.
const USED_PTE_ADDR: usize = PT_ADDR + TSS_PAGE * 4;
const UNUSED_PTE_ADDR: usize = PT_ADDR + (TSS_PAGE + 1) * 4;

/// V86 guest, paging on, low memory identity-mapped through a 4 KiB page
/// table, and a TSS whose I/O bitmap permits `PORT`.
fn setup_v86_with_iopm() -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cr3 = PD_ADDR as u32;
    cpu.regs.set_v86_mode(true);

    // PDE 0 -> page table, present + rw + user. A deliberately CLEAR.
    mem.write_u32_physical(PD_ADDR, (PT_ADDR as u32) | 0x07);
    // Identity-map the first 4 MiB. A deliberately CLEAR on every PTE.
    for page in 0..1024u32 {
        mem.write_u32_physical(PT_ADDR + (page as usize) * 4, (page << 12) | 0x07);
    }

    cpu.regs.tss_base = TSS_BASE;
    cpu.regs.tss_limit = 0x2000;
    // TSS+0x66: I/O map base.
    mem.write_u32_physical(
        (TSS_BASE + 0x64) as usize,
        (IO_MAP_BASE << 16) | 0x0000, // low half unused, high half = base
    );
    // Bitmap byte for PORT: 0 == permitted.
    mem.write_u8((TSS_BASE + IO_MAP_BASE + (PORT / 8) as u32) as usize, 0x00);

    (cpu, mem)
}

fn permission(cpu: &Cpu, mem: &mut Memory) -> IoPermission {
    let ram = mem.ram_ptr_mut();
    let ram_len = mem.usable_len();
    check_io_permission(cpu, mem, ram, ram_len, PORT, 1)
}

#[test]
fn iopm_walk_sets_accessed_on_the_pde_and_pte_it_used() {
    let (cpu, mut mem) = setup_v86_with_iopm();
    assert_eq!(mem.read_u32_physical(PD_ADDR) & ACCESSED, 0);
    assert_eq!(mem.read_u32_physical(USED_PTE_ADDR) & ACCESSED, 0);

    assert_eq!(permission(&cpu, &mut mem), IoPermission::Allowed);

    assert_ne!(
        mem.read_u32_physical(PD_ADDR) & ACCESSED,
        0,
        "the PDE used by the IOPM walk must be marked accessed"
    );
    assert_ne!(
        mem.read_u32_physical(USED_PTE_ADDR) & ACCESSED,
        0,
        "the PTE used by the IOPM walk must be marked accessed"
    );
}

#[test]
fn iopm_walk_leaves_dirty_clear_because_the_fetch_is_a_read() {
    let (cpu, mut mem) = setup_v86_with_iopm();

    assert_eq!(permission(&cpu, &mut mem), IoPermission::Allowed);

    assert_eq!(
        mem.read_u32_physical(PD_ADDR) & DIRTY,
        0,
        "an IOPM fetch reads; it must never set PDE Dirty"
    );
    assert_eq!(
        mem.read_u32_physical(USED_PTE_ADDR) & DIRTY,
        0,
        "an IOPM fetch reads; it must never set PTE Dirty"
    );
}

#[test]
fn iopm_walk_does_not_touch_entries_it_did_not_use() {
    let (cpu, mut mem) = setup_v86_with_iopm();

    assert_eq!(permission(&cpu, &mut mem), IoPermission::Allowed);

    assert_eq!(
        mem.read_u32_physical(UNUSED_PTE_ADDR) & ACCESSED,
        0,
        "only the entries on the translated path may become accessed"
    );
}

#[test]
fn iopm_pse_walk_sets_accessed_on_the_large_page_pde_only() {
    let (cpu, mut mem) = setup_v86_with_iopm();
    let mut cpu = cpu;
    cpu.regs.cr4 |= 1 << 4; // CR4.PSE
                            // Replace PDE 0 with a 4 MiB identity mapping: PS=1, A clear.
    mem.write_u32_physical(PD_ADDR, 0x0000_0000 | 0x87);
    assert_eq!(mem.read_u32_physical(PD_ADDR) & ACCESSED, 0);

    assert_eq!(permission(&cpu, &mut mem), IoPermission::Allowed);

    assert_ne!(
        mem.read_u32_physical(PD_ADDR) & ACCESSED,
        0,
        "a 4 MiB PDE is the only entry the translation used, so it gets A"
    );
    assert_eq!(
        mem.read_u32_physical(PD_ADDR) & DIRTY,
        0,
        "a read must not dirty the large-page PDE"
    );
    assert_eq!(
        mem.read_u32_physical(USED_PTE_ADDR) & ACCESSED,
        0,
        "the PSE walk never reads a page table, so no PTE may be touched"
    );
}

/// Fault ordering matches `translate_linear_supervisor_checked`: a walk
/// that does not complete leaves every entry untouched.
#[test]
fn iopm_walk_leaves_accessed_clear_when_the_pte_is_not_present() {
    let (cpu, mut mem) = setup_v86_with_iopm();
    let pte = mem.read_u32_physical(USED_PTE_ADDR);
    mem.write_u32_physical(USED_PTE_ADDR, pte & !1); // clear P

    assert_eq!(
        permission(&cpu, &mut mem),
        IoPermission::PageFault {
            linear: TSS_BASE + 0x66
        },
        "a non-present PTE on the implicit-supervisor walk is a #PF"
    );

    assert_eq!(
        mem.read_u32_physical(PD_ADDR) & ACCESSED,
        0,
        "an incomplete walk must not mark the PDE accessed"
    );
    assert_eq!(
        mem.read_u32_physical(USED_PTE_ADDR) & ACCESSED,
        0,
        "an incomplete walk must not mark the non-present PTE accessed"
    );
}

#[test]
fn iopm_walk_leaves_accessed_clear_when_the_pde_is_not_present() {
    let (cpu, mut mem) = setup_v86_with_iopm();
    let pde = mem.read_u32_physical(PD_ADDR);
    mem.write_u32_physical(PD_ADDR, pde & !1); // clear P

    assert_eq!(
        permission(&cpu, &mut mem),
        IoPermission::PageFault {
            linear: TSS_BASE + 0x66
        },
        "a non-present PDE on the implicit-supervisor walk is a #PF"
    );

    assert_eq!(
        mem.read_u32_physical(PD_ADDR) & ACCESSED,
        0,
        "an incomplete walk must not mark the non-present PDE accessed"
    );
}
