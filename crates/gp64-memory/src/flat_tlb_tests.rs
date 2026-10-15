//! Flat TLB projection — coherence oracles (O-P1 .. O-P3, plus the watch fold).
//!
//! These tests are the executable form of the projection's load-bearing claims.
//! Each is written to FAIL against a naive implementation that omits the
//! protocol clause it guards, and each failure was observed before the clause
//! was trusted:
//!
//! * O-P2 collision eviction — fails without eviction mirroring in `insert`
//! * O-P2 INVLPG idempotence — fails without UNCONDITIONAL projection nulling
//! * O-P1(b) stale survivors — the full-scan form of both of the above
//! * O-P3 flag lattice — fails if view nullity stops matching the hit rule

use crate::flat_tlb::{FlatView, FLAT_VIEW_CPL3_DELTA, FLAT_VIEW_WRITE_DELTA};
use crate::{Memory, SoftwareTlb, TLB_SIZE};

/// First physical page of the VGA window (`0xA0000 >> 12`), which the PC
/// publication proof never admits.
const TLB_VGA_PAGE_START: u32 = 0xA0;

/// Every view of `page` reads as a miss.
fn assert_all_views_null(mem: &Memory, page: u32, why: &str) {
    for view in [
        FlatView::Read0,
        FlatView::Write0,
        FlatView::Read3,
        FlatView::Write3,
    ] {
        assert_eq!(
            mem.core.tlb().flat().entry(view, page),
            0,
            "page {page:#x} view {view:?} should be a miss: {why}"
        );
    }
}

/// Find a distinct linear page that hashes to the same 4096-slot index.
fn colliding_page(page: u32) -> u32 {
    let want = SoftwareTlb::index(page);
    (0u32..0x10_0000)
        .find(|&q| q != page && SoftwareTlb::index(q) == want)
        .expect("the XOR hash must collide somewhere in a 1 Mi-page space")
}

/// Both projection oracles must be green; panics carrying the oracle's message.
fn assert_projection_coherent(mem: &Memory, when: &str) {
    if let Err(e) = mem.oracle_check_flat_projection() {
        panic!("projection incoherent {when}: {e}");
    }
    if let Err(e) = mem.oracle_check_flat_no_stale_survivors() {
        panic!("stale survivor {when}: {e}");
    }
}

/// Deterministic LCG. No `rand`/`Date` in this environment, and a fixed sequence
/// makes any failure reproducible from the seed alone.
struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n)) as u32
    }
}

#[test]
fn a_fill_publishes_a_bias_that_reconstructs_the_host_pointer() {
    // Adding the raw linear address to the bias must recover the host byte.
    let mut mem = Memory::new();
    let page = 0x20u32; // physical 0x20000: plain RAM, below the VGA aperture
    mem.tlb_insert(page, page, true, true, true);

    let entry = mem.core.tlb().flat().entry(FlatView::Read0, page);
    assert_ne!(entry, 0, "a plain writable RAM page must publish a hit");
    for offset in [0u32, 1, 0x555, 0xFFF] {
        let raw_linear = (page << 12) | offset;
        let host = entry.wrapping_add(raw_linear as u64) as *const u8;
        let want = unsafe {
            mem.ram
                .host_base()
                .cast_const()
                .add(((page << 12) | offset) as usize)
        };
        assert_eq!(
            host, want,
            "bias must reconstruct the host byte at +{offset:#x}"
        );
    }
    assert_projection_coherent(&mem, "after a plain fill");
}

#[test]
fn op2_a_colliding_insert_nulls_the_displaced_pages_views() {
    // REGRESSION GUARD (observed red without eviction mirroring): when a hashed
    // insert displaces a DIFFERENT page, that page's four projection slots must
    // be nulled BEFORE the new page is published. Without the mirroring clause
    // in `SoftwareTlb::insert`, the displaced page keeps a live biased pointer
    // that no authoritative entry backs.
    let mut mem = Memory::new();
    let p = 0x30u32;
    let q = colliding_page(p);
    assert_eq!(SoftwareTlb::index(p), SoftwareTlb::index(q));

    mem.tlb_insert(p, 0x30, true, true, true);
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Read0, p),
        0,
        "test is vacuous unless p is live first"
    );

    mem.tlb_insert(q, 0x31, true, true, true);

    // Check the oracle first, then specific slots, to prove the oracle
    // itself detects the corruption rather than only a bespoke assertion.
    assert_projection_coherent(&mem, "after a colliding insert");
    assert_all_views_null(&mem, p, "p was collision-evicted by q");
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Read0, q),
        0,
        "q must be live after its own insert"
    );
    assert_projection_coherent(&mem, "after a colliding insert");
}

#[test]
fn op2_alternating_collisions_never_leave_a_survivor() {
    // The stress form: ping-pong two colliding pages and assert after EVERY step
    // that exactly one is live and the full-scan oracle sees no survivor.
    let mut mem = Memory::new();
    let p = 0x40u32;
    let q = colliding_page(p);

    for round in 0..8u32 {
        let (live, dead, phys) = if round % 2 == 0 {
            (p, q, 0x40)
        } else {
            (q, p, 0x41)
        };
        mem.tlb_insert(live, phys, true, true, true);
        assert_ne!(
            mem.core.tlb().flat().entry(FlatView::Read0, live),
            0,
            "round {round}: the just-inserted page must be live"
        );
        assert_all_views_null(&mem, dead, "displaced by the alternating insert");
        assert_projection_coherent(&mem, &format!("after alternating round {round}"));
    }
}

#[test]
fn op2_invlpg_on_a_collision_evicted_page_is_idempotent_and_unconditional() {
    // REGRESSION GUARD (observed red with a CONDITIONAL null): INVLPG on a page
    // that was already collision-evicted finds no matching hashed tag, so a
    // projection null guarded by "if the tag matches" would do nothing. The null
    // must be unconditional — which also makes it self-healing.
    let mut mem = Memory::new();
    let p = 0x50u32;
    let q = colliding_page(p);

    mem.tlb_insert(p, 0x50, true, true, true);
    mem.tlb_insert(q, 0x51, true, true, true); // p is now evicted, tag gone

    mem.invalidate_tlb_page(p << 12);
    assert_all_views_null(&mem, p, "INVLPG on an evicted page");
    mem.invalidate_tlb_page(p << 12); // idempotent
    assert_all_views_null(&mem, p, "INVLPG twice");
    assert_projection_coherent(&mem, "after INVLPG on an evicted page");
}

#[test]
fn a_flush_nulls_every_live_page() {
    // A flush must null the projection for every live tag — and must do it by
    // walking the 4096 hashed slots, never by memsetting 32 MiB.
    let mut mem = Memory::new();
    let pages: Vec<u32> = (0x60..0x80).collect();
    for &pg in &pages {
        mem.tlb_insert(pg, pg, true, true, true);
    }
    assert!(
        pages
            .iter()
            .any(|&pg| mem.core.tlb().flat().entry(FlatView::Read0, pg) != 0),
        "test is vacuous unless some page went live"
    );

    mem.invalidate_tlb();

    for &pg in &pages {
        assert_all_views_null(&mem, pg, "after a full flush");
    }
    assert_projection_coherent(&mem, "after a full flush");
}

#[test]
fn op3_view_nullity_matches_the_interpreter_hit_rule_across_the_flag_lattice() {
    // For every flag combination, a nonzero view must agree with the
    // authoritative entry's fast-path permission at that CPL. Writes need
    // a writable, dirty, ROM-allowed, host-accessible page outside VGA.
    let mut checked = 0usize;
    let mut saw_read_hit = false;
    let mut saw_write_hit = false;
    let mut saw_miss = false;

    for &phys in &[0x90u32, TLB_VGA_PAGE_START, 0xF0, 0x7FFF] {
        for &allow_rom in &[false, true] {
            for &writable in &[false, true] {
                for &user in &[false, true] {
                    for &dirty in &[false, true] {
                        let mut mem = Memory::new();
                        mem.set_allow_rom_writes(allow_rom);
                        let page = 0x100u32 + phys;
                        mem.tlb_insert(page, phys, writable, user, dirty);

                        let e = mem.core.tlb().entries[SoftwareTlb::index(page)];
                        assert_eq!(e.linear_page, page, "the fill must have landed");

                        let read_hit = !e.host_read.is_null();
                        let write_hit = !e.host_write.is_null();
                        saw_read_hit |= read_hit;
                        saw_write_hit |= write_hit;
                        saw_miss |= !read_hit;

                        let f = mem.core.tlb().flat();
                        let ctx = format!(
                            "phys={phys:#x} rom={allow_rom} w={writable} u={user} d={dirty}"
                        );
                        assert_eq!(
                            f.entry(FlatView::Read0, page) != 0,
                            read_hit,
                            "R0 nullity must match the supervisor read-hit rule ({ctx})"
                        );
                        assert_eq!(
                            f.entry(FlatView::Write0, page) != 0,
                            write_hit,
                            "W0 nullity must match the supervisor write-hit rule ({ctx})"
                        );
                        assert_eq!(
                            f.entry(FlatView::Read3, page) != 0,
                            read_hit && user,
                            "R3 must additionally require USER ({ctx})"
                        );
                        assert_eq!(
                            f.entry(FlatView::Write3, page) != 0,
                            write_hit && user,
                            "W3 must additionally require USER ({ctx})"
                        );
                        assert_projection_coherent(&mem, "in the flag lattice");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 4 * 2 * 2 * 2 * 2, "lattice coverage");
    assert!(
        saw_read_hit && saw_write_hit && saw_miss,
        "lattice not vacuous"
    );
}

#[test]
fn op3_supervisor_write_to_a_read_only_page_takes_the_slow_path_either_way() {
    // The one behavioural corner CR0.WP would seem to need a view for: CPL 0,
    // WP=0, read-only page — a write x86 PERMITS. The interpreter's own
    // host_write is null there (not writable), so it already slow-paths and
    // performs the write in the slow path. The flat view is 0 for exactly the
    // same reason, so behaviour is identical and WP needs neither a view nor a
    // flush on flip.
    let mut mem = Memory::new();
    let page = 0xA0u32;
    mem.tlb_insert(page, 0x70, /* writable */ false, false, true);
    let e = mem.core.tlb().entries[SoftwareTlb::index(page)];
    assert!(
        e.host_write.is_null(),
        "read-only implies no host_write, by construction"
    );
    assert_eq!(mem.core.tlb().flat().entry(FlatView::Write0, page), 0);
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Read0, page),
        0,
        "reads still hit"
    );
}

#[test]
fn watching_a_page_nulls_its_write_views_and_unwatching_restores_them() {
    // O-P5 precondition (the Rust half): the SMC fast-path predicate folded into
    // W nullity, and the sweep that recomputes it.
    let mut mem = Memory::new();
    let page = 0xB0u32;
    let phys = 0x70u32;
    mem.tlb_insert(page, phys, true, true, true);
    let live_w = mem.core.tlb().flat().entry(FlatView::Write0, page);
    assert_ne!(live_w, 0, "test is vacuous unless the page starts writable");

    mem.set_page_watched(phys, true);
    assert_eq!(
        mem.core.tlb().flat().entry(FlatView::Write0, page),
        0,
        "a watched page must publish a write miss"
    );
    assert_eq!(mem.core.tlb().flat().entry(FlatView::Write3, page), 0);
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Read0, page),
        0,
        "watching must NEVER restrict reads"
    );
    assert_projection_coherent(&mem, "while watched");

    mem.set_page_watched(phys, false);
    assert_eq!(
        mem.core.tlb().flat().entry(FlatView::Write0, page),
        live_w,
        "unwatching must restore the exact same biased pointer"
    );
    assert_projection_coherent(&mem, "after unwatching");
}

#[test]
fn a_fill_onto_an_already_watched_page_publishes_a_write_miss() {
    // Order independence: watch-then-fill must agree with fill-then-watch.
    let mut mem = Memory::new();
    let phys = 0x71u32;
    mem.set_page_watched(phys, true);
    let page = 0xC0u32;
    mem.tlb_insert(page, phys, true, true, true);
    assert_eq!(
        mem.core.tlb().flat().entry(FlatView::Write0, page),
        0,
        "a fill must consult the watch bitmap"
    );
    assert_ne!(mem.core.tlb().flat().entry(FlatView::Read0, page), 0);
    assert_projection_coherent(&mem, "filling onto a watched page");
}

#[test]
fn the_watch_sweep_covers_every_linear_alias_of_one_physical_page() {
    // Several linear pages can map one physical page; the sweep is keyed on
    // physical, so all of their write views must flip together.
    let mut mem = Memory::new();
    let phys = 0x72u32;
    let aliases = [0xD0u32, 0xD1, 0x1_D0D0];
    for &a in &aliases {
        mem.tlb_insert(a, phys, true, true, true);
    }
    for &a in &aliases {
        assert_ne!(
            mem.core.tlb().flat().entry(FlatView::Write0, a),
            0,
            "alias {a:#x}"
        );
    }
    mem.set_page_watched(phys, true);
    for &a in &aliases {
        assert_eq!(
            mem.core.tlb().flat().entry(FlatView::Write0, a),
            0,
            "alias {a:#x} must be swept too"
        );
    }
    assert_projection_coherent(&mem, "after sweeping aliases");
}

#[test]
fn a_repeated_watch_transition_does_no_sweep_work() {
    // The sweep is O(4096); only a real bit flip may pay for it. This is the
    // cost claim the stage-1 counters exist to validate on real workloads.
    let mut mem = Memory::new();
    let phys = 0x73u32;
    mem.set_page_watched(phys, true);
    let (t1, s1, _) = mem.flat_projection_counters();
    mem.set_page_watched(phys, true);
    mem.set_page_watched(phys, true);
    let (t2, s2, _) = mem.flat_projection_counters();
    assert_eq!(t1, t2, "a no-op transition must not count");
    assert_eq!(s1, s2, "a no-op transition must not sweep");
    assert_eq!(t1, 1);
    assert_eq!(
        s1, TLB_SIZE as u64,
        "one transition sweeps the 4096 slots once"
    );
}

#[test]
fn any_instant_smc_fold_holds_under_arbitrary_interleavings() {
    // O-SMC-ANYINSTANT as a driven property (build journal D-B8; adopted from the
    // D-B2 second-opinion consults). Drive arbitrary interleavings of the four
    // authoritative mutations — TLB fill, INVLPG, full flush, watch set/clear —
    // over a small physical-page working set (so collisions, aliasing, and
    // watch/fill races all actually occur), and assert after EVERY operation:
    //
    //   watched(phys) ⇒ every live hashed entry mapping phys has W0 == W3 == 0.
    //
    // A watched page must never have a permissive write view, including
    // between mutations rather than only at observation boundaries.
    // We also re-assert the full projection coherence after every op.
    //
    // Deterministic across a spread of seeds; a failure is reproducible from the
    // printed seed alone.
    //
    // The linear working set is built to GUARANTEE hashed collisions — for each
    // base page we include a page that hashes to the same 4096-slot index — so
    // the eviction-mirroring path is genuinely exercised, not merely present. A
    // handful of physical pages shared across many linear aliases forces the
    // watch sweep to touch several live entries at once. Without the deliberate
    // collision, a driver over small linear pages never evicts and the "assert
    // coherence after every op" check is far weaker than it looks.
    const PHYS_PAGES: u32 = 8;
    // Pairs of (linear, colliding-linear); each pair shares one hashed slot.
    let linear_pool: Vec<u32> = {
        let mut v = Vec::new();
        for base in [0x200u32, 0x211, 0x333, 0x480, 0x1_2340, 0x5_5551] {
            v.push(base);
            v.push(colliding_page(base));
        }
        v
    };
    let pool_len = linear_pool.len() as u32;
    let mut checked_ops: u64 = 0;
    let mut saw_watched_live = false;
    let mut saw_eviction = false;

    for seed in 0u64..64 {
        let mut mem = Memory::new();
        let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));

        for step in 0..400u32 {
            match rng.below(5) {
                0 | 1 => {
                    // Fill: a pooled linear page onto a small set of physical
                    // pages, so aliases and collisions both occur.
                    let linear = linear_pool[rng.below(pool_len) as usize];
                    let phys = rng.below(PHYS_PAGES) + 0x40; // clear of the VGA aperture
                                                             // Detect a genuine collision-eviction for non-vacuity.
                    let idx = SoftwareTlb::index(linear);
                    let prior = mem.core.tlb().entries[idx].linear_page;
                    if prior != u32::MAX && prior != linear {
                        saw_eviction = true;
                    }
                    let writable = rng.below(2) == 0;
                    let user = rng.below(2) == 0;
                    let dirty = rng.below(2) == 0;
                    mem.tlb_insert(linear, phys, writable, user, dirty);
                }
                2 => {
                    let linear = linear_pool[rng.below(pool_len) as usize];
                    mem.invalidate_tlb_page(linear << 12);
                }
                3 => {
                    let phys = rng.below(PHYS_PAGES) + 0x40;
                    let on = rng.below(2) == 0;
                    mem.set_page_watched(phys, on);
                }
                4 => {
                    // Full flush occasionally — must null every projection entry.
                    if rng.below(8) == 0 {
                        mem.invalidate_tlb();
                    }
                    continue;
                }
                _ => unreachable!(),
            }

            // Cheap O(4096) checks after EVERY single mutation: the killing
            // invariant, and O-P1(a)+O-P4 (bias correctness of every live tag).
            // The expensive O-P1(b) full stale-survivor scan runs once per seed
            // (below) — the same per-op-cheap / per-scenario-expensive split the
            // differential harness uses.
            if let Err(e) = mem.oracle_check_smc_fold_any_instant() {
                panic!("seed {seed} step {step}: {e}");
            }
            if let Err(e) = mem.oracle_check_flat_projection() {
                panic!("seed {seed} step {step}: O-P1(a) {e}");
            }
            checked_ops += 1;

            // Non-vacuity: record that we actually reached the interesting state
            // — a watched physical page that has a LIVE hashed entry mapping it
            // (Read view nonzero). Without this, a run where watch-sets and fills
            // never coincide on the same page would pass while checking nothing.
            if !saw_watched_live {
                for &linear in &linear_pool {
                    let e = mem.core.tlb().entries[SoftwareTlb::index(linear)];
                    if e.linear_page == linear
                        && mem.is_page_watched(e.physical_page)
                        && mem.core.tlb().flat().entry(FlatView::Read0, linear) != 0
                    {
                        saw_watched_live = true;
                        break;
                    }
                }
            }
        }

        // O-P1(b) full stale-survivor scan once per seed — the check that catches
        // a broken eviction mirror (a stale survivor has no live tag, so the
        // per-op O-P1(a) cannot see it).
        if let Err(e) = mem.oracle_check_flat_no_stale_survivors() {
            panic!("seed {seed}: {e}");
        }
    }

    assert!(checked_ops > 10_000, "interleaving driver ran too few ops");
    assert!(
        saw_eviction,
        "test is vacuous — the interleaving never caused a collision eviction, \
         so eviction mirroring was never exercised"
    );
    assert!(
        saw_watched_live,
        "test is vacuous — never reached a watched page with a live mapping"
    );
}

#[test]
fn view_layout_constants_match_what_emitted_code_will_assume() {
    // Read-to-write and supervisor-to-user deltas must select the same
    // relative view offsets for both CPL states.
    assert_eq!(FLAT_VIEW_WRITE_DELTA, 0x0080_0000);
    assert_eq!(FLAT_VIEW_CPL3_DELTA, 0x0100_0000);
    let mem = Memory::new();
    let base = mem.flat_views_ptr() as usize;
    assert_ne!(base, 0, "the projection must be allocated");
    assert_eq!(base % 8, 0, "u64 views must be 8-byte aligned");
}

// ---- O3 suppression vs the watch-transition publisher (MDD-BUG-CRUCIBLE-00416) ----

/// Un-watching must not republish write views during O3 suppression.
/// Fill a writable page, arm suppression, watch, then unwatch: all four
/// views must remain null. Without suppression, unwatching must restore
/// the exact biased pointer, as checked by the complementary test above.
#[test]
fn o3_suppression_keeps_write_views_null_across_a_watch_unwatch_cycle() {
    let mut mem = Memory::new();
    let page = 0xB4u32;
    let phys = 0x74u32;
    mem.tlb_insert(page, phys, true, true, true);

    // Precondition, or the test proves nothing: the page starts writable, so
    // there is a nonzero write view available to leak.
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Write0, page),
        0,
        "test is vacuous unless the page starts writable"
    );

    mem.set_flat_projection_suppressed(true);
    assert_all_views_null(&mem, page, "arming O3 nulls the whole projection");

    mem.set_page_watched(phys, true);
    assert_all_views_null(&mem, page, "watching under O3 keeps every view a miss");

    mem.set_page_watched(phys, false);
    assert_all_views_null(
        &mem,
        page,
        "unwatching under O3 must NOT republish a write view — that is the leak",
    );
}

/// Suppression withholds the republish without disturbing the watch bit.
///
/// The fix must not be "skip the transition": eviction and reconciliation still
/// depend on the bookkeeping being correct, and the sweep on TLB slots still
/// has to be counted. Only the nonzero publish is withheld.
#[test]
fn o3_suppression_still_tracks_the_watch_bit_and_counts_the_sweep() {
    let mut mem = Memory::new();
    let page = 0xB5u32;
    let phys = 0x75u32;
    mem.tlb_insert(page, phys, true, true, true);
    mem.set_flat_projection_suppressed(true);

    let scanned_before = mem.core.tlb().flat().watch_slots_scanned();

    mem.set_page_watched(phys, true);
    assert!(
        mem.is_page_watched(phys),
        "the watch bit must still be set under suppression"
    );

    mem.set_page_watched(phys, false);
    assert!(
        !mem.is_page_watched(phys),
        "and cleared again — bookkeeping is unaffected by the sweep"
    );

    assert!(
        mem.core.tlb().flat().watch_slots_scanned() > scanned_before,
        "the transition still scans and counts TLB slots"
    );
}

/// A PT frame keeps its write views null across arming and disarming O3.
///
/// Named for what it actually pins. It does NOT prove the two predicates are
/// independent at the unwatch — while O3 is armed the suppression term alone
/// suffices, so dropping the PT-frame term still passes here. That case is
/// covered by `recorded_pt_frame_nulls_write_views_like_a_watched_page`, which
/// does turn red for exactly that mutation.
#[test]
fn a_pt_frame_stays_write_null_through_o3_arm_and_disarm() {
    let mut mem = Memory::new();
    let page = 0xB6u32;
    let phys = 0x76u32;
    mem.tlb_insert(page, phys, true, true, true);
    mem.record_pt_frame(phys);
    mem.set_flat_projection_suppressed(true);

    mem.set_page_watched(phys, true);
    mem.set_page_watched(phys, false);
    assert_all_views_null(&mem, page, "PT frame and O3 both still demand a miss");

    // Disarming flushes both halves, so the next access re-walks; the PT-frame
    // fold must survive that on its own.
    mem.set_flat_projection_suppressed(false);
    mem.tlb_insert(page, phys, true, true, true);
    assert_eq!(
        mem.core.tlb().flat().entry(FlatView::Write0, page),
        0,
        "a PT frame keeps its write views null once O3 is disarmed"
    );
    assert_ne!(
        mem.core.tlb().flat().entry(FlatView::Read0, page),
        0,
        "reads are never restricted by either predicate"
    );
}

/// A fill that happens WHILE the sweep is armed publishes nothing.
///
/// Arming only nulls the pages already published, so this is the other half of
/// the contract: a page mapped afterwards must not project either. Found by
/// mutation — making the fill path ignore suppression left the whole suite
/// green, because every other O3 test arms after its last fill.
#[test]
fn o3_suppression_also_withholds_a_fill_performed_while_armed() {
    let mut mem = Memory::new();
    let page = 0xB7u32;
    let phys = 0x77u32;

    mem.set_flat_projection_suppressed(true);
    mem.tlb_insert(page, phys, true, true, true);
    assert_all_views_null(&mem, page, "a fill under O3 must publish no view at all");

    // And the hashed half still filled, which is what lets the interpreter keep
    // running the same program by the slow route.
    assert_eq!(
        mem.core.tlb().entries[SoftwareTlb::index(page)].linear_page,
        page,
        "the authoritative hashed tag must still be present"
    );
}

/// The census decomposition of a null view must name the predicate that
/// actually nulled it, one page per cause, and must agree with the view it
/// explains (a non-null view is `Other`).
#[test]
fn flat_null_cause_names_the_predicate_that_nulled_the_view() {
    use crate::flat_tlb::FlatNullCause;
    let mut mem = Memory::new();

    // Empty slot, then a conflict: `q` evicts `p`.
    let p = 0x40;
    let q = colliding_page(p);
    assert_eq!(
        mem.flat_null_cause(p, false, false),
        FlatNullCause::TlbEmpty
    );
    mem.tlb_insert(p, 0x40, true, true, true);
    mem.tlb_insert(q, 0x41, true, true, true);
    assert_eq!(
        mem.flat_null_cause(p, false, false),
        FlatNullCause::TlbConflict
    );
    assert_eq!(mem.flat_null_cause(q, true, true), FlatNullCause::Other);
    assert_ne!(mem.core.tlb().flat().entry(FlatView::Write3, q), 0);

    // Supervisor page at CPL 3; readable at CPL 0.
    mem.tlb_insert(0x42, 0x42, true, false, true);
    assert_eq!(
        mem.flat_null_cause(0x42, false, true),
        FlatNullCause::UserDenied
    );
    assert_eq!(
        mem.flat_null_cause(0x42, false, false),
        FlatNullCause::Other
    );

    // VGA physical range: no host pointer at all.
    mem.tlb_insert(0x43, TLB_VGA_PAGE_START, true, true, true);
    assert_eq!(
        mem.flat_null_cause(0x43, false, false),
        FlatNullCause::NotRam
    );

    // Write-side predicates, each on its own page.
    mem.tlb_insert(0x44, 0x44, false, true, true);
    assert_eq!(
        mem.flat_null_cause(0x44, true, false),
        FlatNullCause::WriteReadOnly
    );
    assert_eq!(
        mem.flat_null_cause(0x44, false, false),
        FlatNullCause::Other
    );
    mem.tlb_insert(0x45, 0x45, true, true, false);
    assert_eq!(
        mem.flat_null_cause(0x45, true, false),
        FlatNullCause::WriteNotDirty
    );
    mem.tlb_insert(0x46, 0x46, true, true, true);
    mem.set_page_watched(0x46, true);
    assert_eq!(
        mem.flat_null_cause(0x46, true, true),
        FlatNullCause::WriteCodeWatched
    );
    assert_eq!(mem.core.tlb().flat().entry(FlatView::Write3, 0x46), 0);
}
