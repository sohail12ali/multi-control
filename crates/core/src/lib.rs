mod desk;
mod hid;
mod input;
mod ipc;

pub use desk::{Cursor, Desk, Direction, MachineId, MoveOutcome, Screen, SNAP_PX};
pub use hid::{arrow, is_alt, is_ctrl, jump_hotkey, KEY_LEFT_ALT, KEY_LEFT_CTRL};
pub use input::{
    generate_pair_code, pair_codes_match, KeyHandoff, KeyState, Message, MoveCoalescer, PeerRecord,
};
pub use ipc::{
    data_dir, ipc_endpoint, DiscoveredPeer, InputDevice, InputKind, IpcEvent, IpcRequest, PairingView,
    PermissionState, StateSnapshot, DEFAULT_SESSION_PORT, IPC_TCP_PORT,
};
