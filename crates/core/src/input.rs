//! Physical keys versus the keys a focused machine has been told about.
//! Leaving a machine, or losing it, always produces a key-up for every key it
//! still thinks is down.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyState {
    physical: BTreeSet<u16>,
    echoed: BTreeSet<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHandoff {
    pub release: Vec<u16>,
    pub press: Vec<u16>,
}

impl KeyState {
    pub fn physical(&self) -> impl Iterator<Item = u16> + '_ {
        self.physical.iter().copied()
    }

    pub fn contains_physical(&self, usage: u16) -> bool {
        self.physical.contains(&usage)
    }

    /// Key activity on the machine the user is physically using, while focus is local.
    pub fn note_local(&mut self, usage: u16, down: bool) {
        if down {
            self.physical.insert(usage);
            self.echoed.insert(usage);
        } else {
            self.physical.remove(&usage);
            self.echoed.remove(&usage);
        }
    }

    /// Key activity while a remote machine is focused.
    /// Returns true when the remote should be told about this edge.
    pub fn note_remote(&mut self, usage: u16, down: bool) -> bool {
        let was = self.echoed.contains(&usage);
        if down {
            self.physical.insert(usage);
            self.echoed.insert(usage);
            !was
        } else {
            self.physical.remove(&usage);
            self.echoed.remove(&usage);
            was
        }
    }

    /// Focus moved. Release everything echoed on the old machine and press
    /// what is still physically held on the new one.
    pub fn handoff(&mut self) -> KeyHandoff {
        let release: Vec<u16> = self.echoed.iter().copied().collect();
        let press: Vec<u16> = self.physical.iter().copied().collect();
        self.echoed.clone_from(&self.physical);
        KeyHandoff { release, press }
    }

    /// The link died. `release` goes to the remote if it can still hear us.
    /// `press` is what the local machine should be put back into.
    pub fn disconnect(&mut self) -> KeyHandoff {
        self.handoff()
    }
}

/// Latest pointer position, flushed on a short interval so a burst of moves
/// becomes one packet.
#[derive(Clone, Debug)]
pub struct MoveCoalescer {
    pending: Option<(i32, i32)>,
    last_sent: Option<std::time::Instant>,
    interval: std::time::Duration,
}

impl MoveCoalescer {
    pub fn new(interval: std::time::Duration) -> Self {
        Self {
            pending: None,
            last_sent: None,
            interval,
        }
    }

    pub fn push(&mut self, x: i32, y: i32, now: std::time::Instant) -> Option<(i32, i32)> {
        self.pending = Some((x, y));
        let due = self
            .last_sent
            .map(|t| now.saturating_duration_since(t) >= self.interval)
            .unwrap_or(true);
        if due {
            self.last_sent = Some(now);
            self.pending.take()
        } else {
            None
        }
    }

    pub fn poll(&mut self, now: std::time::Instant) -> Option<(i32, i32)> {
        let due = self
            .last_sent
            .map(|t| now.saturating_duration_since(t) >= self.interval)
            .unwrap_or(true);
        if due {
            self.flush(now)
        } else {
            None
        }
    }

    pub fn flush(&mut self, now: std::time::Instant) -> Option<(i32, i32)> {
        if self.pending.is_some() {
            self.last_sent = Some(now);
        }
        self.pending.take()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        id: String,
        name: String,
        width: u32,
        height: u32,
        fingerprint: String,
        port: u16,
    },
    Pair {
        code: String,
    },
    PairOk,
    PairReject,
    Enter {
        x: i32,
        y: i32,
    },
    Leave,
    Move {
        x: i32,
        y: i32,
    },
    Button {
        button: u8,
        down: bool,
    },
    Wheel {
        dx: i32,
        dy: i32,
    },
    Key {
        usage: u16,
        down: bool,
    },
    ReleaseKeys {
        usages: Vec<u16>,
    },
    PressKeys {
        usages: Vec<u16>,
    },
    Layout {
        rev: u64,
        screens: Vec<crate::desk::Screen>,
    },
    Identify,
    Heartbeat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub host: String,
    pub port: u16,
}

pub fn generate_pair_code() -> String {
    use rand::Rng;
    let n: u32 = rand::rng().random_range(0..1_000_000);
    format!("{n:06}")
}

pub fn pair_codes_match(expected: &str, given: &str) -> bool {
    let expected = expected.trim();
    let given = given.trim();
    if expected.len() != given.len() || expected.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(given.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_releases_echoed_keys_and_represses_what_is_held() {
        let mut keys = KeyState::default();
        keys.note_local(0x04, true);
        keys.note_local(0x16, true);
        let handoff = keys.handoff();
        assert_eq!(handoff.release, vec![0x04, 0x16]);
        assert_eq!(handoff.press, vec![0x04, 0x16]);
        assert!(keys.note_remote(0x16, false));
        let gone = keys.disconnect();
        assert_eq!(gone.release, vec![0x04]);
        assert_eq!(gone.press, vec![0x04]);
    }

    #[test]
    fn remote_key_up_is_forwarded_once() {
        let mut keys = KeyState::default();
        keys.note_local(0x04, true);
        keys.handoff();
        assert!(!keys.note_remote(0x04, true));
        assert!(keys.note_remote(0x04, false));
        assert!(!keys.note_remote(0x04, false));
    }

    #[test]
    fn coalescer_keeps_only_the_latest_move_inside_the_window() {
        let start = std::time::Instant::now();
        let mut coal = MoveCoalescer::new(std::time::Duration::from_millis(4));
        assert_eq!(coal.push(1, 2, start), Some((1, 2)));
        assert_eq!(coal.push(3, 4, start), None);
        assert_eq!(coal.push(5, 6, start), None);
        let later = start + std::time::Duration::from_millis(4);
        assert_eq!(coal.poll(later), Some((5, 6)));
        assert_eq!(coal.flush(later), None);
    }

    #[test]
    fn flush_sends_a_pending_move_before_a_click() {
        let start = std::time::Instant::now();
        let mut coal = MoveCoalescer::new(std::time::Duration::from_millis(4));
        coal.push(1, 1, start);
        assert_eq!(coal.push(8, 9, start), None);
        assert_eq!(coal.flush(start), Some((8, 9)));
    }

    #[test]
    fn pair_code_match_is_exact() {
        assert!(pair_codes_match("004821", "004821"));
        assert!(!pair_codes_match("004821", "004822"));
        assert!(!pair_codes_match("004821", "04821"));
        assert_eq!(generate_pair_code().len(), 6);
    }

    #[test]
    fn message_roundtrip() {
        let msg = Message::Enter { x: 3, y: 4 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }
}
