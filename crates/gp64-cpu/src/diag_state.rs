use crate::ExecutionRingBuffer;

/// CPU diagnostics state (ring buffer, loop detection, sampling).
pub struct DiagState {
    pub enabled: bool,
    pub verbose: bool,
    pub sample_rate: u64,
    pub instr_counter: u64,
    pub loop_threshold: u32,
    pub loop_history: [u32; 8],
    pub loop_pos: u8,
    pub loop_filled: u8,
    pub loop2_count: u32,
    pub loop4_count: u32,
    pub ring: ExecutionRingBuffer,
}

impl DiagState {
    pub fn new() -> Self {
        Self {
            enabled: false,
            verbose: false,
            sample_rate: 10000,
            instr_counter: 0,
            loop_threshold: 100,
            loop_history: [0; 8],
            loop_pos: 0,
            loop_filled: 0,
            loop2_count: 0,
            loop4_count: 0,
            ring: ExecutionRingBuffer::new(),
        }
    }
}

impl Default for DiagState {
    fn default() -> Self {
        Self::new()
    }
}
