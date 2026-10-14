//! LAN auto-link for one cable or phone line. An enabled link listens on an
//! OS-chosen port; LAN discovery advertises it, and of two gp64 processes
//! that both have the link enabled, the one with the higher instance id
//! places the call. Nothing here touches a socket: [`AutoLink::step`] says
//! what the frontend should do with the line next.
use gp64_machine::modem::LineStatus;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

/// Wait after a failed or dropped connection before trying again.
const RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    None,
    /// Listen on an OS-chosen port.
    Listen,
    /// Dial this peer, replacing the idle listener.
    Call(SocketAddr),
    HangUp,
}

#[derive(Debug, Default)]
pub struct AutoLink {
    pub enabled: bool,
    last: LineStatus,
    retry_at: Option<Instant>,
}
impl AutoLink {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }
    /// The next action for a line in `status`. `peers` are the discovered
    /// processes' ids and their listening addresses for this link.
    pub fn step(
        &mut self,
        status: &LineStatus,
        own_id: u64,
        mut peers: impl Iterator<Item = (u64, SocketAddr)>,
        now: Instant,
    ) -> Step {
        let dropped = matches!(self.last, LineStatus::Connected(_))
            && !matches!(status, LineStatus::Connected(_));
        let failed =
            matches!(status, LineStatus::Failed(_)) && !matches!(self.last, LineStatus::Failed(_));
        if dropped || failed {
            self.retry_at = Some(now + RETRY);
        }
        self.last = status.clone();
        if !self.enabled {
            return if status.is_active() {
                Step::HangUp
            } else {
                Step::None
            };
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return Step::None;
        }
        match status {
            LineStatus::Off | LineStatus::Failed(_) => Step::Listen,
            LineStatus::Listening(_) => peers
                .find(|(id, _)| *id < own_id)
                .map_or(Step::None, |(_, address)| Step::Call(address)),
            LineStatus::Connecting(_) | LineStatus::Connected(_) => Step::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: u64, port: u16) -> (u64, SocketAddr) {
        (id, SocketAddr::from(([192, 168, 1, 9], port)))
    }

    /// Two real modem lines on loopback, each seeing the other only through
    /// its advertised listening port, link up with one call.
    #[test]
    fn two_enabled_links_find_each_other_and_connect() {
        use gp64_hw::serial::SerialPorts;
        use gp64_machine::modem::{ModemClock, ModemConnection, TcpModem};

        let mut serial = [SerialPorts::new(), SerialPorts::new()];
        let [low, high] = &mut serial;
        let modems = [
            TcpModem::attach_line(2, low, ModemClock::RealTime).unwrap(),
            TcpModem::attach_line(2, high, ModemClock::RealTime).unwrap(),
        ];
        let lines = modems.each_ref().map(TcpModem::line);
        let ids = [3, 9];
        let mut links = [AutoLink::new(true), AutoLink::new(true)];
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut calls = 0;
        while !lines
            .iter()
            .all(|line| matches!(line.status(), LineStatus::Connected(_)))
        {
            assert!(
                Instant::now() < deadline,
                "{:?}",
                lines.each_ref().map(|l| l.status())
            );
            for me in 0..2 {
                let other = 1 - me;
                let advertised = match lines[other].status() {
                    LineStatus::Listening(port) => {
                        Some((ids[other], SocketAddr::from(([127, 0, 0, 1], port))))
                    }
                    _ => None,
                };
                let status = lines[me].status();
                match links[me].step(&status, ids[me], advertised.into_iter(), Instant::now()) {
                    Step::Listen => {
                        lines[me].start(&ModemConnection::Host(0)).unwrap();
                    }
                    Step::Call(address) => {
                        assert_eq!(me, 1, "only the higher id calls");
                        calls += 1;
                        lines[me].call(&address.to_string()).unwrap();
                    }
                    Step::None => {}
                    Step::HangUp => panic!("an enabled link never hangs up"),
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_disabled_link_hangs_up_anything_active_and_otherwise_rests() {
        let now = Instant::now();
        let mut link = AutoLink::new(false);
        for status in [
            LineStatus::Listening(5),
            LineStatus::Connecting("h:1".into()),
            LineStatus::Connected("h:1".into()),
        ] {
            assert_eq!(link.step(&status, 9, std::iter::empty(), now), Step::HangUp);
        }
        for status in [LineStatus::Off, LineStatus::Failed("refused".into())] {
            assert_eq!(link.step(&status, 9, std::iter::empty(), now), Step::None);
        }
    }

    #[test]
    fn an_enabled_link_listens_then_the_higher_id_calls() {
        let now = Instant::now();
        let mut link = AutoLink::new(true);
        assert_eq!(
            link.step(&LineStatus::Off, 5, std::iter::empty(), now),
            Step::Listen
        );
        let listening = LineStatus::Listening(4000);
        // Only lower ids are called; a higher id calls us instead.
        assert_eq!(
            link.step(&listening, 5, [peer(8, 1)].into_iter(), now),
            Step::None
        );
        assert_eq!(
            link.step(&listening, 5, [peer(8, 1), peer(3, 2)].into_iter(), now),
            Step::Call(peer(3, 2).1)
        );
        for status in [
            LineStatus::Connecting("h:1".into()),
            LineStatus::Connected("h:1".into()),
        ] {
            assert_eq!(
                link.step(&status, 5, [peer(3, 2)].into_iter(), now),
                Step::None
            );
        }
    }

    #[test]
    fn a_failure_or_a_dropped_call_waits_before_retrying() {
        let now = Instant::now();
        let mut link = AutoLink::new(true);
        let failed = LineStatus::Failed("refused".into());
        assert_eq!(link.step(&failed, 5, std::iter::empty(), now), Step::None);
        assert_eq!(
            link.step(&failed, 5, std::iter::empty(), now + RETRY / 2),
            Step::None
        );
        assert_eq!(
            link.step(&failed, 5, std::iter::empty(), now + RETRY),
            Step::Listen
        );

        let later = now + 10 * RETRY;
        link.step(
            &LineStatus::Connected("h:1".into()),
            5,
            std::iter::empty(),
            later,
        );
        let listening = LineStatus::Listening(4000);
        assert_eq!(
            link.step(&listening, 5, [peer(3, 2)].into_iter(), later),
            Step::None
        );
        assert_eq!(
            link.step(&listening, 5, [peer(3, 2)].into_iter(), later + RETRY),
            Step::Call(peer(3, 2).1)
        );
    }
}
