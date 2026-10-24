mod fake;
mod keymap;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
#[path = "hid_mac.rs"]
mod hid_mac;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::sync::mpsc::{Receiver, Sender};

pub use fake::FakeHandle;
pub use keymap::{evdev_from_hid, hid_from_evdev, hid_from_mac, hid_from_vk, hid_from_x11, mac_from_hid, vk_from_hid, x11_from_hid};

use multicontrol_core::PermissionState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Captured {
    MouseDelta { dx: i32, dy: i32 },
    Button { button: u8, down: bool },
    Wheel { dx: i32, dy: i32 },
    Key { usage: u16, down: bool },
    Devices(Vec<HwDevice>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HwKind {
    Keyboard,
    Pointer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HwDevice {
    pub id: String,
    pub name: String,
    pub kind: HwKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    MouseAbsolute { x: i32, y: i32 },
    Button { button: u8, down: bool },
    Wheel { dx: i32, dy: i32 },
    Key { usage: u16, down: bool },
}

#[derive(Clone, Debug)]
pub enum PlatCmd {
    Swallow(bool),
    HideCursor(bool),
    Warp { x: i32, y: i32 },
    Inject(DeviceEvent),
    Identify(String),
    OpenSettings,
    SetShared { share_all: bool, ids: Vec<String> },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeRecord {
    Inject(DeviceEvent),
    Swallow(bool),
    Hide(bool),
    Warp { x: i32, y: i32 },
    Identify(String),
    Settings,
}

pub struct Endpoint {
    pub events: Receiver<Captured>,
    pub cmds: Sender<PlatCmd>,
    pub permission: PermissionState,
    pub recheck_permission: Option<fn() -> PermissionState>,
    pub size: (u32, u32),
    pub fake: Option<FakeHandle>,
}

pub fn launch(fake_screen: Option<(u32, u32)>) -> Result<Endpoint, String> {
    if let Some(size) = fake_screen {
        let (events, cmds, permission, size, fake) = fake::launch(size);
        return Ok(Endpoint {
            events,
            cmds,
            permission,
            size,
            fake: Some(fake),
            recheck_permission: None,
        });
    }
    #[cfg(target_os = "macos")]
    {
        return macos::launch();
    }
    #[cfg(target_os = "windows")]
    {
        return windows::launch();
    }
    #[cfg(target_os = "linux")]
    {
        return linux::launch();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("this operating system is not supported".into())
    }
}
