use super::*;
use gp64_cpu::Cpu;
use gp64_memory::Memory;

/// The reserved index the hosted design uses: `MAPLS_FIRST_INDEX`.
const RESERVED: u16 = 0x100;

fn reserved_dpmi() -> (Dpmi, Cpu, Memory) {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let memory = Memory::new();
    cpu.regs.ldt_limit = 0x7FFF; // 4096 entries, as the thunk window's LDT
    dpmi.reserve_ldt_indices_from(RESERVED);
    (dpmi, cpu, memory)
}

fn index_of(selector: u16) -> u16 {
    selector >> 3
}

/// Without a reservation nothing changes — the property that makes every
/// existing DPMI and 16-bit guest byte-identical.
#[test]
fn an_unreserved_ldt_admits_every_run() {
    let dpmi = Dpmi::new();
    assert_eq!(dpmi.reserved_ldt_index(), None);
    assert_eq!(dpmi.system_flat_ldt_index(), None);
    assert!(dpmi.selector_run_admitted(0x0007, 1));
    // A plain DPMI client may be handed synthetic flat indices 0x2C/0x2D.
    assert!(dpmi.selector_run_admitted((0x2C << 3) | 7, 2));
    assert!(dpmi.selector_run_admitted(0xFFF7, 64));
}

/// The 32-bit system LDT's null entry and flat code/data pair are never
/// admitted, alone or inside a run, while the neighbours still are.
#[test]
fn a_system_ldt_never_admits_its_slots() {
    let mut dpmi = Dpmi::new();
    dpmi.reserve_system_ldt_slots(0x2C);
    for index in [0u16, 0x2C, 0x2D] {
        assert!(
            !dpmi.selector_run_admitted((index << 3) | 7, 1),
            "index {index:#x} admitted"
        );
    }
    assert!(!dpmi.selector_run_admitted((0x2B << 3) | 7, 2));
    assert!(!dpmi.selector_run_admitted((0x2D << 3) | 7, 2));
    assert!(dpmi.selector_run_admitted((0x2B << 3) | 7, 1));
    assert!(dpmi.selector_run_admitted((0x2E << 3) | 7, 1));
    assert!(dpmi.selector_run_admitted(0xFFF7, 64));
}

/// The bump cursor steps over the flat pair: a single allocation lands just
/// past it, and so does a run that would straddle it.
#[test]
fn bump_allocation_skips_the_system_flat_pair() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();
    dpmi.reserve_system_ldt_slots(0x2C);
    dpmi.next_selector = (0x2B << 3) | 7;
    assert_eq!(
        dpmi.allocate_descriptors(&mut cpu, &mut memory, 1),
        (0x2B << 3) | 7
    );
    assert_eq!(
        dpmi.allocate_descriptors(&mut cpu, &mut memory, 1),
        (0x2E << 3) | 7
    );

    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();
    dpmi.reserve_system_ldt_slots(0x2C);
    dpmi.next_selector = (0x2B << 3) | 7;
    assert_eq!(
        dpmi.allocate_descriptors(&mut cpu, &mut memory, 2),
        (0x2E << 3) | 7
    );
}

/// The bump cursor can never walk into the reserved range, however many
/// single allocations a guest makes.
#[test]
fn the_bump_cursor_stops_below_the_reservation() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();

    let mut last = 0u16;
    let mut allocations = 0usize;
    loop {
        let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
        if sel == 0 {
            break;
        }
        assert!(
            index_of(sel) < RESERVED,
            "allocation {allocations} handed out {sel:#06x} (index {:#x}), \
             which is inside the reserved range",
            index_of(sel)
        );
        last = sel;
        allocations += 1;
        assert!(allocations < 4096, "allocation never refused");
    }

    assert!(allocations > 0, "the low range must still be usable");
    assert_eq!(
        index_of(last),
        RESERVED - 1,
        "the cursor must be exhausted exactly at the boundary"
    );
}

/// The case a cursor cap silently admits: a multi-descriptor run that
/// *starts* below the boundary and ends past it.
#[test]
fn a_run_straddling_the_boundary_is_refused_whole() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();

    // Park the cursor four entries below the boundary and ask for eight.
    dpmi.next_selector = ((RESERVED - 4) << 3) | 0x07;
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 8);

    assert_eq!(sel, 0, "a straddling run must fail, not be truncated");
    assert!(
        dpmi.selectors.is_empty(),
        "a refused run must not leave partial descriptors behind"
    );
    // And the four entries that *do* fit are still available.
    assert_eq!(
        dpmi.allocate_descriptors(&mut cpu, &mut memory, 4),
        ((RESERVED - 4) << 3) | 0x07,
        "the admissible prefix must still allocate"
    );
}

/// Free-list reuse: a selector that somehow entered the free list from the
/// reserved range is never handed back out, on either reuse route.
#[test]
fn free_list_reuse_never_returns_a_reserved_selector() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();

    let reserved_sel = (RESERVED << 3) | 0x07;
    dpmi.free_selectors.push(reserved_sel);
    dpmi.free_selectors.push(((RESERVED + 1) << 3) | 0x07);

    // Single-descriptor route (pops the free list first).
    let one = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert!(one != 0 && index_of(one) < RESERVED, "got {one:#06x}");

    // Multi-descriptor route (searches the free list for a run).
    dpmi.free_selectors.push(reserved_sel);
    dpmi.free_selectors.push(((RESERVED + 1) << 3) | 0x07);
    let two = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);
    assert!(
        two == 0 || index_of(two) + 1 < RESERVED,
        "multi-descriptor reuse handed out {two:#06x}"
    );
    assert!(
        !dpmi.free_selectors.contains(&reserved_sel),
        "a reserved selector must be purged from the free list, not parked"
    );
}

/// `INT 31h/000Dh` (allocate-specific) reaches any selector the guest names
/// and advances the cursor by its own route, so it asks the same chokepoint.
#[test]
fn allocate_specific_refuses_a_reserved_selector() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();

    let reserved_sel = (RESERVED << 3) | 0x07;
    // Put it in the free list so the pre-existing index > 0x0F guard would
    // otherwise let it through — this is the path that guard misses.
    dpmi.free_selectors.push(reserved_sel);

    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(reserved_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "000Dh must fail for a reserved selector"
    );
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
    assert!(
        !dpmi.selectors.iter().any(|s| s.selector == reserved_sel),
        "a refused 000Dh must not record the selector"
    );
    assert!(
        index_of(dpmi.next_selector) <= RESERVED,
        "a refused 000Dh must not push the cursor into the reserved range"
    );
}

/// B4 property (2): the budget is a budget, not a countdown. Freeing
/// genuinely returns selectors, so a long-running guest that allocates and
/// frees in a loop never exhausts the low range.
#[test]
fn freeing_returns_selectors_so_the_budget_is_not_a_countdown() {
    let (mut dpmi, mut cpu, mut memory) = reserved_dpmi();

    // Drain the range so only reuse can satisfy the next request.
    let mut held = Vec::new();
    loop {
        let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
        if sel == 0 {
            break;
        }
        held.push(sel);
    }
    assert!(!held.is_empty());
    assert_eq!(
        dpmi.allocate_descriptors(&mut cpu, &mut memory, 1),
        0,
        "the range must be genuinely exhausted first"
    );

    // Churn: free and reallocate many more times than the range holds.
    let rounds = held.len() * 3;
    for round in 0..rounds {
        let slot = round % held.len();
        let victim = held[slot];
        assert!(
            dpmi.free_descriptor(&mut cpu, &mut memory, victim),
            "round {round}: free must succeed"
        );
        let again = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
        assert_ne!(again, 0, "round {round}: reuse must succeed after a free");
        assert!(
            index_of(again) < RESERVED,
            "round {round}: reuse handed out {again:#06x} from the reserved range"
        );
        held[slot] = again;
    }
}

/// Two owners cannot widen each other's reservation, and clearing releases
/// the whole range again.
#[test]
fn reservations_narrow_and_clear() {
    let mut dpmi = Dpmi::new();

    dpmi.reserve_ldt_indices_from(0x200);
    dpmi.reserve_ldt_indices_from(0x100);
    assert_eq!(dpmi.reserved_ldt_index(), Some(0x100));
    dpmi.reserve_ldt_indices_from(0x400);
    assert_eq!(
        dpmi.reserved_ldt_index(),
        Some(0x100),
        "a wider request must not relax an existing reservation"
    );

    dpmi.clear_ldt_reservation();
    assert_eq!(dpmi.reserved_ldt_index(), None);
    assert!(dpmi.selector_run_admitted(0x0807, 8));
}

/// A new client must not inherit the previous address space's reservation.
#[test]
fn reset_releases_the_reservation() {
    let (mut dpmi, _cpu, _memory) = reserved_dpmi();
    dpmi.reset();
    assert_eq!(dpmi.reserved_ldt_index(), None);
}
