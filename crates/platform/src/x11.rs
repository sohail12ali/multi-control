//! X11 capture via the keymap and pointer, and injection via XTest.
//! Raw motion supplies deltas after the cursor is clamped at the screen edge.

use crate::{keymap, Captured, DeviceEvent, Endpoint, PlatCmd};
use multicontrol_core::PermissionState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

pub fn launch() -> Result<Endpoint, String> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|err| err.to_string())?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let size = (screen.width_in_pixels as u32, screen.height_in_pixels as u32);
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    thread::Builder::new()
        .name("multicontrol-x11".into())
        .spawn(move || run(conn, root, event_tx, cmd_rx))
        .map_err(|err| err.to_string())?;
    Ok(Endpoint {
        events: event_rx,
        cmds: cmd_tx,
        permission: PermissionState::Granted,
        recheck_permission: None,
        size,
        fake: None,
    })
}

fn run(conn: RustConnection, root: u32, events: Sender<Captured>, cmds: mpsc::Receiver<PlatCmd>) {
    let swallow = AtomicBool::new(false);
    let mut keys = [0u8; 32];
    let mut last = query_pointer(&conn, root).unwrap_or((0, 0));
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                PlatCmd::Swallow(on) => swallow.store(on, Ordering::SeqCst),
                PlatCmd::Warp { x, y } => {
                    let _ = conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x as i16, y as i16);
                    let _ = conn.flush();
                    last = (x, y);
                }
                PlatCmd::Inject(event) => inject(&conn, root, event),
                PlatCmd::HideCursor(_) | PlatCmd::Identify(_) | PlatCmd::OpenSettings => {}
                PlatCmd::Shutdown => return,
            }
        }
        if let Some(next) = query_pointer(&conn, root) {
            let dx = next.0 - last.0;
            let dy = next.1 - last.1;
            if swallow.load(Ordering::SeqCst) {
                if dx != 0 || dy != 0 {
                    let _ = events.send(Captured::MouseDelta { dx, dy });
                    let _ = conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, last.0 as i16, last.1 as i16);
                    let _ = conn.flush();
                }
            } else if dx != 0 || dy != 0 {
                let _ = events.send(Captured::MouseDelta { dx, dy });
                last = next;
            }
        }
        if let Ok(cookie) = conn.query_keymap() {
            if let Ok(reply) = cookie.reply() {
            for code in 8..256 {
                let before = key_down(&keys, code);
                let after = key_down(&reply.keys, code);
                if before != after {
                    if let Some(usage) = keymap::hid_from_x11(code as u16) {
                        let _ = events.send(Captured::Key { usage, down: after });
                    }
                }
            }
            keys = reply.keys;
            }
        }
        thread::sleep(Duration::from_millis(8));
        let _ = KeyButMask::default();
    }
}

fn key_down(map: &[u8; 32], code: usize) -> bool {
    map[code / 8] & (1 << (code % 8)) != 0
}

fn query_pointer(conn: &RustConnection, root: u32) -> Option<(i32, i32)> {
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((reply.root_x as i32, reply.root_y as i32))
}

fn inject(conn: &RustConnection, root: u32, event: DeviceEvent) {
    const KEY_PRESS: u8 = 2;
    const KEY_RELEASE: u8 = 3;
    const BUTTON_PRESS: u8 = 4;
    const BUTTON_RELEASE: u8 = 5;
    const MOTION: u8 = 6;
    let send = |kind: u8, detail: u8, x: i16, y: i16| {
        let _ = conn.xtest_fake_input(kind, detail, 0, root, x, y, 0);
        let _ = conn.flush();
    };
    match event {
        DeviceEvent::MouseAbsolute { x, y } => send(MOTION, 0, x as i16, y as i16),
        DeviceEvent::Button { button, down } => {
            let detail = match button {
                1 => 3,
                2 => 2,
                _ => 1,
            };
            send(if down { BUTTON_PRESS } else { BUTTON_RELEASE }, detail, 0, 0);
        }
        DeviceEvent::Wheel { dx, dy } => {
            let (button, times) = if dy > 0 {
                (4, dy)
            } else if dy < 0 {
                (5, -dy)
            } else if dx > 0 {
                (7, dx)
            } else {
                (6, -dx)
            };
            for _ in 0..times.max(0) {
                send(BUTTON_PRESS, button, 0, 0);
                send(BUTTON_RELEASE, button, 0, 0);
            }
        }
        DeviceEvent::Key { usage, down } => {
            let Some(code) = keymap::x11_from_hid(usage) else { return };
            send(if down { KEY_PRESS } else { KEY_RELEASE }, code as u8, 0, 0);
        }
    }
}
