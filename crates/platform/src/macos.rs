//! macOS capture via a Quartz event tap, and injection via CGEventPost.
//! The tap callback swallows events while this machine is driving another one.

use crate::{Captured, DeviceEvent, Endpoint, PlatCmd};
use multicontrol_core::PermissionState;
use objc2_core_foundation::{
    CGPoint, CFMachPort, CFMachPortCreateRunLoopSource, CFRetained, CFRunLoopAddSource,
    CFRunLoopGetCurrent, CFRunLoopRun, CFRunLoopStop, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayHideCursor,
    CGDisplayPixelsHigh, CGDisplayPixelsWide, CGDisplayShowCursor, CGEvent, CGEventCreate,
    CGEventCreateKeyboardEvent, CGEventCreateMouseEvent, CGEventField, CGEventGetFlags,
    CGEventGetIntegerValueField, CGEventGetLocation, CGEventMask, CGEventPost,
    CGEventSetIntegerValueField, CGEventSetType, CGEventSource,
    CGEventTapCreate, CGEventTapEnable, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGKeyCode, CGMainDisplayID, CGMouseButton,
    CGWarpMouseCursorPosition,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

const MAGIC: i64 = 0x4D43;

struct TapCtx {
    events: Sender<Captured>,
    swallow: AtomicBool,
    port: AtomicPtr<CFMachPort>,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

fn permission() -> PermissionState {
    if trusted() {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

fn screen_size() -> (u32, u32) {
    unsafe {
        let id: CGDirectDisplayID = CGMainDisplayID();
        (CGDisplayPixelsWide(id) as u32, CGDisplayPixelsHigh(id) as u32)
    }
}

fn event_mask() -> CGEventMask {
    let types = [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
    ];
    types.into_iter().fold(0, |mask, kind| {
        mask | (1u64 << (kind.0 as u64))
    })
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user: *mut c_void,
) -> *mut CGEvent {
    let ctx = unsafe { &*(user as *const TapCtx) };
    let event_ref = unsafe { event.as_ref() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        let port = ctx.port.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(&*port, true) };
        }
        return event.as_ptr();
    }
    let magic = CGEventGetIntegerValueField(Some(event_ref), CGEventField::EventSourceUserData);
    if magic == MAGIC {
        return event.as_ptr();
    }
    dispatch(ctx, kind, event_ref);
    if ctx.swallow.load(Ordering::SeqCst) {
        std::ptr::null_mut()
    } else {
        event.as_ptr()
    }
}

fn dispatch(ctx: &TapCtx, kind: CGEventType, event: &CGEvent) {
    let captured = if kind == CGEventType::MouseMoved
        || kind == CGEventType::LeftMouseDragged
        || kind == CGEventType::RightMouseDragged
        || kind == CGEventType::OtherMouseDragged
    {
        let dx = CGEventGetIntegerValueField(Some(event), CGEventField::MouseEventDeltaX) as i32;
        let dy = CGEventGetIntegerValueField(Some(event), CGEventField::MouseEventDeltaY) as i32;
        if dx == 0 && dy == 0 {
            None
        } else {
            Some(Captured::MouseDelta { dx, dy })
        }
    } else if kind == CGEventType::LeftMouseDown || kind == CGEventType::LeftMouseUp {
        Some(Captured::Button {
            button: 0,
            down: kind == CGEventType::LeftMouseDown,
        })
    } else if kind == CGEventType::RightMouseDown || kind == CGEventType::RightMouseUp {
        Some(Captured::Button {
            button: 1,
            down: kind == CGEventType::RightMouseDown,
        })
    } else if kind == CGEventType::OtherMouseDown || kind == CGEventType::OtherMouseUp {
        Some(Captured::Button {
            button: 2,
            down: kind == CGEventType::OtherMouseDown,
        })
    } else if kind == CGEventType::ScrollWheel {
        let dy = CGEventGetIntegerValueField(Some(event), CGEventField::ScrollWheelEventDeltaAxis1) as i32;
        let dx = CGEventGetIntegerValueField(Some(event), CGEventField::ScrollWheelEventDeltaAxis2) as i32;
        Some(Captured::Wheel { dx, dy })
    } else if kind == CGEventType::KeyDown || kind == CGEventType::KeyUp {
        let code = CGEventGetIntegerValueField(Some(event), CGEventField::KeyboardEventKeycode) as u16;
        crate::keymap::hid_from_mac(code).map(|usage| Captured::Key {
            usage,
            down: kind == CGEventType::KeyDown,
        })
    } else if kind == CGEventType::FlagsChanged {
        let code = CGEventGetIntegerValueField(Some(event), CGEventField::KeyboardEventKeycode) as u16;
        let flags = unsafe { CGEventGetFlags(Some(event)) };
        crate::keymap::hid_from_mac(code).map(|usage| Captured::Key {
            usage,
            down: modifier_is_down(flags, code),
        })
    } else {
        None
    };
    if let Some(captured) = captured {
        let _ = ctx.events.send(captured);
    }
}

fn modifier_is_down(flags: objc2_core_graphics::CGEventFlags, mac_code: u16) -> bool {
    let bit: u64 = match mac_code {
        0x38 | 0x3C => 0x00020000, // shift
        0x3B | 0x3E => 0x00040000, // control
        0x3A | 0x3D => 0x00080000, // option
        0x37 | 0x36 => 0x00100000, // command
        _ => return false,
    };
    flags.0 & bit != 0
}

fn tag(event: &CGEvent) {
    CGEventSetIntegerValueField(Some(event), CGEventField::EventSourceUserData, MAGIC);
}

fn inject(event: DeviceEvent) {
    match event {
        DeviceEvent::MouseAbsolute { x, y } => {
            let point = CGPoint { x: x as f64, y: y as f64 };
            if let Some(ev) = unsafe {
                CGEventCreateMouseEvent(
                    None::<&CGEventSource>,
                    CGEventType::MouseMoved,
                    point,
                    CGMouseButton::Left,
                )
            } {
                tag(&ev);
                unsafe { CGEventPost(CGEventTapLocation::HIDEventTap, Some(&ev)) };
            }
        }
        DeviceEvent::Button { button, down } => {
            let (kind, which) = match (button, down) {
                (1, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                (1, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                (2, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                (2, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
                (_, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                (_, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            };
            let loc = current_location();
            if let Some(ev) = unsafe {
                CGEventCreateMouseEvent(None::<&CGEventSource>, kind, loc, which)
            } {
                tag(&ev);
                unsafe { CGEventPost(CGEventTapLocation::HIDEventTap, Some(&ev)) };
            }
        }
        DeviceEvent::Wheel { dx, dy } => {
            if let Some(ev) = unsafe { CGEventCreate(None::<&CGEventSource>) } {
                unsafe { CGEventSetType(Some(&ev), CGEventType::ScrollWheel) };
                CGEventSetIntegerValueField(Some(&ev), CGEventField::ScrollWheelEventDeltaAxis1, dy as i64);
                CGEventSetIntegerValueField(Some(&ev), CGEventField::ScrollWheelEventDeltaAxis2, dx as i64);
                tag(&ev);
                unsafe { CGEventPost(CGEventTapLocation::HIDEventTap, Some(&ev)) };
            }
        }
        DeviceEvent::Key { usage, down } => {
            let Some(code) = crate::keymap::mac_from_hid(usage) else {
                return;
            };
            if let Some(ev) = unsafe {
                CGEventCreateKeyboardEvent(None::<&CGEventSource>, code as CGKeyCode, down)
            } {
                tag(&ev);
                unsafe { CGEventPost(CGEventTapLocation::HIDEventTap, Some(&ev)) };
            }
        }
    }
}

fn current_location() -> CGPoint {
    unsafe { CGEventCreate(None::<&CGEventSource>) }
        .map(|event| CGEventGetLocation(Some(&event)))
        .unwrap_or(CGPoint { x: 0.0, y: 0.0 })
}

fn open_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

pub fn launch() -> Result<Endpoint, String> {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<PlatCmd>();
    let ctx: &'static TapCtx = Box::leak(Box::new(TapCtx {
        events: event_tx,
        swallow: AtomicBool::new(false),
        port: AtomicPtr::new(std::ptr::null_mut()),
    }));
    let loop_ptr = Arc::new(AtomicPtr::<c_void>::new(std::ptr::null_mut()));
    let loop_for_tap = Arc::clone(&loop_ptr);

    thread::Builder::new()
        .name("multicontrol-tap".into())
        .spawn(move || unsafe {
            let port = CGEventTapCreate(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                event_mask(),
                Some(tap_callback),
                ctx as *const TapCtx as *mut c_void,
            );
            let Some(port) = port else {
                log_tap_failure();
                return;
            };
            ctx.port.store(CFRetained::as_ptr(&port).as_ptr(), Ordering::SeqCst);
            let source = CFMachPortCreateRunLoopSource(None, Some(&port), 0);
            let run_loop = CFRunLoopGetCurrent().expect("run loop");
            CFRunLoopAddSource(&run_loop, source.as_deref(), kCFRunLoopCommonModes);
            CGEventTapEnable(&port, true);
            loop_for_tap.store(
                CFRetained::as_ptr(&run_loop).as_ptr().cast(),
                Ordering::SeqCst,
            );
            std::mem::forget(run_loop);
            std::mem::forget(port);
            CFRunLoopRun();
        })
        .map_err(|err| err.to_string())?;

    thread::Builder::new()
        .name("multicontrol-inject".into())
        .spawn(move || {
            let mut hidden = false;
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    PlatCmd::Swallow(on) => ctx.swallow.store(on, Ordering::SeqCst),
                    PlatCmd::HideCursor(on) => {
                        if hidden != on {
                            let id = unsafe { CGMainDisplayID() };
                            if on {
                                unsafe { CGDisplayHideCursor(id) };
                            } else {
                                unsafe { CGDisplayShowCursor(id) };
                            }
                            hidden = on;
                        }
                    }
                    PlatCmd::Warp { x, y } => unsafe {
                        CGWarpMouseCursorPosition(CGPoint { x: x as f64, y: y as f64 });
                        CGAssociateMouseAndMouseCursorPosition(true);
                    },
                    PlatCmd::Inject(event) => inject(event),
                    PlatCmd::OpenSettings => open_settings(),
                    PlatCmd::Identify(_) => {}
                    PlatCmd::Shutdown => {
                        let ptr = loop_ptr.load(Ordering::SeqCst);
                        if !ptr.is_null() {
                            unsafe { CFRunLoopStop(&*(ptr as *const objc2_core_foundation::CFRunLoop)) };
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
        permission: permission(),
        recheck_permission: Some(permission),
        size: screen_size(),
        fake: None,
    })
}

fn log_tap_failure() {
    eprintln!("Multi Control could not create an event tap. Grant Accessibility to multicontrol-daemon.");
}
