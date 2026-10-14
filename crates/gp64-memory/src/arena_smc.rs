//! Process-seam code coherence for the shared flat-memory arena.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

use crate::arena::{ARENA_PHYS_END, ARENA_PHYS_START};

const LOG_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaWrite {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Default, Clone)]
pub struct ArenaSmc {
    watches: BTreeMap<u32, BTreeSet<u32>>,
    counts: BTreeMap<u32, u32>,
    log: VecDeque<(u64, ArenaWrite)>,
    next_sequence: u64,
}

impl ArenaSmc {
    pub fn watch(&mut self, pid: u32, page: u32, on: bool) -> bool {
        if !((ARENA_PHYS_START >> 12)..(ARENA_PHYS_END >> 12)).contains(&(page as usize)) {
            return false;
        }
        let pages = self.watches.entry(pid).or_default();
        if pages.contains(&page) == on {
            return false;
        }
        if on {
            pages.insert(page);
            *self.counts.entry(page).or_default() += 1;
        } else {
            pages.remove(&page);
            let count = self
                .counts
                .get_mut(&page)
                .expect("watched arena page has count");
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&page);
            }
        }
        true
    }

    pub fn remove_process(&mut self, pid: u32) {
        if let Some(pages) = self.watches.remove(&pid) {
            for page in pages {
                let count = self
                    .counts
                    .get_mut(&page)
                    .expect("watched arena page has count");
                *count -= 1;
                if *count == 0 {
                    self.counts.remove(&page);
                }
            }
        }
    }

    pub fn remote_pages(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        self.counts
            .keys()
            .copied()
            .filter(move |page| self.remote_watches(pid, *page))
    }

    pub fn remote_watches(&self, pid: u32, page: u32) -> bool {
        self.counts.get(&page).copied().unwrap_or(0)
            > u32::from(
                self.watches
                    .get(&pid)
                    .is_some_and(|pages| pages.contains(&page)),
            )
    }

    pub fn interest_in(&self, pid: u32, range: Range<u32>) -> (bool, bool) {
        if range.start >= range.end {
            return (false, false);
        }
        let pages = (range.start >> 12)..=((range.end - 1) >> 12);
        let local = self
            .watches
            .get(&pid)
            .is_some_and(|owned| owned.range(pages.clone()).next().is_some());
        let remote = self
            .counts
            .range(pages)
            .any(|(&page, _)| self.remote_watches(pid, page));
        (local, remote)
    }

    pub fn append(&mut self, range: Range<u32>) {
        if range.start >= range.end {
            return;
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("arena log sequence exhausted");
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back((
            self.next_sequence,
            ArenaWrite {
                start: range.start,
                end: range.end,
            },
        ));
    }

    pub fn cursor(&self) -> u64 {
        self.next_sequence
    }

    /// Returns `None` if the caller must flush its arena blocks after a wrap.
    pub fn since(&self, cursor: u64) -> Option<Vec<ArenaWrite>> {
        if cursor > self.next_sequence
            || self
                .log
                .front()
                .is_some_and(|(first, _)| cursor < first.saturating_sub(1))
        {
            return None;
        }
        Some(
            self.log
                .iter()
                .filter(|(sequence, _)| *sequence > cursor)
                .map(|(_, write)| *write)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arena::ArenaStore, Memory};
    use std::sync::{Arc, Mutex};

    #[test]
    fn local_eviction_keeps_remote_interest_and_exit_releases_it() {
        let page = (ARENA_PHYS_START >> 12) as u32;
        let mut smc = ArenaSmc::default();
        assert!(smc.watch(1, page, true));
        assert!(smc.watch(2, page, true));
        assert!(!smc.watch(2, page, true));
        assert_eq!(
            smc.interest_in(1, (page << 12)..((page + 1) << 12)),
            (true, true)
        );
        assert!(smc.watch(1, page, false));
        assert_eq!(
            smc.interest_in(1, (page << 12)..((page + 1) << 12)),
            (false, true)
        );
        smc.remove_process(2);
        assert_eq!(
            smc.interest_in(1, (page << 12)..((page + 1) << 12)),
            (false, false)
        );
    }

    #[test]
    fn wrap_requires_arena_only_flush_and_exact_spans_survive_before_wrap() {
        let mut smc = ArenaSmc::default();
        let initial = smc.cursor();
        let first = ARENA_PHYS_START as u32 + 0x123;
        smc.append(first..first + 3);
        assert_eq!(
            smc.since(initial).unwrap(),
            vec![ArenaWrite {
                start: first,
                end: first + 3
            }]
        );
        let recent = smc.cursor();
        for n in 0..LOG_CAPACITY {
            let addr = ARENA_PHYS_START as u32 + ((n as u32) << 12);
            smc.append(addr..addr + 1);
        }
        assert!(smc.since(initial).is_none());
        assert_eq!(smc.since(recent).unwrap().len(), LOG_CAPACITY);
    }

    #[test]
    fn remote_interpreter_watch_reaches_empty_writer_cache_and_hle_index() {
        let (store, token) = ArenaStore::new().unwrap();
        let (mut reader, mut writer) = (Memory::new(), Memory::new());
        reader.attach_arena_store(store.clone()).unwrap();
        writer.attach_arena_store(store).unwrap();
        let physical = ARENA_PHYS_START + 0x2000;
        reader.give_arena_admission(token);
        reader
            .commit_arena_host_range(physical..physical + 0x1000)
            .unwrap();
        let shared = Arc::new(Mutex::new(ArenaSmc::default()));
        reader.attach_arena_smc(2, shared.clone());
        reader.mark_instr_exec_page(physical - 0x00FF_0000);
        assert!(reader.is_page_watched((physical >> 12) as u32));
        reader.park_arena_smc();
        let token = reader.take_arena_admission().unwrap();
        writer.give_arena_admission(token);
        writer.attach_arena_smc(1, shared.clone());
        assert!(!writer.smc_has_code_pages());
        assert!(writer.is_page_watched((physical >> 12) as u32));
        writer.set_page_watched((physical >> 12) as u32, true);
        writer.set_page_watched((physical >> 12) as u32, false);
        assert!(writer.is_page_watched((physical >> 12) as u32));
        let span = ArenaWrite {
            start: physical as u32,
            end: physical as u32 + 1,
        };
        // The store funnel logs (an interpreter store with a snoop bound)...
        assert!(writer.smc_before_write(physical, 1));
        assert_eq!(shared.lock().unwrap().since(0).unwrap(), vec![span]);
        // ...and so does an HLE write through `IndexMut`, on its own.
        writer.ram[physical] = 0x90;
        assert_eq!(shared.lock().unwrap().since(1).unwrap(), vec![span]);
    }

    #[test]
    fn hle_write_straddling_into_remote_code_page_logs_full_span() {
        let (store, token) = ArenaStore::new().unwrap();
        let (mut reader, mut writer) = (Memory::new(), Memory::new());
        reader.attach_arena_store(store.clone()).unwrap();
        writer.attach_arena_store(store).unwrap();
        let physical = ARENA_PHYS_START + 0x4000;
        reader.give_arena_admission(token);
        reader
            .commit_arena_host_range(physical..physical + 0x2000)
            .unwrap();
        let shared = Arc::new(Mutex::new(ArenaSmc::default()));
        reader.attach_arena_smc(1, shared.clone());
        reader.mark_instr_exec_page(physical + 0x1000 - 0x00FF_0000);
        reader.park_arena_smc();
        let token = reader.take_arena_admission().unwrap();
        writer.give_arena_admission(token);
        writer.attach_arena_smc(2, shared.clone());
        assert!(!writer.smc_has_code_pages());
        writer.ram[physical + 0xfff..physical + 0x1001].copy_from_slice(&[0x90, 0xc3]);
        assert_eq!(
            shared.lock().unwrap().since(0).unwrap(),
            vec![ArenaWrite {
                start: (physical + 0xfff) as u32,
                end: (physical + 0x1001) as u32,
            }]
        );
    }
}
