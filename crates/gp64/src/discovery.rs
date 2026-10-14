//! Non-blocking IPv4 LAN discovery for gp64 modem hosts.
//!
//! Discovery listens while the frontend is open and advertises only while a
//! modem line is listening. The frontend calls [`Discovery::tick`] from its
//! redraw loop; this module owns no threads.

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
const PROTOCOL_VERSION: u16 = 1;
const SERVICE: &str = "gp64";
const SERVICE_FIELD_SIZE: usize = 32;
const NAME_FIELD_SIZE: usize = 32;
const PACKET_SIZE: usize = 4 + 2 + 2 + SERVICE_FIELD_SIZE + NAME_FIELD_SIZE + 8;

/// A gp64 host found on the local network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Stable identifier for the remote gp64 process.
    pub id: u64,
    /// Generic hostname advertised by the remote process.
    pub name: String,
    /// Address and TCP port to use when connecting to the modem host.
    pub address: SocketAddr,
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
    host_port: Option<u16>,
    last_advertisement: Option<Instant>,
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
            host_port: None,
            last_advertisement: None,
            peers: Vec::new(),
            error: None,
        }
    }

    /// Bind the first available UDP discovery port in 23517–23520.
    ///
    /// The socket is non-blocking and supports IPv4 broadcast. Calling this
    /// more than once after a successful start is harmless.
    pub fn start(&mut self) -> io::Result<()> {
        if self.socket.is_some() {
            return Ok(());
        }

        match bind_discovery_socket() {
            Ok(socket) => {
                self.socket = Some(socket);
                self.last_advertisement = None;
                self.error = None;
                Ok(())
            }
            Err(error) => {
                self.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Advertise a listening modem port, or withdraw the current host.
    ///
    /// A zero port is never advertised as a joinable peer. If a host stops
    /// listening, a withdrawal packet is sent immediately when discovery has
    /// a socket; peers continue to be received while idle.
    pub fn set_host(&mut self, port: Option<u16>) {
        let port = port.filter(|port| *port != 0);
        let previous = self.host_port;
        if previous == port {
            return;
        }

        self.host_port = port;
        self.last_advertisement = None;

        if previous.is_some() && port.is_none() {
            self.send_advertisement(0);
        }
    }

    /// Process pending datagrams, expire stale peers, and announce a host
    /// when due. Call this from the frontend redraw loop.
    pub fn tick(&mut self) {
        let now = Instant::now();

        if let Some(port) = self.host_port {
            let due = self
                .last_advertisement
                .is_none_or(|sent| now.duration_since(sent) >= BROADCAST_INTERVAL);
            if due {
                self.last_advertisement = Some(now);
                self.send_advertisement(port);
            }
        }

        self.receive_pending(now);
        self.expire_peers(now);
    }

    /// Return discovered peers in stable insertion order.
    pub fn peers(&self) -> Vec<Peer> {
        self.peers.iter().map(|entry| entry.peer.clone()).collect()
    }

    /// Return the latest non-fatal socket error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn send_advertisement(&mut self, port: u16) {
        let Some(socket) = self.socket.as_ref() else {
            return;
        };
        let packet = build_packet(port, &self.display_name, self.instance_id);
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
        let Some(parsed) = parse_packet(packet) else {
            return;
        };
        if parsed.instance_id == self.instance_id {
            return;
        }

        if parsed.host_port == 0 {
            self.peers
                .retain(|entry| entry.peer.id != parsed.instance_id);
            return;
        }

        self.upsert_peer(
            parsed.instance_id,
            parsed.name,
            SocketAddr::new(source.ip(), parsed.host_port),
            now,
        );
    }

    fn upsert_peer(&mut self, id: u64, name: String, address: SocketAddr, now: Instant) {
        if let Some(entry) = self.peers.iter_mut().find(|entry| entry.peer.id == id) {
            // A local loopback copy and a LAN broadcast can arrive for the
            // same instance. Keep the routable address when both are seen.
            entry.peer.address = preferred_address(entry.peer.address, address);
            entry.peer.name = name;
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
            peer: Peer { id, name, address },
            last_seen: now,
        });
    }

    fn expire_peers(&mut self, now: Instant) {
        self.peers
            .retain(|entry| now.duration_since(entry.last_seen) < PEER_TIMEOUT);
    }
}

fn preferred_address(previous: SocketAddr, incoming: SocketAddr) -> SocketAddr {
    if !previous.ip().is_loopback() && incoming.ip().is_loopback() {
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
    host_port: u16,
    name: String,
    instance_id: u64,
}

fn build_packet(host_port: u16, name: &str, instance_id: u64) -> [u8; PACKET_SIZE] {
    let mut packet = [0u8; PACKET_SIZE];
    packet[0..4].copy_from_slice(MAGIC);
    packet[4..6].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    packet[6..8].copy_from_slice(&host_port.to_le_bytes());
    write_fixed_string(&mut packet[8..8 + SERVICE_FIELD_SIZE], SERVICE);
    write_fixed_string(
        &mut packet[8 + SERVICE_FIELD_SIZE..8 + SERVICE_FIELD_SIZE + NAME_FIELD_SIZE],
        name,
    );
    packet[PACKET_SIZE - 8..].copy_from_slice(&instance_id.to_le_bytes());
    packet
}

fn parse_packet(data: &[u8]) -> Option<ParsedPacket> {
    if data.len() != PACKET_SIZE || &data[0..4] != MAGIC {
        return None;
    }
    if u16::from_le_bytes([data[4], data[5]]) != PROTOCOL_VERSION {
        return None;
    }

    let host_port = u16::from_le_bytes([data[6], data[7]]);
    let service_end = 8 + SERVICE_FIELD_SIZE;
    if read_fixed_string(&data[8..service_end])? != SERVICE {
        return None;
    }

    let name_end = service_end + NAME_FIELD_SIZE;
    let name = sanitize_name(read_fixed_string(&data[service_end..name_end])?);
    let id_start = PACKET_SIZE - 8;
    let instance_id = u64::from_le_bytes(data[id_start..].try_into().ok()?);
    if instance_id == 0 || (host_port == 0 && name.is_empty()) {
        return None;
    }

    Some(ParsedPacket {
        host_port,
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

    #[test]
    fn packet_parser_filters_service_and_rejects_malformed_fields() {
        let packet = build_packet(23_001, "workstation", 0x1234_5678_9ABC_DEF0);
        let parsed = parse_packet(&packet).unwrap();
        assert_eq!(parsed.host_port, 23_001);
        assert_eq!(parsed.name, "workstation");
        assert_eq!(parsed.instance_id, 0x1234_5678_9ABC_DEF0);

        let mut wrong_service = packet;
        wrong_service[8] = b'x';
        assert!(parse_packet(&wrong_service).is_none());
        assert!(parse_packet(&packet[..PACKET_SIZE - 1]).is_none());

        let mut wrong_version = packet;
        wrong_version[4] = 2;
        assert!(parse_packet(&wrong_version).is_none());

        let mut control_character = packet;
        control_character[8 + SERVICE_FIELD_SIZE] = b'\n';
        assert!(parse_packet(&control_character).is_none());

        let mut invalid_utf8 = packet;
        invalid_utf8[8 + SERVICE_FIELD_SIZE] = 0xFF;
        assert!(parse_packet(&invalid_utf8).is_none());
    }

    #[test]
    fn expiry_uses_the_last_seen_time_without_sleeping() {
        let now = Instant::now();
        let mut discovery = Discovery::new();
        discovery.upsert_peer(
            10,
            "host-a".to_owned(),
            SocketAddr::from(([192, 168, 1, 10], 23_001)),
            now,
        );
        discovery.upsert_peer(
            20,
            "host-b".to_owned(),
            SocketAddr::from(([192, 168, 1, 20], 23_002)),
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
        let own_id = discovery.instance_id;
        let self_packet = build_packet(23_101, "self", own_id);
        discovery.process_datagram(
            &self_packet,
            SocketAddr::from(([127, 0, 0, 1], 23_517)),
            now,
        );
        assert!(discovery.peers().is_empty());

        let peer_id = own_id.wrapping_add(1).max(1);
        let packet = build_packet(23_102, "remote", peer_id);
        discovery.process_datagram(&packet, SocketAddr::from(([127, 0, 0, 1], 23_517)), now);
        discovery.process_datagram(
            &packet,
            SocketAddr::from(([192, 168, 1, 22], 23_518)),
            now + Duration::from_millis(1),
        );

        assert_eq!(discovery.peers().len(), 1);
        assert_eq!(discovery.peers()[0].id, peer_id);
        assert_eq!(
            discovery.peers()[0].address.ip(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 22))
        );

        let withdrawal = build_packet(0, "remote", peer_id);
        discovery.process_datagram(
            &withdrawal,
            SocketAddr::from(([192, 168, 1, 22], 23_518)),
            now + Duration::from_millis(2),
        );
        assert!(discovery.peers().is_empty());
    }

    #[test]
    fn local_udp_unicast_roundtrip_uses_the_discovery_packet() {
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut discovery = Discovery::new();
        let own_id = discovery.instance_id;
        let peer_id = own_id.wrapping_add(1).max(1);
        let receiver_addr = receiver.local_addr().unwrap();
        discovery.socket = Some(receiver);

        let packet = build_packet(23_111, "local-host", peer_id);
        sender.send_to(&packet, receiver_addr).unwrap();
        let self_packet = build_packet(23_112, "self", own_id);
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
        assert_eq!(
            peers[0].address,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 23_111))
        );

        let withdrawal = build_packet(0, "local-host", peer_id);
        sender.send_to(&withdrawal, receiver_addr).unwrap();
        let withdrawal_deadline = Instant::now() + Duration::from_secs(2);
        while !discovery.peers().is_empty() {
            discovery.tick();
            assert!(
                Instant::now() < withdrawal_deadline,
                "withdrawal should remove the peer"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}
