//! USB HID keyboard usages and the hotkey that jumps between machines.
#![allow(dead_code)]

use crate::desk::Direction;

pub const KEY_A: u16 = 0x04;
pub const KEY_ENTER: u16 = 0x28;
pub const KEY_ESCAPE: u16 = 0x29;
pub const KEY_BACKSPACE: u16 = 0x2A;
pub const KEY_TAB: u16 = 0x2B;
pub const KEY_SPACE: u16 = 0x2C;
pub const KEY_CAPS: u16 = 0x39;
pub const KEY_F1: u16 = 0x3A;
pub const KEY_INSERT: u16 = 0x49;
pub const KEY_HOME: u16 = 0x4A;
pub const KEY_PAGE_UP: u16 = 0x4B;
pub const KEY_DELETE: u16 = 0x4C;
pub const KEY_END: u16 = 0x4D;
pub const KEY_PAGE_DOWN: u16 = 0x4E;
pub const KEY_RIGHT: u16 = 0x4F;
pub const KEY_LEFT: u16 = 0x50;
pub const KEY_DOWN: u16 = 0x51;
pub const KEY_UP: u16 = 0x52;
pub const KEY_LEFT_CTRL: u16 = 0xE0;
pub const KEY_LEFT_SHIFT: u16 = 0xE1;
pub const KEY_LEFT_ALT: u16 = 0xE2;
pub const KEY_LEFT_META: u16 = 0xE3;
pub const KEY_RIGHT_CTRL: u16 = 0xE4;
pub const KEY_RIGHT_SHIFT: u16 = 0xE5;
pub const KEY_RIGHT_ALT: u16 = 0xE6;
pub const KEY_RIGHT_META: u16 = 0xE7;

pub fn is_ctrl(usage: u16) -> bool {
    usage == KEY_LEFT_CTRL || usage == KEY_RIGHT_CTRL
}

pub fn is_alt(usage: u16) -> bool {
    usage == KEY_LEFT_ALT || usage == KEY_RIGHT_ALT
}

pub fn arrow(usage: u16) -> Option<Direction> {
    match usage {
        KEY_LEFT => Some(Direction::Left),
        KEY_RIGHT => Some(Direction::Right),
        KEY_UP => Some(Direction::Up),
        KEY_DOWN => Some(Direction::Down),
        _ => None,
    }
}

/// Ctrl+Alt+Arrow jumps along the desk. The arrow itself is not forwarded.
pub fn jump_hotkey(held: &std::collections::BTreeSet<u16>, usage: u16, down: bool) -> Option<Direction> {
    if !down {
        return None;
    }
    let ctrl = held.iter().copied().any(is_ctrl);
    let alt = held.iter().copied().any(is_alt);
    if ctrl && alt {
        arrow(usage)
    } else {
        None
    }
}
