//! Wire codec for Raft [`Message`]s.
//!
//! A compact hand-rolled binary encoding (matching the style of
//! `cs-net::framing` and `cs-distrib::pid` — no serde dependency) so consensus
//! RPCs can ride cs-net's `Channel::Consensus`. The sender's identity is *not*
//! encoded: the driver knows which peer a frame arrived from (it polls each
//! peer's transport), so `from` is supplied out-of-band.

use crate::raft::{ConfState, Entry, EntryPayload, Message};
use crate::ReplicaId;

// ---- writer helpers ----

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u64(out, b.len() as u64);
    out.extend_from_slice(b);
}
fn put_ids(out: &mut Vec<u8>, ids: &[ReplicaId]) {
    put_u64(out, ids.len() as u64);
    for id in ids {
        put_u64(out, id.0);
    }
}

fn put_conf(out: &mut Vec<u8>, c: &ConfState) {
    match c {
        ConfState::Simple(v) => {
            put_u8(out, 0);
            put_ids(out, v);
        }
        ConfState::Joint { old, new } => {
            put_u8(out, 1);
            put_ids(out, old);
            put_ids(out, new);
        }
    }
}

fn put_entry(out: &mut Vec<u8>, e: &Entry) {
    put_u64(out, e.term);
    put_u64(out, e.index);
    match &e.payload {
        EntryPayload::Command(c) => {
            put_u8(out, 0);
            put_bytes(out, c);
        }
        EntryPayload::Noop => put_u8(out, 1),
        EntryPayload::Config(c) => {
            put_u8(out, 2);
            put_conf(out, c);
        }
    }
}

/// Encode a Raft message to bytes.
pub fn encode(msg: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    match msg {
        Message::RequestVote {
            term,
            candidate,
            last_log_index,
            last_log_term,
        } => {
            put_u8(&mut out, 0);
            put_u64(&mut out, *term);
            put_u64(&mut out, candidate.0);
            put_u64(&mut out, *last_log_index);
            put_u64(&mut out, *last_log_term);
        }
        Message::RequestVoteResp { term, granted } => {
            put_u8(&mut out, 1);
            put_u64(&mut out, *term);
            put_u8(&mut out, *granted as u8);
        }
        Message::AppendEntries {
            term,
            leader,
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit,
            read_seq,
        } => {
            put_u8(&mut out, 2);
            put_u64(&mut out, *term);
            put_u64(&mut out, leader.0);
            put_u64(&mut out, *prev_log_index);
            put_u64(&mut out, *prev_log_term);
            put_u64(&mut out, entries.len() as u64);
            for e in entries {
                put_entry(&mut out, e);
            }
            put_u64(&mut out, *leader_commit);
            put_u64(&mut out, *read_seq);
        }
        Message::AppendEntriesResp {
            term,
            success,
            match_index,
            conflict_index,
            read_seq,
        } => {
            put_u8(&mut out, 3);
            put_u64(&mut out, *term);
            put_u8(&mut out, *success as u8);
            put_u64(&mut out, *match_index);
            put_u64(&mut out, *conflict_index);
            put_u64(&mut out, *read_seq);
        }
        Message::InstallSnapshot {
            term,
            leader,
            last_included_index,
            last_included_term,
            last_included_config,
            data,
            read_seq,
        } => {
            put_u8(&mut out, 4);
            put_u64(&mut out, *term);
            put_u64(&mut out, leader.0);
            put_u64(&mut out, *last_included_index);
            put_u64(&mut out, *last_included_term);
            put_conf(&mut out, last_included_config);
            put_bytes(&mut out, data);
            put_u64(&mut out, *read_seq);
        }
    }
    out
}

// ---- reader ----

/// Decode error: a frame was malformed or truncated.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodeError(pub &'static str);

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError("eof u8"))?;
        self.pos += 1;
        Ok(b)
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        let end = self.pos + 8;
        let s = self.buf.get(self.pos..end).ok_or(DecodeError("eof u64"))?;
        self.pos = end;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u64()? as usize;
        let end = self.pos + n;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(DecodeError("eof bytes"))?;
        self.pos = end;
        Ok(s.to_vec())
    }
    fn ids(&mut self) -> Result<Vec<ReplicaId>, DecodeError> {
        let n = self.u64()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(ReplicaId(self.u64()?));
        }
        Ok(v)
    }
    fn conf(&mut self) -> Result<ConfState, DecodeError> {
        match self.u8()? {
            0 => Ok(ConfState::Simple(self.ids()?)),
            1 => Ok(ConfState::Joint {
                old: self.ids()?,
                new: self.ids()?,
            }),
            _ => Err(DecodeError("bad conf tag")),
        }
    }
    fn entry(&mut self) -> Result<Entry, DecodeError> {
        let term = self.u64()?;
        let index = self.u64()?;
        let payload = match self.u8()? {
            0 => EntryPayload::Command(self.bytes()?),
            1 => EntryPayload::Noop,
            2 => EntryPayload::Config(self.conf()?),
            _ => return Err(DecodeError("bad payload tag")),
        };
        Ok(Entry {
            term,
            index,
            payload,
        })
    }
}

/// Decode a Raft message produced by [`encode`].
pub fn decode(buf: &[u8]) -> Result<Message, DecodeError> {
    let mut c = Cursor::new(buf);
    let msg = match c.u8()? {
        0 => Message::RequestVote {
            term: c.u64()?,
            candidate: ReplicaId(c.u64()?),
            last_log_index: c.u64()?,
            last_log_term: c.u64()?,
        },
        1 => Message::RequestVoteResp {
            term: c.u64()?,
            granted: c.u8()? != 0,
        },
        2 => {
            let term = c.u64()?;
            let leader = ReplicaId(c.u64()?);
            let prev_log_index = c.u64()?;
            let prev_log_term = c.u64()?;
            let n = c.u64()? as usize;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(c.entry()?);
            }
            Message::AppendEntries {
                term,
                leader,
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit: c.u64()?,
                read_seq: c.u64()?,
            }
        }
        3 => Message::AppendEntriesResp {
            term: c.u64()?,
            success: c.u8()? != 0,
            match_index: c.u64()?,
            conflict_index: c.u64()?,
            read_seq: c.u64()?,
        },
        4 => Message::InstallSnapshot {
            term: c.u64()?,
            leader: ReplicaId(c.u64()?),
            last_included_index: c.u64()?,
            last_included_term: c.u64()?,
            last_included_config: c.conf()?,
            data: c.bytes()?,
            read_seq: c.u64()?,
        },
        _ => return Err(DecodeError("bad message tag")),
    };
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: Message) {
        assert_eq!(decode(&encode(&m)), Ok(m));
    }

    #[test]
    fn all_messages_round_trip() {
        roundtrip(Message::RequestVote {
            term: 7,
            candidate: ReplicaId(2),
            last_log_index: 9,
            last_log_term: 6,
        });
        roundtrip(Message::RequestVoteResp {
            term: 7,
            granted: true,
        });
        roundtrip(Message::AppendEntries {
            term: 3,
            leader: ReplicaId(1),
            prev_log_index: 4,
            prev_log_term: 2,
            entries: vec![
                Entry {
                    term: 3,
                    index: 5,
                    payload: EntryPayload::Command(vec![1, 2, 3]),
                },
                Entry {
                    term: 3,
                    index: 6,
                    payload: EntryPayload::Noop,
                },
                Entry {
                    term: 3,
                    index: 7,
                    payload: EntryPayload::Config(ConfState::Joint {
                        old: vec![ReplicaId(0), ReplicaId(1)],
                        new: vec![ReplicaId(0), ReplicaId(1), ReplicaId(2)],
                    }),
                },
            ],
            leader_commit: 4,
            read_seq: 11,
        });
        roundtrip(Message::AppendEntriesResp {
            term: 3,
            success: false,
            match_index: 0,
            conflict_index: 5,
            read_seq: 11,
        });
        roundtrip(Message::InstallSnapshot {
            term: 9,
            leader: ReplicaId(1),
            last_included_index: 100,
            last_included_term: 8,
            last_included_config: ConfState::Simple(vec![ReplicaId(0), ReplicaId(1), ReplicaId(2)]),
            data: vec![9, 8, 7, 6],
            read_seq: 0,
        });
    }

    #[test]
    fn truncated_frame_errors() {
        let bytes = encode(&Message::RequestVoteResp {
            term: 1,
            granted: true,
        });
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn empty_frame_errors() {
        assert!(decode(&[]).is_err());
    }
}
