//! BIOS Data Area helpers (0040:xxxx)
//!
//! Helpers for keyboard ring buffer and shift flags in the BDA.
//! Used by the keyboard module to maintain BDA state for programs
//! that poll the BDA directly rather than using BIOS services.

use gp64_memory::Memory;

const BDA_BASE: usize = 0x400;

const KBD_SHIFT_FLAGS: usize = BDA_BASE + 0x0017;
/// Left Ctrl/Alt, SysRq, Pause and pressed lock-key flags (0040:0018).
const KBD_EXT_SHIFT_FLAGS: usize = BDA_BASE + 0x0018;
/// Enhanced keyboard flags, including right Ctrl/Alt in bits 2/3.
const KBD_KEYBOARD_FLAGS: usize = BDA_BASE + 0x0096;
const KBD_HEAD_PTR: usize = BDA_BASE + 0x001A;
const KBD_TAIL_PTR: usize = BDA_BASE + 0x001C;

// Keyboard buffer bounds pointers (offsets in segment 0x0040).
const KBD_BUF_START_PTR: usize = BDA_BASE + 0x0080;
const KBD_BUF_END_PTR: usize = BDA_BASE + 0x0082;

// Public exports for external use (e.g., BIOS INT 16h/13h handlers)
/// BDA keyboard buffer head pointer address
pub const BDA_KBD_BUFFER_HEAD: usize = KBD_HEAD_PTR;
/// BDA keyboard buffer tail pointer address
pub const BDA_KBD_BUFFER_TAIL: usize = KBD_TAIL_PTR;
/// BDA keyboard buffer start offset pointer address
pub const BDA_KBD_BUFFER_START: usize = KBD_BUF_START_PTR;
/// BDA keyboard buffer end offset pointer address
pub const BDA_KBD_BUFFER_END: usize = KBD_BUF_END_PTR;

// Defaults: ring of 16 words => 32 bytes => offsets 0x001E..0x003D, end marker 0x003E.
const KBD_BUF_START_DEFAULT: u16 = 0x001E;
const KBD_BUF_END_DEFAULT: u16 = 0x003E;

fn kbd_buf_range(memory: &Memory) -> (u16, u16) {
    // These are offsets within the 0x40 segment, not linear addresses.
    let mut start = memory.read_u16(KBD_BUF_START_PTR);
    let mut end = memory.read_u16(KBD_BUF_END_PTR);

    // Basic sanity:
    // - start/end must be even (word aligned)
    // - end must be strictly greater than start
    // - the ring must be at least 2 bytes (1 word) and fit within the BDA page
    let valid =
        (start & 1) == 0 && (end & 1) == 0 && end > start && (end - start) >= 2 && end <= 0x00FF;

    if !valid {
        start = KBD_BUF_START_DEFAULT;
        end = KBD_BUF_END_DEFAULT;
    }
    (start, end)
}

fn sanitize_kbd_ptr(p: u16, start: u16, end: u16) -> u16 {
    // BIOS head/tail pointers are offsets in [start, end). `end` is the exclusive wrap marker.
    if (p & 1) != 0 || p < start || p >= end {
        start
    } else {
        p
    }
}

fn kbd_next(p: u16, start: u16, end: u16) -> u16 {
    let next = p.wrapping_add(2);
    if next >= end {
        start
    } else {
        next
    }
}

pub fn kbd_get_shift_flags(memory: &Memory) -> u8 {
    memory.read_u8(KBD_SHIFT_FLAGS)
}

pub fn kbd_set_shift_flags(memory: &mut Memory, flags: u8) {
    memory.write_u8(KBD_SHIFT_FLAGS, flags);
}

pub fn kbd_get_extended_shift_flags(memory: &Memory) -> u8 {
    memory.read_u8(KBD_EXT_SHIFT_FLAGS)
}

pub fn kbd_set_extended_shift_flags(memory: &mut Memory, flags: u8) {
    memory.write_u8(KBD_EXT_SHIFT_FLAGS, flags);
}

pub fn kbd_get_keyboard_flags(memory: &Memory) -> u8 {
    memory.read_u8(KBD_KEYBOARD_FLAGS)
}

pub fn kbd_set_keyboard_flags(memory: &mut Memory, flags: u8) {
    memory.write_u8(KBD_KEYBOARD_FLAGS, flags);
}

/// INT 16h AH=12h status combines both BDA flag bytes. SysRq moves from
/// 0040:0018 bit 2 to AH bit 7; Pause and Insert-down are not returned.
pub fn kbd_get_enhanced_shift_flags(memory: &Memory) -> u8 {
    let left = kbd_get_extended_shift_flags(memory);
    (left & 0x73) | (kbd_get_keyboard_flags(memory) & 0x0c) | ((left & 0x04) << 5)
}

pub fn kbd_peek(memory: &Memory) -> Option<(u8, u8)> {
    let (start, end) = kbd_buf_range(memory);
    let head = sanitize_kbd_ptr(memory.read_u16(KBD_HEAD_PTR), start, end);
    let tail = sanitize_kbd_ptr(memory.read_u16(KBD_TAIL_PTR), start, end);
    if head == tail {
        return None;
    }
    let addr = BDA_BASE + (head as usize);
    let word = memory.read_u16(addr);
    Some(((word >> 8) as u8, (word & 0xFF) as u8))
}

pub fn kbd_dequeue(memory: &mut Memory) -> Option<(u8, u8)> {
    let (start, end) = kbd_buf_range(memory);
    let head = sanitize_kbd_ptr(memory.read_u16(KBD_HEAD_PTR), start, end);
    let tail = sanitize_kbd_ptr(memory.read_u16(KBD_TAIL_PTR), start, end);
    if head == tail {
        return None;
    }
    let addr = BDA_BASE + (head as usize);
    let word = memory.read_u16(addr);
    let next_head = kbd_next(head, start, end);
    memory.write_u16(KBD_HEAD_PTR, next_head);
    if crate::kbd_flow_trace_enabled() {
        tracing::info!(
            target: "gp64::kbd::flow",
            scancode = format_args!("{:02X}", (word >> 8) as u8),
            ascii = format_args!("{:02X}", (word & 0xFF) as u8),
            head = format_args!("{:04X}", head),
            next_head = format_args!("{:04X}", next_head),
            tail = format_args!("{:04X}", tail),
            "BDA_KBD_DEQUEUE"
        );
    }
    Some(((word >> 8) as u8, (word & 0xFF) as u8))
}

pub fn kbd_enqueue(memory: &mut Memory, scancode: u8, ascii: u8) -> bool {
    if scancode == 0 && ascii == 0 {
        return true;
    }
    let (start, end) = kbd_buf_range(memory);
    let head = sanitize_kbd_ptr(memory.read_u16(KBD_HEAD_PTR), start, end);
    let mut tail = sanitize_kbd_ptr(memory.read_u16(KBD_TAIL_PTR), start, end);
    let next_tail = kbd_next(tail, start, end);
    if next_tail == head {
        tracing::debug!(
            target: "gp64::bda::kbd",
            scancode = format_args!("{:02X}", scancode),
            ascii = format_args!("{:02X}", ascii),
            "BDA_KBD_ENQUEUE: buffer full"
        );
        if crate::kbd_flow_trace_enabled() {
            tracing::info!(
                target: "gp64::kbd::flow",
                scancode = format_args!("{:02X}", scancode),
                ascii = format_args!("{:02X}", ascii),
                head = format_args!("{:04X}", head),
                tail = format_args!("{:04X}", tail),
                "BDA_KBD_FULL"
            );
        }
        return false;
    }
    let addr = BDA_BASE + (tail as usize);
    let word = ((scancode as u16) << 8) | (ascii as u16);
    memory.write_u16(addr, word);
    tracing::debug!(
        target: "gp64::bda::kbd",
        scancode = format_args!("{:02X}", scancode),
        ascii = format_args!("{:02X}", ascii),
        head = format_args!("{:04X}", head),
        tail = format_args!("{:04X}", next_tail),
        "BDA_KBD_ENQUEUE"
    );
    tail = next_tail;
    memory.write_u16(KBD_TAIL_PTR, tail);
    if crate::kbd_flow_trace_enabled() {
        tracing::info!(
            target: "gp64::kbd::flow",
            scancode = format_args!("{:02X}", scancode),
            ascii = format_args!("{:02X}", ascii),
            head = format_args!("{:04X}", head),
            tail = format_args!("{:04X}", tail),
            "BDA_KBD_ENQUEUE"
        );
    }
    true
}

/// Write the ring's control words back in their validated form, so code that
/// reads them RAW sees exactly the state `kbd_peek` / `kbd_dequeue` compute.
///
/// Returns `true` if anything was rewritten.
///
/// The four INT 16h ROM stubs (`Bios::INT16_WAIT_00/10`, `INT16_PEEK_01/11`)
/// are real 8086 code modelled on the IBM AT BIOS: they read head, tail, start
/// and end straight out of the BDA and apply only the raw end/start wrap. The
/// Rust helpers above instead sanitize odd or out-of-range pointers and fall
/// back to the default bounds when the range itself is nonsense. Identical BDA
/// state therefore produced two different answers depending on whether INT 16h
/// stayed in HLE or entered a stub — with default bounds, raw head 0000h and
/// tail 001Eh, the helpers report an empty ring while the stub reports a key,
/// returns the word at 0040:0000 (normally the COM1 port base) and writes a
/// head of 0002h back (MDD-BUG-CRUCIBLE-00429).
///
/// Normalising here keeps ONE validator — this module — rather than
/// duplicating its rules in four hand-assembled stubs where they would drift.
/// It is deliberately narrow: valid state is left byte-for-byte alone, so a
/// guest running a legitimate enlarged ring (a keyboard-buffer TSR) is
/// untouched. Only values the helpers already refuse to honour are rewritten.
pub fn kbd_normalize_ring(memory: &mut Memory) -> bool {
    let (start, end) = kbd_buf_range(memory);
    let mut changed = false;

    // `kbd_buf_range` silently substitutes the defaults for an invalid range;
    // persist that substitution so a raw reader wraps on the same bounds.
    for (addr, value) in [(KBD_BUF_START_PTR, start), (KBD_BUF_END_PTR, end)] {
        if memory.read_u16(addr) != value {
            memory.write_u16(addr, value);
            changed = true;
        }
    }

    for addr in [KBD_HEAD_PTR, KBD_TAIL_PTR] {
        let raw = memory.read_u16(addr);
        let sane = sanitize_kbd_ptr(raw, start, end);
        if raw != sane {
            memory.write_u16(addr, sane);
            changed = true;
        }
    }

    if changed {
        tracing::debug!(
            target: "gp64::bda::kbd",
            start = format_args!("{:04X}", start),
            end = format_args!("{:04X}", end),
            head = format_args!("{:04X}", memory.read_u16(KBD_HEAD_PTR)),
            tail = format_args!("{:04X}", memory.read_u16(KBD_TAIL_PTR)),
            "BDA_KBD_NORMALIZE"
        );
    }
    changed
}

pub fn kbd_clear(memory: &mut Memory) {
    let (start, _end) = kbd_buf_range(memory);
    memory.write_u16(KBD_HEAD_PTR, start);
    memory.write_u16(KBD_TAIL_PTR, start);
}

pub fn kbd_debug_pointers(memory: &Memory) -> (u16, u16, u16, u16) {
    let (start, end) = kbd_buf_range(memory);
    let head = memory.read_u16(KBD_HEAD_PTR);
    let tail = memory.read_u16(KBD_TAIL_PTR);
    (start, end, head, tail)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tracing_subscriber::EnvFilter;

    fn init_custom_ring(memory: &mut Memory, start: u16, end: u16, head: u16, tail: u16) {
        memory.write_u16(KBD_BUF_START_PTR, start);
        memory.write_u16(KBD_BUF_END_PTR, end);
        memory.write_u16(KBD_HEAD_PTR, head);
        memory.write_u16(KBD_TAIL_PTR, tail);
    }

    #[test]
    fn test_kbd_buf_range_invalid_bounds_fallback_to_defaults() {
        let mut memory = Memory::new();

        for (start, end) in [
            (0x001F, 0x003E), // odd start
            (0x001E, 0x003F), // odd end
            (0x003E, 0x001E), // end < start
            (0x001E, 0x001E), // end == start
            (0x00FE, 0x0100), // end > 0x00FF
        ] {
            init_custom_ring(&mut memory, start, end, start, start);
            let (actual_start, actual_end, _, _) = kbd_debug_pointers(&memory);
            assert_eq!(actual_start, KBD_BUF_START_DEFAULT);
            assert_eq!(actual_end, KBD_BUF_END_DEFAULT);
        }
    }

    #[test]
    fn test_kbd_peek_sanitizes_invalid_head_tail_to_start() {
        let mut memory = Memory::new();
        init_custom_ring(&mut memory, 0x0020, 0x0028, 0x0021, 0x001E); // odd head, tail < start

        assert_eq!(kbd_peek(&memory), None);
    }

    #[test]
    fn test_kbd_enqueue_wraps_tail_when_reaching_end_marker() {
        let mut memory = Memory::new();
        let start = 0x0020;
        let end = 0x0028;
        init_custom_ring(&mut memory, start, end, start + 2, end - 2);

        assert!(kbd_enqueue(&mut memory, 0x9A, 0xBC));
        assert_eq!(memory.read_u16(KBD_TAIL_PTR), start);
        assert_eq!(memory.read_u16(BDA_BASE + (end - 2) as usize), 0x9ABC);
    }

    #[test]
    fn test_kbd_dequeue_trace_enabled_path() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::kbd::flow=info"))
            .with_test_writer()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut memory = Memory::new();
        kbd_clear(&mut memory);
        assert!(kbd_enqueue(&mut memory, 0x1C, 0x0D));

        let head_before = memory.read_u16(KBD_HEAD_PTR);
        assert_eq!(kbd_dequeue(&mut memory), Some((0x1C, 0x0D)));
        assert_eq!(
            memory.read_u16(KBD_HEAD_PTR),
            kbd_next(head_before, KBD_BUF_START_DEFAULT, KBD_BUF_END_DEFAULT)
        );
    }

    #[test]
    fn test_kbd_enqueue_full_trace_enabled_path() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::kbd::flow=info"))
            .with_test_writer()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut memory = Memory::new();
        kbd_clear(&mut memory);

        for i in 0..15u8 {
            assert!(kbd_enqueue(&mut memory, 0x20 + i, 0x30 + i));
        }

        assert!(!kbd_enqueue(&mut memory, 0xEE, 0xFF));
        assert_eq!(kbd_peek(&memory), Some((0x20, 0x30)));
    }

    #[test]
    fn test_kbd_enqueue_zero_scancode_nonzero_ascii_is_not_noop() {
        let mut memory = Memory::new();
        kbd_clear(&mut memory);

        assert!(kbd_enqueue(&mut memory, 0x00, 0x41));
        assert_eq!(kbd_peek(&memory), Some((0x00, 0x41)));
    }

    #[test]
    fn test_kbd_enqueue_both_zero_is_noop_true() {
        let mut memory = Memory::new();
        kbd_clear(&mut memory);
        let head_before = memory.read_u16(KBD_HEAD_PTR);
        let tail_before = memory.read_u16(KBD_TAIL_PTR);

        assert!(kbd_enqueue(&mut memory, 0x00, 0x00));
        assert_eq!(memory.read_u16(KBD_HEAD_PTR), head_before);
        assert_eq!(memory.read_u16(KBD_TAIL_PTR), tail_before);
    }

    #[test]
    fn test_kbd_peek_sanitizes_pointer_at_end_to_start() {
        let mut memory = Memory::new();
        init_custom_ring(&mut memory, 0x0020, 0x0028, 0x0028, 0x0028); // head/tail >= end
        assert_eq!(kbd_peek(&memory), None);
    }

    #[test]
    fn test_kbd_dequeue_empty_returns_none() {
        let mut memory = Memory::new();
        kbd_clear(&mut memory);
        assert_eq!(kbd_dequeue(&mut memory), None);
    }

    #[test]
    fn test_kbd_clear_uses_custom_ring_start() {
        let mut memory = Memory::new();
        init_custom_ring(&mut memory, 0x0030, 0x0040, 0x0038, 0x003C);

        kbd_clear(&mut memory);

        assert_eq!(memory.read_u16(KBD_HEAD_PTR), 0x0030);
        assert_eq!(memory.read_u16(KBD_TAIL_PTR), 0x0030);
    }

    #[test]
    fn test_kbd_enqueue_debug_trace_success_path() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::bda::kbd=debug"))
            .with_test_writer()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut memory = Memory::new();
        kbd_clear(&mut memory);

        assert!(kbd_enqueue(&mut memory, 0x2A, 0x3B));
        assert_eq!(kbd_peek(&memory), Some((0x2A, 0x3B)));
    }

    #[test]
    fn test_kbd_enqueue_debug_trace_full_path() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::bda::kbd=debug"))
            .with_test_writer()
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut memory = Memory::new();
        kbd_clear(&mut memory);

        for i in 0..15u8 {
            assert!(kbd_enqueue(&mut memory, 0x40 + i, 0x50 + i));
        }
        assert!(!kbd_enqueue(&mut memory, 0xEE, 0xFF));
    }

    #[test]
    fn test_kbd_buf_range_edge_cases() {
        let mut mem = Memory::new();

        // Case 1: start == end (invalid)
        mem.write_u16(KBD_BUF_START_PTR, 0x0020);
        mem.write_u16(KBD_BUF_END_PTR, 0x0020);
        let (start, end) = kbd_buf_range(&mem);
        assert_eq!(start, KBD_BUF_START_DEFAULT);
        assert_eq!(end, KBD_BUF_END_DEFAULT);

        // Case 2: end - start < 2 (invalid, need at least 2 bytes for 1 word)
        mem.write_u16(KBD_BUF_START_PTR, 0x0020);
        mem.write_u16(KBD_BUF_END_PTR, 0x0021); // Diff 1, also odd (double invalid)
        let (start, _end) = kbd_buf_range(&mem);
        assert_eq!(start, KBD_BUF_START_DEFAULT);

        // Case 3: end > 0x00FF (invalid, outside BDA standard area usually)
        mem.write_u16(KBD_BUF_START_PTR, 0x0020);
        mem.write_u16(KBD_BUF_END_PTR, 0x0100); // 256
        let (start, end) = kbd_buf_range(&mem);
        assert_eq!(start, KBD_BUF_START_DEFAULT);
        assert_eq!(end, KBD_BUF_END_DEFAULT);

        // Case 4: Valid custom range
        mem.write_u16(KBD_BUF_START_PTR, 0x0040);
        mem.write_u16(KBD_BUF_END_PTR, 0x0060);
        let (start, end) = kbd_buf_range(&mem);
        assert_eq!(start, 0x0040);
        assert_eq!(end, 0x0060);
    }

    // -----------------------------------------------------------------------
    // Tests recovered from deleted src/bda.rs and crates/gp64-memory/src/bda.rs
    // -----------------------------------------------------------------------

    #[test]
    fn test_bda_addresses() {
        // Verify BDA constant values match PC BIOS spec.
        // BDA segment is 0x0040. Linear address = 0x400 + offset.
        assert_eq!(KBD_SHIFT_FLAGS, 0x417);
        assert_eq!(KBD_HEAD_PTR, 0x41A);
        assert_eq!(KBD_TAIL_PTR, 0x41C);
        assert_eq!(KBD_BUF_START_PTR, 0x480);
        assert_eq!(KBD_BUF_END_PTR, 0x482);
    }

    #[test]
    fn test_kbd_get_set_shift_flags() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Initial state
        assert_eq!(kbd_get_shift_flags(&mem), 0);

        // Set shift flags
        kbd_set_shift_flags(&mut mem, 0x0F);
        assert_eq!(kbd_get_shift_flags(&mem), 0x0F);

        // Set different value
        kbd_set_shift_flags(&mut mem, 0x03);
        assert_eq!(kbd_get_shift_flags(&mem), 0x03);
    }

    #[test]
    fn test_kbd_enqueue_dequeue() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Queue should be empty
        assert!(kbd_peek(&mem).is_none());
        assert!(kbd_dequeue(&mut mem).is_none());

        // Enqueue a key
        assert!(kbd_enqueue(&mut mem, 0x1E, b'a'));

        // Peek should return the key without removing it
        assert_eq!(kbd_peek(&mem), Some((0x1E, b'a')));

        // Peek again should still return same key
        assert_eq!(kbd_peek(&mem), Some((0x1E, b'a')));

        // Dequeue should return and remove the key
        assert_eq!(kbd_dequeue(&mut mem), Some((0x1E, b'a')));

        // Queue should now be empty
        assert!(kbd_peek(&mem).is_none());
        assert!(kbd_dequeue(&mut mem).is_none());
    }

    #[test]
    fn test_kbd_enqueue_multiple() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Enqueue multiple keys
        assert!(kbd_enqueue(&mut mem, 0x1E, b'a'));
        assert!(kbd_enqueue(&mut mem, 0x30, b'b'));
        assert!(kbd_enqueue(&mut mem, 0x2E, b'c'));

        // Dequeue in FIFO order
        assert_eq!(kbd_dequeue(&mut mem), Some((0x1E, b'a')));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x30, b'b')));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x2E, b'c')));
        assert_eq!(kbd_dequeue(&mut mem), None);
    }

    #[test]
    fn test_kbd_buffer_wrap_around() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Enqueue 3 keys
        kbd_enqueue(&mut mem, 0x10, 0x10);
        kbd_enqueue(&mut mem, 0x11, 0x11);
        kbd_enqueue(&mut mem, 0x12, 0x12);

        // Dequeue 2
        assert_eq!(kbd_dequeue(&mut mem), Some((0x10, 0x10)));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x11, 0x11)));

        // Enqueue 2 more (these may wrap around the ring)
        kbd_enqueue(&mut mem, 0x20, 0x20);
        kbd_enqueue(&mut mem, 0x21, 0x21);

        // Dequeue remaining in order
        assert_eq!(kbd_dequeue(&mut mem), Some((0x12, 0x12)));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x20, 0x20)));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x21, 0x21)));
        assert_eq!(kbd_dequeue(&mut mem), None);
    }

    #[test]
    fn test_kbd_debug_pointers() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );
        let (start, end, head, tail) = kbd_debug_pointers(&mem);

        assert_eq!(start, KBD_BUF_START_DEFAULT);
        assert_eq!(end, KBD_BUF_END_DEFAULT);
        assert_eq!(head, KBD_BUF_START_DEFAULT);
        assert_eq!(tail, KBD_BUF_START_DEFAULT);
    }

    #[test]
    fn test_sanitize_kbd_ptr_valid() {
        // Valid pointer within range
        assert_eq!(sanitize_kbd_ptr(0x20, 0x1E, 0x3E), 0x20);
    }

    #[test]
    fn test_sanitize_kbd_ptr_below_start() {
        // Pointer below start resets to start
        assert_eq!(sanitize_kbd_ptr(0x10, 0x1E, 0x3E), 0x1E);
    }

    #[test]
    fn test_sanitize_kbd_ptr_odd() {
        // Odd pointer (not word-aligned) resets to start
        assert_eq!(sanitize_kbd_ptr(0x21, 0x1E, 0x3E), 0x1E);
    }

    #[test]
    fn test_sanitize_kbd_ptr_above_end() {
        // Pointer well above end resets to start
        assert_eq!(sanitize_kbd_ptr(0x50, 0x1E, 0x3E), 0x1E);
    }

    #[test]
    fn test_kbd_next_normal() {
        assert_eq!(kbd_next(0x20, 0x1E, 0x3E), 0x22);
    }

    #[test]
    fn test_kbd_next_wrap() {
        // At 0x3C (last valid slot), next should wrap to start
        assert_eq!(kbd_next(0x3C, 0x1E, 0x3E), 0x1E);
    }

    #[test]
    fn test_kbd_enqueue_partial_zeros() {
        let mut mem = Memory::new();
        kbd_clear(&mut mem);

        // Enqueue only ascii (scancode=0, ascii!=0)
        assert!(kbd_enqueue(&mut mem, 0, b'a'));
        assert_eq!(kbd_dequeue(&mut mem), Some((0, b'a')));

        // Enqueue only scancode (scancode!=0, ascii=0) — e.g. F1
        assert!(kbd_enqueue(&mut mem, 0x3B, 0));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x3B, 0)));
    }

    #[test]
    fn test_kbd_operations_with_custom_buffer() {
        let mut mem = Memory::new();
        // Set up a smaller custom buffer (4 word slots = 8 bytes)
        init_custom_ring(&mut mem, 0x20, 0x28, 0x20, 0x20);

        // Enqueue and dequeue with custom buffer
        assert!(kbd_enqueue(&mut mem, 0x1E, b'a'));
        assert!(kbd_enqueue(&mut mem, 0x30, b'b'));
        assert!(kbd_enqueue(&mut mem, 0x2E, b'c'));

        // Buffer should be full now (3 entries in 4-slot buffer, 1 slot kept empty)
        assert!(!kbd_enqueue(&mut mem, 0x20, b'd'));

        // Dequeue all
        assert_eq!(kbd_dequeue(&mut mem), Some((0x1E, b'a')));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x30, b'b')));
        assert_eq!(kbd_dequeue(&mut mem), Some((0x2E, b'c')));
        assert_eq!(kbd_dequeue(&mut mem), None);
    }

    #[test]
    fn test_kbd_with_corrupted_head_pointer() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Enqueue a key
        kbd_enqueue(&mut mem, 0x1E, b'a');

        // Corrupt the head pointer to an odd value
        mem.write_u16(KBD_HEAD_PTR, 0x1F);

        // Dequeue should sanitize the pointer and work correctly
        // (the corrupted pointer gets reset to start, so the key is still at start)
        let result = kbd_dequeue(&mut mem);
        assert_eq!(result, Some((0x1E, b'a')));
    }

    #[test]
    fn test_kbd_with_corrupted_tail_pointer() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_START_DEFAULT,
        );

        // Set tail to an invalid (above end) position
        mem.write_u16(KBD_TAIL_PTR, 0x50);

        // Enqueue should sanitize tail and work
        assert!(kbd_enqueue(&mut mem, 0x1E, b'a'));

        // The key should be stored at the sanitized position
        assert!(kbd_peek(&mem).is_some());
    }

    #[test]
    fn test_kbd_buf_range_minimum_valid() {
        let mut mem = Memory::new();
        // Minimum valid: exactly 2 bytes (1 word)
        mem.write_u16(KBD_BUF_START_PTR, 0x20);
        mem.write_u16(KBD_BUF_END_PTR, 0x22);

        let (start, end) = kbd_buf_range(&mem);
        assert_eq!(start, 0x20);
        assert_eq!(end, 0x22);
    }

    #[test]
    fn test_kbd_buf_range_end_at_boundary() {
        let mut mem = Memory::new();
        // Valid: end exactly at 0xFE (within 0x00FF limit)
        mem.write_u16(KBD_BUF_START_PTR, 0x1E);
        mem.write_u16(KBD_BUF_END_PTR, 0xFE);

        let (start, end) = kbd_buf_range(&mem);
        assert_eq!(start, 0x1E);
        assert_eq!(end, 0xFE);
    }

    /// MDD-BUG-CRUCIBLE-00429: the state the record names — default bounds, raw
    /// head 0000h, tail 001Eh. `kbd_peek` calls that empty; a raw reader calls
    /// it non-empty and returns the word at 0040:0000. After normalising, both
    /// agree.
    #[test]
    fn kbd_normalize_ring_agrees_with_peek_on_an_out_of_range_head() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            0x0000,
            KBD_BUF_START_DEFAULT,
        );
        assert_eq!(kbd_peek(&mem), None, "precondition: the helpers see empty");

        assert!(kbd_normalize_ring(&mut mem), "the head was out of range");

        let (start, end, head, tail) = kbd_debug_pointers(&mem);
        assert_eq!((start, end), (KBD_BUF_START_DEFAULT, KBD_BUF_END_DEFAULT));
        assert_eq!(
            head, tail,
            "a raw head==tail comparison must now also see the ring as empty"
        );
        assert_eq!(head, KBD_BUF_START_DEFAULT);
    }

    /// An odd head must land on the same word `kbd_peek` reads, so a raw
    /// reader dereferences an aligned in-range pointer.
    #[test]
    fn kbd_normalize_ring_realigns_an_odd_head() {
        let mut mem = Memory::new();
        init_custom_ring(
            &mut mem,
            KBD_BUF_START_DEFAULT,
            KBD_BUF_END_DEFAULT,
            0x001F,
            0x0020,
        );
        mem.write_u16(BDA_BASE + KBD_BUF_START_DEFAULT as usize, 0x1E61);

        assert_eq!(kbd_peek(&mem), Some((0x1E, b'a')));
        assert!(kbd_normalize_ring(&mut mem));

        let (_, _, head, _) = kbd_debug_pointers(&mem);
        assert_eq!(head, KBD_BUF_START_DEFAULT, "odd head realigned to start");
        assert_eq!(mem.read_u16(BDA_BASE + head as usize), 0x1E61);
    }

    /// An invalid range is substituted with the defaults `kbd_buf_range`
    /// already uses, so a raw reader wraps on the same bounds.
    #[test]
    fn kbd_normalize_ring_persists_the_default_bounds_for_a_nonsense_range() {
        let mut mem = Memory::new();
        init_custom_ring(&mut mem, 0x0000, 0x0000, 0x0000, 0x0000);

        assert!(kbd_normalize_ring(&mut mem));

        assert_eq!(mem.read_u16(KBD_BUF_START_PTR), KBD_BUF_START_DEFAULT);
        assert_eq!(mem.read_u16(KBD_BUF_END_PTR), KBD_BUF_END_DEFAULT);
        assert_eq!(mem.read_u16(KBD_HEAD_PTR), KBD_BUF_START_DEFAULT);
        assert_eq!(mem.read_u16(KBD_TAIL_PTR), KBD_BUF_START_DEFAULT);
    }

    /// The narrowness that keeps a legitimate enlarged ring safe: a valid
    /// custom range with valid pointers must come back byte-for-byte identical,
    /// and `kbd_normalize_ring` must report that it changed nothing.
    #[test]
    fn kbd_normalize_ring_leaves_a_valid_custom_ring_untouched() {
        let mut mem = Memory::new();
        init_custom_ring(&mut mem, 0x0020, 0x0060, 0x0030, 0x0040);

        assert!(
            !kbd_normalize_ring(&mut mem),
            "valid state must not be rewritten"
        );

        assert_eq!(mem.read_u16(KBD_BUF_START_PTR), 0x0020);
        assert_eq!(mem.read_u16(KBD_BUF_END_PTR), 0x0060);
        assert_eq!(mem.read_u16(KBD_HEAD_PTR), 0x0030);
        assert_eq!(mem.read_u16(KBD_TAIL_PTR), 0x0040);
    }
}
