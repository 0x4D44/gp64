//! Two DOS machines, Hayes command flow and verified payload over real TCP PCM.
use gp64_machine::{
    modem::{LineStatus, ModemClock, ModemConnection, ModemLine},
    DosMachine, DosRunExit,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Drive(PathBuf);
impl Drive {
    fn new(caller: &[u8], answer: &[u8]) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!(
                "serial-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("CALL.COM"), caller).unwrap();
        fs::write(path.join("ANSWER.COM"), answer).unwrap();
        Self(path)
    }
}
impl Drop for Drive {
    fn drop(&mut self) {
        fs::remove_file(self.0.join("CALL.COM")).unwrap();
        fs::remove_file(self.0.join("ANSWER.COM")).unwrap();
        fs::remove_dir(&self.0).unwrap();
    }
}
/// How the phone lines start and how the call ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// Lines start with the modems, as launch flags do.
    Launch,
    /// Modems attach with no line; lines start after the guests run.
    RunTime,
    /// As `RunTime`; once both guests have their payload, the host (0) or
    /// joiner (1) side hangs up the line instead of the guest.
    HangUp(usize),
}
/// The test program's offset of its stage byte from the end of the image.
const STAGE_FROM_END: usize = 3;
const STAGE_HANGING_UP: u8 = 5;
fn call(caller: &[u8], answer: &[u8], com: u8) {
    call_with(caller, answer, com, Line::Launch);
}
/// The caller raises DTR again where it would drop it, so only a line
/// hang-up can end the call. The answerer never hangs up by itself.
fn caller_keeping_dtr(caller: &[u8], base: u16) -> Vec<u8> {
    let [low, high] = (base + 4).to_le_bytes();
    // mov dx, BASE+4 / mov al, 2 / out dx, al
    let drop_dtr = [0xba, low, high, 0xb0, 0x02, 0xee];
    let at = caller
        .windows(drop_dtr.len())
        .position(|window| window == drop_dtr)
        .expect("caller drops DTR once");
    let mut patched = caller.to_vec();
    patched[at + 4] = 0x03;
    patched
}
fn call_with(caller: &[u8], answer: &[u8], com: u8, line: Line) {
    let drive = Drive::new(caller, answer);
    let mut host = DosMachine::launch(&drive.0, "ANSWER.COM", "").unwrap();
    let mut join = DosMachine::launch(&drive.0, "CALL.COM", "").unwrap();
    let diagnostic = [
        usize::from(host.dos.current_psp()) * 16 + 0x100 + answer.len() - STAGE_FROM_END,
        usize::from(join.dos.current_psp()) * 16 + 0x100 + caller.len() - STAGE_FROM_END,
    ];
    let rate = 1_000_000;
    host.machine.set_cycles_per_second(rate);
    join.machine.set_cycles_per_second(rate);
    let mut lines: Option<[ModemLine; 2]> = None;
    if line == Line::Launch {
        let address = host
            .machine
            .attach_modem_with_clock(&ModemConnection::Host(0), com, ModemClock::EmulatedTime)
            .expect("bind real TCP modem listener (sandbox may deny sockets)")
            .unwrap();
        join.machine
            .attach_modem_with_clock(
                &ModemConnection::Join(format!("127.0.0.1:{}", address.port())),
                com,
                ModemClock::EmulatedTime,
            )
            .expect("connect real TCP modem joiner");
    } else {
        let attached = [&mut host, &mut join].map(|dos| {
            dos.machine
                .attach_modem_line_with_clock(com, ModemClock::EmulatedTime)
                .unwrap()
        });
        // The guests program their UARTs and talk to the modems before
        // any line exists.
        for dos in [&mut host, &mut join] {
            assert!(matches!(
                dos.run_for(rate / 200).unwrap(),
                DosRunExit::Running
            ));
        }
        assert_eq!(attached[0].status(), LineStatus::Off);
        let address = attached[0]
            .start(&ModemConnection::Host(0))
            .expect("bind real TCP modem listener (sandbox may deny sockets)")
            .unwrap();
        attached[1]
            .start(&ModemConnection::Join(format!(
                "127.0.0.1:{}",
                address.port()
            )))
            .unwrap();
        lines = Some(attached);
    }
    // Each machine's cycle when the line was hung up.
    let mut hung_up: Option<[u64; 2]> = None;
    let start = Instant::now();
    let mut exits = [false; 2];
    let mut exit_cycles = [0; 2];
    // 90 emulated seconds decide the result. Wall time only guards a hang.
    let emulated_deadline = rate * 90;
    while host.machine.cycle() < emulated_deadline && join.machine.cycle() < emulated_deadline {
        for (index, dos) in [&mut host, &mut join].into_iter().enumerate() {
            if exits[index] {
                continue;
            }
            match dos.run_for(rate / 1000).unwrap() {
                DosRunExit::Machine(exit) => {
                    panic!("COM{com} peer {index}: unexpected machine exit {exit:?}")
                }
                DosRunExit::Running => {}
                DosRunExit::Exited(code) => {
                    assert_eq!(
                        code, 0,
                        "COM{com} peer {index}: UART payload or carrier assertion failed; stage/received/expected {:?}", &dos.machine.memory.ram[diagnostic[index]..diagnostic[index]+3]
                    );
                    exits[index] = true;
                    exit_cycles[index] = dos.machine.cycle();
                }
            }
        }
        if let (Line::HangUp(side), Some(lines), None) = (line, &lines, hung_up) {
            let stages = [
                host.machine.memory.ram[diagnostic[0]],
                join.machine.memory.ram[diagnostic[1]],
            ];
            if stages == [STAGE_HANGING_UP; 2] {
                assert!(lines
                    .iter()
                    .all(|line| matches!(line.status(), LineStatus::Connected(_))));
                lines[side].hang_up();
                hung_up = Some([host.machine.cycle(), join.machine.cycle()]);
            }
        }
        if exits.iter().all(|exit| *exit) {
            if let (Line::HangUp(side), Some(lines)) = (line, &lines) {
                let at = hung_up.expect("the guests ended the call themselves");
                // Carrier drops promptly, not after the modem's own
                // lost-carrier timeout.
                for index in 0..2 {
                    let after = exit_cycles[index] - at[index];
                    assert!(
                        after < rate / 4,
                        "COM{com} peer {index}: NO CARRIER {after} cycles after hang-up"
                    );
                }
                assert_eq!(lines[side].status(), LineStatus::Off);
                // A host keeps listening after the caller goes; a joiner is done.
                if side == 1 {
                    assert!(matches!(lines[0].status(), LineStatus::Listening(_)));
                } else {
                    assert_eq!(lines[1].status(), LineStatus::Off);
                }
            } else if let Some(lines) = &lines {
                assert!(matches!(lines[0].status(), LineStatus::Listening(_)));
                assert_eq!(lines[1].status(), LineStatus::Off);
            }
            for dos in [&mut host, &mut join] {
                let uart = &mut dos.machine.io.serial.ports[usize::from(com - 1)];
                assert_eq!(
                    uart.read(uart.base_port() + 6) & 0x80,
                    0,
                    "carrier remains after hangup"
                );
            }
            eprintln!(
                "COM{com}: completed; host cycles {}, join cycles {}, rate {rate}",
                host.machine.cycle(),
                join.machine.cycle()
            );
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(600),
            "COM{com}: wall-clock modem timeout; host cycle {}, join cycle {}, exits {exits:?}",
            host.machine.cycle(),
            join.machine.cycle()
        );
    }
    panic!("COM{com}: modem exceeded 90 emulated seconds; host cycle {}, join cycle {}, exits {exits:?}", host.machine.cycle(), join.machine.cycle());
}
#[test]
fn dial_ata_bidirectional_data_and_dtr_hangup_com2() {
    call(
        include_bytes!("serial/caller.com"),
        include_bytes!("serial/answer.com"),
        2,
    );
}
#[test]
fn dial_autoanswer_bidirectional_data_and_ath_hangup_com1() {
    call(
        include_bytes!("serial/caller-ath.com"),
        include_bytes!("serial/answer-auto.com"),
        1,
    );
}
#[test]
fn run_time_host_and_join_carry_data_both_ways_com2() {
    call_with(
        include_bytes!("serial/caller.com"),
        include_bytes!("serial/answer.com"),
        2,
        Line::RunTime,
    );
}
#[test]
fn host_hang_up_drops_carrier_on_both_guests() {
    call_with(
        &caller_keeping_dtr(include_bytes!("serial/caller.com"), 0x2f8),
        include_bytes!("serial/answer.com"),
        2,
        Line::HangUp(0),
    );
}
#[test]
fn joiner_hang_up_drops_carrier_on_both_guests() {
    call_with(
        &caller_keeping_dtr(include_bytes!("serial/caller.com"), 0x2f8),
        include_bytes!("serial/answer.com"),
        2,
        Line::HangUp(1),
    );
}
