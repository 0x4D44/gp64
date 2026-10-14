//! Publication transaction tests.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use gp64_types::SmcNotify;

use crate::testing::NullMachine;
use crate::*;

const RAM_PAGES: usize = 64;

/// A notifier that records every range it is told about.
#[derive(Default)]
struct RangeLog(Mutex<Vec<(usize, usize)>>);

impl SmcNotify for RangeLog {
    fn notify_write(&self, address: usize, size: usize) {
        self.0.lock().unwrap().push((address, address + size));
    }
    fn notify_write_range(&self, start: usize, end: usize) {
        self.0.lock().unwrap().push((start, end));
    }
}

/// Every word a publication may move, as the probe sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Words {
    memory_policy: u64,
    direct_memory_policy: u64,
    dynrec_translation: u64,
    translation_structure: u64,
    alias: u64,
    global_flush: u64,
    pt_write: u64,
    descriptor: u64,
    gens: DirectGenerationsSnapshot,
    page_words: Vec<u32>,
    instr_cache_dirty: bool,
    exec_pages_clear: bool,
    smc_ranges: usize,
}

/// Pages whose per-page words the tests watch: 5 and 6 guarded, 6, 7 and 9
/// affected, 8 untouched.
const WATCHED_PAGES: [u32; 5] = [5, 6, 7, 8, 9];

fn words(core: &MemoryCore, log: &RangeLog) -> Words {
    Words {
        memory_policy: core.memory_policy_epoch(),
        direct_memory_policy: core.direct_memory_policy_epoch(),
        dynrec_translation: core.dynrec_translation_epoch(),
        translation_structure: core.dynrec_translation_structure_epoch(),
        alias: core.alias_generation(),
        global_flush: core.dynrec_global_tlb_flush_epoch(),
        pt_write: core.dynrec_pt_write_epoch(),
        descriptor: core.dynrec_descriptor_generation(),
        gens: core.direct_generations().snapshot(),
        page_words: WATCHED_PAGES
            .iter()
            .map(|&p| core.page_generation(p).unwrap())
            .collect(),
        instr_cache_dirty: core.is_instr_cache_dirty(),
        exec_pages_clear: core.instr_exec_pages().iter().all(|&w| w == 0),
        smc_ranges: log.0.lock().unwrap().len(),
    }
}

const RW_DIRTY: PagePerms = PagePerms {
    writable: true,
    user: true,
    dirty: true,
};

/// A warm core: two direct TLB entries (one global), guarded pages 5 and 6,
/// a GDT snooped on page 7, an executed page, and a recording notifier.
fn warm_machine(global: bool) -> (NullMachine, Arc<RangeLog>) {
    let mut machine = NullMachine::new(RAM_PAGES * 4096);
    let log = Arc::new(RangeLog::default());
    let core = machine.core_mut();
    core.set_gdt_snoop_range(0x7000, 0xFFF);
    for (linear, page) in [(0x10u32, 5u32), (0x11, 6)] {
        // SAFETY: both pages lie inside the machine's RAM.
        unsafe {
            core.tlb_fill(TlbFill {
                linear: LinearPage(linear),
                phys: PhysPage(page),
                perms: RW_DIRTY,
                direct: Some(DirectPage::new(BackingPage(page), true, true)),
            });
        }
        core.register_guarded_page(page);
    }
    if global {
        core.mark_tlb_global(0x10, 5 << 12, false, 0);
    }
    core.instr_exec_pages[0] = 1;
    core.set_direct_proofs_live(true);
    core.bind_smc(Some(log.clone() as Arc<dyn SmcNotify>));
    (machine, log)
}

const AFFECTED: [u32; 3] = [6, 7, 9];
const CHANGED: [(usize, usize); 2] = [(0x6000, 0x8000), (0x9000, 0xA000)];

/// One expected epoch-set delta per kind (LLD §11.3 table):
/// `(memory_policy, direct_memory_policy, dynrec_translation,
/// translation_structure, alias)`.
fn epoch_set_delta(kinds: KindSet) -> [u64; 5] {
    let dt = u64::from(kinds.contains(KindSet::DIRECT_TRANSLATION));
    let alias = u64::from(kinds.contains(KindSet::ALIAS));
    [dt, dt, 1, 1, alias]
}

fn epochs(w: &Words) -> [u64; 5] {
    [
        w.memory_policy,
        w.direct_memory_policy,
        w.dynrec_translation,
        w.translation_structure,
        w.alias,
    ]
}

fn delta(after: &[u32], before: &[u32]) -> Vec<u32> {
    after.iter().zip(before).map(|(a, b)| a - b).collect()
}

fn add(a: [u64; 5], b: [u64; 5]) -> [u64; 5] {
    std::array::from_fn(|i| a[i] + b[i])
}

/// The counter test (§12.3, §13.2): per kind, the stages run in order, nothing
/// observable moves before the sever stage, and every global word moves by
/// exactly one whatever the number of pages.
#[test]
fn publication_stages_run_in_order_and_bump_each_word_once() {
    for kinds in [KindSet::DIRECT_TRANSLATION, KindSet::ALIAS, KindSet::BOTH] {
        let (mut machine, log) = warm_machine(true);
        let core = machine.core_mut();
        let before = words(core, &log);
        assert!(core.tlb_lookup(0x10).is_some() && core.flat_read_published(0x11));

        let mut seen: Vec<(PublicationStage, Words, bool, bool)> = Vec::new();
        let receipt = core.begin_publication(kinds).commit_probed(
            &PublicationPlan {
                affected_pages: &AFFECTED,
                changed_ranges: &CHANGED,
            },
            &mut |stage, core| {
                seen.push((
                    stage,
                    words(core, &log),
                    core.tlb_lookup(0x10).is_some() || core.flat_read_published(0x11),
                    core.direct_proofs_live(),
                ));
            },
        );

        let stages: Vec<_> = seen.iter().map(|s| s.0).collect();
        assert_eq!(
            stages,
            [
                PublicationStage::Revoked,
                PublicationStage::Severed,
                PublicationStage::PendingBumpsApplied,
                PublicationStage::EpochSetApplied,
                PublicationStage::GlobalFlushEpoch,
                PublicationStage::DecodeCacheCleared,
                PublicationStage::SmcNotified,
            ],
            "{kinds:?}"
        );
        // Revocation is complete at the first stage, and nothing observable
        // has moved through the sever stage.
        for (stage, w, translations_live, proofs_live) in &seen[..2] {
            assert!(!translations_live, "{kinds:?} {stage:?}");
            assert!(!proofs_live, "{kinds:?} {stage:?}");
            assert_eq!(w, &before, "{kinds:?} {stage:?}: a word moved before sever");
        }

        // The pending bumps: per-page words (guarded 5, 6 and affected 6, 7,
        // 9, each once; 8 untouched), then each global word once.
        let bumps = &seen[2].1;
        assert_eq!(
            delta(&bumps.page_words, &before.page_words),
            vec![1, 1, 1, 0, 1],
            "{kinds:?}"
        );
        let g = &before.gens;
        assert_eq!(bumps.gens.proof_generation, g.proof_generation + 1);
        assert_eq!(bumps.gens.source_generation, g.source_generation + 1);
        assert_eq!(
            bumps.descriptor,
            before.descriptor + 1,
            "GDT page 7 changed"
        );
        assert_eq!(bumps.gens.descriptor_generation as u64, bumps.descriptor);
        assert_eq!(bumps.pt_write, before.pt_write + 1);
        assert_eq!(epochs(bumps), epochs(&before), "{kinds:?}");
        assert_eq!(bumps.global_flush, before.global_flush);

        // The epoch set, once, per the table.
        let set = &seen[3].1;
        assert_eq!(
            epochs(set),
            add(epochs(&before), epoch_set_delta(kinds)),
            "{kinds:?}"
        );
        assert_eq!(set.gens.policy_generation as u64, set.direct_memory_policy);
        assert_eq!(set.gens.map_generation as u64, set.dynrec_translation);
        assert_eq!(set.gens.a20_generation as u64, set.alias);
        assert_eq!(set.global_flush, before.global_flush);
        assert_eq!(
            (set.gens.proof_generation, set.gens.source_generation),
            (bumps.gens.proof_generation, bumps.gens.source_generation)
        );

        // The global flush epoch, from the pre-flush snapshot.
        assert_eq!(seen[4].1.global_flush, before.global_flush + 1);

        // The decode cache, then the notifier over exactly the changed ranges.
        let cleared = &seen[5].1;
        assert!(cleared.instr_cache_dirty && cleared.exec_pages_clear);
        assert_eq!(
            cleared.smc_ranges, 0,
            "notifier ran before the decode clear"
        );
        let notified = &seen[6].1;
        assert_eq!(*log.0.lock().unwrap(), CHANGED.to_vec());

        // Nothing moved after the stage that owns it.
        assert_eq!(&words(core, &log), notified);
        assert!(core.take_gdt_dirty());
        assert_eq!(
            receipt,
            PublicationReceipt {
                kinds,
                revoked_linear_pages: 2,
                page_words_bumped: 4,
                descriptor_tables_marked: true,
                global_flush_epoch_bumped: true,
            }
        );
    }
}

/// The global word moves once for many pages: the commit must not take the
/// per-page `bump_dynrec_source_generation_for_page` route (§13.2 mutant).
#[test]
fn many_affected_pages_move_each_global_word_once() {
    let (mut machine, log) = warm_machine(false);
    let core = machine.core_mut();
    let before = words(core, &log);
    let pages: Vec<u32> = (0..RAM_PAGES as u32).collect();
    let receipt = core
        .begin_publication(KindSet::DIRECT_TRANSLATION)
        .commit(&PublicationPlan {
            affected_pages: &pages,
            changed_ranges: &[],
        });
    let after = words(core, &log);
    assert_eq!(
        after.gens.source_generation,
        before.gens.source_generation + 1
    );
    assert_eq!(
        after.gens.proof_generation,
        before.gens.proof_generation + 1
    );
    assert_eq!(after.pt_write, before.pt_write + 1);
    assert_eq!(
        delta(&after.page_words, &before.page_words),
        vec![1; WATCHED_PAGES.len()]
    );
    assert_eq!(receipt.page_words_bumped, RAM_PAGES);
    // No changed range touched the GDT, and no entry was global.
    assert_eq!(after.descriptor, before.descriptor);
    assert!(!receipt.descriptor_tables_marked);
    assert_eq!(after.global_flush, before.global_flush);
    assert!(!receipt.global_flush_epoch_bumped);
    assert!(log.0.lock().unwrap().is_empty());
}

/// P2 (§11.3, amendment A-M5-2): a live engine entry refuses publication in
/// every build; leaving the engine allows it.
#[test]
fn a_live_engine_entry_refuses_publication() {
    let mut machine = NullMachine::new(RAM_PAGES * 4096);
    let entry = machine.core().enter_engine();
    let nested = machine.core().enter_engine();
    assert_eq!(machine.core().engine_depth(), 2);
    let core = machine.core_mut();
    let refused = catch_unwind(AssertUnwindSafe(|| {
        let _ = core.begin_publication(KindSet::ALIAS);
    }));
    assert!(refused.is_err());
    // The refusal happened before a publication existed, so nothing poisoned.
    assert!(!core.publication_poisoned());
    drop(nested);
    drop(entry);
    assert_eq!(core.engine_depth(), 0);
    let receipt = core
        .begin_publication(KindSet::ALIAS)
        .commit(&PublicationPlan::default());
    assert_eq!(receipt.kinds, KindSet::ALIAS);
}

/// A panic between begin and the receipt poisons the core: no engine entry and
/// no further publication, ever.
#[test]
fn an_unwound_publication_poisons_the_core() {
    let mut machine = NullMachine::new(RAM_PAGES * 4096);
    let core = machine.core_mut();
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let _publication = core.begin_publication(KindSet::DIRECT_TRANSLATION);
        panic!("the machine's install step failed");
    }));
    assert!(unwound.is_err());
    assert!(core.publication_poisoned());
    assert!(catch_unwind(AssertUnwindSafe(|| core.enter_engine())).is_err());
    assert_eq!(core.engine_depth(), 0);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _ = core.begin_publication(KindSet::ALIAS);
    }))
    .is_err());
}

/// A commit that completes leaves the core clean.
#[test]
fn a_committed_publication_does_not_poison() {
    let mut machine = NullMachine::new(RAM_PAGES * 4096);
    let core = machine.core_mut();
    let _ = core
        .begin_publication(KindSet::BOTH)
        .commit(&PublicationPlan::default());
    assert!(!core.publication_poisoned());
    drop(core.enter_engine());
}

/// The plan's page list must ascend without duplicates; a malformed plan is a
/// fatal contract break that poisons the core.
#[test]
fn an_unordered_plan_is_refused_and_poisons() {
    for pages in [[7u32, 6], [6, 6]] {
        let mut machine = NullMachine::new(RAM_PAGES * 4096);
        let core = machine.core_mut();
        let refused = catch_unwind(AssertUnwindSafe(|| {
            let _ = core
                .begin_publication(KindSet::ALIAS)
                .commit(&PublicationPlan {
                    affected_pages: &pages,
                    changed_ranges: &[],
                });
        }));
        assert!(refused.is_err(), "{pages:?}");
        assert!(core.publication_poisoned());
    }
}

/// The in-commit reprojection hook (LLD §12.3 step 3, §14.3 as built): it
/// runs once, after the revoke and before the `Revoked` probe, so before the
/// sever; nothing observable moves through the sever stage; step 5 installs
/// the returned ranges and hull without a flush or policy bump, sets the
/// moved tables' dirty flags, and the descriptor generation moves exactly
/// once.
#[test]
fn the_reprojection_hook_runs_before_sever_and_bumps_the_descriptor_once() {
    for kinds in [KindSet::DIRECT_TRANSLATION, KindSet::ALIAS] {
        let (mut machine, log) = warm_machine(false);
        let core = machine.core_mut();
        let _ = core.take_gdt_dirty();
        let before = words(core, &log);
        let calls = std::cell::Cell::new(0usize);
        let mut seen: Vec<(PublicationStage, Words, usize)> = Vec::new();
        let receipt = core.begin_publication(kinds).commit_reprojecting_probed(
            &PublicationPlan {
                affected_pages: &[9],
                changed_ranges: &[(0x9000, 0xA000)],
            },
            &mut |core| {
                calls.set(calls.get() + 1);
                // The revoke is complete and no word has moved yet.
                assert!(core.tlb_lookup(0x10).is_none() && !core.flat_read_published(0x11));
                assert!(!core.direct_proofs_live());
                assert_eq!(
                    words(core, &log),
                    before,
                    "{kinds:?}: moved before the hook"
                );
                DescriptorReprojection {
                    gdt_range: Some((0xB000, 0xB100)),
                    ldt_range: None,
                    gdt_moved: true,
                    ldt_moved: true,
                }
            },
            &mut |stage, core| {
                seen.push((stage, words(core, &log), calls.get()));
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(seen[0].0, PublicationStage::Revoked);
        assert_eq!(
            seen[0].2, 1,
            "{kinds:?}: the hook ran after the Revoked probe"
        );
        for (stage, w, _) in &seen[..2] {
            assert_eq!(w, &before, "{kinds:?} {stage:?}: a word moved before sever");
        }
        let bumps = &seen[2].1;
        assert_eq!(seen[2].0, PublicationStage::PendingBumpsApplied);
        assert_eq!(
            bumps.descriptor,
            before.descriptor + 1,
            "{kinds:?}: one descriptor bump"
        );
        assert_eq!(
            (bumps.memory_policy, bumps.dynrec_translation),
            (before.memory_policy, before.dynrec_translation),
            "{kinds:?}: the hull change bumped a policy word"
        );
        assert_eq!(
            (core.gdt_snoop_base(), core.gdt_snoop_end()),
            (0xB000, 0xB100)
        );
        assert_eq!(core.snoop_hull(), (0xB000, 0xB100));
        assert!(core.take_gdt_dirty());
        assert!(core.take_ldt_dirty());
        assert!(receipt.descriptor_tables_marked);
        let after = words(core, &log);
        assert_eq!(after.descriptor, before.descriptor + 1, "{kinds:?}");
    }
}
