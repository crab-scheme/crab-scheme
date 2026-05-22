//! Length-prefixed channel framing for stream transports (SDK M02.C).
//!
//! The [`sim`](crate::sim) transport moves whole frames in memory, but a
//! byte-stream transport (TCP) carries all six logical [`Channel`]s over one
//! connection and must frame them. Each frame is:
//!
//! ```text
//! ┌────────────┬───────────────┬─────────────────┐
//! │ channel:u8 │ len:u32 (BE)  │ payload (len B) │
//! └────────────┴───────────────┴─────────────────┘
//! ```
//!
//! [`encode_frame`] writes one frame; [`FrameDecoder`] reassembles frames
//! from arbitrarily-chunked reads (TCP gives no message boundaries), so the
//! multiplexer can demux channels off a single stream. Malformed frames
//! (unknown channel tag, length over the configured max) are rejected
//! before any payload is buffered.

use crate::{Channel, TransportError};

/// Frame header: 1-byte channel tag + 4-byte big-endian length.
pub const HEADER_LEN: usize = 5;

fn channel_from_tag(tag: u8) -> Option<Channel> {
    Channel::ALL.into_iter().find(|c| *c as u8 == tag)
}

/// Append a framed `(channel, payload)` to `out`.
pub fn encode_frame(channel: Channel, payload: &[u8], out: &mut Vec<u8>) {
    out.reserve(HEADER_LEN + payload.len());
    out.push(channel as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Encode a single frame into a fresh `Vec`.
pub fn encode_frame_vec(channel: Channel, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    encode_frame(channel, payload, &mut out);
    out
}

/// Streaming frame reassembler. Push bytes as the transport reads them;
/// pull complete frames. Holds at most one partial frame plus any
/// not-yet-consumed complete frames.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    max_frame_bytes: usize,
}

impl FrameDecoder {
    pub fn new(max_frame_bytes: usize) -> Self {
        FrameDecoder {
            buf: Vec::new(),
            max_frame_bytes,
        }
    }

    /// Append freshly-read bytes from the transport.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes buffered but not yet consumed as a complete frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Pull the next complete frame. `Ok(None)` means more bytes are
    /// needed; `Err(Framing)` means the stream is malformed (unknown
    /// channel tag or an oversized length) and the connection should be
    /// torn down.
    pub fn next_frame(&mut self) -> Result<Option<(Channel, Vec<u8>)>, TransportError> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let tag = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        // Validate the header before waiting on (or allocating for) the
        // payload, so a bogus huge length can't make us buffer forever.
        if len > self.max_frame_bytes {
            return Err(TransportError::Framing(format!(
                "frame length {len} exceeds max {} bytes",
                self.max_frame_bytes
            )));
        }
        let channel = channel_from_tag(tag)
            .ok_or_else(|| TransportError::Framing(format!("unknown channel tag {tag}")))?;
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None); // payload hasn't fully arrived yet
        }
        let payload = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..HEADER_LEN + len);
        Ok(Some((channel, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(decoder: &mut FrameDecoder) -> Vec<(Channel, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            out.push(frame);
        }
        out
    }

    #[test]
    fn round_trip_single_frame() {
        let bytes = encode_frame_vec(Channel::Messages, b"hello");
        let mut d = FrameDecoder::new(1024);
        d.push(&bytes);
        assert_eq!(
            d.next_frame().unwrap(),
            Some((Channel::Messages, b"hello".to_vec()))
        );
        assert_eq!(d.next_frame().unwrap(), None);
    }

    #[test]
    fn empty_payload_round_trips() {
        let bytes = encode_frame_vec(Channel::Control, b"");
        let mut d = FrameDecoder::new(1024);
        d.push(&bytes);
        assert_eq!(d.next_frame().unwrap(), Some((Channel::Control, vec![])));
    }

    #[test]
    fn multiple_frames_in_one_buffer_decode_in_order() {
        let mut wire = Vec::new();
        encode_frame(Channel::Control, b"a", &mut wire);
        encode_frame(Channel::Messages, b"bb", &mut wire);
        encode_frame(Channel::Bulk, b"ccc", &mut wire);
        let mut d = FrameDecoder::new(1024);
        d.push(&wire);
        assert_eq!(
            decode_all(&mut d),
            vec![
                (Channel::Control, b"a".to_vec()),
                (Channel::Messages, b"bb".to_vec()),
                (Channel::Bulk, b"ccc".to_vec()),
            ]
        );
    }

    #[test]
    fn frame_split_across_pushes_reassembles() {
        let bytes = encode_frame_vec(Channel::Workflow, b"reassemble-me");
        let mut d = FrameDecoder::new(1024);
        // Feed it one byte at a time — the worst-case TCP chunking.
        for (i, b) in bytes.iter().enumerate() {
            d.push(&[*b]);
            if i + 1 < bytes.len() {
                assert_eq!(d.next_frame().unwrap(), None, "premature frame at byte {i}");
            }
        }
        assert_eq!(
            d.next_frame().unwrap(),
            Some((Channel::Workflow, b"reassemble-me".to_vec()))
        );
    }

    #[test]
    fn partial_header_then_rest() {
        let bytes = encode_frame_vec(Channel::Consensus, b"xy");
        let mut d = FrameDecoder::new(1024);
        d.push(&bytes[..3]); // partial header
        assert_eq!(d.next_frame().unwrap(), None);
        d.push(&bytes[3..]);
        assert_eq!(
            d.next_frame().unwrap(),
            Some((Channel::Consensus, b"xy".to_vec()))
        );
    }

    #[test]
    fn trailing_partial_frame_is_held() {
        let mut wire = encode_frame_vec(Channel::Messages, b"complete");
        wire.extend_from_slice(&encode_frame_vec(Channel::Messages, b"partial")[..4]);
        let mut d = FrameDecoder::new(1024);
        d.push(&wire);
        assert_eq!(
            d.next_frame().unwrap(),
            Some((Channel::Messages, b"complete".to_vec()))
        );
        assert_eq!(d.next_frame().unwrap(), None);
        assert!(d.buffered() > 0, "partial frame retained");
    }

    #[test]
    fn oversized_length_is_rejected() {
        let mut d = FrameDecoder::new(8);
        // channel 2, length 1000 (> max 8).
        d.push(&[2, 0, 0, 0x03, 0xE8]);
        match d.next_frame() {
            Err(TransportError::Framing(msg)) => assert!(msg.contains("exceeds max")),
            other => panic!("expected Framing error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_channel_tag_is_rejected() {
        let mut d = FrameDecoder::new(1024);
        d.push(&[99, 0, 0, 0, 0]); // tag 99 is not a Channel
        match d.next_frame() {
            Err(TransportError::Framing(msg)) => assert!(msg.contains("unknown channel tag 99")),
            other => panic!("expected Framing error, got {other:?}"),
        }
    }
}
