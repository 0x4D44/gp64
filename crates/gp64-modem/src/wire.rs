//! Audio transport abstraction and TCP chunk reassembly for PCM audio.
//!
//! The modem wire format is raw 320-byte chunks (160 i16 samples at 16000 Hz,
//! 10 ms of audio per chunk). TCP is a stream protocol and doesn't preserve
//! message boundaries, so this module handles reassembly.
//!
//! The [`WireTransport`] trait abstracts the audio path between the modem DSP
//! and the remote end, allowing TCP (emulator-to-emulator) and future PSTN
//! audio (cpal sound-card) transports to be used interchangeably.

/// Number of PCM samples per chunk (10 ms at 16000 Hz).
pub const CHUNK_SAMPLES: usize = 160;

/// Size of one PCM chunk in bytes (160 samples * 2 bytes/sample).
pub const CHUNK_BYTES: usize = CHUNK_SAMPLES * 2;

/// Sample rate for the wire format.
pub const WIRE_SAMPLE_RATE: u32 = 16000;

/// Silence chunk (all zeros).
pub const SILENCE: [i16; CHUNK_SAMPLES] = [0i16; CHUNK_SAMPLES];

/// Maximum backlog, as a whole number of PCM chunks: 204 chunks = 65,280 bytes
/// (~2.04 s of audio), just under the historic 64 KB cap.
///
/// Whole chunks, not a round byte count, is the point. The wire format carries
/// no framing markers, so a receiver's alignment is purely positional: discard
/// any number of bytes that is not a multiple of [`CHUNK_BYTES`] and every
/// later chunk decodes across a boundary, permanently. 65,536 is 204.8 chunks,
/// so the old cap could only be hit mid-chunk (MDD-BUG-KIL-00837).
const MAX_BUFFER_CHUNKS: usize = 204;

/// Maximum backlog in bytes. Always a multiple of [`CHUNK_BYTES`].
const MAX_BUFFER_SIZE: usize = MAX_BUFFER_CHUNKS * CHUNK_BYTES;

/// Reassembly buffer for incoming TCP data.
pub struct WireBuffer {
    buf: Vec<u8>,
}

impl WireBuffer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append raw bytes received from TCP.
    ///
    /// A backlog past [`MAX_BUFFER_SIZE`] is trimmed by dropping the OLDEST
    /// whole chunks, never by truncating the incoming bytes. Both ends of the
    /// buffer then only ever move in [`CHUNK_BYTES`] units — `take_chunk`
    /// drains a whole chunk from the front, this drops whole chunks from the
    /// front — so the bytes that remain keep the stream's chunk alignment.
    ///
    /// Dropping the oldest audio under overload is a jitter-buffer overrun: it
    /// is audible as a gap and then recovers. Truncating at an arbitrary byte
    /// offset is not recoverable — the retained partial chunk concatenates with
    /// whatever arrives next and every later sample decodes one or more bytes
    /// out of phase, for the life of the connection, because the buffer never
    /// again drains anything but whole chunks (MDD-BUG-KIL-00837).
    ///
    /// The send side already refuses to put a partial frame on the wire, force-
    /// disconnecting instead (`write_pcm_chunk` in `src/emulator/modem_thread.rs`);
    /// this is the same contract on the receive side.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);

        let overflow = self.buf.len().saturating_sub(MAX_BUFFER_SIZE);
        if overflow == 0 {
            return;
        }

        // Round UP to a whole chunk: rounding down would leave the buffer over
        // the cap, and trimming the exact overflow is the misalignment itself.
        let dropped = (overflow.div_ceil(CHUNK_BYTES) * CHUNK_BYTES).min(self.buf.len());
        tracing::warn!(
            target: "gp64::modem",
            "WireBuffer backlog exceeded {} bytes; dropping the oldest {} chunks \
             ({} bytes) to stay chunk-aligned",
            MAX_BUFFER_SIZE,
            dropped / CHUNK_BYTES,
            dropped
        );
        self.buf.drain(..dropped);
    }

    /// Try to extract one complete chunk (160 i16 samples).
    /// Returns None if fewer than 320 bytes are buffered.
    pub fn take_chunk(&mut self) -> Option<[i16; CHUNK_SAMPLES]> {
        if self.buf.len() < CHUNK_BYTES {
            return None;
        }

        let mut samples = [0i16; CHUNK_SAMPLES];
        for (i, sample) in samples.iter_mut().enumerate() {
            let lo = self.buf[i * 2];
            let hi = self.buf[i * 2 + 1];
            *sample = i16::from_le_bytes([lo, hi]);
        }

        // Remove the consumed bytes from the front.
        self.buf.drain(..CHUNK_BYTES);

        Some(samples)
    }

    /// Number of complete chunks available.
    pub fn chunks_available(&self) -> usize {
        self.buf.len() / CHUNK_BYTES
    }

    /// Number of bytes buffered.
    pub fn buffered_bytes(&self) -> usize {
        self.buf.len()
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Takes every buffered byte, whole chunks or not: the rest of a
    /// stream that has switched from PCM to relay frames.
    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

// ---------------------------------------------------------------------------
// Data relay
// ---------------------------------------------------------------------------
//
// Two gp64 modems that both relay stop sending each other line audio once a
// V.32bis call is online, and send the DTE bytes themselves. A relaying
// engine opens each connection with RELAY_HELLO; a peer that never sends it
// keeps the call on audio. Each direction switches on its own: the sender
// ends its PCM with RELAY_MARKER, and everything after the marker is relay
// frames, one length byte then that many data bytes.
//
// Both signal chunks are whole PCM frames of +1/-1 samples, -90 dBFS, so a
// modem that does not relay hears 10 ms of silence. A modem never sends
// them: its silence is exact zeros and its carrier is far louder.

/// Builds a signal chunk whose samples spell `signature` as +1/-1 bits.
const fn signal_chunk(signature: &[u8; CHUNK_SAMPLES / 8]) -> [i16; CHUNK_SAMPLES] {
    let mut chunk = [0i16; CHUNK_SAMPLES];
    let mut i = 0;
    while i < CHUNK_SAMPLES {
        chunk[i] = if signature[i / 8] & (0x80 >> (i % 8)) != 0 {
            1
        } else {
            -1
        };
        i += 1;
    }
    chunk
}

/// Opens a relaying engine's side of a connection.
pub const RELAY_HELLO: [i16; CHUNK_SAMPLES] = signal_chunk(b"gp64 modem relay v1:");

/// Ends a direction's PCM; relay frames follow.
pub const RELAY_MARKER: [i16; CHUNK_SAMPLES] = signal_chunk(b"gp64 relay data next");

/// Most data bytes in one relay frame.
pub const RELAY_FRAME_MAX: usize = u8::MAX as usize;

/// Appends relay frames carrying `data` to `out`.
pub fn encode_relay_frames(data: &[u8], out: &mut impl Extend<u8>) {
    for frame in data.chunks(RELAY_FRAME_MAX) {
        out.extend([frame.len() as u8]);
        out.extend(frame.iter().copied());
    }
}

/// Reassembles relay frames from the byte stream after a marker.
#[derive(Default)]
pub struct RelayFrames {
    buf: Vec<u8>,
}

impl RelayFrames {
    /// Adds received stream bytes and returns the data of every frame now
    /// complete; a partial frame waits for the rest.
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(&len) = self.buf.get(at) {
            let end = at + 1 + usize::from(len);
            if end > self.buf.len() {
                break;
            }
            out.extend_from_slice(&self.buf[at + 1..end]);
            at = end;
        }
        self.buf.drain(..at);
        out
    }
}

impl Default for WireBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode a chunk of PCM samples into bytes for TCP transmission.
pub fn encode_chunk(samples: &[i16; CHUNK_SAMPLES]) -> [u8; CHUNK_BYTES] {
    let mut out = [0u8; CHUNK_BYTES];
    for (i, &sample) in samples.iter().enumerate() {
        let bytes = sample.to_le_bytes();
        out[i * 2] = bytes[0];
        out[i * 2 + 1] = bytes[1];
    }
    out
}

// ---------------------------------------------------------------------------
// WireTransport trait
// ---------------------------------------------------------------------------

/// Audio transport between modem DSP and the wire (TCP or PSTN audio).
pub trait WireTransport: Send {
    /// Send a 10 ms chunk of PCM to the remote end.
    fn send_chunk(&mut self, samples: &[i16; CHUNK_SAMPLES]) -> std::io::Result<()>;

    /// Receive a 10 ms chunk of PCM from the remote end.
    /// Returns `None` if not yet available (non-blocking).
    fn recv_chunk(&mut self) -> std::io::Result<Option<[i16; CHUNK_SAMPLES]>>;

    /// Whether this transport has echo (PSTN = yes, TCP = no).
    fn has_echo(&self) -> bool;
}

// ---------------------------------------------------------------------------
// TcpTransport
// ---------------------------------------------------------------------------

use std::io::{Read, Write};
use std::net::TcpStream;

/// Write as much of `buf` as the (non-blocking) writer will take right now,
/// returning how many bytes went out.
///
/// MDD-BUG-CRUCIBLE-00202. Three rules the previous inline loops got wrong:
///
/// - `Ok(0)` with a non-empty buffer is **no progress**, so it is
///   [`ErrorKind::WriteZero`], not a successful iteration. The old loops did
///   `written += 0` / `drain(..0)` and span forever on a dead-but-writable
///   socket.
/// - `Interrupted` is retried, per the `io::Write` contract.
/// - `WouldBlock` is not an error — it is a partial write, reported as the
///   byte count so the caller can buffer the remainder.
///
/// Taking `W: Write` rather than the socket is what lets a zero-progress
/// writer prove prompt termination in a test; a real `TcpStream` cannot be
/// made to misbehave on demand.
fn write_nonblocking<W: Write>(writer: &mut W, buf: &[u8]) -> std::io::Result<usize> {
    let mut written = 0usize;
    while written < buf.len() {
        match writer.write(&buf[written..]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(n) => written += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(written),
            Err(e) => return Err(e),
        }
    }
    Ok(written)
}

fn flush_pending_with<W: Write>(writer: &mut W, pending: &mut Vec<u8>) -> std::io::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let written = write_nonblocking(writer, pending)?;
    pending.drain(..written);
    Ok(())
}

fn send_encoded_chunk<W: Write>(
    writer: &mut W,
    pending: &mut Vec<u8>,
    bytes: &[u8],
) -> std::io::Result<()> {
    // Flush only what was already queued. If the writer would-block again,
    // the new chunk must be appended behind that older remainder.
    flush_pending_with(writer, pending)?;

    if pending.len().saturating_add(bytes.len()) > MAX_BUFFER_SIZE {
        return Err(std::io::Error::other("send buffer backpressure exceeded"));
    }
    pending.extend_from_slice(bytes);
    flush_pending_with(writer, pending)
}

/// TCP-based transport for emulator-to-emulator modem connections.
pub struct TcpTransport {
    stream: TcpStream,
    rx_buffer: WireBuffer,
    /// Bytes that could not be sent due to WouldBlock on the non-blocking socket.
    tx_pending: Vec<u8>,
    /// Whether the peer has half-closed its side (a read returned zero).
    /// Durable: a socket at EOF returns zero on every later read, so the state
    /// is latched rather than re-derived (MDD-BUG-CRUCIBLE-00202).
    peer_closed: bool,
}

impl TcpTransport {
    pub fn new(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            rx_buffer: WireBuffer::new(),
            tx_pending: Vec::new(),
            peer_closed: false,
        })
    }

    /// Attempt to flush pending bytes that were buffered due to WouldBlock.
    fn flush_pending(&mut self) -> std::io::Result<()> {
        flush_pending_with(&mut self.stream, &mut self.tx_pending)
    }
}

impl WireTransport for TcpTransport {
    fn send_chunk(&mut self, samples: &[i16; CHUNK_SAMPLES]) -> std::io::Result<()> {
        let bytes = encode_chunk(samples);
        send_encoded_chunk(&mut self.stream, &mut self.tx_pending, &bytes)
    }

    fn recv_chunk(&mut self) -> std::io::Result<Option<[i16; CHUNK_SAMPLES]>> {
        // Opportunistically drain pending TX bytes while we're here.
        self.flush_pending()?;

        // Try to read available data into the reassembly buffer. Once the peer
        // has closed, stop reading — a closed socket returns Ok(0) forever.
        if !self.peer_closed {
            let mut buf = [0u8; 1024];
            loop {
                match self.stream.read(&mut buf) {
                    // MDD-BUG-CRUCIBLE-00202: a read of zero is EOF, NOT an
                    // idle socket. Folding it into the WouldBlock arm made a
                    // closed peer indistinguishable from one that simply has
                    // nothing to say, so a caller polling for data waited on a
                    // connection that could never deliver again.
                    Ok(0) => {
                        self.peer_closed = true;
                    }
                    Ok(n) => self.rx_buffer.push(&buf[..n]),
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
                break;
            }
        }

        // EOF must not discard data already received: hand back every complete
        // chunk first, and only report the close once the buffer is dry. A
        // trailing partial chunk is unrecoverable by definition — the bytes
        // that would complete it are never coming.
        if let Some(chunk) = self.rx_buffer.take_chunk() {
            return Ok(Some(chunk));
        }
        if self.peer_closed {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        Ok(None)
    }

    fn has_echo(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let mut samples = [0i16; CHUNK_SAMPLES];
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = (i as i16) * 100 - 4000;
        }

        let encoded = encode_chunk(&samples);

        let mut wb = WireBuffer::new();
        wb.push(&encoded);

        let decoded = wb.take_chunk().expect("should have one chunk");
        assert_eq!(decoded, samples);
    }

    #[test]
    fn test_partial_push() {
        let mut wb = WireBuffer::new();

        // Push 200 bytes — not enough for a chunk (need 320).
        wb.push(&[0u8; 200]);
        assert_eq!(wb.chunks_available(), 0);
        assert!(wb.take_chunk().is_none());

        // Push 120 more — now we have exactly 320.
        wb.push(&[0u8; 120]);
        assert_eq!(wb.chunks_available(), 1);
        assert!(wb.take_chunk().is_some());
    }

    #[test]
    fn test_multiple_chunks() {
        let mut wb = WireBuffer::new();
        wb.push(&[0u8; 640]);

        assert_eq!(wb.chunks_available(), 2);

        let c1 = wb.take_chunk();
        assert!(c1.is_some());
        assert_eq!(wb.chunks_available(), 1);

        let c2 = wb.take_chunk();
        assert!(c2.is_some());
        assert_eq!(wb.chunks_available(), 0);

        assert!(wb.take_chunk().is_none());
    }

    #[test]
    fn test_empty_buffer() {
        let mut wb = WireBuffer::new();
        assert_eq!(wb.chunks_available(), 0);
        assert_eq!(wb.buffered_bytes(), 0);
        assert!(wb.take_chunk().is_none());
    }

    #[test]
    fn test_silence_is_zeros() {
        for &s in SILENCE.iter() {
            assert_eq!(s, 0);
        }
    }

    #[test]
    fn test_endianness() {
        // i16 value 0x0102 = 258 decimal
        // Little-endian: low byte 0x02, high byte 0x01
        let mut samples = [0i16; CHUNK_SAMPLES];
        samples[0] = 0x0102;

        let encoded = encode_chunk(&samples);
        assert_eq!(encoded[0], 0x02); // low byte
        assert_eq!(encoded[1], 0x01); // high byte

        // Negative value: -1 = 0xFFFF in two's complement
        samples[0] = -1;
        let encoded = encode_chunk(&samples);
        assert_eq!(encoded[0], 0xFF);
        assert_eq!(encoded[1], 0xFF);
    }

    #[test]
    fn test_clear() {
        let mut wb = WireBuffer::new();
        wb.push(&[0u8; 640]);
        assert_eq!(wb.chunks_available(), 2);
        wb.clear();
        assert_eq!(wb.chunks_available(), 0);
        assert_eq!(wb.buffered_bytes(), 0);
    }

    #[test]
    fn test_default_trait() {
        let wb = WireBuffer::default();
        assert_eq!(wb.buffered_bytes(), 0);
        assert_eq!(wb.chunks_available(), 0);
    }

    #[test]
    fn test_partial_chunk_preserved_after_take() {
        let mut wb = WireBuffer::new();
        // Push 1.5 chunks (480 bytes)
        wb.push(&[0xAB; 480]);
        assert_eq!(wb.chunks_available(), 1);

        let _ = wb.take_chunk().unwrap();
        // 160 bytes should remain
        assert_eq!(wb.buffered_bytes(), 160);
        assert_eq!(wb.chunks_available(), 0);

        // Push the remaining 160 bytes to complete the second chunk
        wb.push(&[0xCD; 160]);
        assert_eq!(wb.chunks_available(), 1);
        assert!(wb.take_chunk().is_some());
    }

    #[test]
    fn test_encode_min_max_values() {
        let mut samples = [0i16; CHUNK_SAMPLES];
        samples[0] = i16::MIN; // -32768
        samples[1] = i16::MAX; // 32767

        let encoded = encode_chunk(&samples);
        let mut wb = WireBuffer::new();
        wb.push(&encoded);
        let decoded = wb.take_chunk().unwrap();
        assert_eq!(decoded[0], i16::MIN);
        assert_eq!(decoded[1], i16::MAX);
    }

    #[test]
    fn test_many_small_pushes() {
        let mut wb = WireBuffer::new();
        // Push one byte at a time until we have a chunk
        for i in 0..CHUNK_BYTES {
            wb.push(&[i as u8]);
            if i < CHUNK_BYTES - 1 {
                assert_eq!(wb.chunks_available(), 0);
            }
        }
        assert_eq!(wb.chunks_available(), 1);
        assert!(wb.take_chunk().is_some());
    }

    #[test]
    fn test_constants() {
        assert_eq!(CHUNK_SAMPLES, 160);
        assert_eq!(CHUNK_BYTES, 320);
        assert_eq!(WIRE_SAMPLE_RATE, 16000);
    }

    // -- WireTransport / TcpTransport tests --

    #[test]
    fn test_tcp_transport_roundtrip() {
        // Create a loopback TCP pair via a listener.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let mut tx = TcpTransport::new(client).unwrap();
        let mut rx = TcpTransport::new(server).unwrap();

        // Build a test chunk with a recognizable pattern.
        let mut samples = [0i16; CHUNK_SAMPLES];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = (i as i16) * 200 - 16000;
        }

        // Send from tx, receive on rx.
        tx.send_chunk(&samples).unwrap();

        // Non-blocking recv — may need a few attempts for TCP delivery.
        let mut received = None;
        for _ in 0..100 {
            match rx.recv_chunk() {
                Ok(Some(chunk)) => {
                    received = Some(chunk);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(e) => panic!("recv error: {e}"),
            }
        }

        let received = received.expect("should have received a chunk");
        assert_eq!(received, samples);
    }

    #[test]
    fn test_tcp_transport_recv_returns_none_when_empty() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (_server, _) = listener.accept().unwrap();

        let mut rx = TcpTransport::new(client).unwrap();

        // Nothing sent, recv should return None (not error).
        let result = rx.recv_chunk().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_tcp_transport_has_no_echo() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(addr).unwrap();
        let (_server, _) = listener.accept().unwrap();

        let transport = TcpTransport::new(stream).unwrap();
        assert!(!transport.has_echo());
    }

    #[test]
    fn test_tcp_transport_multiple_chunks() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let mut tx = TcpTransport::new(client).unwrap();
        let mut rx = TcpTransport::new(server).unwrap();

        // Send two distinct chunks.
        let mut chunk1 = [0i16; CHUNK_SAMPLES];
        let mut chunk2 = [0i16; CHUNK_SAMPLES];
        chunk1[0] = 1000;
        chunk2[0] = 2000;

        tx.send_chunk(&chunk1).unwrap();
        tx.send_chunk(&chunk2).unwrap();

        // Receive both.
        let mut received = Vec::new();
        for _ in 0..200 {
            match rx.recv_chunk() {
                Ok(Some(chunk)) => received.push(chunk),
                Ok(None) => {
                    if received.len() >= 2 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("recv error: {e}"),
            }
        }

        assert_eq!(received.len(), 2);
        assert_eq!(received[0], chunk1);
        assert_eq!(received[1], chunk2);
    }

    // -- WireBuffer overflow/capacity tests --

    #[test]
    fn test_push_caps_at_max_buffer_size() {
        let mut wb = WireBuffer::new();

        // Fill the buffer to exactly MAX_BUFFER_SIZE.
        let chunk = [0xAAu8; CHUNK_BYTES];
        for _ in 0..MAX_BUFFER_CHUNKS {
            wb.push(&chunk);
        }
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE);

        // A further push stays bounded — by dropping the oldest chunk, so the
        // backlog is still a whole number of chunks (MDD-BUG-KIL-00837).
        wb.push(&[0xBB; CHUNK_BYTES]);
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE);
        assert_eq!(wb.buffered_bytes() % CHUNK_BYTES, 0);

        // A push that overshoots the cap is accepted WHOLE; the trim comes off
        // the front, rounded up to a chunk. 65,270 + 100 overshoots by 90, so
        // one 320-byte chunk goes.
        let mut wb2 = WireBuffer::new();
        wb2.push(&vec![0u8; MAX_BUFFER_SIZE - 10]);
        assert_eq!(wb2.buffered_bytes(), MAX_BUFFER_SIZE - 10);
        wb2.push(&[0xCC; 100]);
        assert_eq!(
            wb2.buffered_bytes(),
            MAX_BUFFER_SIZE - 10 + 100 - CHUNK_BYTES
        );
        assert!(wb2.buffered_bytes() <= MAX_BUFFER_SIZE);
    }

    #[test]
    fn test_push_after_drain_allows_more_data() {
        let mut wb = WireBuffer::new();

        // Fill to capacity.
        wb.push(&vec![0u8; MAX_BUFFER_SIZE]);
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE);

        // Drain some data by taking chunks.
        let _ = wb.take_chunk();
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE - CHUNK_BYTES);

        // Now we can push again, up to CHUNK_BYTES more.
        wb.push(&[0xFF; CHUNK_BYTES]);
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE);

        // Beyond that the cap holds by dropping a whole chunk off the front,
        // never by truncating the newest byte (MDD-BUG-KIL-00837).
        wb.push(&[0xEE; 1]);
        assert_eq!(wb.buffered_bytes(), MAX_BUFFER_SIZE + 1 - CHUNK_BYTES);
    }

    /// A chunk whose every sample identifies it, so a misaligned decode cannot
    /// pass: sample 0 carries a marker bit the payload never sets, and the rest
    /// carry the id. A one-sample rotation loses the marker; a one-BYTE
    /// rotation shreds every sample.
    fn marked_chunk(id: usize) -> [i16; CHUNK_SAMPLES] {
        let mut samples = [id as i16; CHUNK_SAMPLES];
        samples[0] = 0x4000 | id as i16;
        samples
    }

    /// Recover a `marked_chunk`'s id, asserting the chunk arrived intact.
    fn intact_chunk_id(samples: &[i16; CHUNK_SAMPLES], context: &str) -> usize {
        let marker = samples[0];
        assert_eq!(
            marker & 0x4000,
            0x4000,
            "{context}: chunk lost its marker sample — decoded off a chunk boundary"
        );
        let id = (marker & 0x3FFF) as usize;
        assert!(
            samples[1..].iter().all(|&s| s == id as i16),
            "{context}: chunk {id} carries samples from another chunk — the \
             buffer decoded across a frame boundary"
        );
        id
    }

    /// Overflow must never split a chunk, whatever the TCP read sizes are.
    ///
    /// The wire format has no framing markers, so alignment is purely
    /// positional and one discarded odd byte desynchronises the stream for
    /// good: the buffer only ever drains whole chunks, so `buf.len() % 320`
    /// never recovers. The old cap (65,536 = 204.8 chunks) could only be
    /// reached mid-chunk, and truncated the incoming bytes to fit
    /// (MDD-BUG-KIL-00837).
    ///
    /// Feed sizes land the overflow at hostile offsets: 321 straddles a chunk
    /// by one byte, 7 and 1 force odd-byte boundaries, and 4096 is what the
    /// emulator's TCP reader actually uses (`src/emulator/modem_thread.rs`,
    /// `src/emulator/serial_net.rs`). A truncating `push` fails this test on
    /// the retained-chunk count; it is the slow-consumer test below that
    /// catches the splice, at feed=4096.
    #[test]
    fn overflow_drops_whole_chunks_and_never_splits_one() {
        const TOTAL_CHUNKS: usize = MAX_BUFFER_CHUNKS * 2 + 3;

        let mut stream = Vec::with_capacity(TOTAL_CHUNKS * CHUNK_BYTES);
        for id in 0..TOTAL_CHUNKS {
            stream.extend_from_slice(&encode_chunk(&marked_chunk(id)));
        }

        for feed in [321usize, 7, 4096, 1] {
            let context = format!("feed={feed}");
            let mut wb = WireBuffer::new();
            for slice in stream.chunks(feed) {
                wb.push(slice);
                assert!(
                    wb.buffered_bytes() <= MAX_BUFFER_SIZE,
                    "{context}: backlog {} exceeded the cap",
                    wb.buffered_bytes()
                );
            }

            let mut ids = Vec::new();
            while let Some(samples) = wb.take_chunk() {
                ids.push(intact_chunk_id(&samples, &context));
            }
            assert!(
                wb.buffered_bytes() < CHUNK_BYTES,
                "{context}: {} bytes left over — the stream was a whole number \
                 of chunks, so anything past a partial tail is misalignment",
                wb.buffered_bytes()
            );

            assert!(!ids.is_empty(), "{context}: nothing survived the overflow");
            assert!(
                ids.len() <= MAX_BUFFER_CHUNKS,
                "{context}: {} chunks retained, cap is {MAX_BUFFER_CHUNKS}",
                ids.len()
            );
            for pair in ids.windows(2) {
                assert_eq!(
                    pair[1],
                    pair[0] + 1,
                    "{context}: retained chunks must be a contiguous run — a gap \
                     inside the backlog means a partial chunk was dropped"
                );
            }
            assert_eq!(
                *ids.last().expect("non-empty"),
                TOTAL_CHUNKS - 1,
                "{context}: the NEWEST audio must survive; overflow drops the oldest"
            );
        }
    }

    /// The corruption the record describes, reproduced: a consumer slower than
    /// the wire.
    ///
    /// Overflow alone cannot desynchronise a buffer nobody drains — it simply
    /// stays full. The damage needs a SPLICE: a push truncated at the cap, then
    /// a later push accepted after `take_chunk` freed room, so the retained
    /// prefix is concatenated with bytes that are not its successors. Every
    /// sample after that seam decodes across a frame boundary, and the buffer
    /// never recovers because it only ever drains whole chunks
    /// (MDD-BUG-KIL-00837).
    ///
    /// Dropping the oldest whole chunks instead keeps every retained chunk a
    /// real chunk: the audio has a gap, which is what a jitter-buffer overrun
    /// sounds like, and the framing survives it.
    #[test]
    fn a_slow_consumer_never_receives_a_spliced_chunk() {
        const TOTAL_CHUNKS: usize = MAX_BUFFER_CHUNKS * 3;

        let mut stream = Vec::with_capacity(TOTAL_CHUNKS * CHUNK_BYTES);
        for id in 0..TOTAL_CHUNKS {
            stream.extend_from_slice(&encode_chunk(&marked_chunk(id)));
        }

        for feed in [321usize, 7, 4096, 1] {
            let context = format!("feed={feed}");
            let mut wb = WireBuffer::new();
            let mut ids = Vec::new();

            // Drain one chunk per push: slower than the wire once a push
            // carries more than a chunk, which is what saturates the backlog.
            for slice in stream.chunks(feed) {
                wb.push(slice);
                if let Some(samples) = wb.take_chunk() {
                    ids.push(intact_chunk_id(&samples, &context));
                }
            }
            while let Some(samples) = wb.take_chunk() {
                ids.push(intact_chunk_id(&samples, &context));
            }

            assert!(!ids.is_empty(), "{context}: the consumer received nothing");
            for pair in ids.windows(2) {
                assert!(
                    pair[1] > pair[0],
                    "{context}: chunk ids must advance — {} then {} means the \
                     consumer was handed the same audio twice",
                    pair[0],
                    pair[1]
                );
            }
            assert_eq!(
                *ids.last().expect("non-empty"),
                TOTAL_CHUNKS - 1,
                "{context}: the stream's last chunk must still arrive intact"
            );
        }
    }

    // ---- EOF and zero-progress writes (MDD-BUG-CRUCIBLE-00202) ----

    /// A `Write` that reports success having written nothing. A real socket
    /// must not do this, but the adapter is a public contract and a loop that
    /// trusts `Ok(0)` as progress never terminates.
    struct ZeroProgressWriter {
        calls: usize,
    }

    impl Write for ZeroProgressWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            Ok(0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A `Write` that is interrupted `pending` times before accepting the data.
    struct InterruptingWriter {
        pending: usize,
        written: Vec<u8>,
    }

    impl Write for InterruptingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.pending > 0 {
                self.pending -= 1;
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A `Write` accepting `accept` bytes total, then reporting `WouldBlock`.
    struct PartialWriter {
        accept: usize,
        written: Vec<u8>,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.accept == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
            }
            let n = buf.len().min(self.accept);
            self.written.extend_from_slice(&buf[..n]);
            self.accept -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A deterministic writer that partially accepts both flush attempts on
    /// the second send. A buggy direct write of the newer chunk can therefore
    /// overtake the older pending tail.
    struct OrderedPartialWriter {
        step: usize,
        written: Vec<u8>,
    }

    impl Write for OrderedPartialWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self.step {
                0 => {
                    self.step = 1;
                    let n = buf.len().min(5);
                    self.written.extend_from_slice(&buf[..n]);
                    Ok(n)
                }
                1 => {
                    self.step = 2;
                    Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                }
                2 => {
                    self.step = 3;
                    let n = buf.len().min(3);
                    self.written.extend_from_slice(&buf[..n]);
                    Ok(n)
                }
                3 => {
                    self.step = 4;
                    Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                }
                _ => {
                    self.written.extend_from_slice(buf);
                    Ok(buf.len())
                }
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The whole point of the injectable seam: a writer that never progresses
    /// must terminate promptly with `WriteZero`, not spin. If this regresses,
    /// the test hangs rather than fails — which is exactly the production
    /// symptom.
    #[test]
    fn zero_progress_write_terminates_with_write_zero() {
        let mut w = ZeroProgressWriter { calls: 0 };
        let err = write_nonblocking(&mut w, &[0xAAu8; 64])
            .expect_err("a writer that never progresses must not loop forever");
        assert_eq!(err.kind(), std::io::ErrorKind::WriteZero);
        assert_eq!(w.calls, 1, "one Ok(0) is enough to prove no progress");
    }

    /// An empty buffer is not a zero-progress write — there is nothing to do.
    #[test]
    fn zero_progress_write_on_empty_input_is_not_an_error() {
        let mut w = ZeroProgressWriter { calls: 0 };
        assert_eq!(write_nonblocking(&mut w, &[]).unwrap(), 0);
        assert_eq!(w.calls, 0, "an empty write must not touch the writer");
    }

    #[test]
    fn interrupted_writes_are_retried() {
        let mut w = InterruptingWriter {
            pending: 3,
            written: Vec::new(),
        };
        let n = write_nonblocking(&mut w, &[1u8, 2, 3, 4]).unwrap();
        assert_eq!(n, 4);
        assert_eq!(w.written, vec![1, 2, 3, 4]);
    }

    #[test]
    fn would_block_reports_the_bytes_actually_written() {
        let mut w = PartialWriter {
            accept: 3,
            written: Vec::new(),
        };
        let n = write_nonblocking(&mut w, &[1u8, 2, 3, 4, 5]).unwrap();
        assert_eq!(n, 3, "WouldBlock is not an error — it is a partial write");
        assert_eq!(w.written, vec![1, 2, 3]);
    }

    #[test]
    fn pending_tail_stays_before_new_encoded_chunk() {
        let first = vec![0xA1; 12];
        let second = vec![0xB2; 9];
        let mut pending = Vec::new();
        let mut writer = OrderedPartialWriter {
            step: 0,
            written: Vec::new(),
        };

        send_encoded_chunk(&mut writer, &mut pending, &first).unwrap();
        assert_eq!(pending, first[5..]);

        send_encoded_chunk(&mut writer, &mut pending, &second).unwrap();

        let mut expected = first;
        expected.extend_from_slice(&second);
        assert!(pending.is_empty(), "all bytes should have flushed");
        assert_eq!(writer.written, expected);
    }

    #[test]
    fn pending_queue_rejects_a_chunk_that_would_exceed_the_cap() {
        let mut pending = vec![0u8; MAX_BUFFER_SIZE - CHUNK_BYTES + 1];
        let mut writer = PartialWriter {
            accept: 0,
            written: Vec::new(),
        };

        let error = send_encoded_chunk(&mut writer, &mut pending, &[0u8; CHUNK_BYTES])
            .expect_err("the post-append queue length must stay bounded");

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(pending.len(), MAX_BUFFER_SIZE - CHUNK_BYTES + 1);
    }

    /// A peer that has closed is not an idle peer. Reporting `Ok(None)`
    /// forever leaves a caller waiting on a connection that can never deliver.
    #[test]
    fn recv_reports_eof_after_peer_close() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let mut rx = TcpTransport::new(server).unwrap();
        drop(client); // Peer closes with nothing in flight.

        let mut saw_eof = false;
        for _ in 0..200 {
            match rx.recv_chunk() {
                Ok(Some(_)) => panic!("no data was sent, so no chunk can arrive"),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(e) => {
                    assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
                    saw_eof = true;
                    break;
                }
            }
        }
        assert!(saw_eof, "a closed peer must surface as EOF, not idleness");
    }

    /// EOF must not discard data already in flight: every complete buffered
    /// chunk is delivered first, and only then does the stream report EOF.
    #[test]
    fn recv_drains_buffered_chunks_before_reporting_eof() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let mut tx = TcpTransport::new(client).unwrap();
        let mut rx = TcpTransport::new(server).unwrap();

        let samples = [0x1234i16; CHUNK_SAMPLES];
        tx.send_chunk(&samples).unwrap();
        tx.send_chunk(&samples).unwrap();
        drop(tx); // Close immediately behind the data.

        let mut chunks = 0;
        let mut saw_eof = false;
        for _ in 0..400 {
            match rx.recv_chunk() {
                Ok(Some(chunk)) => {
                    assert_eq!(chunk, samples);
                    chunks += 1;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(e) => {
                    assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
                    saw_eof = true;
                    break;
                }
            }
        }
        assert_eq!(chunks, 2, "both in-flight chunks must be delivered");
        assert!(saw_eof, "EOF must follow once the buffer is drained");
    }

    /// EOF is durable: once seen, every later call keeps reporting it rather
    /// than lapsing back into "idle".
    #[test]
    fn eof_is_sticky_across_calls() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let mut rx = TcpTransport::new(server).unwrap();
        drop(client);

        // Reach EOF.
        let mut reached = false;
        for _ in 0..200 {
            match rx.recv_chunk() {
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Ok(Some(_)) => panic!("no data was sent"),
                Err(_) => {
                    reached = true;
                    break;
                }
            }
        }
        assert!(reached, "precondition: EOF observed");

        for _ in 0..3 {
            let err = rx.recv_chunk().expect_err("EOF must stay EOF");
            assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        }
    }
}
