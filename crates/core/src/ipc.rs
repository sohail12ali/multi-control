//! Shared paths and the IPC messages between the panel and the daemon.

use crate::desk::Screen;
use crate::input::PeerRecord;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Unknown,
    Granted,
    Denied,
    Unsupported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub paired: bool,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingView {
    ShowingCode {
        peer_name: String,
        code: String,
    },
    EnterCode {
        peer_name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub local_id: String,
    pub local_name: String,
    pub paused: bool,
    pub permission: PermissionState,
    pub focused_id: String,
    pub focused_name: String,
    pub controlling_remote: bool,
    pub screens: Vec<Screen>,
    pub discovered: Vec<DiscoveredPeer>,
    pub paired: Vec<PeerRecord>,
    pub pairing: Option<PairingView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    SetPaused { paused: bool },
    SetName { name: String },
    BeginAdd { peer_id: String },
    BeginAddAddress { host: String, port: u16 },
    SubmitCode { code: String },
    CancelPair,
    Place { id: String, x: i32, y: i32 },
    Unplace { id: String },
    Identify { id: String },
    Jump { target: String },
    OpenPermissionSettings,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcEvent {
    State { snapshot: StateSnapshot },
    Identify { label: String },
    Error { message: String },
}

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MULTICONTROL_HOME") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        return PathBuf::from(home).join("Library/Application Support/Multi Control");
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        return PathBuf::from(base).join("Multi Control");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            format!("{home}/.local/share")
        });
        return PathBuf::from(base).join("multi-control");
    }
}

pub fn ipc_endpoint(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in home.to_string_lossy().as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        std::path::PathBuf::from(format!("/tmp/multicontrol-{hash:016x}.sock"))
    }
    #[cfg(windows)]
    {
        let _ = home;
        std::path::PathBuf::from(format!("127.0.0.1:{IPC_TCP_PORT}"))
    }
}

pub const IPC_TCP_PORT: u16 = 47321;
pub const DEFAULT_SESSION_PORT: u16 = 47320;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_request_roundtrip() {
        let req = IpcRequest::Place {
            id: "a".into(),
            x: 10,
            y: -4,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<IpcRequest>(&json).unwrap(), req);
    }
}
