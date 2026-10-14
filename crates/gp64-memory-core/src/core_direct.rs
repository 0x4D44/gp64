//! Policy-free direct-proof generations and guarded-page invalidation.
//!
//! Stable allocations remain owned by the core; machine wrappers retain
//! backing resize, publication and address-classification policy.

use std::cell::Cell;

use crate::{generations::DirectGenerations, memory_core::MemoryCore};

impl MemoryCore {
    #[inline]
    pub fn direct_generations_ptr(&self) -> *const DirectGenerations {
        &*self.direct_generations
    }

    #[inline]
    pub fn direct_generations(&self) -> &DirectGenerations {
        &self.direct_generations
    }

    #[inline]
    pub fn dynrec_descriptor_generation(&self) -> u64 {
        self.descriptor_generation.get()
    }

    #[inline]
    pub fn bump_descriptor_generation(&self) {
        let next = self.descriptor_generation.get().wrapping_add(1);
        self.descriptor_generation.set(next);
        self.direct_generations.record_descriptor_epoch(next);
    }

    pub fn stale_all_guarded_pages(&self) {
        self.direct_generations.bump_proof_word();
        let guarded = self.guarded_pages.borrow();
        for &page in guarded.iter() {
            if let Some(word) = self.page_generation_table.get(page as usize) {
                word.set(word.get().wrapping_add(1));
            }
        }
        self.direct_generation_sweeps
            .set(self.direct_generation_sweeps.get().wrapping_add(1));
        self.direct_swept_pages_total.set(
            self.direct_swept_pages_total
                .get()
                .wrapping_add(guarded.len() as u64),
        );
    }

    /// Invalidate direct-write proofs over one newly protected page. If `physical_page_id` is in
    /// the
    /// guarded set, bump its generation word, then the proof word once, and
    /// return `true`; otherwise nothing moves. The global sweep counters keep
    /// their meaning and do not move. Cold; the PC never calls it.
    #[cold]
    pub fn stale_guarded_page(&self, physical_page_id: u32) -> bool {
        if !self.guarded_pages.borrow().contains(&physical_page_id) {
            return false;
        }
        if let Some(word) = self.page_generation_table.get(physical_page_id as usize) {
            word.set(word.get().wrapping_add(1));
        }
        self.direct_generations.bump_proof_word();
        true
    }

    /// Persistently revoke the write views of every TLB entry mapping
    /// `physical_page`: the stored `host_write`
    /// first, then the flat view. Bumps no epoch. Returns the entries
    /// revoked. Cold; the PC never calls it.
    #[cold]
    pub fn revoke_host_write(&mut self, physical_page: u32) -> usize {
        self.tlb.revoke_host_write(physical_page)
    }

    #[inline]
    pub fn bump_dynrec_source_generation_for_page(&self, physical_page_id: u32) {
        self.direct_generations.bump_source_word();
        if let Some(word) = self.page_generation_table.get(physical_page_id as usize) {
            word.set(word.get().wrapping_add(1));
            self.direct_page_source_bumps
                .set(self.direct_page_source_bumps.get().wrapping_add(1));
        }
    }

    #[inline]
    pub fn page_generation_table_len(&self) -> usize {
        self.page_generation_table.len()
    }

    #[inline]
    pub fn page_generation_word_addr(&self, physical_page_id: u32) -> Option<*const Cell<u32>> {
        self.page_generation_table
            .get(physical_page_id as usize)
            .map(|word| word as *const Cell<u32>)
    }

    #[inline]
    pub fn page_generation_table_ptr(&self) -> (*const Cell<u32>, usize) {
        (
            self.page_generation_table.as_ptr(),
            self.page_generation_table.len(),
        )
    }

    #[inline]
    pub fn page_generation_table_identity(&self) -> u64 {
        self.page_generation_table_identity
    }

    #[inline]
    pub fn page_generation(&self, physical_page_id: u32) -> Option<u32> {
        self.page_generation_table
            .get(physical_page_id as usize)
            .map(Cell::get)
    }

    #[inline]
    pub fn register_guarded_page(&self, physical_page_id: u32) {
        self.guarded_pages.borrow_mut().insert(physical_page_id);
    }

    #[inline]
    pub fn clear_guarded_pages(&self) {
        self.guarded_pages.borrow_mut().clear();
    }

    #[inline]
    pub fn guarded_page_count(&self) -> usize {
        self.guarded_pages.borrow().len()
    }

    #[inline]
    pub fn dynrec_generation_sweeps(&self) -> u64 {
        self.direct_generation_sweeps.get()
    }

    #[inline]
    pub fn dynrec_swept_pages_total(&self) -> u64 {
        self.direct_swept_pages_total.get()
    }

    #[inline]
    pub fn dynrec_page_source_bumps(&self) -> u64 {
        self.direct_page_source_bumps.get()
    }

    #[inline]
    pub fn direct_proofs_live(&self) -> bool {
        self.direct_proofs_live.get()
    }
}
