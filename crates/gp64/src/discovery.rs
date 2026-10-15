//! Non-blocking IPv4 LAN discovery for gp64 serial and modem links.
//!
//! Discovery opens its socket only while a link is enabled, and then
//! advertises the TCP ports its links listen on. The frontend calls
//! [`Discovery::tick`] from its redraw loop; this module owns no threads.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISCOVERY_PORT_BASE: u16 = 23_517;
const DISCOVERY_PORT_COUNT: u16 = 4;
const BROADCAST_INTERVAL: Duration = Duration::from_secs(2);
const PEER_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_RECEIVES_PER_TICK: usize = 64;
const MAX_PEERS: usize = 64;

const MAGIC: &[u8; 4] = b"GP64";
const PROTOCOL_VERSION: u16 = 2;
const SERVICE: &str = "gp64";
const SERVICE_FIELD_SIZE: usize = 32;
const NAME_FIELD_SIZE: usize = 32;
const PACKET_SIZE: usize = 4 + 2 + 4 + SERVICE_FIELD_SIZE + NAME_FIELD_SIZE + 8;
/// Where the service field starts, after the magic, version and ports.
const SERVICE_START: usize = 10;
/// How long a failed bind waits before the next attempt.
const BIND_RETRY: Duration = Duration::from_secs(2);

/// The TCP ports a gp64 process listens on for each link; zero for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ports {
    pub serial: u16,
    pub modem: u16,
}

/// A gp64 process found on the local network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Stable identifier for the remote gp64 process.
    pub id: u64,
    /// Generic hostname advertised by the remote process.
    pub name: String,
    /// Address the remote process advertised from.
    pub ip: IpAddr,
    /// Its links waiting for a caller.
    pub ports: Ports,
}

struct PeerEntry {
    peer: Peer,
    last_seen: Instant,
}

/// Frontend-owned LAN discovery service. All socket work is non-blocking.
pub struct Discovery {
    socket: Option<UdpSocket>,
    instance_id: u64,
    display_name: String,
    /// What peers must advertise to be seen; see [`service_name`].
    service: String,
    ports: Ports,
    last_advertisement: Option<Instant>,
    /// When a failed bind may be tried again.
    retry_bind: Option<Instant>,
    peers: Vec<PeerEntry>,
    error: Option<String>,
}

impl Discovery {
    /// Create discovery without opening a socket. Call [`Self::start`] once
    /// the frontend is ready to listen for peers.
    pub fn new() -> Self {
        Self {
            socket: None,
            instance_id: generate_instance_id(),
            display_name: local_display_name(),
            service: service_name(std::env::var("GP64_LINK_GROUP").ok().as_deref()),
            ports: Ports::default(),
            last_advertisement: None,
            retry_bind: None,
            peers: Vec::new(),
            error: None,
        }
    }

    /// This process's identifier; the higher of two peers places the call.
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Opens the socket on the first free UDP port in 23517–23520 while
    /// `active`, retrying a failed bind every two seconds; closes it and
    /// forgets peers otherwise. A failure is reported once on `stderr`.
    pub fn set_active(&mut self, active: bool) {
        if !active {
            self.socket = None;
            self.peers.clear();
            self.error = None;
            self.retry_bind = None;
            return;
        }
        let now = Instant::now();
        if self.socket.is_some() || self.retry_bind.is_some_and(|at| now < at) {
            return;
        }
        match bind_discovery_socket() {
            Ok(socket) => {
                self.socket = Some(socket);
                self.last_advertisement = None;
                self.retry_bind = None;
                self.error = None;
            }
            Err(error) => {
                let error = error.to_string();
                if self.error.as_ref() != Some(&error) {
                    eprintln!("gp64: LAN discovery unavailable: {error}");
                }
                self.error = Some(error);
                self.retry_bind = Some(now + BIND_RETRY);
            }
        }
    }

    /// The ports to advertise; a change is announced at the next tick.
    pub fn set_ports(&mut self, ports: Ports) {
        if self.ports != ports {
            self.ports = ports;
            self.last_advertisement = None;
        }
    }

    /// Process pending datagrams, expire stale peers, and announce this
    /// process when due. Call this from the frontend redraw loop.
    pub fn tick(&mut self) {
        let now = Instant::now();

        let due = self
            .last_advertisement
            .is_none_or(|sent| now.duration_since(sent) >= BROADCAST_INTERVAL);
        if due && self.socket.is_some() {
            self.last_advertisement = Some(now);
            self.send_advertisement();
        }

        self.receive_pending(now);
        self.expire_peers(now);
    }

    /// Return discovered peers in stable insertion order.
    pub fn peers(&self) -> Vec<Peer> {
        self.peers.iter().map(|entry| entry.peer.clone()).collect()
    }

    fn send_advertisement(&mut self) {
        let Some(socket) = self.socket.as_ref() else {
            return;
        };
        let packet = build_packet(
            &self.service,
            self.ports,
            &self.display_name,
            self.instance_id,
        );
        match send_to_discovery_targets(socket, &packet) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn receive_pending(&mut self, now: Instant) {
        let Some(socket) = self.socket.as_ref() else {
            return;
        };

        let mut packet = [0u8; PACKET_SIZE + 1];
        let mut incoming = Vec::with_capacity(MAX_RECEIVES_PER_TICK);
        for _ in 0..MAX_RECEIVES_PER_TICK {
            match socket.recv_from(&mut packet) {
                Ok((length, source)) => {
                    incoming.push((packet[..length].to_vec(), source));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.error = Some(error.to_string());
                    break;
                }
            }
        }
        for (packet, source) in incoming {
            self.process_datagram(&packet, source, now);
        }
    }

    fn process_datagram(&mut self, packet: &[u8], source: SocketAddr, now: Instant) {
        if !usable_source(source.ip()) {
            return;
        }
        let Some(parsed) = parse_packet(&self.service, packet) else {
            return;
        };
        if parsed.instance_id == self.instance_id {
            return;
        }
        self.upsert_peer(
            parsed.instance_id,
            parsed.name,
            source.ip(),
            parsed.ports,
            now,
        );
    }

    fn upsert_peer(&mut self, id: u64, name: String, ip: IpAddr, ports: Ports, now: Instant) {
        if let Some(entry) = self.peers.iter_mut().find(|entry| entry.peer.id == id) {
            // A local loopback copy and a LAN broadcast can arrive for the
            // same instance. Keep the routable address when both are seen.
            entry.peer.ip = preferred_ip(entry.peer.ip, ip);
            entry.peer.name = name;
            entry.peer.ports = ports;
            entry.last_seen = now;
            return;
        }

        if self.peers.len() == MAX_PEERS {
            if let Some((oldest, _)) = self
                .peers
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_seen)
            {
                self.peers.remove(oldest);
            }
        }

        self.peers.push(PeerEntry {
            peer: Peer {
                id,
                name,
                ip,
                ports,
            },
            last_seen: now,
        });
    }

    fn expire_peers(&mut self, now: Instant) {
        self.peers
            .retain(|entry| now.duration_since(entry.last_seen) < PEER_TIMEOUT);
    }
}

fn preferred_ip(previous: IpAddr, incoming: IpAddr) -> IpAddr {
    if !previous.is_loopback() && incoming.is_loopback() {
        previous
    } else {
        incoming
    }
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct ParsedPacket {
    ports: Ports,
    name: String,
    instance_id: u64,
}

/// The advertised service: `gp64`, or `gp64-GROUP` for a test group, so
/// copies in a group link only with each other and never with ordinary
/// copies on the network.
fn service_name(group: Option<&str>) -> String {
    let Some(group) = group.filter(|group| !group.is_empty()) else {
        return SERVICE.to_owned();
    };
    let mut service = format!("{SERVICE}-");
    for character in group.chars().filter(|c| c.is_ascii_graphic()) {
        if service.len() + 1 > SERVICE_FIELD_SIZE - 1 {
            break;
        }
        service.push(character);
    }
    service
}

fn build_packet(service: &str, ports: Ports, name: &str, instance_id: u64) -> [u8; PACKET_SIZE] {
    let mut packet = [0u8; PACKET_SIZE];
    packet[0..4].copy_from_slice(MAGIC);
    packet[4..6].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    packet[6..8].copy_from_slice(&ports.serial.to_le_bytes());
    packet[8..10].copy_from_slice(&ports.modem.to_le_bytes());
    let service_end = SERVICE_START + SERVICE_FIELD_SIZE;
    write_fixed_string(&mut packet[SERVICE_START..service_end], service);
    write_fixed_string(
        &mut packet[service_end..service_end + NAME_FIELD_SIZE],
        name,
    );
    packet[PACKET_SIZE - 8..].copy_from_slice(&instance_id.to_le_bytes());
    packet
}

fn parse_packet(service: &str, data: &[u8]) -> Option<ParsedPacket> {
    if data.len() != PACKET_SIZE || &data[0..4] != MAGIC {
        return None;
    }
    if u16::from_le_bytes([data[4], data[5]]) != PROTOCOL_VERSION {
        return None;
    }

    let ports = Ports {
        serial: u16::from_le_bytes([data[6], data[7]]),
        modem: u16::from_le_bytes([data[8], data[9]]),
    };
    let service_end = SERVICE_START + SERVICE_FIELD_SIZE;
    if read_fixed_string(&data[SERVICE_START..service_end])? != service {
        return None;
    }

    let name_end = service_end + NAME_FIELD_SIZE;
    let name = sanitize_name(read_fixed_string(&data[service_end..name_end])?);
    let id_start = PACKET_SIZE - 8;
    let instance_id = u64::from_le_bytes(data[id_start..].try_into().ok()?);
    if instance_id == 0 {
        return None;
    }

    Some(ParsedPacket {
        ports,
        name,
        instance_id,
    })
}

fn write_fixed_string(buffer: &mut [u8], value: &str) {
    let max_len = buffer.len().saturating_sub(1);
    let mut written = 0;
    for character in value.chars() {
        let mut encoded = [0u8; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        if written + bytes.len() > max_len {
            break;
        }
        buffer[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
    }
}

fn read_fixed_string(buffer: &[u8]) -> Option<&str> {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    if buffer[end..].iter().any(|byte| *byte != 0) {
        return None;
    }
    let value = std::str::from_utf8(&buffer[..end]).ok()?;
    if value.chars().any(char::is_control) {
        return None;
    }
    Some(value)
}

fn sanitize_name(value: &str) -> String {
    let mut result = String::new();
    for character in value.trim().chars() {
        if character.is_control() {
            continue;
        }
        let mut encoded = [0u8; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        if result.len() + bytes.len() > NAME_FIELD_SIZE - 1 {
            break;
        }
        result.push(character);
    }
    if result.is_empty() {
        SERVICE.to_owned()
    } else {
        result
    }
}

fn local_display_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|name| sanitize_name(&name))
        .unwrap_or_else(|_| SERVICE.to_owned())
}

fn usable_source(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast() && ip != Ipv4Addr::BROADCAST,
        IpAddr::V6(ip) => !ip.is_unspecified() && !ip.is_multicast(),
    }
}

fn send_to_discovery_targets(socket: &UdpSocket, packet: &[u8]) -> io::Result<()> {
    let mut first_error = None;
    let mut sent = false;
    for port_offset in 0..DISCOVERY_PORT_COUNT {
        let port = DISCOVERY_PORT_BASE + port_offset;
        for address in [Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST] {
            match socket.send_to(packet, SocketAddr::from((address, port))) {
                Ok(_) => sent = true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
    }
    if sent {
        Ok(())
    } else {
        Err(first_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AddrNotAvailable, "no discovery targets")
        }))
    }
}

fn bind_discovery_socket() -> io::Result<UdpSocket> {
    let mut last_error = None;
    for port_offset in 0..DISCOVERY_PORT_COUNT {
        let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT_BASE + port_offset));
        match UdpSocket::bind(address) {
            Ok(socket) => {
                socket.set_nonblocking(true)?;
                socket.set_broadcast(true)?;
                return Ok(socket);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrInUse,
            "all discovery ports are unavailable",
        )
    }))
}

fn generate_instance_id() -> u64 {
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

    let sequence = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let stack_address = (&sequence as *const u64 as usize) as u64;
    let mut value = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ sequence.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ stack_address;

    // SplitMix64 finalizer spreads nearby clock and sequence values without
    // adding a random-number dependency.
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    if value == 0 {
        1
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn ports(serial: u16, modem: u16) -> Ports {
        Ports { serial, modem }
    }

    #[test]
    fn packet_parser_filters_service_and_rejects_malformed_fields() {
        let packet = build_packet(
            SERVICE,
            ports(23_001, 23_002),
            "workstation",
            0x1234_5678_9ABC_DEF0,
        );
        let parsed = parse_packet(SERVICE, &packet).unwrap();
        assert_eq!(parsed.ports, ports(23_001, 23_002));
        assert_eq!(parsed.name, "workstation");
        assert_eq!(parsed.instance_id, 0x1234_5678_9ABC_DEF0);

        let mut wrong_service = packet;
        wrong_service[SERVICE_START] = b'x';
        assert!(parse_packet(SERVICE, &wrong_service).is_none());
        assert!(parse_packet(SERVICE, &packet[..PACKET_SIZE - 1]).is_none());

        let mut wrong_version = packet;
        wrong_version[4] = 1;
        assert!(parse_packet(SERVICE, &wrong_version).is_none());

        let mut control_character = packet;
        control_character[SERVICE_START + SERVICE_FIELD_SIZE] = b'\n';
        assert!(parse_packet(SERVICE, &control_character).is_none());

        let mut invalid_utf8 = packet;
        invalid_utf8[SERVICE_START + SERVICE_FIELD_SIZE] = 0xFF;
        assert!(parse_packet(SERVICE, &invalid_utf8).is_none());

        let mut no_id = packet;
        no_id[PACKET_SIZE - 8..].fill(0);
        assert!(parse_packet(SERVICE, &no_id).is_none());
    }

    #[test]
    fn a_test_group_and_ordinary_copies_ignore_each_other() {
        assert_eq!(service_name(None), "gp64");
        assert_eq!(service_name(Some("")), "gp64");
        let group = service_name(Some("bench 1"));
        assert_eq!(group, "gp64-bench1");
        assert!(service_name(Some(&"x".repeat(64))).len() < SERVICE_FIELD_SIZE);
        let grouped = build_packet(&group, ports(1, 2), "a", 7);
        let ordinary = build_packet(SERVICE, ports(1, 2), "b", 8);
        assert!(parse_packet(SERVICE, &grouped).is_none());
        assert!(parse_packet(&group, &ordinary).is_none());
        assert_eq!(parse_packet(&group, &grouped).unwrap().instance_id, 7);
    }

    #[test]
    fn expiry_uses_the_last_seen_time_without_sleeping() {
        let now = Instant::now();
        let mut discovery = Discovery::new();
        let lan = |last| IpAddr::V4(Ipv4Addr::new(192, 168, 1, last));
        discovery.upsert_peer(10, "host-a".to_owned(), lan(10), ports(1, 0), now);
        discovery.upsert_peer(
            20,
            "host-b".to_owned(),
            lan(20),
            ports(0, 2),
            now - Duration::from_secs(5),
        );

        discovery.expire_peers(now + Duration::from_secs(1));
        assert_eq!(
            discovery
                .peers()
                .iter()
                .map(|peer| peer.id)
                .collect::<Vec<_>>(),
            [10]
        );
    }

    #[test]
    fn self_packets_are_ignored_and_same_host_routes_are_deduplicated() {
        let now = Instant::now();
        let mut discovery = Discovery::new();
        let own_id = discovery.instance_id();
        let self_packet = build_packet(SERVICE, ports(23_101, 0), "self", own_id);
        discovery.process_datagram(
            &self_packet,
            SocketAddr::from(([127, 0, 0, 1], 23_517)),
            now,
        );
        assert!(discovery.peers().is_empty());

        let peer_id = own_id.wrapping_add(1).max(1);
        let packet = build_packet(SERVICE, ports(23_102, 0), "remote", peer_id);
        discovery.process_datagram(&packet, SocketAddr::from(([127, 0, 0, 1], 23_517)), now);
        discovery.process_datagram(
            &packet,
            SocketAddr::from(([192, 168, 1, 22], 23_518)),
            now + Duration::from_millis(1),
        );
        assert_eq!(discovery.peers().len(), 1);
        assert_eq!(discovery.peers()[0].id, peer_id);
        assert_eq!(
            discovery.peers()[0].ip,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 22))
        );

        // A later announcement replaces the ports; none open keeps the peer.
        let busy = build_packet(SERVICE, ports(0, 0), "remote", peer_id);
        discovery.process_datagram(
            &busy,
            SocketAddr::from(([192, 168, 1, 22], 23_518)),
            now + Duration::from_millis(2),
        );
        assert_eq!(discovery.peers()[0].ports, ports(0, 0));
    }

    #[test]
    fn an_inactive_discovery_holds_no_socket_and_forgets_peers() {
        let mut discovery = Discovery::new();
        assert!(discovery.socket.is_none());
        discovery.upsert_peer(
            7,
            "peer".into(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            ports(1, 1),
            Instant::now(),
        );
        discovery.socket = Some(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
        discovery.set_active(false);
        assert!(discovery.socket.is_none());
        assert!(discovery.peers().is_empty());
        discovery.tick();
        assert!(discovery.socket.is_none());
    }

    #[test]
    fn local_udp_unicast_roundtrip_uses_the_discovery_packet() {
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut discovery = Discovery::new();
        let own_id = discovery.instance_id();
        let peer_id = own_id.wrapping_add(1).max(1);
        let receiver_addr = receiver.local_addr().unwrap();
        discovery.socket = Some(receiver);

        let packet = build_packet(SERVICE, ports(23_111, 23_112), "local-host", peer_id);
        sender.send_to(&packet, receiver_addr).unwrap();
        let self_packet = build_packet(SERVICE, ports(23_113, 0), "self", own_id);
        sender.send_to(&self_packet, receiver_addr).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while discovery.peers().is_empty() {
            discovery.tick();
            assert!(
                Instant::now() < deadline,
                "local UDP peer was not discovered"
            );
            thread::sleep(Duration::from_millis(1));
        }

        let peers = discovery.peers();
        assert_eq!(peers.len(), 1, "self packets must not appear as peers");
        assert_eq!(peers[0].id, peer_id);
        assert_eq!(peers[0].name, "local-host");
        assert_eq!(peers[0].ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(peers[0].ports, ports(23_111, 23_112));
    }
}
