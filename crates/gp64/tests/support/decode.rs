use std::path::Path;
#[derive(Default)]
pub struct Summary {
    pub video: Vec<(i64, Vec<u8>)>,
    pub audio: Vec<(i64, Vec<u8>)>,
    pub duration_ms: f64,
    pub cues: bool,
    pub width: u64,
    pub height: u64,
    pub pre_skip: usize,
}
fn vint(data: &[u8], pos: usize, strip: bool) -> (u64, usize) {
    let n = data[pos].leading_zeros() as usize + 1;
    assert!(n <= 8);
    let mut v = u64::from(if strip {
        data[pos] & (0xffu8.checked_shr(n as u32).unwrap_or(0))
    } else {
        data[pos]
    });
    for byte in &data[pos + 1..pos + n] {
        v = v * 256 + u64::from(*byte);
    }
    (v, n)
}
fn uint(data: &[u8]) -> u64 {
    data.iter().fold(0, |v, b| v * 256 + u64::from(*b))
}
fn walk(data: &[u8], start: usize, end: usize, summary: &mut Summary, mut time: i64) {
    let mut pos = start;
    while pos < end {
        let (id, ni) = vint(data, pos, false);
        let (size, ns) = vint(data, pos + ni, true);
        let body = pos + ni + ns;
        let next = body.checked_add(size as usize).unwrap();
        assert!(next <= end, "unfinalised or truncated EBML element {id:x}");
        let bytes = &data[body..next];
        match id {
            0x18538067 | 0x1549a966 | 0x1654ae6b | 0xae | 0xe0 | 0xe1 | 0x1f43b675 | 0xa0 => {
                walk(data, body, next, summary, time)
            }
            0xe7 => time = uint(bytes) as i64,
            0x4489 => summary.duration_ms = f64::from_be_bytes(bytes.try_into().unwrap()),
            0x1c53bb6b => {
                summary.cues = true;
                assert!(!bytes.is_empty());
            }
            0xb0 => summary.width = uint(bytes),
            0xba => summary.height = uint(bytes),
            0x63a2 if bytes.starts_with(b"OpusHead") => {
                summary.pre_skip = usize::from(u16::from_le_bytes([bytes[10], bytes[11]]))
            }
            0xa3 | 0xa1 => {
                let (track, n) = vint(bytes, 0, true);
                let timestamp = time + i64::from(i16::from_be_bytes([bytes[n], bytes[n + 1]]));
                assert_eq!(bytes[n + 2] & 0x06, 0, "no lacing");
                let packet = (timestamp, bytes[n + 3..].to_vec());
                match track {
                    1 => summary.video.push(packet),
                    2 => summary.audio.push(packet),
                    _ => panic!("unexpected track"),
                }
            }
            _ => {}
        }
        pos = next;
    }
    assert_eq!(pos, end);
}
pub fn read(path: &Path) -> Summary {
    let data = std::fs::read(path).unwrap();
    let mut result = Summary::default();
    walk(&data, 0, data.len(), &mut result, 0);
    result
}
pub fn decode_video(summary: &Summary) -> Vec<u64> {
    let mut decoder = rvp9_decoder::Decoder::new();
    summary
        .video
        .iter()
        .map(|(_, bytes)| {
            let packet = decoder.parse_packet(bytes).unwrap();
            let frame = decoder.decode_frame(&packet.frames()[0]).unwrap();
            assert!(frame.y_plane().width() >= 320);
            assert_eq!(frame.y_plane().height(), 200);
            (0..200)
                .flat_map(|y| frame.y_plane().row(y)[..320].to_vec())
                .map(u64::from)
                .sum()
        })
        .collect()
}
pub fn decode_audio(summary: &Summary) -> Vec<i16> {
    let mut decoder = ropus::Decoder::new(48_000, ropus::Channels::Stereo).unwrap();
    let mut pcm = Vec::new();
    let mut buffer = vec![0; 5760 * 2];
    for (_, bytes) in &summary.audio {
        let count = decoder
            .decode(bytes, &mut buffer, ropus::DecodeMode::Normal)
            .unwrap();
        pcm.extend_from_slice(&buffer[..count * 2]);
    }
    pcm.drain(..summary.pre_skip * 2);
    pcm
}
