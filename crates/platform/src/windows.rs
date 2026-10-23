//! Windows capture with low-level hooks and injection with SendInput.
//! Hooks swallow events while this computer is driving another one.

use crate::{keymap, Captured, DeviceEvent, Endpoint, PlatCmd};
use multicontrol_core::PermissionState;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread;
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, GetSystemMetrics, PostThreadMessageW,
    SetCursorPos, SetWindowsHookExW, TranslateMessage, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    SM_CXSCREEN, SM_CYSCREEN,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

const MAGIC: usize = 0x4D43;

static EVENTS: OnceLock<Sender<Captured>> = OnceLock::new();
static SWALLOW: AtomicBool = AtomicBool::new(false);
static THREAD: AtomicU32 = AtomicU32::new(0);

fn screen_size() -> (u32, u32) {
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(1) as u32,
            GetSystemMetrics(SM_CYSCREEN).max(1) as u32,
        )
    }
}

pub fn launch() -> Result<Endpoint, String> {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<PlatCmd>();
    let _ = EVENTS.set(event_tx);
    thread::Builder::new()
        .name("multicontrol-hooks".into())
        .spawn(|| unsafe {
            THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
            let _mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0);
            let _key = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0);
            let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .map_err(|err| err.to_string())?;

    thread::Builder::new()
        .name("multicontrol-inject".into())
        .spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    PlatCmd::Swallow(on) => SWALLOW.store(on, Ordering::SeqCst),
                    PlatCmd::HideCursor(_) | PlatCmd::Identify(_) | PlatCmd::OpenSettings => {}
                    PlatCmd::Warp { x, y } => unsafe {
                        let _ = SetCursorPos(x, y);
                    },
                    PlatCmd::Inject(event) => inject(event),
                    PlatCmd::Shutdown => {
                        let id = THREAD.load(Ordering::SeqCst);
                        if id != 0 {
                            unsafe {
                                let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
                            }
                        }
                        break;
                    }
                }
            }
        })
        .map_err(|err| err.to_string())?;

    Ok(Endpoint {
        events: event_rx,
        cmds: cmd_tx,
        permission: PermissionState::Granted,
        recheck_permission: None,
        size: screen_size(),
        fake: None,
    })
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    if info.dwExtraInfo == MAGIC {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let message = wparam.0 as u32;
    if let Some(tx) = EVENTS.get() {
        match message {
            WM_MOUSEMOVE => {
                if let Some((dx, dy)) = pointer_delta(info.pt.x, info.pt.y) {
                    let _ = tx.send(Captured::MouseDelta { dx, dy });
                }
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP => {
                let _ = tx.send(Captured::Button {
                    button: 0,
                    down: message == WM_LBUTTONDOWN,
                });
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP => {
                let _ = tx.send(Captured::Button {
                    button: 1,
                    down: message == WM_RBUTTONDOWN,
                });
            }
            WM_MBUTTONDOWN | WM_MBUTTONUP => {
                let _ = tx.send(Captured::Button {
                    button: 2,
                    down: message == WM_MBUTTONDOWN,
                });
            }
            WM_MOUSEWHEEL => {
                let delta = ((info.mouseData >> 16) as i16) as i32;
                let _ = tx.send(Captured::Wheel { dx: 0, dy: delta / 120 });
            }
            WM_MOUSEHWHEEL => {
                let delta = ((info.mouseData >> 16) as i16) as i32;
                let _ = tx.send(Captured::Wheel { dx: delta / 120, dy: 0 });
            }
            _ => {}
        }
    }
    if SWALLOW.load(Ordering::SeqCst) && message == WM_MOUSEMOVE {
        let _ = SetCursorPos(info.pt.x, info.pt.y);
    }
    if SWALLOW.load(Ordering::SeqCst) {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    if info.dwExtraInfo == MAGIC {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let down = wparam.0 != 0x0101 && wparam.0 != 0x0105;
    if let Some(usage) = keymap::hid_from_vk(info.vkCode as u16) {
        if let Some(tx) = EVENTS.get() {
            let _ = tx.send(Captured::Key { usage, down });
        }
    }
    if SWALLOW.load(Ordering::SeqCst) {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

static LAST_POINT: std::sync::Mutex<Option<(i32, i32)>> = std::sync::Mutex::new(None);

fn pointer_delta(x: i32, y: i32) -> Option<(i32, i32)> {
    let mut last = LAST_POINT.lock().expect("cursor");
    let Some((lx, ly)) = *last else {
        *last = Some((x, y));
        return None;
    };
    let dx = x - lx;
    let dy = y - ly;
    if SWALLOW.load(Ordering::SeqCst) {
        unsafe { let _ = SetCursorPos(lx, ly); }
        if dx == 0 && dy == 0 {
            None
        } else {
            Some((dx, dy))
        }
    } else if dx == 0 && dy == 0 {
        None
    } else {
        *last = Some((x, y));
        Some((dx, dy))
    }
}

fn inject(event: DeviceEvent) {
    let mut input = INPUT::default();
    match event {
        DeviceEvent::MouseAbsolute { x, y } => {
            let (w, h) = screen_size();
            input.r#type = windows::Win32::UI::Input::KeyboardAndMouse::INPUT_MOUSE;
            input.Anonymous = INPUT_0 {
                mi: MOUSEINPUT {
                    dx: x.saturating_mul(65535) / (w as i32 - 1).max(1),
                    dy: y.saturating_mul(65535) / (h as i32 - 1).max(1),
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                    time: 0,
                    dwExtraInfo: MAGIC,
                },
            };
        }
        DeviceEvent::Button { button, down } => {
            let flags = match (button, down) {
                (1, true) => MOUSEEVENTF_RIGHTDOWN,
                (1, false) => MOUSEEVENTF_RIGHTUP,
                (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                (2, false) => MOUSEEVENTF_MIDDLEUP,
                (_, true) => MOUSEEVENTF_LEFTDOWN,
                (_, false) => MOUSEEVENTF_LEFTUP,
            };
            input.r#type = windows::Win32::UI::Input::KeyboardAndMouse::INPUT_MOUSE;
            input.Anonymous = INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: MAGIC,
                },
            };
        }
        DeviceEvent::Wheel { dx, dy } => {
            input.r#type = windows::Win32::UI::Input::KeyboardAndMouse::INPUT_MOUSE;
            input.Anonymous = INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: if dy != 0 { (dy * 120) as u32 } else { (dx * 120) as u32 },
                    dwFlags: if dy != 0 { MOUSEEVENTF_WHEEL } else { MOUSEEVENTF_HWHEEL },
                    time: 0,
                    dwExtraInfo: MAGIC,
                },
            };
        }
        DeviceEvent::Key { usage, down } => {
            let Some(vk) = keymap::vk_from_hid(usage) else { return };
            let mut flags = windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS::default();
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            input.r#type = windows::Win32::UI::Input::KeyboardAndMouse::INPUT_KEYBOARD;
            input.Anonymous = INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: MAGIC,
                },
            };
        }
    }
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}
