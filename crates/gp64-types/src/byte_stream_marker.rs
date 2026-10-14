//! Bounded one-shot matching over successful guest byte-stream writes.

const MAX_MARKER_BYTES: usize = 256;

/// Incremental one-shot substring matcher that preserves matches across writes.
#[derive(Debug, Clone)]
pub struct ByteStreamMarker {
    needle: Vec<u8>,
    failure: Vec<usize>,
    matched_prefix: usize,
    hit_pending: bool,
    complete: bool,
}

impl ByteStreamMarker {
    pub fn new(needle: &[u8]) -> Result<Self, &'static str> {
        if needle.is_empty() {
            return Err("write marker must not be empty");
        }
        if needle.len() > MAX_MARKER_BYTES {
            return Err("write marker exceeds 256 bytes");
        }

        let mut failure = vec![0; needle.len()];
        let mut prefix = 0;
        for i in 1..needle.len() {
            while prefix > 0 && needle[i] != needle[prefix] {
                prefix = failure[prefix - 1];
            }
            if needle[i] == needle[prefix] {
                prefix += 1;
                failure[i] = prefix;
            }
        }

        Ok(Self {
            needle: needle.to_vec(),
            failure,
            matched_prefix: 0,
            hit_pending: false,
            complete: false,
        })
    }

    pub fn observe(&mut self, bytes: &[u8]) {
        if self.complete {
            return;
        }
        for &byte in bytes {
            while self.matched_prefix > 0 && byte != self.needle[self.matched_prefix] {
                self.matched_prefix = self.failure[self.matched_prefix - 1];
            }
            if byte == self.needle[self.matched_prefix] {
                self.matched_prefix += 1;
                if self.matched_prefix == self.needle.len() {
                    self.complete = true;
                    self.hit_pending = true;
                    return;
                }
            }
        }
    }

    /// Consume a newly recognized marker. A completed matcher stays complete.
    pub fn take_hit(&mut self) -> bool {
        std::mem::take(&mut self.hit_pending)
    }
}

#[cfg(test)]
mod tests {
    use super::ByteStreamMarker;

    #[test]
    fn matches_across_writes_and_reports_once() {
        let mut marker = ByteStreamMarker::new(b"test-marker").unwrap();
        marker.observe(b"noise test-mar");
        assert!(!marker.take_hit());
        marker.observe(b"ker tail test-marker");
        assert!(marker.take_hit());
        assert!(!marker.take_hit());
    }

    #[test]
    fn handles_overlap_and_rejects_unbounded_needles() {
        let mut marker = ByteStreamMarker::new(b"ababac").unwrap();
        marker.observe(b"ababab");
        assert!(!marker.take_hit());
        marker.observe(b"ac");
        assert!(marker.take_hit());
        assert!(ByteStreamMarker::new(b"").is_err());
        assert!(ByteStreamMarker::new(&vec![b'x'; 257]).is_err());
    }
}
