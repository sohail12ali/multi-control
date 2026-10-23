use crate::{Captured, DeviceEvent, FakeRecord, PlatCmd};
use multicontrol_core::PermissionState;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct FakeHandle {
    pub input: Sender<Captured>,
    pub log: Arc<Mutex<Vec<FakeRecord>>>,
}

pub fn launch(size: (u32, u32)) -> (Receiver<Captured>, Sender<PlatCmd>, PermissionState, (u32, u32), FakeHandle) {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let input = event_tx.clone();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_thread = Arc::clone(&log);
    thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            let mut log = log_thread.lock().expect("fake log");
            match cmd {
                PlatCmd::Swallow(on) => log.push(FakeRecord::Swallow(on)),
                PlatCmd::HideCursor(on) => log.push(FakeRecord::Hide(on)),
                PlatCmd::Warp { x, y } => log.push(FakeRecord::Warp { x, y }),
                PlatCmd::Inject(event) => log.push(FakeRecord::Inject(event)),
                PlatCmd::Identify(label) => log.push(FakeRecord::Identify(label)),
                PlatCmd::OpenSettings => log.push(FakeRecord::Settings),
                PlatCmd::Shutdown => break,
            }
        }
    });
    // Keep the channel alive so the daemon can also observe captures the test injects.
    let _keep = event_tx;
    let _ = size;
    let _ = Duration::from_millis(1);
    (
        event_rx,
        cmd_tx,
        PermissionState::Granted,
        size,
        FakeHandle { input, log },
    )
}

pub fn matches_absolute(record: &FakeRecord, x: i32, y: i32) -> bool {
    matches!(record, FakeRecord::Inject(DeviceEvent::MouseAbsolute { x: ax, y: ay }) if *ax == x && *ay == y)
}
