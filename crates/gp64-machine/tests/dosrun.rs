//! Synthetic DOS acceptance programs: every opcode is supplied locally.
use gp64_machine::{DosMachine, DosRunExit};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Drive(PathBuf);
impl Drive {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!(
                "dosrun-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
    }
    fn launch(&self, name: &str) -> DosMachine {
        DosMachine::launch(&self.0, name, "").unwrap()
    }
}
impl Drop for Drive {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}
fn finish(pc: &mut DosMachine, expected: u8) {
    for _ in 0..100 {
        match pc.run_for(100_000).unwrap() {
            DosRunExit::Running => (),
            DosRunExit::Exited(code) => {
                assert_eq!(code, expected);
                return;
            }
            other => panic!("unexpected machine stop: {other:?}"),
        }
    }
    panic!("DOS program did not terminate within 10 million cycles");
}
// A tiny fixture assembler resolves COM absolute pointers and signed short branches.
#[derive(Default)]
struct Com {
    bytes: Vec<u8>,
    labels: Vec<(&'static str, usize)>,
    patches: Vec<(usize, &'static str, bool)>,
}
impl Com {
    fn emit(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
    fn label(&mut self, name: &'static str) {
        self.labels.push((name, self.bytes.len()));
    }
    fn pointer(&mut self, name: &'static str) {
        self.patches.push((self.bytes.len(), name, false));
        self.emit(&[0, 0]);
    }
    fn branch(&mut self, opcode: u8, name: &'static str) {
        self.emit(&[opcode]);
        self.patches.push((self.bytes.len(), name, true));
        self.emit(&[0]);
    }
    fn done(mut self) -> Vec<u8> {
        for (at, name, relative) in self.patches {
            let target = self
                .labels
                .iter()
                .find(|(label, _)| *label == name)
                .unwrap()
                .1;
            if relative {
                self.bytes[at] = i8::try_from(target as isize - at as isize - 1).unwrap() as u8;
            } else {
                self.bytes[at..at + 2].copy_from_slice(&(0x100 + target as u16).to_le_bytes());
            }
        }
        self.bytes
    }
    fn exits(&mut self) {
        self.emit(&[0xb8, 0x00, 0x4c, 0xcd, 0x21]); // mov ax,4c00h; int 21h
        self.label("fail");
        self.emit(&[0xb8, 0x01, 0x4c, 0xcd, 0x21]); // mov ax,4c01h; int 21h
    }
}
fn print_program(text: &[u8], code: u8) -> Vec<u8> {
    let mut p = Com::default();
    p.emit(&[0xba]);
    p.pointer("text"); // mov dx,text
    p.emit(&[0xb4, 0x09, 0xcd, 0x21, 0xb8, code, 0x4c, 0xcd, 0x21]); // print '$' string; terminate
    p.label("text");
    p.emit(text);
    p.emit(b"$");
    p.done()
}

#[test]
fn launch_preserves_entire_bios_rom_font_advertised_by_int10() {
    let drive = Drive::new();
    for selector in [6, 7] {
        // Request the 8x16 font, then stop with interrupts disabled so the
        // returned ES:BP and CX remain available to the host assertion.
        drive.put(
            "FONT.COM",
            &[0xb8, 0x30, 0x11, 0xb7, selector, 0xcd, 0x10, 0xfa, 0xf4],
        );
        let mut pc = drive.launch("FONT.COM");
        assert!(matches!(pc.run_for(1000).unwrap(), DosRunExit::Running));
        assert!(pc.machine.cpu.halted, "font pointer fixture reached HLT");
        let regs = &pc.machine.cpu.regs;
        assert_eq!((regs.es, regs.bp(), regs.cx()), (0xf000, 0xe800, 16));
        let base = gp64_memory::Memory::seg_offset_to_linear(regs.es, regs.bp());
        for (row, &expected) in gp64_hw::vga::CP437_FONT_8X16.iter().enumerate() {
            assert_eq!(
                pc.machine.memory.read_u8(base + row),
                expected,
                "full DOS launch preserves advertised 8x16 font byte {row} (BH={selector})"
            );
        }
    }
}

#[test]
fn launch_starts_on_a_blank_light_grey_text_screen() {
    let drive = Drive::new();
    // Read the character and attribute under the cursor, then stop.
    drive.put(
        "CELL.COM",
        &[0xb4, 0x08, 0xb7, 0x00, 0xcd, 0x10, 0xfa, 0xf4],
    );
    let mut pc = drive.launch("CELL.COM");
    assert!(matches!(pc.run_for(1000).unwrap(), DosRunExit::Running));
    assert!(pc.machine.cpu.halted, "cell fixture reached HLT");
    // Setup programs clear the screen with the attribute they read here, then
    // print through DOS, which keeps each cell's attribute.
    assert_eq!(
        pc.machine.cpu.regs.ax(),
        0x0720,
        "blank cell, light grey on black"
    );

    drive.put("PRINT.COM", &print_program(b"Select", 0));
    let mut pc = drive.launch("PRINT.COM");
    finish(&mut pc, 0);
    let frame = pc.machine.frame();
    assert!(
        frame
            .pixels
            .iter()
            .any(|&index| frame.palette[usize::from(index)] != (0, 0, 0)),
        "DOS output is visible on the launch screen"
    );
}

// Independent INT 15h/AH=83h fixtures use a flag outside the COM image.
const EVENT_WAIT_FLAG: usize = 0x5000;
fn event_wait_program(halt: bool) -> Vec<u8> {
    let mut p = Com::default();
    p.emit(&[0xfa, 0x31, 0xc0, 0x8e, 0xc0]); // cli; xor ax,ax; mov es,ax
    p.emit(&[0xbb, 0x00, 0x50, 0xb9, 0, 0, 0xba, 0x10, 0x27]); // flag; 10000us
    p.emit(&[0xb8, 0x00, 0x83, 0xcd, 0x15]); // start wait
    if halt {
        p.emit(&[0xf4]);
        p.emit(&[0xb8, 0x00, 0x83, 0xcd, 0x15, 0xf4]); // attempt another start
        p.emit(&[0xb8, 0x01, 0x83, 0xcd, 0x15, 0xf4]); // cancel
        p.emit(&[0xb8, 0x00, 0x83, 0xcd, 0x15, 0xf4]); // restart
    } else {
        p.emit(&[0xeb, 0xfe]); // executing poll loop
    }
    p.done()
}
fn start_event_wait(pc: &mut DosMachine) -> u64 {
    pc.machine.memory.write_u8(EVENT_WAIT_FLAG, 0xb5);
    for _ in 0..100 {
        assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
        if pc.machine.memory.read_u8(EVENT_WAIT_FLAG) == 0x35 {
            assert!(!pc.machine.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
            return pc.machine.cycle();
        }
    }
    panic!("synthetic guest did not start the BIOS event wait");
}
fn event_wait_until(pc: &mut DosMachine, cycle: u64, slice: u64) {
    while pc.machine.cycle() < cycle {
        let budget = (cycle - pc.machine.cycle()).min(slice);
        assert!(matches!(pc.run_for(budget).unwrap(), DosRunExit::Running));
    }
}
#[test]
fn bios_event_wait_expires_across_executing_slices_and_restarts() {
    let drive = Drive::new();
    drive.put("WAIT.COM", &event_wait_program(false));
    let mut pc = drive.launch("WAIT.COM");
    pc.machine.set_cycles_per_second(3_000_000);
    // Time before the start must not be charged to the new interval.
    pc.machine.cpu.cycles = 60_000;
    let start = start_event_wait(&mut pc);
    event_wait_until(&mut pc, start + 29_700, 731);
    assert_eq!(pc.machine.memory.read_u8(EVENT_WAIT_FLAG), 0x35);
    event_wait_until(&mut pc, start + 30_300, 731);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0xb5,
        "executing guest expires its asynchronous wait"
    );
    pc.machine.cpu.regs.set_ip(0x100);
    let restart = start_event_wait(&mut pc);
    event_wait_until(&mut pc, restart + 29_700, 719);
    assert_eq!(pc.machine.memory.read_u8(EVENT_WAIT_FLAG), 0x35);
    event_wait_until(&mut pc, restart + 30_300, 719);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0xb5,
        "expired wait permits a fresh full interval"
    );
}
#[test]
fn bios_event_wait_expires_at_final_halted_slice_boundary() {
    let drive = Drive::new();
    drive.put("WAIT.COM", &event_wait_program(true));
    let mut pc = drive.launch("WAIT.COM");
    pc.machine.set_cycles_per_second(3_000_000);
    let start = start_event_wait(&mut pc);
    assert_ne!(start % 3, 0, "fixture starts at a fractional microsecond");
    event_wait_until(&mut pc, start + 100, 100);
    pc.machine.cpu.halted = false;
    assert!(matches!(pc.run_for(100).unwrap(), DosRunExit::Running));
    assert!(pc.machine.cpu.halted);
    assert!(pc.machine.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    event_wait_until(&mut pc, start + 29_999, 1);
    assert!(pc.machine.cpu.halted);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0x35,
        "wait cannot expire before 10000us from start"
    );
    event_wait_until(&mut pc, start + 30_000, 1);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0xb5,
        "halted guest retains fractional time and expires at the final boundary"
    );
}
#[test]
fn bios_event_wait_cancel_preserves_flag_and_allows_new_start() {
    let drive = Drive::new();
    drive.put("WAIT.COM", &event_wait_program(true));
    let mut pc = drive.launch("WAIT.COM");
    pc.machine.set_cycles_per_second(3_000_000);
    let start = start_event_wait(&mut pc);
    event_wait_until(&mut pc, start + 100, 100);
    pc.machine.cpu.halted = false;
    assert!(matches!(pc.run_for(100).unwrap(), DosRunExit::Running));
    assert!(pc.machine.cpu.halted);
    assert!(
        pc.machine.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a pending interval rejects a second start"
    );
    pc.machine.cpu.halted = false;
    assert!(matches!(pc.run_for(100).unwrap(), DosRunExit::Running));
    assert!(pc.machine.cpu.halted);
    assert!(!pc.machine.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    event_wait_until(&mut pc, start + 60_000, 731);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0x35,
        "cancelled wait never sets its flag"
    );
    pc.machine.cpu.halted = false;
    let restart = start_event_wait(&mut pc);
    event_wait_until(&mut pc, restart + 29_700, 719);
    assert_eq!(pc.machine.memory.read_u8(EVENT_WAIT_FLAG), 0x35);
    event_wait_until(&mut pc, restart + 30_300, 719);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0xb5,
        "cancelled wait permits a fresh full interval"
    );
}
#[test]
fn bios_event_wait_retains_fraction_across_cpu_rate_change() {
    let drive = Drive::new();
    drive.put("WAIT.COM", &event_wait_program(true));
    let mut pc = drive.launch("WAIT.COM");
    pc.machine.set_cycles_per_second(3_000_000);
    let start = start_event_wait(&mut pc);
    event_wait_until(&mut pc, start + 15_001, 1); // 5000 + 1/3us
    assert_eq!(pc.machine.memory.read_u8(EVENT_WAIT_FLAG), 0x35);
    pc.machine.set_cycles_per_second(6_000_000);
    let changed = pc.machine.cycle();
    event_wait_until(&mut pc, changed + 29_997, 1);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0x35,
        "rate change must not expire the interval early"
    );
    event_wait_until(&mut pc, changed + 29_998, 1);
    assert_eq!(
        pc.machine.memory.read_u8(EVENT_WAIT_FLAG),
        0xb5,
        "rate change preserves elapsed guest microseconds and fractions"
    );
}
#[test]
fn com_console_and_exit_status() {
    let drive = Drive::new();
    drive.put("OK.COM", &print_program(b"OK", 7));
    let mut pc = drive.launch("OK.COM");
    finish(&mut pc, 7);
    assert_eq!(pc.programs_started(), ["OK.COM"]);
    assert_eq!(pc.take_console_output(), b"OK");
    assert!(pc.take_console_output().is_empty());
}
#[test]
fn mode13_guest_pixel_reaches_frame() {
    let drive = Drive::new();
    drive.put(
        "PIXEL.COM",
        &[
            0xb8, 0x13, 0x00, 0xcd, 0x10, // mov ax,13h; int 10h
            0xb8, 0x00, 0xa0, 0x8e, 0xc0, // mov ax,a000h; mov es,ax
            0x26, 0xc6, 0x06, 0x00, 0x00, 0x0c, // mov byte es:[0],12
            0xb8, 0x00, 0x4c, 0xcd, 0x21, // exit 0
        ],
    );
    let mut pc = drive.launch("PIXEL.COM");
    finish(&mut pc, 0);
    let frame = pc.machine.frame();
    assert_eq!((frame.width, frame.height), (320, 200));
    assert_eq!(frame.palette[frame.pixels[0] as usize], frame.palette[12]);
    assert_ne!(frame.pixels[0], frame.pixels[1]);
}
#[test]
fn exec_passes_tail_and_preserves_child_return_code() {
    let drive = Drive::new();
    drive.put(
        "CHILD.COM",
        &[
            0x31, 0xc9, 0x8a, 0x0e, 0x80, 0x00, // xor cx,cx; mov cl,[80h]
            0xba, 0x81, 0x00, 0xbb, 0x01, 0x00, 0xb4, 0x40, 0xcd,
            0x21, // write tail to stdout
            0xb8, 0x03, 0x4c, 0xcd, 0x21, // exit 3
        ],
    );
    let mut p = Com::default();
    p.emit(&[0x1e, 0x07, 0xbb, 0x00, 0x10, 0xb4, 0x4a, 0xcd, 0x21]); // ES=DS; resize own block
    p.branch(0x72, "fail");
    p.emit(&[0x8c, 0xd8, 0xa3]);
    p.pointer("tail_seg"); // mov ax,ds; mov [tail_seg],ax
    p.emit(&[0xba]);
    p.pointer("child");
    p.emit(&[0xbb]);
    p.pointer("params");
    p.emit(&[0xb8, 0x00, 0x4b, 0xcd, 0x21]); // EXEC ES:BX, DS:DX
    p.branch(0x72, "fail");
    p.emit(&[0xb4, 0x4d, 0xcd, 0x21, 0x3d, 0x03, 0x00]); // get status; cmp ax,0003h
    p.branch(0x75, "fail");
    p.exits();
    p.label("child");
    p.emit(b"CHILD.COM\0");
    p.label("params");
    p.emit(&[0, 0]);
    p.pointer("tail");
    p.label("tail_seg");
    p.emit(&[0; 10]);
    p.label("tail");
    p.emit(b"\x06 CHILD\r");
    drive.put("PARENT.COM", &p.done());
    let mut pc = drive.launch("PARENT.COM");
    finish(&mut pc, 0);
    assert_eq!(pc.programs_started(), ["PARENT.COM", "CHILD.COM"]);
    assert_eq!(pc.take_console_output(), b" CHILD");
}
fn marker_program(name: &[u8], text: &[u8]) -> Vec<u8> {
    let mut p = Com::default();
    p.emit(&[0xba]);
    p.pointer("name");
    p.emit(&[0x31, 0xc9, 0xb4, 0x3c, 0xcd, 0x21]); // create marker
    p.branch(0x72, "fail");
    p.emit(&[0x89, 0xc3, 0xba]);
    p.pointer("text");
    p.emit(&[0xb9, text.len() as u8, 0, 0xb4, 0x40, 0xcd, 0x21]); // write marker
    p.branch(0x72, "fail");
    p.emit(&[0xb4, 0x3e, 0xcd, 0x21]); // close
    p.emit(&[0xba]);
    p.pointer("text");
    p.emit(&[0xb4, 0x09, 0xcd, 0x21]); // print marker
    p.exits();
    p.label("name");
    p.emit(name);
    p.emit(&[0]);
    p.label("text");
    p.emit(text);
    p.emit(b"$");
    p.done()
}
#[test]
fn batch_runs_two_external_commands_in_order() {
    let drive = Drive::new();
    drive.put("FIRST.COM", &marker_program(b"FIRST.TXT", b"first\r\n"));
    drive.put("SECOND.COM", &marker_program(b"SECOND.TXT", b"second\r\n"));
    drive.put("RUN.BAT", b"@FIRST.COM\r\n@SECOND.COM\r\n");
    let mut pc = drive.launch("RUN.BAT");
    finish(&mut pc, 0);
    assert_eq!(pc.programs_started(), ["FIRST.COM", "SECOND.COM"]);
    assert_eq!(pc.take_console_output(), b"first\r\nsecond\r\n");
    assert_eq!(fs::read(drive.0.join("FIRST.TXT")).unwrap(), b"first\r\n");
    assert_eq!(fs::read(drive.0.join("SECOND.TXT")).unwrap(), b"second\r\n");
}

struct CallDrive(Drive);
impl CallDrive {
    fn new() -> Self {
        let drive = Drive::new();
        fs::create_dir(drive.0.join("TOOLS")).unwrap();
        Self(drive)
    }
}
impl Drop for CallDrive {
    fn drop(&mut self) {
        let tools = self.0 .0.join("TOOLS");
        for entry in fs::read_dir(&tools).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(tools).unwrap();
    }
}

fn assert_call_path_batch_resumes(program: &str) {
    let fixture = CallDrive::new();
    let drive = &fixture.0;
    drive.put("TOOLS/CHILD.BAT", b"@ECHO child %1\r\n");
    drive.put(
        "RUN.BAT",
        format!("@PATH C:\\TOOLS\r\n@CALL {program} argument\r\n@ECHO resumed\r\n").as_bytes(),
    );
    let mut pc = drive.launch("RUN.BAT");
    finish_call(&mut pc, program);
    assert_eq!(
        pc.take_console_output(),
        b"child argument\r\nresumed\r\n",
        "PATH-resolved CALL {program} must resume its caller"
    );
}

fn finish_call(pc: &mut DosMachine, program: &str) {
    let mut exited = false;
    for _ in 0..100 {
        let result = pc.run_for(100_000);
        assert!(
            matches!(result, Ok(DosRunExit::Running | DosRunExit::Exited(0))),
            "CALL {program} must execute and resume its caller: {result:?}"
        );
        if matches!(result, Ok(DosRunExit::Exited(0))) {
            exited = true;
            break;
        }
    }
    assert!(exited, "CALL {program} must finish its caller");
}

#[test]
fn call_path_extensionless_batch_resumes_caller() {
    assert_call_path_batch_resumes("CHILD");
}

#[test]
fn call_path_explicit_batch_resumes_caller() {
    assert_call_path_batch_resumes("CHILD.BAT");
}

fn assert_call_local_precedence(extensions: &[&str], expected: &str) {
    let fixture = CallDrive::new();
    let drive = &fixture.0;
    drive.put("TOOLS/CHILD.COM", &[0xb8, 0, 0x4c, 0xcd, 0x21]);
    for extension in extensions {
        let bytes = match *extension {
            "BAT" => b"@ECHO local batch\r\n".to_vec(),
            "EXE" => mz(&[0xb8, 0, 0x4c, 0xcd, 0x21], None),
            _ => vec![0xb8, 0, 0x4c, 0xcd, 0x21],
        };
        drive.put(&format!("CHILD.{extension}"), &bytes);
    }
    drive.put(
        "RUN.BAT",
        b"@PATH C:\\TOOLS\r\n@CALL CHILD\r\n@ECHO resumed\r\n",
    );
    let mut pc = drive.launch("RUN.BAT");
    finish_call(&mut pc, "CHILD");
    if expected == "BAT" {
        assert!(pc.programs_started().is_empty());
        assert_eq!(pc.take_console_output(), b"local batch\r\nresumed\r\n");
    } else {
        assert_eq!(
            pc.programs_started(),
            [format!("CHILD.{expected}")],
            "CALL must preserve local COM/EXE/BAT precedence"
        );
        assert_eq!(pc.take_console_output(), b"resumed\r\n");
    }
}

#[test]
fn call_local_com_precedes_exe_and_batch() {
    assert_call_local_precedence(&["COM", "EXE", "BAT"], "COM");
}

#[test]
fn call_local_exe_precedes_batch() {
    assert_call_local_precedence(&["EXE", "BAT"], "EXE");
}

#[test]
fn call_local_batch_precedes_path_executable() {
    assert_call_local_precedence(&["BAT"], "BAT");
}

#[test]
fn call_path_executable_resumes_caller() {
    assert_call_local_precedence(&[], "COM");
}
fn word(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn mz(image: &[u8], relocation: Option<u16>) -> Vec<u8> {
    let mut file = vec![0; 32];
    file[..2].copy_from_slice(b"MZ");
    word(&mut file, 2, (32 + image.len()) as u16);
    word(&mut file, 4, 1);
    word(&mut file, 6, u16::from(relocation.is_some()));
    word(&mut file, 8, 2);
    word(&mut file, 10, 0x20);
    word(&mut file, 12, 0xffff);
    word(&mut file, 16, 0x200);
    word(&mut file, 24, 28);
    if let Some(offset) = relocation {
        word(&mut file, 28, offset);
    }
    file.extend_from_slice(image);
    file
}
#[test]
fn mz_relocates_loaded_segment() {
    let drive = Drive::new();
    // The immediate operand at offset 1 must be relocated to the image's CS.
    let image = [
        0xb8, 0, 0, 0x0e, 0x5b, 0x39, 0xd8, 0x75, 5, 0xb8, 0, 0x4c, 0xcd, 0x21, 0xb8, 1, 0x4c,
        0xcd, 0x21,
    ];
    drive.put("RELOC.EXE", &mz(&image, Some(1)));
    finish(&mut drive.launch("RELOC.EXE"), 0);
}
#[test]
fn exepack_static_unpacking_executes_original_entry() {
    // EXEPACK reverse literal command B3: 16 original bytes, length, final command.
    let mut packed = vec![0xb8, 0x06, 0x4c, 0xcd, 0x21]; // original entry: exit 6
    packed.resize(16, 0x90);
    packed.extend_from_slice(&[16, 0, 0xb3]);
    let header = packed.len();
    packed.resize(header + 16, 0);
    word(
        &mut packed,
        header + 6,
        (16 + b"Packed file is corrupt".len() + 32) as u16,
    ); // 16-byte header + error string + 16 relocation counts
    word(&mut packed, header + 8, 0x200);
    word(&mut packed, header + 12, 1);
    word(&mut packed, header + 14, 0x4252); // RB signature
    packed.extend_from_slice(b"Packed file is corrupt");
    packed.extend_from_slice(&[0; 32]);
    let drive = Drive::new();
    drive.put("PACKED.EXE", &mz(&packed, None));
    finish(&mut drive.launch("PACKED.EXE"), 6);
}
#[test]
fn xms_far_entry_exists_and_dpmi_is_absent() {
    let mut p = Com::default();
    p.emit(&[0xb8, 0x00, 0x43, 0xcd, 0x2f, 0x3c, 0x80]); // XMS installed? AL=80h
    p.branch(0x75, "fail");
    p.emit(&[0xb8, 0x10, 0x43, 0xcd, 0x2f, 0x89, 0x1e]);
    p.pointer("entry");
    p.emit(&[0x8c, 0x06]);
    p.pointer("entry_seg"); // save ES:BX entry
    p.emit(&[0xb4, 0, 0xff, 0x1e]);
    p.pointer("entry"); // AH=0; call far [entry]: version
    p.emit(&[0x85, 0xc0]);
    p.branch(0x74, "fail");
    p.emit(&[0xb8, 0x87, 0x16, 0xcd, 0x2f, 0x85, 0xc0]); // DPMI probe must return nonzero AX
    p.branch(0x74, "fail");
    p.exits();
    p.label("entry");
    p.emit(&[0, 0]);
    p.label("entry_seg");
    p.emit(&[0, 0]);
    let drive = Drive::new();
    drive.put("XMS.COM", &p.done());
    finish(&mut drive.launch("XMS.COM"), 0);
}
#[test]
fn keyboard_scan_code_becomes_bios_ascii() {
    let mut p = Com::default();
    p.emit(&[0xb4, 0, 0xcd, 0x16, 0x3d, 0x61, 0x1e]); // read key; cmp ax,1e61h
    p.branch(0x75, "fail");
    p.emit(&[0x88, 0xc2, 0xb4, 2, 0xcd, 0x21]); // DL=AL; print character
    p.exits();
    let drive = Drive::new();
    drive.put("KEY.COM", &p.done());
    let mut pc = drive.launch("KEY.COM");
    pc.key_down(0x1e);
    pc.key_up(0x1e);
    finish(&mut pc, 0);
    assert_eq!(pc.take_console_output(), b"a");
}

#[test]
fn indos_keyboard_wait_targets_installed_stub() {
    let drive = Drive::new();
    drive.put("ROUTE.COM", &[0xb4, 0x07, 0xcd, 0x21]);
    let mut pc = drive.launch("ROUTE.COM");
    for _ in 0..1000 {
        assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
        if pc.machine.cpu.regs.cs == 0xf000 {
            assert_eq!(
                pc.machine.cpu.regs.ip(),
                gp64_bios::Bios::INT21_KBD_WAIT,
                "wait dispatch must enter installed stub"
            );
            return;
        }
    }
    panic!("console call never entered BIOS wait code");
}

#[test]
fn keyboard_wait_preserves_guest_ds_and_returns_the_key() {
    use gp64_bios::Bios;
    use gp64_cpu::Flags;

    let mut p = Com::default();
    p.emit(&[0xb8, 1, 7, 0xbb]); // AX=0701h; BX points at writable guest data.
    p.pointer("sentinel");
    p.emit(&[0x31, 0xf6, 0xfa, 0xcd, 0x21, 0x3c, b'a']); // SI=0; CLI; AH=07h.
    p.branch(0x75, "fail");
    p.emit(&[0xa2]);
    p.pointer("returned_char");
    p.emit(&[0xb8, 0, 0x4c, 0xcd, 0x21]);
    p.label("fail");
    p.emit(&[0xb8, 1, 0x4c, 0xcd, 0x21]);
    p.label("sentinel");
    p.emit(&[0x5a]);
    p.label("returned_char");
    p.emit(&[0]);
    let address =
        |name| 0x100 + p.labels.iter().find(|(label, _)| *label == name).unwrap().1 as u16;
    let sentinel = address("sentinel");
    let returned_char = address("returned_char");

    let drive = Drive::new();
    drive.put("WAIT.COM", &p.done());
    let mut pc = drive.launch("WAIT.COM");
    let ds = pc.machine.cpu.regs.ds;
    let mut stub_entry = None;
    for _ in 0..1000 {
        assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
        if pc.machine.cpu.regs.cs == 0xf000 {
            stub_entry = Some(pc.machine.cpu.regs.ip());
            break;
        }
    }
    let stub_entry = stub_entry.expect("console call must enter BIOS code");
    assert!(
        !pc.machine.cpu.regs.flags.contains(Flags::INTERRUPT),
        "guest CLI must leave IF clear at the wait dispatch"
    );
    assert!(matches!(pc.run_for(100_000).unwrap(), DosRunExit::Running));
    assert_eq!(
        pc.machine.memory.read_u8_seg(ds, sentinel),
        0x5a,
        "AH=07 wait must preserve guest data while the keyboard is empty"
    );
    assert_eq!(
        stub_entry,
        Bios::INT21_KBD_WAIT,
        "AH=07 dispatch must enter the installed keyboard wait stub"
    );
    assert_eq!(pc.machine.cpu.regs.cs, 0xf000);
    assert!(
        (Bios::INT21_KBD_WAIT..Bios::INT21_KBD_WAIT + 25).contains(&pc.machine.cpu.regs.ip()),
        "empty keyboard must leave the guest waiting in the installed stub"
    );

    pc.key_down(0x1e);
    pc.key_up(0x1e);
    finish(&mut pc, 0);
    assert_eq!(
        pc.machine.memory.read_u8_seg(ds, sentinel),
        0x5a,
        "AH=07 wait must leave guest data unchanged after resuming"
    );
    assert_eq!(
        pc.machine.memory.read_u8_seg(ds, returned_char),
        b'a',
        "AH=07 must return the injected character"
    );
}

#[test]
fn indos_keyboard_wait_resumes_through_rom_stub() {
    keyboard_wait_resume(false, false);
}

#[test]
fn indos_keyboard_wait_resumes_through_chained_hook() {
    keyboard_wait_resume(true, false);
}

#[test]
fn indos_buffered_input_resumes_partial_line() {
    keyboard_wait_resume(false, true);
}

fn keyboard_wait_resume(hook: bool, buffered: bool) {
    use gp64_bios::Bios;
    use gp64_memory::Memory;
    for depth in [0, 3] {
        let mut p = Com::default();
        if hook {
            p.emit(&[0xb8, 0x21, 0x35, 0xcd, 0x21, 0x89, 0x1e]); // get INT21; save BX
            p.pointer("old_int21");
            p.emit(&[0x8c, 0xc0, 0xa3]); // MOV AX,ES; save segment
            p.pointer("old_int21_segment");
            p.emit(&[0xba]);
            p.pointer("hook");
            p.emit(&[0xb8, 0x21, 0x25, 0xcd, 0x21]); // install DS:DX
        }
        if buffered {
            p.emit(&[0xba]);
            p.pointer("buffer");
            p.emit(&[0xb4, 0x0a, 0xcd, 0x21, 0x80, 0x3e]); // buffered input; CMP byte[count],1
            p.pointer("count");
            p.emit(&[1]);
            p.branch(0x75, "fail");
            p.emit(&[0x80, 0x3e]); // CMP byte[data],'a'
            p.pointer("data");
            p.emit(b"a");
            p.branch(0x75, "fail");
        } else {
            for _ in 0..2 {
                p.emit(&[0xb4, 0x07, 0xcd, 0x21, 0x3c, b'a']); // blocking input; cmp al,'a'
                p.branch(0x75, "fail");
            }
        }
        p.emit(&[0x89, 0x26]); // save SP after console completion, before exit hook
        p.pointer("completed_sp");
        p.exits();
        p.label("completed_sp");
        p.emit(&[0, 0]);
        if buffered {
            p.label("buffer");
            p.emit(&[8]);
            p.label("count");
            p.emit(&[0]);
            p.label("data");
            p.emit(&[0; 8]);
        }
        let drive = Drive::new();
        if hook {
            p.label("hook");
            p.emit(&[0x9c, 0xff, 0x1e]); // PUSHF; CALL FAR [old_int21]
            p.pointer("old_int21");
            p.emit(&[0xcf]); // IRET
            p.label("old_int21");
            p.emit(&[0, 0]);
            p.label("old_int21_segment");
            p.emit(&[0, 0]);
        }
        let completed_sp = 0x100
            + p.labels
                .iter()
                .find(|(name, _)| *name == "completed_sp")
                .unwrap()
                .1 as u16;
        drive.put("WAIT.COM", &p.done());
        let mut pc = drive.launch("WAIT.COM");
        // Query the documented InDOS pointer without assuming its placement.
        let mut probe = gp64_cpu::Cpu::new();
        pc.dos.get_indos_flag_address(&mut probe);
        let indos = Memory::seg_offset_to_linear(probe.regs.es, probe.regs.bx());
        pc.machine.memory.write_u8(indos, depth);
        let original_sp = pc.machine.cpu.regs.sp();
        for attempt in 0..2 {
            let result = pc.run_for(10_000);
            assert!(
                matches!(result, Ok(DosRunExit::Running)),
                "wait attempt={attempt}: {result:?}"
            );
            assert_eq!(pc.machine.cpu.regs.cs, 0xf000);
            assert!(
                (Bios::INT21_KBD_WAIT..Bios::INT21_KBD_WAIT + 25)
                    .contains(&pc.machine.cpu.regs.ip()),
                "must execute installed wait loop"
            );
            assert_eq!(
                pc.machine.memory.read_u8(indos),
                depth + 1,
                "active wait attempt={attempt} owns one entry"
            );
            let key = if buffered && attempt == 1 { 0x1c } else { 0x1e };
            pc.key_down(key);
            pc.key_up(key);
        }
        finish(&mut pc, 0);
        assert_eq!(
            pc.machine.memory.read_u8(indos),
            depth,
            "completed waits preserve preexisting depth"
        );
        assert_eq!(
            pc.machine
                .memory
                .read_u16_seg(pc.machine.cpu.regs.ds, completed_sp),
            original_sp,
            "wait frames fully unwind"
        );
    }
}
#[test]
fn mouse_reset_motion_and_button_are_available() {
    let mut p = Com::default();
    p.emit(&[0x31, 0xc0, 0xcd, 0x33, 0x3d, 0xff, 0xff]); // reset; installed AX=ffffh
    p.branch(0x75, "fail");
    p.emit(&[0xb8, 4, 0, 0xb9, 100, 0, 0xba, 100, 0, 0xcd, 0x33]); // set position to (100,100)
    p.label("poll");
    p.emit(&[0xb8, 0x0b, 0, 0xcd, 0x33, 0x85, 0xc9]); // read relative motion
    p.branch(0x74, "poll");
    p.emit(&[0x83, 0xf9, 12]);
    p.branch(0x75, "fail");
    p.emit(&[0x83, 0xfa, 0xfc]);
    p.branch(0x75, "fail"); // cmp dx,-4
    p.emit(&[0xb8, 3, 0, 0xcd, 0x33, 0xf6, 0xc3, 1]); // query buttons; test bl,1
    p.branch(0x74, "fail");
    p.emit(&[0x83, 0xf9, 112]);
    p.branch(0x75, "fail"); // X increased by 12
    p.emit(&[0x83, 0xfa, 98]);
    p.branch(0x75, "fail"); // Y decreased by 2 with default ratio
    p.exits();
    let drive = Drive::new();
    drive.put("MOUSE.COM", &p.done());
    let mut pc = drive.launch("MOUSE.COM");
    assert!(matches!(pc.run_for(10_000).unwrap(), DosRunExit::Running));
    pc.mouse_move(12, -4);
    pc.mouse_button(0, true);
    finish(&mut pc, 0);
    pc.mouse_button(0, false);
}

#[test]
fn unhandled_invalid_opcode_reports_exception() {
    let drive = Drive::new();
    drive.put("UD.COM", &[0x0f, 0x0b]); // UD2: invalid opcode exception, vector 6
    let mut pc = drive.launch("UD.COM");
    assert!(matches!(
        pc.run_for(100_000).unwrap(),
        DosRunExit::Machine(gp64_machine::RunExit::UnhandledException { vector: 6, .. })
    ));
}

#[test]
fn guest_invalid_opcode_handler_runs_and_returns() {
    let mut p = Com::default();
    p.emit(&[0x31, 0xc0, 0x8e, 0xc0, 0x26, 0xc7, 0x06, 0x18, 0]); // ES=0; mov word es:[vector6],handler
    p.pointer("handler");
    p.emit(&[0x8c, 0xc8, 0x26, 0xa3, 0x1a, 0, 0x31, 0xdb, 0x0f, 0x0b]); // vector segment=CS; BX=0; UD2
    p.emit(&[0x83, 0xfb, 1]);
    p.branch(0x75, "fail");
    p.exits();
    p.label("handler");
    p.emit(&[0x43, 0x55, 0x89, 0xe5, 0x36, 0x83, 0x46, 2, 2, 0x5d, 0xcf]); // inc bx; push bp; BP=SP; add ss:[BP+2],2; pop bp; iret
    let drive = Drive::new();
    drive.put("HANDLER.COM", &p.done());
    finish(&mut drive.launch("HANDLER.COM"), 0);
}

#[test]
fn missing_batch_program_returns_clear_error() {
    let drive = Drive::new();
    drive.put("MISSING.BAT", b"@ABSENT.COM\r\n");
    let mut pc = drive.launch("MISSING.BAT");
    let error = pc.run_for(100_000).unwrap_err().to_string();
    assert!(
        error.contains("program not found") && error.contains("ABSENT.COM"),
        "{error}"
    );
}

#[test]
fn guest_int21_handler_chains_saved_firmware_vector() {
    let mut p = Com::default();
    p.emit(&[0x31, 0xc0, 0x8e, 0xc0, 0x26, 0xa1, 0x84, 0, 0xa3]); // ES=0; AX=old INT21 offset
    p.pointer("old_vector");
    p.emit(&[0x26, 0xa1, 0x86, 0, 0xa3]);
    p.pointer("old_segment"); // save old segment
    p.emit(&[0x26, 0xc7, 0x06, 0x84, 0]);
    p.pointer("handler"); // install handler offset
    p.emit(&[0x8c, 0xc8, 0x26, 0xa3, 0x86, 0]); // install CS
    p.emit(&[0xba]);
    p.pointer("text");
    p.emit(&[0xb4, 9, 0xcd, 0x21, 0x80, 0x3e]);
    p.pointer("calls"); // print via handler; cmp byte [calls],1
    p.emit(&[1]);
    p.branch(0x75, "fail");
    p.emit(&[0xb8, 7, 0x4c, 0xcd, 0x21]); // exit 7 through same handler
    p.label("fail");
    p.emit(&[0xb8, 1, 0x4c, 0xcd, 0x21]);
    p.label("handler");
    p.emit(&[0x2e, 0xfe, 0x06]);
    p.pointer("calls"); // inc byte cs:[calls]
    p.emit(&[0x9c, 0x2e, 0xff, 0x1e]);
    p.pointer("old_vector"); // pushf; call far cs:[old_vector]
    p.emit(&[0xcf]); // iret to application
    p.label("calls");
    p.emit(&[0]);
    p.label("old_vector");
    p.emit(&[0, 0]);
    p.label("old_segment");
    p.emit(&[0, 0]);
    p.label("text");
    p.emit(b"CHAIN$");
    let drive = Drive::new();
    drive.put("CHAIN.COM", &p.done());
    let mut pc = drive.launch("CHAIN.COM");
    finish(&mut pc, 7);
    assert_eq!(pc.take_console_output(), b"CHAIN");
}

#[test]
fn batch_pause_keyboard_irq_resumes_next_command() {
    let drive = Drive::new();
    drive.put("PAUSE.BAT", b"@PAUSE\r\n@ECHO awake\r\n");
    let mut pc = drive.launch("PAUSE.BAT");
    for _ in 0..3 {
        assert!(matches!(pc.run_for(10_000).unwrap(), DosRunExit::Running));
    }
    assert_eq!(
        pc.take_console_output(),
        b"Press any key to continue . . .\r\n"
    );
    pc.key_down(0x1e);
    pc.key_up(0x1e);
    finish(&mut pc, 0);
    assert_eq!(pc.take_console_output(), b"awake\r\n");
}

// Synthetic callbacks deliberately outlive a slice while the shell is waiting.
fn batch_wait_irq_case(choice: bool, irq: u8, enqueue_before_resume: bool) {
    use gp64_hw::bda;
    use gp64_types::bios_layout::DOS_IDLE_STUB_START;

    let drive = Drive::new();
    let command = if choice { "CHOICE /C:A /N" } else { "PAUSE" };
    drive.put(
        "WAIT.BAT",
        format!("@{command}\r\n@ECHO first\r\n@CHILD.COM\r\n@{command}\r\n@ECHO second\r\n")
            .as_bytes(),
    );
    drive.put("CHILD.COM", &[0xb8, 0, 0x4c, 0xcd, 0x21]);
    let mut pc = drive.launch("WAIT.BAT");
    assert!(matches!(pc.run_for(0).unwrap(), DosRunExit::Running));
    assert!(matches!(pc.run_for(100).unwrap(), DosRunExit::Running));
    pc.take_console_output();
    assert_eq!(pc.machine.cpu.regs.ss, 0);
    assert_eq!(pc.machine.cpu.regs.sp(), 0x6f00);

    // INT1C or INT15/4F runs from a distinct guest segment and returns by IRET.
    let vector = if irq == 0 { 0x1c } else { 0x15 };
    let callback_segment = 0x2200;
    pc.machine.memory.write_u16(vector * 4, 0);
    pc.machine
        .memory
        .write_u16(vector * 4 + 2, callback_segment);
    let mut callback = vec![0x90; 256];
    callback.extend_from_slice(&[0x2e, 0xff, 0x06, 0x10, 0x01, 0xcf]); // inc word cs:[110]; iret
    for (offset, byte) in callback.into_iter().enumerate() {
        pc.machine.memory.write_u8(0x22000 + offset, byte);
    }
    pc.machine.cpu.regs.set_bx(0x4321);
    if irq == 0 {
        pc.machine.io.pic.raise_irq(0);
    } else {
        pc.key_down(0x1e);
    }
    assert!(matches!(pc.run_for(200).unwrap(), DosRunExit::Running));
    assert_eq!(pc.machine.cpu.regs.cs, callback_segment);
    assert_ne!(
        pc.machine.io.pic.diagnostic_snapshot().master.isr & (1 << irq),
        0
    );
    let sp = pc.machine.cpu.regs.sp();
    assert!(
        sp < 0x6f00,
        "callback must retain the IRQ and software INT frames"
    );
    let saved_frame: Vec<_> = (sp..0x6f00)
        .map(|offset| pc.machine.memory.read_u8(usize::from(offset)))
        .collect();
    if enqueue_before_resume {
        assert!(bda::kbd_enqueue(&mut pc.machine.memory, 0x1e, b'a'));
    }
    assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
    if enqueue_before_resume {
        assert_eq!(
            bda::kbd_peek(&pc.machine.memory),
            Some((0x1e, b'a')),
            "batch must not consume a queued key inside a live IRQ callback"
        );
    }
    assert_eq!(
        pc.machine.cpu.regs.cs, callback_segment,
        "batch wait must resume its unfinished IRQ callback across slices"
    );
    assert_eq!(pc.machine.cpu.regs.sp(), sp);
    assert_eq!(
        (sp..0x6f00)
            .map(|offset| pc.machine.memory.read_u8(usize::from(offset)))
            .collect::<Vec<_>>(),
        saved_frame
    );
    assert!(pc.take_console_output().is_empty());
    if irq == 0 && !enqueue_before_resume {
        assert!(bda::kbd_enqueue(&mut pc.machine.memory, 0x1e, b'a'));
    }

    let mut saw_key_in_irq = enqueue_before_resume || irq == 0;
    let mut saw_eoi_before_return = false;
    for _ in 0..2000 {
        let regs = &pc.machine.cpu.regs;
        let parked = regs.cs == 0xf000
            && (DOS_IDLE_STUB_START..=DOS_IDLE_STUB_START + 2).contains(&regs.ip())
            && regs.ss == 0
            && regs.sp() == 0x6f00;
        if parked {
            break;
        }
        let key = bda::kbd_peek(&pc.machine.memory);
        let isr = pc.machine.io.pic.diagnostic_snapshot().master.isr;
        saw_key_in_irq |= key.is_some() && isr & (1 << irq) != 0;
        saw_eoi_before_return |= isr == 0;
        assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
        if let Some(key) = key {
            assert_eq!(
                bda::kbd_peek(&pc.machine.memory),
                Some(key),
                "batch must defer key dequeue until the idle stack is restored"
            );
        }
        assert!(
            pc.take_console_output().is_empty(),
            "batch command ran before IRQ return"
        );
    }
    assert!(saw_key_in_irq, "exercise a queued key before EOI");
    assert!(
        saw_eoi_before_return,
        "exercise EOI with an unfinished return frame"
    );
    assert_eq!(pc.machine.memory.read_u16(0x22110), 1);
    assert_eq!(pc.machine.cpu.regs.cs, 0xf000);
    assert_eq!(pc.machine.cpu.regs.ss, 0);
    assert_eq!(pc.machine.cpu.regs.sp(), 0x6f00);
    assert_eq!(pc.machine.cpu.regs.bx(), 0x4321);
    assert_eq!(pc.machine.io.pic.diagnostic_snapshot().master.isr, 0);
    bda::kbd_clear(&mut pc.machine.memory); // discard a possible second synthetic key
    assert!(bda::kbd_enqueue(&mut pc.machine.memory, 0x1e, b'a'));

    // The next wait follows a child launch, so its idle context must be entered anew.
    let mut output = Vec::new();
    for _ in 0..100 {
        assert!(matches!(pc.run_for(1000).unwrap(), DosRunExit::Running));
        output.extend(pc.take_console_output());
        if !pc.programs_started().is_empty()
            && pc.machine.cpu.regs.cs == 0xf000
            && pc.machine.cpu.regs.ss == 0
            && pc.machine.cpu.regs.sp() == 0x6f00
        {
            break;
        }
    }
    assert_eq!(pc.programs_started(), ["CHILD.COM"]);
    assert!(output.windows(7).any(|bytes| bytes == b"first\r\n"));
    if choice {
        assert!(bda::kbd_enqueue(&mut pc.machine.memory, 0x2d, b'x'));
        assert!(matches!(pc.run_for(0).unwrap(), DosRunExit::Running));
        assert_eq!(bda::kbd_peek(&pc.machine.memory), None);
        assert!(pc.take_console_output().is_empty());
        // An invalid key returns Continue while CHOICE is still waiting. Enter
        // another hardware IRQ before polling again to expose lost wait state.
        pc.machine.io.pic.lower_irq(0);
        pc.machine.io.pic.raise_irq(0);
        assert!(matches!(
            pc.machine.run_for(1),
            gp64_machine::RunExit::BudgetUsed
        ));
        assert_ne!(pc.machine.io.pic.diagnostic_snapshot().master.isr & 1, 0);
        assert!(pc.machine.cpu.regs.sp() < 0x6f00);
        assert!(matches!(pc.run_for(1).unwrap(), DosRunExit::Running));
        assert!(
            pc.machine.cpu.regs.sp() < 0x6f00,
            "invalid CHOICE key must retain active wait and its next IRQ frame"
        );
    }
    assert!(bda::kbd_enqueue(&mut pc.machine.memory, 0x1e, b'a'));
    finish(&mut pc, if choice { 1 } else { 0 });
    assert_eq!(pc.take_console_output(), b"second\r\n");
}

#[test]
fn batch_pause_irq0_retains_callback_across_slices() {
    batch_wait_irq_case(false, 0, false);
}
#[test]
fn batch_choice_irq1_retains_callback_across_slices() {
    batch_wait_irq_case(true, 1, false);
}
#[test]
fn batch_pause_irq1_defers_queued_key_until_return() {
    batch_wait_irq_case(false, 1, true);
}
#[test]
fn batch_choice_irq0_defers_queued_key_until_return() {
    batch_wait_irq_case(true, 0, true);
}

#[test]
fn drop_in_launcher_prefers_batch_and_falls_back_to_executable() {
    use gp64_machine::launcher::{launch_default, DEFAULT_BATCH, DEFAULT_PROGRAM};
    let drive = Drive::new();
    drive.put(DEFAULT_PROGRAM, &mz(&[0xb8, 3, 0x4c, 0xcd, 0x21], None));
    let mut fallback = launch_default(&drive.0).unwrap();
    finish(&mut fallback, 3);
    assert_eq!(fallback.programs_started(), [DEFAULT_PROGRAM]);
    drive.put("FIRST.COM", &print_program(b"first", 0));
    drive.put("SECOND.COM", &print_program(b"second", 0));
    drive.put(
        &DEFAULT_BATCH.to_ascii_lowercase(),
        b"@FIRST.COM\r\n@SECOND.COM\r\n",
    );
    let mut batch = launch_default(&drive.0).unwrap();
    finish(&mut batch, 0);
    assert_eq!(batch.programs_started(), ["FIRST.COM", "SECOND.COM"]);
    assert_eq!(batch.take_console_output(), b"firstsecond");
}

// Independently assembled console/INT23 guests; no external program bytes.
fn ctrl_c_case(ah: u8, waited: bool, handler: u8, hook: bool) {
    for depth in [0, 3] {
        ctrl_c_input_case(ah, waited, handler, hook, false, false, depth);
    }
}
fn ctrl_c_input_case(
    ah: u8,
    waited: bool,
    handler: u8,
    hook: bool,
    flush: bool,
    partial: bool,
    depth: u8,
) {
    use gp64_memory::Memory;
    let mut p = Com::default();
    // Obtain the InDOS pointer for the guest handler's observation.
    p.emit(&[0xb4, 0x34, 0xcd, 0x21, 0x89, 0x1e]);
    p.pointer("indos_ptr");
    p.emit(&[0x8c, 0xc0, 0xa3]);
    p.pointer("indos_segment");
    if hook {
        p.emit(&[0xb8, 0x21, 0x35, 0xcd, 0x21, 0x89, 0x1e]);
        p.pointer("old_int21");
        p.emit(&[0x8c, 0xc0, 0xa3]);
        p.pointer("old_int21_segment");
        p.emit(&[0xba]);
        p.pointer("int21_hook");
        p.emit(&[0xb8, 0x21, 0x25, 0xcd, 0x21]);
    }
    if handler != 4 {
        p.emit(&[0xba]);
        p.pointer("handler");
        p.emit(&[0xb8, 0x23, 0x25, 0xcd, 0x21]);
    }
    p.emit(&[0xba]);
    p.pointer("buffer");
    p.emit(&[0xbb, 0, 0, 0xb9, 8, 0]);
    if flush {
        p.emit(&[0xb8, ah, 0x0c]);
    } else {
        p.emit(&[0xb4, ah]);
    }
    p.emit(&[0xcd, 0x21]);
    p.label("resumed");
    p.emit(&[0xa3]);
    p.pointer("result_ax");
    p.emit(&[0x89, 0x26]); // save the resumed caller's SP
    p.pointer("completed_sp");
    p.emit(&[0xc6, 0x06]);
    p.pointer("completed");
    p.emit(&[1]);
    p.exits();
    p.label("handler");
    p.emit(&[0xff, 0x06]); // INC word[counter]
    p.pointer("counter");
    p.emit(&[0x50, 0x53, 0x51, 0x52, 0x06, 0xc4, 0x1e]); // save AX/BX/CX/DX/ES; LES BX,[InDOS pointer]
    p.pointer("indos_ptr");
    p.emit(&[0x26, 0x8a, 0x07, 0xa2]); // MOV AL,ES:[BX]; save active InDOS
    p.pointer("handler_indos");
    p.emit(&[0x9c, 0xb4, 0x30, 0xcd, 0x21, 0xa2]); // nested DOS version query
    p.pointer("nested_version");
    p.emit(&[0x9d, 0x07, 0x5a, 0x59, 0x5b, 0x58]);
    match handler {
        0 => p.emit(&[0xcf]),       // IRET: restart interrupted DOS service
        1 => p.emit(&[0xf8, 0xcb]), // CLC; RETF: continue service
        2 => p.emit(&[0xf9, 0xcb]), // STC; RETF: terminate by Ctrl-C
        3 => p.emit(&[0xb8, 0x07, 0x4c, 0xcd, 0x21]), // handler calls DOS exit
        4 => (),
        _ => unreachable!(),
    }
    if hook {
        p.label("int21_hook");
        p.emit(&[0x9c, 0xff, 0x1e]); // PUSHF; CALL FAR saved DOS vector
        p.pointer("old_int21");
        p.emit(&[0xcf]);
    }
    for name in [
        "counter",
        "completed_sp",
        "indos_ptr",
        "indos_segment",
        "result_ax",
        "old_int21",
        "old_int21_segment",
    ] {
        p.label(name);
        p.emit(&[0, 0]);
    }
    p.label("completed");
    p.emit(&[0]);
    p.label("nested_version");
    p.emit(&[0]);
    p.label("handler_indos");
    p.emit(&[0xff]);
    p.label("buffer");
    p.emit(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let address = |name| 0x100 + p.labels.iter().find(|(n, _)| *n == name).unwrap().1 as u16;
    let counter = address("counter");
    let completed = address("completed");
    let completed_sp = address("completed_sp");
    let handler_indos = address("handler_indos");
    let buffer = address("buffer");
    let result_ax = address("result_ax");
    let nested_version = address("nested_version");
    let drive = Drive::new();
    drive.put("BREAK.COM", &p.done());
    let mut pc = drive.launch("BREAK.COM");
    let ds = pc.machine.cpu.regs.ds;
    let original_sp = pc.machine.cpu.regs.sp();
    let mut probe = gp64_cpu::Cpu::new();
    pc.dos.get_indos_flag_address(&mut probe);
    let indos = Memory::seg_offset_to_linear(probe.regs.es, probe.regs.bx());
    pc.machine.memory.write_u8(indos, depth);
    if partial {
        gp64_hw::bda::kbd_enqueue(&mut pc.machine.memory, 0x2c, b'z');
    }
    if waited {
        assert!(matches!(pc.run_for(100_000).unwrap(), DosRunExit::Running));
        assert_eq!(pc.machine.memory.read_u16_seg(ds, counter), 0);
        if ah == 0x3f {
            assert!(pc.machine.cpu.halted, "cooked read actually waits in HLT");
            assert_eq!(pc.machine.memory.read_u8(indos), depth);
        } else {
            assert_eq!(pc.machine.cpu.regs.cs, 0xf000);
            assert!(
                (gp64_bios::Bios::INT21_KBD_WAIT..gp64_bios::Bios::INT21_KBD_WAIT_RETURN + 1)
                    .contains(&pc.machine.cpu.regs.ip()),
                "console actually waits in ROM"
            );
            assert_eq!(pc.machine.memory.read_u8(indos), depth + 1);
        }
    }
    if waited {
        pc.key_down(0x1d); // Ctrl
        pc.key_down(0x2e); // C
        pc.key_up(0x2e);
        pc.key_up(0x1d);
    } else {
        gp64_hw::bda::kbd_enqueue(&mut pc.machine.memory, 0x2e, 3);
    }
    let result = pc.run_for(100_000).unwrap();
    if handler == 4 {
        assert!(
            matches!(result, DosRunExit::Exited(_)),
            "default INT23 must terminate, got {result:?}"
        );
    } else {
        assert_eq!(
            pc.machine.memory.read_u16_seg(ds, counter),
            1,
            "INT23 handler must actually execute"
        );
        assert_eq!(
            pc.machine.memory.read_u8_seg(ds, handler_indos),
            depth,
            "INT23 permits DOS reentry"
        );
        assert_eq!(
            pc.machine.memory.read_u8_seg(ds, nested_version),
            6,
            "INT23 nested DOS call executes"
        );
        if handler >= 2 {
            let expected = if handler == 3 { 7 } else { 0 };
            assert!(
                matches!(result, DosRunExit::Exited(code) if code == expected),
                "INT23 termination must complete: {result:?}"
            );
        } else {
            assert!(
                matches!(result, DosRunExit::Running),
                "returning INT23 must restart input: {result:?}"
            );
            assert_eq!(
                pc.machine.memory.read_u8_seg(ds, completed),
                0,
                "caller must not resume before input completes"
            );
            pc.key_down(0x1e);
            pc.key_up(0x1e);
            if matches!(ah, 0x0a | 0x3f) {
                pc.key_down(0x1c);
                pc.key_up(0x1c);
            }
            finish(&mut pc, 0);
            assert_eq!(
                pc.machine.memory.read_u8_seg(ds, completed),
                1,
                "original caller IP resumes"
            );
            assert_eq!(
                pc.machine.memory.read_u16_seg(ds, completed_sp),
                original_sp,
                "INT21 and INT23 frames fully unwind"
            );
            if matches!(ah, 1 | 8) {
                assert_eq!(pc.machine.memory.read_u16_seg(ds, result_ax) as u8, b'a');
            } else if ah == 0x3f {
                assert_eq!(pc.machine.memory.read_u16_seg(ds, result_ax), 3);
            }
            if ah == 0x0a {
                assert_eq!(
                    pc.machine.memory.read_u8_seg(ds, buffer + 1),
                    if partial { 2 } else { 1 }
                );
                assert_eq!(
                    pc.machine.memory.read_u8_seg(ds, buffer + 2),
                    if partial { b'z' } else { b'a' }
                );
            } else if ah == 0x3f {
                assert_eq!(pc.machine.memory.read_u8_seg(ds, buffer), b'a');
            }
        }
    }
    assert_eq!(
        pc.machine.memory.read_u8(indos),
        depth,
        "InDOS ownership fully released"
    );
    if handler == 2 || handler == 4 {
        assert_eq!(pc.dos.last_termination_type(), 1, "Ctrl-C termination type");
    }
}
macro_rules! ctrl_c_tests {
    ($($name:ident: ($ah:expr, $waited:expr, $handler:expr)),* $(,)?) => {$ (
        #[test]
        fn $name() { ctrl_c_case($ah, $waited, $handler, false); }
    )*};
}
ctrl_c_tests! {
    ctrl_c_ah01_iret: (1, false, 0),
    ctrl_c_ah08_iret: (8, false, 0),
    ctrl_c_ah0a_iret: (10, false, 0),
    ctrl_c_ah3f_iret: (63, false, 0),
    ctrl_c_ah01_waited: (1, true, 0),
    ctrl_c_ah08_waited: (8, true, 0),
    ctrl_c_ah0a_waited: (10, true, 0),
    ctrl_c_ah3f_waited: (63, true, 0),
    ctrl_c_ah01_retf: (1, false, 1),
    ctrl_c_ah08_retf: (8, false, 1),
    ctrl_c_ah0a_retf: (10, false, 1),
    ctrl_c_ah3f_retf: (63, false, 1),
    ctrl_c_ah01_retf_terminate: (1, false, 2),
    ctrl_c_ah08_retf_terminate: (8, false, 2),
    ctrl_c_ah0a_retf_terminate: (10, false, 2),
    ctrl_c_ah3f_retf_terminate: (63, false, 2),
    ctrl_c_ah01_dos_terminate: (1, false, 3),
    ctrl_c_ah08_dos_terminate: (8, false, 3),
    ctrl_c_ah0a_dos_terminate: (10, false, 3),
    ctrl_c_ah3f_dos_terminate: (63, false, 3),
    ctrl_c_ah01_default: (1, false, 4),
    ctrl_c_ah08_default: (8, false, 4),
    ctrl_c_ah0a_default: (10, false, 4),
    ctrl_c_ah3f_default: (63, false, 4),
}

#[test]
fn ctrl_c_ah01_hooked_wait() {
    ctrl_c_case(1, true, 0, true);
}
#[test]
fn ctrl_c_ah08_hooked_wait() {
    ctrl_c_case(8, true, 0, true);
}
#[test]
fn ctrl_c_ah0a_hooked_wait() {
    ctrl_c_case(10, true, 0, true);
}
ctrl_c_tests! {
    ctrl_c_ah01_waited_retf: (1, true, 1),
    ctrl_c_ah08_waited_retf: (8, true, 1),
    ctrl_c_ah0a_waited_retf: (10, true, 1),
    ctrl_c_ah3f_waited_retf: (63, true, 1),
    ctrl_c_ah01_waited_retf_terminate: (1, true, 2),
    ctrl_c_ah08_waited_retf_terminate: (8, true, 2),
    ctrl_c_ah0a_waited_retf_terminate: (10, true, 2),
    ctrl_c_ah3f_waited_retf_terminate: (63, true, 2),
    ctrl_c_ah01_waited_dos_terminate: (1, true, 3),
    ctrl_c_ah08_waited_dos_terminate: (8, true, 3),
    ctrl_c_ah0a_waited_dos_terminate: (10, true, 3),
    ctrl_c_ah3f_waited_dos_terminate: (63, true, 3),
    ctrl_c_ah01_waited_default: (1, true, 4),
    ctrl_c_ah08_waited_default: (8, true, 4),
    ctrl_c_ah0a_waited_default: (10, true, 4),
    ctrl_c_ah3f_waited_default: (63, true, 4),
}

fn ctrl_c_child_case(child_handler: u8) {
    let drive = Drive::new();
    let mut child = Com::default();
    if child_handler != 4 {
        child.emit(&[0xba]);
        child.pointer("handler");
        child.emit(&[0xb8, 0x23, 0x25, 0xcd, 0x21]);
    }
    child.emit(&[0xb4, 8, 0xcd, 0x21]);
    child.exits();
    child.label("handler");
    child.emit(&[0xb2, b'!', 0xb4, 2, 0xcd, 0x21]); // observable child handler
    if child_handler == 2 {
        child.emit(&[0xf9, 0xcb]);
    } else {
        child.emit(&[0xb8, 7, 0x4c, 0xcd, 0x21]);
    }
    drive.put("CHILD.COM", &child.done());
    let mut p = Com::default();
    p.emit(&[0x1e, 0x07, 0xbb, 0, 0x10, 0xb4, 0x4a, 0xcd, 0x21]); // resize parent for EXEC
    p.emit(&[0x8c, 0xd8, 0xa3]);
    p.pointer("tail_segment");
    p.emit(&[0xba]);
    p.pointer("handler");
    p.emit(&[0xb8, 0x23, 0x25, 0xcd, 0x21, 0xb4, 8, 0xcd, 0x21]);
    p.emit(&[0x89, 0x26]);
    p.pointer("completed_sp");
    p.exits();
    p.label("handler");
    p.emit(&[0xff, 0x06]);
    p.pointer("counter");
    p.emit(&[0x60, 0x1e, 0x06, 0x1e, 0x07, 0xba]); // PUSHA; save DS/ES; ES=DS
    p.pointer("child");
    p.emit(&[0xbb]);
    p.pointer("params");
    p.emit(&[0xb8, 0, 0x4b, 0xcd, 0x21, 0xb4, 0x4d, 0xcd, 0x21, 0xa3]);
    p.pointer("child_status");
    p.emit(&[0x07, 0x1f, 0x61, 0xcf]); // restore; IRET restarts original parent input
    p.label("child");
    p.emit(b"CHILD.COM\0");
    p.label("params");
    p.emit(&[0, 0]);
    p.pointer("tail");
    p.label("tail_segment");
    p.emit(&[0; 10]);
    p.label("tail");
    p.emit(&[0, 13]);
    for label in ["counter", "completed_sp", "child_status"] {
        p.label(label);
        p.emit(&[0, 0]);
    }
    let address = |name| 0x100 + p.labels.iter().find(|(n, _)| *n == name).unwrap().1 as u16;
    let counter = address("counter");
    let completed_sp = address("completed_sp");
    let status = address("child_status");
    drive.put("PARENT.COM", &p.done());
    let mut pc = drive.launch("PARENT.COM");
    let parent = pc.dos.current_psp();
    let ds = pc.machine.cpu.regs.ds;
    let sp = pc.machine.cpu.regs.sp();
    let default = pc.machine.memory.read_u32(0x23 * 4);
    gp64_hw::bda::kbd_enqueue(&mut pc.machine.memory, 0x2e, 3);
    let result = pc.run_for(100_000).unwrap();
    assert_eq!(
        pc.machine.memory.read_u16_seg(ds, counter),
        1,
        "parent INT23 must execute before nested EXEC"
    );
    assert!(
        matches!(result, DosRunExit::Running),
        "handler must reach nested EXEC child: {result:?}"
    );
    assert_ne!(
        pc.dos.current_psp(),
        parent,
        "nested EXEC reaches child wait"
    );
    if child_handler == 4 {
        pc.machine.memory.write_u32(0x23 * 4, default);
    }
    pc.key_down(0x1d);
    pc.key_down(0x2e);
    pc.key_up(0x2e);
    pc.key_up(0x1d);
    assert!(matches!(pc.run_for(100_000).unwrap(), DosRunExit::Running));
    assert_eq!(
        pc.dos.current_psp(),
        parent,
        "Ctrl-C child returns to waiting parent handler"
    );
    assert_eq!(
        pc.machine.memory.read_u16_seg(ds, counter),
        1,
        "child exit retains only parent's suspended service"
    );
    assert_eq!(
        pc.machine.memory.read_u16_seg(ds, status),
        if child_handler == 3 { 7 } else { 0x100 },
        "child termination status reaches parent DOS call"
    );
    if child_handler != 4 {
        assert!(
            pc.take_console_output().contains(&b'!'),
            "child INT23 must execute"
        );
    }
    pc.key_down(0x1e);
    pc.key_up(0x1e);
    finish(&mut pc, 0);
    assert_eq!(
        pc.machine.memory.read_u16_seg(ds, completed_sp),
        sp,
        "nested INT23/EXEC frames fully unwind"
    );
}
#[test]
fn ctrl_c_child_retf_termination_preserves_parent_continuation() {
    ctrl_c_child_case(2);
}
#[test]
fn ctrl_c_child_dos_termination_preserves_parent_continuation() {
    ctrl_c_child_case(3);
}
#[test]
fn ctrl_c_child_default_termination_preserves_parent_continuation() {
    ctrl_c_child_case(4);
}

#[test]
fn ctrl_c_ah0c_ah01_restart_does_not_flush_again() {
    ctrl_c_input_case(1, true, 0, false, true, false, 0);
}
#[test]
fn ctrl_c_ah0c_ah08_restart_does_not_flush_again() {
    ctrl_c_input_case(8, true, 0, false, true, false, 0);
}
#[test]
fn ctrl_c_ah0c_ah0a_restart_does_not_flush_again() {
    ctrl_c_input_case(10, true, 0, false, true, false, 0);
}
#[test]
fn ctrl_c_ah0a_partial_line_survives_restart() {
    ctrl_c_input_case(10, true, 0, false, false, true, 0);
}
#[test]
fn ctrl_c_ah3f_partial_line_discarded_before_restart() {
    ctrl_c_input_case(63, true, 0, false, false, true, 0);
}

// These fixtures feed set-1 bytes through the real controller/PIC/IRQ1 stub.
fn lock_key_run_irq(pc: &mut DosMachine, bytes: &[u8]) {
    pc.machine
        .io
        .keyboard
        .shared_state()
        .inject_scancode(bytes, 0, true);
    pc.machine.io.keyboard.stage_output(&mut pc.machine.io.pic);
    assert!(
        matches!(pc.run_for(100_000).unwrap(), DosRunExit::Running),
        "input unexpectedly resumed/terminated waiting guest"
    );
}
fn lock_key_idle(drive: &Drive) -> DosMachine {
    drive.put("IDLE.COM", &[0xfb, 0xf4, 0xeb, 0xfd]); // sti; hlt; jmp hlt
    drive.launch("IDLE.COM")
}

#[test]
fn lock_key_transitions_update_bda_without_characters() {
    for (scan, bit) in [(0x3a, 0x40), (0x45, 0x20), (0x46, 0x10)] {
        let drive = Drive::new();
        let mut pc = lock_key_idle(&drive);
        // Preserve unrelated active/down bits through both transitions.
        pc.machine.memory.write_u8(0x417, 0x08);
        pc.machine.memory.write_u8(0x418, 0x08);
        lock_key_run_irq(&mut pc, &[scan]);
        assert_eq!(
            gp64_hw::bda::kbd_peek(&pc.machine.memory),
            None,
            "lock make queued a character: {scan:02x}"
        );
        assert_eq!(pc.machine.memory.read_u8(0x417), 0x08 | bit);
        assert_eq!(pc.machine.memory.read_u8(0x418), 0x08 | bit);
        lock_key_run_irq(&mut pc, &[scan | 0x80]);
        assert_eq!(pc.machine.memory.read_u8(0x417), 0x08 | bit);
        assert_eq!(pc.machine.memory.read_u8(0x418), 0x08);
        assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
    }
}

#[test]
fn lock_key_repeat_make_toggles_once_until_break() {
    for (scan, bit) in [(0x3a, 0x40), (0x45, 0x20), (0x46, 0x10)] {
        let drive = Drive::new();
        let mut pc = lock_key_idle(&drive);
        lock_key_run_irq(&mut pc, &[scan, scan]);
        assert_eq!(
            pc.machine.memory.read_u8(0x417) & bit,
            bit,
            "repeat make toggled lock off: {scan:02x}"
        );
        assert_eq!(pc.machine.memory.read_u8(0x418) & bit, bit);
        lock_key_run_irq(&mut pc, &[scan | 0x80, scan]);
        assert_eq!(pc.machine.memory.read_u8(0x417) & bit, 0);
        assert_eq!(pc.machine.memory.read_u8(0x418) & bit, bit);
        lock_key_run_irq(&mut pc, &[scan | 0x80]);
        assert_eq!(pc.machine.memory.read_u8(0x418) & bit, 0);
        assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
    }
}

#[test]
fn lock_key_letter_is_first_guest_int16_read() {
    for (scan, ascii) in [(0x3a, b'A'), (0x45, b'a'), (0x46, b'a')] {
        let mut p = Com::default();
        p.emit(&[0xb4, 0, 0xcd, 0x16, 0x3d, ascii, 0x1e]); // read; cmp ax,letter
        p.branch(0x75, "fail");
        p.exits();
        let drive = Drive::new();
        drive.put("READ.COM", &p.done());
        let mut pc = drive.launch("READ.COM");
        pc.key_down(scan);
        pc.key_up(scan);
        pc.key_down(0x1e);
        pc.key_up(0x1e);
        finish(&mut pc, 0);
        assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
    }
}

#[test]
fn lock_key_pause_sequence_preserves_held_ctrl_and_numlock() {
    let drive = Drive::new();
    let mut pc = lock_key_idle(&drive);
    lock_key_run_irq(&mut pc, &[0x1d, 0x45]); // hold Ctrl and NumLock
    let primary = pc.machine.memory.read_u8(0x417);
    let secondary = pc.machine.memory.read_u8(0x418);
    gp64_hw::bda::kbd_clear(&mut pc.machine.memory);
    for byte in [0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5] {
        lock_key_run_irq(&mut pc, &[byte]);
    }
    assert_eq!(
        pc.machine.memory.read_u8(0x417),
        primary,
        "Pause's fake Ctrl/NumLock bytes changed held state"
    );
    assert_eq!(pc.machine.memory.read_u8(0x418), secondary);
    assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
    lock_key_run_irq(&mut pc, &[0x9d, 0xc5]);
    assert_eq!(pc.machine.memory.read_u8(0x417), 0x20);
    assert_eq!(pc.machine.memory.read_u8(0x418) & 0x20, 0);
    // Ordinary E0 cursor keys still use the extended translation and reset
    // the prefix before the following ordinary letter.
    lock_key_run_irq(&mut pc, &[0xe0]);
    lock_key_run_irq(&mut pc, &[0x48]);
    assert_eq!(
        gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
        Some((0x48, 0xe0))
    );
    assert_eq!(pc.machine.memory.read_u8(0x417), 0x20);
    assert_eq!(pc.machine.memory.read_u8(0x418) & 0x20, 0);
    lock_key_run_irq(&mut pc, &[0x1e]);
    assert_eq!(
        gp64_hw::bda::kbd_peek(&pc.machine.memory),
        Some((0x1e, b'a'))
    );
}

#[test]
fn lock_key_transitions_do_not_resume_batch_pause() {
    let drive = Drive::new();
    drive.put("PAUSE.BAT", b"@PAUSE\r\n@ECHO awake\r\n");
    let mut pc = drive.launch("PAUSE.BAT");
    lock_key_run_irq(&mut pc, &[]);
    assert_eq!(
        pc.take_console_output(),
        b"Press any key to continue . . .\r\n"
    );
    for scan in [0x3a, 0x45, 0x46] {
        lock_key_run_irq(&mut pc, &[scan]);
        lock_key_run_irq(&mut pc, &[scan]); // held repeat must also keep waiting
        lock_key_run_irq(&mut pc, &[scan | 0x80]);
        assert!(
            pc.take_console_output().is_empty(),
            "lock transition resumed batch PAUSE"
        );
    }
    pc.key_down(0x1e);
    pc.key_up(0x1e);
    finish(&mut pc, 0);
    assert_eq!(pc.take_console_output(), b"awake\r\n");
}

#[test]
fn real_idtr_dos_firmware_dispatch() {
    for relocated in [true, false] {
        let drive = Drive::new();
        let [lo, hi] = if relocated { 0x1234_u16 } else { 0 }.to_le_bytes();
        // BX=0; BIOS video query; compare BX; exit zero on match or one on failure.
        drive.put(
            "VECTOR.COM",
            &[
                0xbb, 0, 0, 0xb4, 0x0f, 0xcd, 0x10, 0x81, 0xfb, lo, hi, 0x75, 5, 0xb8, 0, 0x4c,
                0xcd, 0x21, 0xb8, 1, 0x4c, 0xcd, 0x21,
            ],
        );
        let mut pc = drive.launch("VECTOR.COM");
        if relocated {
            pc.machine.memory.ram.copy_within(0..0x400, 0x2000);
            pc.machine.cpu.regs.idtr.base = 0x2000;
            pc.machine.memory.write_u32(0x2040, 0x900);
            // Independent custom video handler: BX=1234h; IRET.
            pc.machine.memory.ram[0x900..0x904].copy_from_slice(&[0xbb, 0x34, 0x12, 0xcf]);
        }
        finish(&mut pc, 0);
    }
}

// Snapshot the BIOS query without disturbing the idle guest's register state.
fn extended_keyboard_status(pc: &mut DosMachine) -> u16 {
    let saved = pc.machine.cpu.regs.clone();
    pc.machine.cpu.regs.set_ah(0x12);
    pc.bios
        .int16(
            &mut pc.machine.cpu,
            &mut pc.machine.memory,
            &mut pc.machine.io.keyboard,
        )
        .unwrap();
    let result = pc.machine.cpu.regs.ax();
    pc.machine.cpu.regs = saved;
    result
}

#[test]
fn extended_keyboard_overlapping_modifiers_survive_either_release_order() {
    for (scan, aggregate, left, right) in [(0x1d, 4, 1, 4), (0x38, 8, 2, 8)] {
        for right_first in [false, true] {
            let drive = Drive::new();
            let mut pc = lock_key_idle(&drive);
            lock_key_run_irq(&mut pc, &[scan, 0xe0, scan]);
            // Release one side, then check that its peer still holds the aggregate.
            let first = if right_first {
                vec![0xe0, scan | 0x80]
            } else {
                vec![scan | 0x80]
            };
            lock_key_run_irq(&mut pc, &first);
            assert_eq!(
                pc.machine.memory.read_u8(0x417) & aggregate,
                aggregate,
                "release of one side cleared the held peer: {scan:02x}, right first={right_first}"
            );
            let remaining = if right_first { left } else { right };
            assert_eq!(
                extended_keyboard_status(&mut pc),
                (remaining << 8) | u16::from(aggregate)
            );
            assert_eq!(
                pc.machine.memory.read_u8(0x418) & 3,
                if right_first { left as u8 } else { 0 }
            );
            assert_eq!(
                pc.machine.memory.read_u8(0x496) & 12,
                if right_first { 0 } else { right as u8 }
            );
            let second = if right_first {
                vec![scan | 0x80]
            } else {
                vec![0xe0, scan | 0x80]
            };
            lock_key_run_irq(&mut pc, &second);
            assert_eq!(extended_keyboard_status(&mut pc), 0);
            assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
            lock_key_run_irq(&mut pc, &[0x1e]);
            assert_eq!(
                gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
                Some((0x1e, b'a'))
            );
        }
    }
}

#[test]
fn extended_keyboard_printscreen_fake_shift_preserves_real_shift() {
    // Existing Shift bit assignment is deliberately outside this fix's scope.
    for (scan, bit) in [(0x2a, 1), (0x36, 2)] {
        let drive = Drive::new();
        let mut pc = lock_key_idle(&drive);
        lock_key_run_irq(&mut pc, &[scan]);
        for byte in [
            0xe0, 0x2a, 0xe0, 0x37, 0xe0, 0xb7, 0xe0, 0xaa, 0xe0, 0x36, 0xe0, 0xb6,
        ] {
            lock_key_run_irq(&mut pc, &[byte]);
            assert_eq!(
                pc.machine.memory.read_u8(0x417),
                bit,
                "PrintScreen fake Shift changed held state: real={scan:02x}, byte={byte:02x}"
            );
        }
        // Retain the existing translation of the actual PrintScreen make.
        assert_eq!(
            gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
            Some((0x37, 0xe0))
        );
        assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
        lock_key_run_irq(&mut pc, &[0x1e]);
        assert_eq!(
            gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
            Some((0x1e, b'A'))
        );
        lock_key_run_irq(&mut pc, &[scan | 0x80, 0x1e]);
        assert_eq!(pc.machine.memory.read_u8(0x417), 0);
        assert_eq!(
            gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
            Some((0x1e, b'a'))
        );
    }
}

#[test]
fn extended_keyboard_query_combines_bda_flags_and_masks_pause_insert() {
    let drive = Drive::new();
    let mut pc = lock_key_idle(&drive);
    pc.machine.memory.write_u8(0x417, 0xa5);
    // Left modifiers, SysRq and held locks; Pause/Insert bits are not AH status bits.
    pc.machine.memory.write_u8(0x418, 0xff);
    pc.machine.memory.write_u8(0x496, 0xac); // right modifiers plus unrelated flags
    assert_eq!(extended_keyboard_status(&mut pc), 0xffa5);
    pc.machine.memory.write_u8(0x418, 0x88); // only Pause and Insert down
    pc.machine.memory.write_u8(0x496, 0xf3); // no right modifier
    assert_eq!(extended_keyboard_status(&mut pc), 0x00a5);
    // Modifier transitions preserve other BIOS bytes' unrelated fields.
    lock_key_run_irq(&mut pc, &[0x1d, 0xe0, 0x38]);
    assert_eq!(pc.machine.memory.read_u8(0x418), 0x89);
    assert_eq!(pc.machine.memory.read_u8(0x496), 0xfb);
    assert_eq!(extended_keyboard_status(&mut pc) >> 8, 9);
}

#[test]
fn extended_keyboard_guest_intercept_absorbs_sequence_before_state_update() {
    let drive = Drive::new();
    let mut pc = lock_key_idle(&drive);
    let vector = pc.machine.memory.read_u32(0x54);
    // Clear CF in the saved INT15 interrupt frame to absorb each input byte.
    for (i, byte) in [0x55, 0x89, 0xe5, 0x83, 0x66, 0x06, 0xfe, 0x5d, 0xcf]
        .into_iter()
        .enumerate()
    {
        pc.machine.memory.write_u8(0x22000 + i, byte);
    }
    pc.machine.memory.write_u32(0x54, 0x22000000);
    for byte in [0xe0, 0x1d, 0xe0, 0x38, 0xe0, 0x2a, 0xe0, 0x37] {
        lock_key_run_irq(&mut pc, &[byte]);
    }
    assert_eq!(pc.machine.memory.read_u8(0x417), 0);
    assert_eq!(pc.machine.memory.read_u8(0x418), 0);
    assert_eq!(pc.machine.memory.read_u8(0x496) & 12, 0);
    assert_eq!(gp64_hw::bda::kbd_peek(&pc.machine.memory), None);
    pc.machine.memory.write_u32(0x54, vector);
    lock_key_run_irq(&mut pc, &[0x1e]);
    assert_eq!(
        gp64_hw::bda::kbd_dequeue(&mut pc.machine.memory),
        Some((0x1e, b'a'))
    );
    lock_key_run_irq(&mut pc, &[0xe0, 0x1d, 0xe0, 0x38]);
    assert_eq!(extended_keyboard_status(&mut pc), 0x0c0c);
}
