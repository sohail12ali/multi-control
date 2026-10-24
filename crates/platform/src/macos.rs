//! macOS capture via a Quartz event tap, and injection via CGEventPost.
//! The tap callback swallows events while this machine is driving another one.

use super::hid_mac;

use crate::{Captured, DeviceEvent, Endpoint, PlatCmd};
use multicontrol_core::PermissionState;
use objc2_core_foundation::{
    CGPoint, CFBoolean, CFDictionary, CFMachPort, CFRetained, CFRunLoop, CFString, kCFBooleanTrue,
    kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayHideCursor,
    CGDisplayPixelsHigh, CGDisplayPixelsWide, CGDisplayShowCursor, CGEvent, CGEventField,
    CGEventMask, CGEventSource, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
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
    swallow: std::sync::Arc<AtomicBool>,
    port: AtomicPtr<CFMachPort>,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: &'static CFString;
}

fn trusted(prompt: bool) -> bool {
    if !prompt {
        return unsafe { AXIsProcessTrusted() != 0 };
    }
    let key = unsafe { kAXTrustedCheckOptionPrompt };
    let Some(yes) = (unsafe { kCFBooleanTrue }) else {
        return unsafe { AXIsProcessTrusted() != 0 };
    };
    let options: CFRetained<CFDictionary<CFString, CFBoolean>> = CFDictionary::from_slices(&[key], &[yes]);
    let ptr = CFRetained::as_ptr(&options);
    unsafe { AXIsProcessTrustedWithOptions(ptr.as_ptr() as *const c_void) != 0 }
}

fn permission() -> PermissionState {
    if trusted(false) {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

fn screen_size() -> (u32, u32) {
    let id: CGDirectDisplayID = CGMainDisplayID();
    (CGDisplayPixelsWide(id) as u32, CGDisplayPixelsHigh(id) as u32)
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
            unsafe { CGEvent::tap_enable(&*port, true) };
        }
        return event.as_ptr();
    }
    if !hid_mac::SHARE_ALL.load(Ordering::SeqCst) {
        return event.as_ptr();
    }
    let magic = CGEvent::integer_value_field(Some(event_ref), CGEventField::EventSourceUserData);
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
        let dx = CGEvent::integer_value_field(Some(event), CGEventField::MouseEventDeltaX) as i32;
        let dy = CGEvent::integer_value_field(Some(event), CGEventField::MouseEventDeltaY) as i32;
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
        let dy = CGEvent::integer_value_field(Some(event), CGEventField::ScrollWheelEventDeltaAxis1) as i32;
        let dx = CGEvent::integer_value_field(Some(event), CGEventField::ScrollWheelEventDeltaAxis2) as i32;
        Some(Captured::Wheel { dx, dy })
    } else if kind == CGEventType::KeyDown || kind == CGEventType::KeyUp {
        let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
        crate::keymap::hid_from_mac(code).map(|usage| Captured::Key {
            usage,
            down: kind == CGEventType::KeyDown,
        })
    } else if kind == CGEventType::FlagsChanged {
        let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
        let flags = CGEvent::flags(Some(event));
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
    CGEvent::set_integer_value_field(Some(event), CGEventField::EventSourceUserData, MAGIC);
}

fn inject(event: DeviceEvent) {
    match event {
        DeviceEvent::MouseAbsolute { x, y } => {
            let point = CGPoint { x: x as f64, y: y as f64 };
            if let Some(ev) = CGEvent::new_mouse_event(
                None::<&CGEventSource>,
                CGEventType::MouseMoved,
                point,
                CGMouseButton::Left,
            ) {
                tag(&ev);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
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
            if let Some(ev) = CGEvent::new_mouse_event(None::<&CGEventSource>, kind, loc, which) {
                tag(&ev);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
            }
        }
        DeviceEvent::Wheel { dx, dy } => {
            if let Some(ev) = CGEvent::new(None::<&CGEventSource>) {
                CGEvent::set_type(Some(&ev), CGEventType::ScrollWheel);
                CGEvent::set_integer_value_field(Some(&ev), CGEventField::ScrollWheelEventDeltaAxis1, dy as i64);
                CGEvent::set_integer_value_field(Some(&ev), CGEventField::ScrollWheelEventDeltaAxis2, dx as i64);
                tag(&ev);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
            }
        }
        DeviceEvent::Key { usage, down } => {
            let Some(code) = crate::keymap::mac_from_hid(usage) else {
                return;
            };
            if let Some(ev) = CGEvent::new_keyboard_event(None::<&CGEventSource>, code as CGKeyCode, down) {
                tag(&ev);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
            }
        }
    }
}

fn current_location() -> CGPoint {
    CGEvent::new(None::<&CGEventSource>)
        .map(|event| CGEvent::location(Some(&event)))
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
    let swallow = std::sync::Arc::new(AtomicBool::new(false));
    let _ = hid_mac::SWALLOW.set(std::sync::Arc::clone(&swallow));
    hid_mac::remember_sender(event_tx.clone());
    hid_mac::watch(event_tx.clone());
    let ctx: &'static TapCtx = Box::leak(Box::new(TapCtx {
        events: event_tx,
        swallow,
        port: AtomicPtr::new(std::ptr::null_mut()),
    }));
    let loop_ptr = Arc::new(AtomicPtr::<c_void>::new(std::ptr::null_mut()));
    let loop_for_tap = Arc::clone(&loop_ptr);

    thread::Builder::new()
        .name("multicontrol-tap".into())
        .spawn(move || {
            let port = unsafe {
                CGEvent::tap_create(
                    CGEventTapLocation::SessionEventTap,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    event_mask(),
                    Some(tap_callback),
                    ctx as *const TapCtx as *mut c_void,
                )
            };
            let Some(port) = port else {
                log_tap_failure();
                return;
            };
            ctx.port.store(CFRetained::as_ptr(&port).as_ptr(), Ordering::SeqCst);
            let source = CFMachPort::new_run_loop_source(None, Some(&port), 0);
            let run_loop = CFRunLoop::current().expect("run loop");
            unsafe { run_loop.add_source(source.as_deref(), kCFRunLoopCommonModes) };
            CGEvent::tap_enable(&port, true);
            loop_for_tap.store(
                CFRetained::as_ptr(&run_loop).as_ptr().cast(),
                Ordering::SeqCst,
            );
            CFRunLoop::run();
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
                            let id = CGMainDisplayID();
                            if on {
                                CGDisplayHideCursor(id);
                            } else {
                                CGDisplayShowCursor(id);
                            }
                            hidden = on;
                        }
                    }
                    PlatCmd::Warp { x, y } => {
                        CGWarpMouseCursorPosition(CGPoint { x: x as f64, y: y as f64 });
                        CGAssociateMouseAndMouseCursorPosition(true);
                    },
                    PlatCmd::Inject(event) => inject(event),
                    PlatCmd::OpenSettings => open_settings(),
                    PlatCmd::SetShared { share_all, ids } => {
                        let run_loop = loop_ptr.load(Ordering::SeqCst);
                        hid_mac::set_shared(run_loop, share_all, &ids);
                    }
                    PlatCmd::Identify(_) => {}
                    PlatCmd::Shutdown => {
                        let ptr = loop_ptr.load(Ordering::SeqCst);
                        if !ptr.is_null() {
                            unsafe { (&*(ptr as *const CFRunLoop)).stop() };
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
        permission: {
            let _ = trusted(true);
            permission()
        },
        recheck_permission: Some(permission),
        size: screen_size(),
        fake: None,
    })
}

fn log_tap_failure() {
    eprintln!("Multi Control could not create an event tap. Grant Accessibility to multicontrol-daemon.");
}
