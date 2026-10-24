//! Lists keyboards and pointing devices, and captures only the ones the user chose to share.
//! The system event tap cannot name the physical device, so a partial selection uses IOHID.

use crate::{Captured, HwDevice, HwKind};
use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2_core_foundation::kCFRunLoopCommonModes;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

pub static SHARE_ALL: AtomicBool = AtomicBool::new(true);

static EVENTS: OnceLock<std::sync::mpsc::Sender<Captured>> = OnceLock::new();
static OPENED: Mutex<Vec<DeviceGrab>> = Mutex::new(Vec::new());
static LAST_AXIS: LazyLock<Mutex<HashMap<usize, (i64, i64)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

struct DeviceGrab {
    device: *mut c_void,
}

unsafe impl Send for DeviceGrab {}

pub fn remember_sender(events: std::sync::mpsc::Sender<Captured>) {
    let _ = EVENTS.set(events);
}

pub fn watch(events: std::sync::mpsc::Sender<Captured>) {
    thread::spawn(move || loop {
        let _ = events.send(Captured::Devices(list_devices()));
        thread::sleep(Duration::from_secs(2));
    });
}

pub fn set_shared(run_loop: *mut c_void, share_all: bool, ids: &[String]) {
    SHARE_ALL.store(share_all, Ordering::SeqCst);
    release_grabs();
    if share_all || run_loop.is_null() {
        return;
    }
    let wanted: Vec<String> = ids.to_vec();
    let Some(manager) = manager() else {
        return;
    };
    let devices = copy_devices(manager);
    for device in devices {
        let id = device_id(device);
        if !wanted.iter().any(|wanted| wanted == &id) {
            unsafe { CFRelease(device) };
            continue;
        }
        unsafe {
            if IOHIDDeviceOpen(device, 1) != 0 {
                CFRelease(device);
                continue;
            }
            IOHIDDeviceRegisterInputValueCallback(device, Some(on_value), std::ptr::null_mut());
            IOHIDDeviceScheduleWithRunLoop(
                device,
                run_loop,
                kCFRunLoopCommonModes
                    .map(|mode| mode as *const _ as *const c_void)
                    .unwrap_or(std::ptr::null()),
            );
        }
        if let Ok(mut opened) = OPENED.lock() {
            opened.push(DeviceGrab { device });
        }
        unsafe { CFRelease(device) };
    }
    unsafe { CFRelease(manager) };
}

pub fn list_devices() -> Vec<HwDevice> {
    let Some(manager) = manager() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for device in copy_devices(manager) {
        let page = property_i64(device, "PrimaryUsagePage").unwrap_or(0);
        let usage = property_i64(device, "PrimaryUsage").unwrap_or(0);
        if page != 1 || (usage != 2 && usage != 6 && usage != 1) {
            continue;
        }
        let kind = if usage == 6 { HwKind::Keyboard } else { HwKind::Pointer };
        let name = property_string(device, "Product").unwrap_or_else(|| match kind {
            HwKind::Keyboard => "Keyboard".into(),
            HwKind::Pointer => "Pointing device".into(),
        });
        found.push(HwDevice {
            id: device_id(device),
            name,
            kind,
        });
        unsafe { CFRelease(device) };
    }
found.sort_by(|a, b| a.id.cmp(&b.id));
    found.dedup_by(|a, b| a.id == b.id);
    found.sort_by(|a, b| a.name.cmp(&b.name));
    unsafe { CFRelease(manager) };
    found
}

fn manager() -> Option<*mut c_void> {
    unsafe {
        let manager = IOHIDManagerCreate(std::ptr::null(), 0);
        if manager.is_null() {
            return None;
        }
        IOHIDManagerSetDeviceMatching(manager, std::ptr::null());
        if IOHIDManagerOpen(manager, 0) != 0 {
            CFRelease(manager);
            return None;
        }
        Some(manager)
    }
}

fn copy_devices(manager: *mut c_void) -> Vec<*mut c_void> {
    unsafe {
        let set_ref = IOHIDManagerCopyDevices(manager);
        if set_ref.is_null() {
            return Vec::new();
        }
        let count = CFSetGetCount(set_ref as *const c_void).max(0) as usize;
        let mut values = vec![std::ptr::null(); count];
        CFSetGetValues(set_ref as *const c_void, values.as_mut_ptr());
        let devices: Vec<*mut c_void> = values
            .into_iter()
            .filter(|device| !device.is_null())
            .map(|device| {
                let device = device as *mut c_void;
                CFRetain(device);
                device
            })
            .collect();
        CFRelease(set_ref as *mut c_void);
        devices
    }
}

fn device_id(device: *mut c_void) -> String {
    let vendor = property_i64(device, "VendorID").unwrap_or(0);
    let product = property_i64(device, "ProductID").unwrap_or(0);
    let serial = property_string(device, "SerialNumber");
    let location = property_i64(device, "LocationID").unwrap_or(0);
    match serial {
        Some(serial) if !serial.is_empty() => format!("{vendor:04x}:{product:04x}:{serial}"),
        _ => format!("{vendor:04x}:{product:04x}:{location:08x}"),
    }
}

fn property_string(device: *mut c_void, key: &str) -> Option<String> {
    let key = CFString::from(key);
    let value = unsafe { IOHIDDeviceGetProperty(device, key.as_concrete_TypeRef() as *const c_void) };
    if value.is_null() {
        return None;
    }
    let text = unsafe { CFString::wrap_under_get_rule(value as _) };
    Some(text.to_string())
}

fn property_i64(device: *mut c_void, key: &str) -> Option<i64> {
    let key = CFString::from(key);
    let value = unsafe { IOHIDDeviceGetProperty(device, key.as_concrete_TypeRef() as *const c_void) };
    if value.is_null() {
        return None;
    }
    let number = unsafe { CFNumber::wrap_under_get_rule(value as _) };
    number.to_i64()
}

fn release_grabs() {
    let opened = OPENED.lock().ok().map(|mut opened| std::mem::take(&mut *opened));
    for grab in opened.unwrap_or_default() {
        unsafe {
            IOHIDDeviceClose(grab.device, 1);
            CFRelease(grab.device);
        }
    }
}

unsafe extern "C" fn on_value(_context: *mut c_void, _result: i32, sender: *mut c_void, value: *mut c_void) {
    if value.is_null() || SHARE_ALL.load(Ordering::SeqCst) {
        return;
    }
    let element = IOHIDValueGetElement(value);
    if element.is_null() {
        return;
    }
    let page = IOHIDElementGetUsagePage(element);
    let usage = IOHIDElementGetUsage(element);
    let integer = IOHIDValueGetIntegerValue(value);
    let Some(events) = EVENTS.get() else {
        return;
    };
    if page == 0x07 && usage != 0 {
        let down = integer != 0;
        let usage = usage as u16;
        let _ = events.send(Captured::Key { usage, down });
        if !swallowed() {
            repost_key(usage, down);
        }
        return;
    }
    if page == 0x09 && usage >= 1 && usage <= 8 {
        let down = integer != 0;
        let button = (usage - 1) as u8;
        let _ = events.send(Captured::Button { button, down });
        if !swallowed() {
            repost_button(button, down);
        }
        return;
    }
    if page == 0x01 && usage == 0x38 {
        let _ = events.send(Captured::Wheel { dx: 0, dy: integer as i32 });
        return;
    }
    if page == 0x01 && (usage == 0x30 || usage == 0x31) {
        let delta = axis_delta(sender as usize, usage, integer as i64);
        if delta == 0 {
            return;
        }
        let (dx, dy) = if usage == 0x30 { (delta, 0) } else { (0, delta) };
        let _ = events.send(Captured::MouseDelta { dx, dy });
        let _ = sender;
    }
}

fn swallowed() -> bool {
    SWALLOW.get().is_some_and(|flag| flag.load(Ordering::SeqCst))
}

pub static SWALLOW: OnceLock<std::sync::Arc<AtomicBool>> = OnceLock::new();

fn axis_delta(device: usize, usage: u32, value: i64) -> i32 {
    let Ok(mut map) = LAST_AXIS.lock() else {
        return value.clamp(-1000, 1000) as i32;
    };
    let entry = map.entry(device).or_insert((0, 0));
    let previous = if usage == 0x30 { entry.0 } else { entry.1 };
    if usage == 0x30 {
        entry.0 = value;
    } else {
        entry.1 = value;
    }
    let delta = if value.abs() > 400 { value - previous } else { value };
    delta.clamp(-1000, 1000) as i32
}

fn repost_key(usage: u16, down: bool) {
    let Some(code) = crate::keymap::mac_from_hid(usage) else {
        return;
    };
    if let Some(event) = CGEvent::new_keyboard_event(None::<&CGEventSource>, code as CGKeyCode, down) {
        tag(&event);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

fn repost_button(button: u8, down: bool) {
    let (kind, which) = match (button, down) {
        (1, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
        (1, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
        (2, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
        (2, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        (_, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
        (_, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
    };
    let loc = CGEvent::new(None::<&CGEventSource>)
        .map(|event| CGEvent::location(Some(&event)))
        .unwrap_or(objc2_core_foundation::CGPoint { x: 0.0, y: 0.0 });
    if let Some(event) = CGEvent::new_mouse_event(None::<&CGEventSource>, kind, loc, which) {
        tag(&event);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

fn tag(event: &CGEvent) {
    CGEvent::set_integer_value_field(Some(event), objc2_core_graphics::CGEventField::EventSourceUserData, 0x4D43);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    fn IOHIDManagerSetDeviceMatching(manager: *mut c_void, matching: *const c_void);
    fn IOHIDManagerOpen(manager: *mut c_void, options: u32) -> i32;
    fn IOHIDManagerCopyDevices(manager: *mut c_void) -> *const c_void;
    fn IOHIDDeviceGetProperty(device: *mut c_void, key: *const c_void) -> *const c_void;
    fn IOHIDDeviceOpen(device: *mut c_void, options: u32) -> i32;
    fn IOHIDDeviceClose(device: *mut c_void, options: u32) -> i32;
    fn IOHIDDeviceRegisterInputValueCallback(
        device: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(device: *mut c_void, run_loop: *const c_void, mode: *const c_void);
    fn IOHIDValueGetElement(value: *mut c_void) -> *mut c_void;
    fn IOHIDValueGetIntegerValue(value: *mut c_void) -> isize;
    fn IOHIDElementGetUsagePage(element: *mut c_void) -> u32;
    fn IOHIDElementGetUsage(element: *mut c_void) -> u32;
    fn CFRetain(cf: *mut c_void) -> *mut c_void;
    fn CFRelease(cf: *mut c_void);
    fn CFSetGetCount(set: *const c_void) -> isize;
    fn CFSetGetValues(set: *const c_void, values: *mut *const c_void);
}
