use super::*;

/// Acceptance criterion 1: every spelling `parse_device_name` accepts must
/// open as a character device — CF clear and a real SFT handle, not the
/// "file not found" this used to return.
#[test]
fn test_int21_open_ah3d_accepts_every_aux_prn_spelling() {
    for name in [
        "AUX",
        "aux",
        "AUX:",
        "PRN",
        "PRN:",
        "C:\\SOMEDIR\\PRN",
        "COM1",
        "LPT1",
        "lpt1.prn",
    ] {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        let _temp = dev_mount_empty_c(&mut dos);

        let (ax, cf) = dev_open(&mut dos, &mut memory, 0x3D, name, 0x02);
        assert!(
            !cf,
            "{name}: open must succeed (CF clear), got AX={ax:#06X}"
        );
        assert!(
            ax >= 5,
            "{name}: expected a real device handle, got AX={ax:#06X}"
        );
        let path = dos
            .file_table
            .get_sft(ax)
            .map(|sft| sft.path.clone())
            .unwrap_or_default();
        assert!(
            path == "AUX" || path == "PRN",
            "{name}: handle must back a character device, SFT path was {path:?}"
        );
    }
}

/// Acceptance criterion 2: this machine has one UART wired as COM1 and one
/// parallel port. The absent ports must be ordinary filenames, not silent
/// aliases onto port 1 — opening `COM3` and getting COM1 is worse than a
/// clean failure.
#[test]
fn test_int21_open_ah3d_rejects_absent_serial_and_parallel_ports() {
    for name in ["COM2", "COM3", "COM4", "LPT2", "LPT3"] {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        let _temp = dev_mount_empty_c(&mut dos);

        let (ax, cf) = dev_open(&mut dos, &mut memory, 0x3D, name, 0x02);
        assert!(
            cf,
            "{name}: must not open as a device; got a handle AX={ax:#06X}"
        );
        assert_eq!(ax, 2, "{name}: expected file-not-found");
    }
}

/// Acceptance criterion 3, and the whole point of the change: `fopen("PRN",
/// "w")` is an AH=3Ch create, so a create that resolved a path first left a
/// host file literally named `PRN` in the guest's directory instead of
/// printing.
#[test]
fn test_int21_create_ah3c_opens_prn_device_and_writes_no_host_file() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let temp = dev_mount_empty_c(&mut dos);

    let (ax, cf) = dev_open(&mut dos, &mut memory, 0x3C, "PRN", 0x00);
    assert!(!cf, "AH=3Ch create of PRN must succeed, got AX={ax:#06X}");
    assert!(ax >= 5, "expected a device handle, got AX={ax:#06X}");
    assert_eq!(
        dos.file_table.get_sft(ax).map(|sft| sft.path.as_str()),
        Some("PRN")
    );

    let entries: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "AH=3Ch on a device must create no host file; drive root holds {entries:?}"
    );
}

/// The other two create/open routes named in the HLD's §3.5 table. AH=5Bh
/// must not report "file exists" for a device, and AH=6Ch must report the
/// device as opened (CX=1), never created.
#[test]
fn test_int21_create_new_ah5b_and_open_ex_ah6c_open_devices_not_files() {
    // AH=5Bh create-new.
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let temp = dev_mount_empty_c(&mut dos);
    let (ax, cf) = dev_open(&mut dos, &mut memory, 0x5B, "PRN", 0x00);
    assert!(!cf, "AH=5Bh on PRN must succeed, got AX={ax:#06X}");
    assert_eq!(
        dos.file_table.get_sft(ax).map(|sft| sft.path.as_str()),
        Some("PRN")
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);

    // AH=6Ch extended open. Name at DS:SI, open mode in BL, action in DL.
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let temp = dev_mount_empty_c(&mut dos);
    let mut cpu = gp64_cpu::Cpu::new();
    write_asciiz(&mut memory, 0x2000, 0x0100, "AUX");
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bx(0x0002); // BL = read/write access
    cpu.regs.set_dx(0x0011); // open if exists | create if not
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf, "AH=6Ch on AUX must succeed, got AX={ax:#06X}");
    assert_eq!(
        dos.file_table.get_sft(ax).map(|sft| sft.path.as_str()),
        Some("AUX")
    );
    assert_eq!(cpu.regs.cx(), 1, "a device is opened, never created");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);

    // AX=716Ch, the LFN twin, is a SECOND copy of the 6Ch logic and had the
    // same hole — an LFN open of PRN dropped a host file named PRN.
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let temp = dev_mount_empty_c(&mut dos);
    let mut cpu = gp64_cpu::Cpu::new();
    write_asciiz(&mut memory, 0x2000, 0x0100, "PRN");
    cpu.regs.set_ax(0x716C);
    cpu.regs.set_bx(0x0001); // BL = write access
    cpu.regs.set_dx(0x0011); // open if exists | create if not
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf, "AX=716Ch on PRN must succeed, got AX={ax:#06X}");
    assert_eq!(
        dos.file_table.get_sft(ax).map(|sft| sft.path.as_str()),
        Some("PRN")
    );
    assert_eq!(cpu.regs.cx(), 1, "a device is opened, never created");
    assert_eq!(
        std::fs::read_dir(temp.path()).unwrap().count(),
        0,
        "the LFN route must not create a host file either"
    );
}

/// Acceptance criterion 4: all three printer write routes converge on the
/// same accumulator — the named handle, the pre-opened STDPRN handle 4, and
/// the single-character AH=05h.
#[test]
fn test_int21_prn_writes_from_every_route_reach_the_bridge() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf);
    let (ax, cf) = dev_write(&mut dos, &mut memory, handle, b"AB");
    assert!(!cf);
    assert_eq!(ax, 2);

    let (ax, cf) = dev_write(&mut dos, &mut memory, 4, b"CD");
    assert!(!cf, "STDPRN (handle 4) must accept a write");
    assert_eq!(ax, 2);

    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x05);
    cpu.regs.set_dl(b'E');
    dev_int21(&mut dos, &mut cpu, &mut memory);

    let out = dos.device_bridge.lock().unwrap().take_prn_out();
    assert_eq!(out, b"ABCDE", "all three PRN routes share one accumulator");
}

/// Acceptance criterion 5, the AUX twin: named handle, pre-opened STDAUX
/// handle 3, and AH=04h all queue for COM1's transmit path.
#[test]
fn test_int21_aux_writes_from_every_route_reach_the_bridge() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "AUX", 0x01);
    assert!(!cf);
    let (ax, cf) = dev_write(&mut dos, &mut memory, handle, b"AB");
    assert!(!cf);
    assert_eq!(ax, 2);

    let (ax, cf) = dev_write(&mut dos, &mut memory, 3, b"CD");
    assert!(!cf, "STDAUX (handle 3) must accept a write");
    assert_eq!(ax, 2);

    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x04);
    cpu.regs.set_dl(b'E');
    dev_int21(&mut dos, &mut cpu, &mut memory);

    let out = dos.device_bridge.lock().unwrap().take_aux_tx();
    assert_eq!(out, b"ABCDE", "all three AUX routes share one queue");
}

/// Acceptance criterion 6. The bridge is bounded, so an oversized write is
/// a short write — and the bytes past AX are NOT retained. Retaining them
/// would make a correct retry (which resends from AX) transmit them twice.
#[test]
fn test_int21_aux_write_over_capacity_is_short_and_retains_nothing() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "AUX", 0x01);
    assert!(!cf);

    let payload = vec![b'Z'; BRIDGE_CAPACITY + 512];
    let (ax, cf) = dev_write(&mut dos, &mut memory, handle, &payload);
    assert!(!cf, "a short write is a success, not an error");
    assert!(
        (ax as usize) < payload.len(),
        "expected a short write, AX={ax} for CX={}",
        payload.len()
    );
    assert_eq!(ax as usize, BRIDGE_CAPACITY);

    let drained = dos.device_bridge.lock().unwrap().take_aux_tx();
    assert_eq!(
        drained.len(),
        ax as usize,
        "the bridge holds exactly what it reported accepting"
    );
    assert!(
        dos.device_bridge.lock().unwrap().take_aux_tx().is_empty(),
        "nothing beyond AX may be retained"
    );
}

/// AH=3Fh on AUX returns bytes the emulator staged from COM1 — through the
/// named handle and through the pre-opened STDAUX handle 3, which used to
/// return "invalid handle". STDPRN reads zero: a printer has nothing to say.
#[test]
fn test_int21_aux_read_ah3f_returns_staged_bytes() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    dev_stage_aux_rx(&dos, b"HELLO");

    let (handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "AUX", 0x00);
    assert!(!cf);

    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(3);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0000);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf);
    assert_eq!(ax, 3);
    let base = Memory::seg_offset_to_linear(0x3000, 0x0000);
    let got: Vec<u8> = (0..3).map(|i| memory.read_u8(base + i)).collect();
    assert_eq!(got, b"HEL");

    // The rest is still there for handle 3, which shares the same queue.
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(3);
    cpu.regs.set_cx(8);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0010);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf, "STDAUX (handle 3) must not be an invalid handle");
    assert_eq!(ax, 2, "a short read, not a block — the declared deviation");
    let base = Memory::seg_offset_to_linear(0x3000, 0x0010);
    let got: Vec<u8> = (0..2).map(|i| memory.read_u8(base + i)).collect();
    assert_eq!(got, b"LO");

    // STDPRN reads clean EOF rather than an error.
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(4);
    cpu.regs.set_cx(4);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0020);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf, "STDPRN read is EOF, not an error");
    assert_eq!(ax, 0);
}

/// Acceptance criterion 9. IOCTL 44h/06h used to answer "ready" for every
/// handle, which turns the zero-byte read that follows into a soft EOF. It
/// must report the true AUX state, and must consume nothing — a consuming
/// status would steal the byte from the read it is gating.
#[test]
fn test_int21_ioctl_input_status_reports_true_aux_state_without_consuming() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (aux_handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "AUX", 0x00);
    assert!(!cf);
    let (prn_handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf);

    let status = |dos: &mut Dos, memory: &mut Memory, handle: u16| -> u8 {
        let mut cpu = gp64_cpu::Cpu::new();
        cpu.regs.set_ah(0x44);
        cpu.regs.set_al(0x06);
        cpu.regs.set_bx(handle);
        let (_, cf) = dev_int21(dos, &mut cpu, memory);
        assert!(!cf, "handle {handle}: 44h/06h must not fail");
        cpu.regs.al()
    };

    // Empty.
    assert_eq!(status(&mut dos, &mut memory, aux_handle), 0x00);
    assert_eq!(status(&mut dos, &mut memory, 3), 0x00);

    // Non-empty.
    dev_stage_aux_rx(&dos, b"X");
    assert_eq!(status(&mut dos, &mut memory, aux_handle), 0xFF);
    assert_eq!(status(&mut dos, &mut memory, 3), 0xFF);
    assert_eq!(
        dos.device_bridge.lock().unwrap().aux_rx_len(),
        1,
        "input status must consume nothing"
    );

    // A byte still sitting in COM1's receiver counts as ready too. Nothing
    // has been staged for it, and the status call must not stage anything —
    // it reports what the emulator last published about the UART.
    {
        let mut bridge = dos.device_bridge.lock().unwrap();
        bridge.read_aux(&mut [0u8; 1]);
        assert_eq!(bridge.aux_rx_len(), 0);
        bridge.set_uart_rx_len(1);
    }
    assert_eq!(status(&mut dos, &mut memory, aux_handle), 0xFF);
    assert_eq!(
        dos.device_bridge.lock().unwrap().aux_rx_demand(),
        0,
        "a status poll must not ask the emulator to move a byte"
    );

    // Every other handle keeps the unconditional ready answer.
    assert_eq!(status(&mut dos, &mut memory, prn_handle), 0xFF);
    assert_eq!(status(&mut dos, &mut memory, 4), 0xFF);
    assert_eq!(status(&mut dos, &mut memory, 1), 0xFF);
}

/// Acceptance criterion 13, half one: two INDEPENDENT opens of `PRN` are
/// two device objects sharing one physical printer, so closing either must
/// not end the job — that would cut the other's output in half.
#[test]
fn test_int21_prn_job_ends_only_when_the_last_independent_open_closes() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (first, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf);
    let (second, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf);
    assert_ne!(first, second, "two opens are two handles");

    dev_close(&mut dos, &mut memory, first);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_job_ends(),
        0,
        "closing one of two independent opens must not end the job"
    );

    dev_close(&mut dos, &mut memory, second);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_job_ends(),
        1,
        "the last close ends exactly one job"
    );
}

/// Acceptance criterion 13, half two: a handle duplicated with AH=45h
/// shares ONE SFT and therefore one device object, so the job ends on the
/// second close, not the first. An SFT-refcount boundary would get the
/// independent-opens case above wrong; a per-handle one would get this wrong.
#[test]
fn test_int21_prn_dup_handle_defers_the_job_end_to_the_last_close() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (original, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf);

    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(original);
    let (dup, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf, "AH=45h dup of a device handle must succeed");
    assert_ne!(dup, original);

    dev_close(&mut dos, &mut memory, original);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_job_ends(),
        0,
        "a dup'd handle still holds the job open"
    );

    dev_close(&mut dos, &mut memory, dup);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_job_ends(),
        1,
        "closing the last reference ends exactly one job"
    );
}

/// A 32-bit DPMI client puts the byte count in ECX, so `vec![0u8; count]`
/// was an allocation the *guest* chose: a request for gigabytes aborts the
/// emulator. The write must be chunked, like every other AH=40h arm, and
/// still report an honest short count — in EAX, because a 32-bit client
/// reads it there and setting only AX leaves stale bits above it.
#[test]
fn test_int21_ah40_char_device_write_does_not_allocate_from_ecx() {
    for handle in [3u16, 4] {
        let mut dos = Dos::new();
        let mut cpu = gp64_cpu::Cpu::new();
        let mut memory = Memory::new();
        let buffer = dev_enter_dpmi32(&mut dos, &mut cpu, &mut memory);
        for i in 0..BRIDGE_CAPACITY {
            memory.write_u8(buffer + i, b'Z');
        }

        cpu.regs.eax = 0xDEAD_0000; // stale high half the call must clear
        cpu.regs.set_ah(0x40);
        cpu.regs.set_bx(handle);
        cpu.regs.ecx = 0xF000_0000; // ~3.75 GiB, if we were foolish enough

        let (_, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
        assert!(!cf, "handle {handle}: a short write is not an error");

        // The bridge holds 4 KiB, so that is all one call can take.
        assert_eq!(
            cpu.regs.eax as usize, BRIDGE_CAPACITY,
            "handle {handle}: EAX must carry the accepted count and nothing else"
        );
        let queued = if handle == 3 {
            dos.device_bridge.lock().unwrap().take_aux_tx()
        } else {
            dos.device_bridge.lock().unwrap().take_prn_out()
        };
        assert_eq!(queued.len(), BRIDGE_CAPACITY);
        assert!(queued.iter().all(|&b| b == b'Z'));
    }
}

/// The demand model. Staging is authorised by a read that came up short and
/// by nothing else, so a byte nobody asked for stays in COM1 — which is what
/// leaves it available to INT 14h and to a direct RBR read.
#[test]
fn test_aux_input_staging_is_demand_driven_not_eager() {
    let dos = Dos::new();
    let mut bridge = dos.device_bridge.lock().unwrap();

    // Nothing has been read, so the emulator may take nothing.
    assert_eq!(bridge.aux_rx_demand(), 0);
    assert!(!bridge.push_aux_rx(b'A'), "an unasked-for byte is refused");
    assert_eq!(bridge.aux_rx_len(), 0);

    // A read of 3 that returned nothing authorises exactly 3.
    bridge.note_aux_read(3, 0);
    assert_eq!(bridge.aux_rx_demand(), 3);
    for b in b"abc" {
        assert!(bridge.push_aux_rx(*b));
    }
    assert_eq!(bridge.aux_rx_demand(), 0, "demand is spent as it is filled");
    assert!(!bridge.push_aux_rx(b'd'), "and a fourth byte is refused");

    // Demand is a high-water mark, not a running total: a poll loop asking
    // for one byte over and over must not accumulate a standing request.
    for _ in 0..10 {
        bridge.note_aux_read(1, 0);
    }
    assert_eq!(bridge.aux_rx_demand(), 1);
}

/// Every AUX read route registers demand, including the two that never
/// construct a device object: `AH=03h` and the pre-opened STDAUX handle 3.
/// Gating on a named `AH=3Dh` open meant those two never received a byte.
#[test]
fn test_every_aux_read_route_registers_demand_without_an_open() {
    // AH=03h — no handle, no open, no device object anywhere.
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x03);
    dev_int21(&mut dos, &mut cpu, &mut memory);
    assert_eq!(cpu.regs.al(), 0, "nothing staged yet");
    assert_eq!(
        dos.device_bridge.lock().unwrap().aux_rx_demand(),
        1,
        "AH=03h must ask for the byte it could not deliver"
    );

    // The emulator fills that demand, and the retry gets the byte.
    dev_stage_aux_rx(&dos, b"Q");
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x03);
    dev_int21(&mut dos, &mut cpu, &mut memory);
    assert_eq!(cpu.regs.al(), b'Q');

    // AH=3Fh on handle 3 — pre-opened by DOS, again with no AH=3Dh.
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(3);
    cpu.regs.set_cx(5);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0000);
    let (ax, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf);
    assert_eq!(ax, 0);
    assert_eq!(
        dos.device_bridge.lock().unwrap().aux_rx_demand(),
        5,
        "handle 3 must ask for the five bytes it could not deliver"
    );

    // AH=3Fh through a named AUX handle does the same.
    let (handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "AUX", 0x00);
    assert!(!cf);
    dos.device_bridge.lock().unwrap().clear_pending_bytes();
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(2);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0000);
    dev_int21(&mut dos, &mut cpu, &mut memory);
    assert_eq!(dos.device_bridge.lock().unwrap().aux_rx_demand(), 2);
}

/// A read that asks for a gigabyte must not size a buffer from ECX either,
/// and its demand is capped at what the bridge could ever stage.
#[test]
fn test_int21_ah3f_stdaux_read_does_not_allocate_from_ecx() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let mut cpu = gp64_cpu::Cpu::new();
    dev_enter_dpmi32(&mut dos, &mut cpu, &mut memory);

    cpu.regs.eax = 0xDEAD_0000;
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(3);
    cpu.regs.ecx = 0xF000_0000;

    let (_, cf) = dev_int21(&mut dos, &mut cpu, &mut memory);
    assert!(!cf);
    assert_eq!(cpu.regs.eax, 0, "nothing staged, and EAX carries the count");
    assert_eq!(
        dos.device_bridge.lock().unwrap().aux_rx_demand(),
        BRIDGE_CAPACITY,
        "demand is capped at what the bridge could hold"
    );
}

/// `AH=04h`/`AH=05h` carry no count register, so a refused byte has nowhere
/// to be reported. Real DOS blocks; we cannot, but the byte must not vanish
/// in silence — it is counted.
#[test]
fn test_int21_single_char_device_writes_do_not_lose_a_byte_silently() {
    for (ah, handle) in [(0x04u8, 3u16), (0x05, 4)] {
        let mut dos = Dos::new();
        let mut memory = Memory::new();

        // Fill the device's queue so the next byte cannot be taken.
        let filler = vec![b'#'; BRIDGE_CAPACITY];
        let accepted = {
            let mut bridge = dos.device_bridge.lock().unwrap();
            if handle == 3 {
                bridge.write_aux(&filler)
            } else {
                bridge.write_prn(&filler)
            }
        };
        assert_eq!(accepted, BRIDGE_CAPACITY);

        let mut cpu = gp64_cpu::Cpu::new();
        cpu.regs.set_ah(ah);
        cpu.regs.set_dl(b'!');
        dev_int21(&mut dos, &mut cpu, &mut memory);

        assert_eq!(
            dos.device_bridge.lock().unwrap().dropped_chars(),
            1,
            "AH={ah:#04X}: a byte the device could not take must be counted"
        );

        // And when there IS room, nothing is counted.
        dos.device_bridge.lock().unwrap().clear_pending_bytes();
        let mut cpu = gp64_cpu::Cpu::new();
        cpu.regs.set_ah(ah);
        cpu.regs.set_dl(b'!');
        dev_int21(&mut dos, &mut cpu, &mut memory);
        assert_eq!(dos.device_bridge.lock().unwrap().dropped_chars(), 1);
    }
}

/// A `PRN` open that FAILS must emit no job boundary.
///
/// It did: the device counted itself on construction, so a failed
/// allocation dropped the temporary and took the open count straight back
/// to zero — which *is* the end-of-job signal. The pending accumulator then
/// got flushed as a job, cutting in half whatever was still being printed
/// (direct port I/O has no handle to hold it open at all).
#[test]
fn test_failed_prn_open_emits_no_job_boundary() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    dev_exhaust_handles(&mut dos, &mut memory);

    let (ax, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(cf, "the PRN open must fail with the table full");
    assert_eq!(ax, 4);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_job_ends(),
        0,
        "a PRN open that never got a handle must not end a job"
    );

    // The create and extended-open routes reach the same constructor.
    for ah in [0x3Cu8, 0x5B] {
        let (_, cf) = dev_open(&mut dos, &mut memory, ah, "PRN", 0x00);
        assert!(cf, "AH={ah:#04X}: the open must fail with the table full");
        assert_eq!(
            dos.device_bridge.lock().unwrap().take_prn_job_ends(),
            0,
            "AH={ah:#04X}: a failed open must not end a job"
        );
    }
}

/// The other half of the same defect: a failed open must not cut short a
/// job an unrelated, still-open `PRN` handle is in the middle of writing.
#[test]
fn test_failed_prn_open_does_not_disturb_an_open_handles_job() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let _temp = dev_mount_empty_c(&mut dos);

    let (open_handle, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(!cf, "the first PRN open must succeed");
    dev_write(&mut dos, &mut memory, open_handle, b"half a job");

    dev_exhaust_handles(&mut dos, &mut memory);
    let (_, cf) = dev_open(&mut dos, &mut memory, 0x3D, "PRN", 0x01);
    assert!(cf, "the second PRN open must fail");
    assert_eq!(dos.device_bridge.lock().unwrap().take_prn_job_ends(), 0);

    // The still-open handle keeps working, ending its job on ITS close.
    dev_write(&mut dos, &mut memory, open_handle, b" and the rest");
    dev_close(&mut dos, &mut memory, open_handle);
    assert_eq!(dos.device_bridge.lock().unwrap().take_prn_job_ends(), 1);
    assert_eq!(
        dos.device_bridge.lock().unwrap().take_prn_out(),
        b"half a job and the rest"
    );
}
/// AH=40h write of `bytes` to `handle`. Returns `(AX, CF)`.
fn dev_write(dos: &mut Dos, memory: &mut Memory, handle: u16, bytes: &[u8]) -> (u16, bool) {
    let mut cpu = gp64_cpu::Cpu::new();
    let base = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, &b) in bytes.iter().enumerate() {
        memory.write_u8(base + i, b);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(bytes.len() as u16);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0000);
    dev_int21(dos, &mut cpu, memory)
}

/// AH=3Eh close of `handle`.
fn dev_close(dos: &mut Dos, memory: &mut Memory, handle: u16) -> (u16, bool) {
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    dev_int21(dos, &mut cpu, memory)
}

/// Stage received bytes the way the emulator's drain does: register the
/// demand a pending read created, then push exactly that much.
///
/// `push_aux_rx` refuses without demand *by design* — that refusal is what
/// stops an eager fill draining COM1's receiver out from under INT 14h and
/// a guest polling the RBR directly.
fn dev_stage_aux_rx(dos: &Dos, bytes: &[u8]) {
    let mut bridge = dos.device_bridge.lock().unwrap();
    bridge.note_aux_read(bytes.len(), 0);
    for &b in bytes {
        assert!(
            bridge.push_aux_rx(b),
            "the demand just registered must authorise staging {b:#04X}"
        );
    }
}

/// Put `cpu` into 32-bit DPMI mode with DS pointing at flat memory, so a
/// test can drive the ECX-based API exactly as a DOS extender client does.
/// Returns the linear address the guest buffer offset maps to.
fn dev_enter_dpmi32(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory) -> usize {
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0002_0000;
    enter_dpmi_32bit(dos, cpu, memory);
    cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);
    cpu.regs.edx = BUFFER_OFFSET;
    cpu.translate_address(cpu.regs.ds, BUFFER_OFFSET)
}

/// Fill the DOS handle table so the next open cannot be satisfied.
fn dev_exhaust_handles(dos: &mut Dos, memory: &mut Memory) {
    for guard in 0..512 {
        let (ax, cf) = dev_open(dos, memory, 0x3D, "NUL", 0x01);
        if cf {
            assert_eq!(ax, 4, "the table must fill with error 4");
            return;
        }
        assert!(guard < 511, "handle table never filled");
    }
}
