//! Wayland capture through the InputCapture portal and injection through RemoteDesktop.
//! This is the path GNOME and KDE expose. Other compositors report that they are unsupported.

use crate::{keymap, Captured, DeviceEvent, Endpoint, PlatCmd};
use multicontrol_core::PermissionState;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub fn launch() -> Result<Endpoint, String> {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (size_tx, size_rx) = mpsc::channel();
    thread::Builder::new()
        .name("multicontrol-wayland".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("wayland runtime");
            runtime.block_on(async {
                let local = tokio::task::LocalSet::new();
                local.run_until(run(event_tx, cmd_rx, size_tx)).await;
            });
        })
        .map_err(|err| err.to_string())?;
    let size = size_rx
        .recv_timeout(std::time::Duration::from_secs(8))
        .unwrap_or((1280, 800));
    Ok(Endpoint {
        events: event_rx,
        cmds: cmd_tx,
        permission: PermissionState::Unknown,
        recheck_permission: None,
        size,
        fake: None,
    })
}

async fn run(events: Sender<Captured>, cmds: Receiver<PlatCmd>, size_tx: Sender<(u32, u32)>) {
    let capture = match setup_capture(&events, &size_tx).await {
        Ok(capture) => Some(capture),
        Err(err) => {
            eprintln!("wayland input capture unavailable: {err}");
            let _ = size_tx.send((1280, 800));
            None
        }
    };
    let inject = match setup_inject().await {
        Ok(session) => Some(session),
        Err(err) => {
            eprintln!("wayland remote desktop unavailable: {err}");
            None
        }
    };
    while let Ok(cmd) = cmds.recv() {
        match cmd {
            PlatCmd::Inject(event) => {
                if let Some(session) = &inject {
                    let _ = apply_inject(session, event).await;
                }
            }
            PlatCmd::Shutdown => break,
            PlatCmd::Swallow(_) | PlatCmd::HideCursor(_) | PlatCmd::Warp { .. } | PlatCmd::Identify(_) | PlatCmd::OpenSettings | PlatCmd::SetShared { .. } => {
                if let Some(capture) = &capture {
                    let _ = capture;
                }
            }
        }
    }
}

struct CaptureSession {
    _proxy: ashpd::desktop::input_capture::InputCapture,
    _session: ashpd::desktop::Session<ashpd::desktop::input_capture::InputCapture>,
}

struct InjectSession {
    proxy: ashpd::desktop::remote_desktop::RemoteDesktop,
    session: ashpd::desktop::Session<ashpd::desktop::remote_desktop::RemoteDesktop>,
}

async fn setup_capture(
    events: &Sender<Captured>,
    size_tx: &Sender<(u32, u32)>,
) -> ashpd::Result<CaptureSession> {
    use ashpd::desktop::input_capture::{
        Barrier, BarrierID, BarrierPosition, Capabilities, CreateSessionOptions, InputCapture,
    };
    let proxy = InputCapture::new().await?;
    let (session, _) = proxy
        .create_session(
            None,
            CreateSessionOptions::default()
                .set_capabilities(Capabilities::Keyboard | Capabilities::Pointer),
        )
        .await?;
    let zones = proxy.zones(&session, Default::default()).await?.response()?;
    let mut width = 0u32;
    let mut height = 0u32;
    let barriers = zones
        .regions()
        .iter()
        .enumerate()
        .flat_map(|(index, region)| {
            width = width.max(region.width());
            height = height.max(region.height());
            let x = region.x_offset();
            let y = region.y_offset();
            let w = region.width() as i32;
            let h = region.height() as i32;
            let id = |n: u32| BarrierID::new((index as u32) * 4 + n + 1).expect("barrier");
            [
                Barrier::new(id(0), BarrierPosition::new(x, y, x, y + h - 1)),
                Barrier::new(id(1), BarrierPosition::new(x + w, y, x + w, y + h - 1)),
                Barrier::new(id(2), BarrierPosition::new(x, y, x + w - 1, y)),
                Barrier::new(id(3), BarrierPosition::new(x, y + h, x + w - 1, y + h)),
            ]
        })
        .collect::<Vec<_>>();
    let _ = size_tx.send((width.max(1), height.max(1)));
    let _ = proxy
        .set_pointer_barriers(&session, &barriers, zones.zone_set(), Default::default())
        .await?
        .response()?;
    let _ = proxy.enable(&session, Default::default()).await;
    let fd = proxy.connect_to_eis(&session, Default::default()).await?;
    let stream = UnixStream::from(fd);
    let _ = stream.set_nonblocking(true);
    let events = events.clone();
    tokio::task::spawn_local(async move {
        if let Err(err) = read_ei(stream, events).await {
            eprintln!("libei capture ended: {err}");
        }
    });
    Ok(CaptureSession { _proxy: proxy, _session: session })
}

async fn read_ei(stream: UnixStream, events: Sender<Captured>) -> Result<(), String> {
    use futures_util::StreamExt;
    use reis::ei::{self, keyboard::KeyState};
    use reis::event::{DeviceCapability, EiEvent};
    let context = ei::Context::new(stream).map_err(|err| err.to_string())?;
    context.flush().map_err(|err| err.to_string())?;
    let (_connection, mut stream) = context
        .handshake_tokio("multicontrol", ei::handshake::ContextType::Receiver)
        .await
        .map_err(|err| err.to_string())?;
    while let Some(event) = stream.next().await {
        let event = event.map_err(|err| err.to_string())?;
        match event {
            EiEvent::SeatAdded(seat) => {
                seat.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Scroll,
                    DeviceCapability::Button,
                ]);
                let _ = context.flush();
            }
            EiEvent::PointerMotion(motion) => {
                let _ = events.send(Captured::MouseDelta {
                    dx: motion.dx as i32,
                    dy: motion.dy as i32,
                });
            }
            EiEvent::Button(button) => {
                let which = match button.button {
                    0x111 => 1,
                    0x112 => 2,
                    _ => 0,
                };
                let _ = events.send(Captured::Button {
                    button: which,
                    down: button.state == reis::ei::button::ButtonState::Press,
                });
            }
            EiEvent::ScrollDelta(scroll) => {
                let _ = events.send(Captured::Wheel {
                    dx: scroll.dx as i32,
                    dy: scroll.dy as i32,
                });
            }
            EiEvent::KeyboardKey(key) => {
                if let Some(usage) = keymap::hid_from_evdev(key.key as u16) {
                    let _ = events.send(Captured::Key {
                        usage,
                        down: key.state == KeyState::Press,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn setup_inject() -> ashpd::Result<InjectSession> {
    use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
    let proxy = RemoteDesktop::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    proxy
        .select_devices(
            &session,
            SelectDevicesOptions::default().set_devices(DeviceType::Keyboard | DeviceType::Pointer),
        )
        .await?;
    let _ = proxy.start(&session, None, Default::default()).await?.response()?;
    Ok(InjectSession { proxy, session })
}

async fn apply_inject(session: &InjectSession, event: DeviceEvent) -> ashpd::Result<()> {
    use ashpd::desktop::remote_desktop::KeyState;
    match event {
        DeviceEvent::MouseAbsolute { x, y } => {
            session
                .proxy
                .notify_pointer_motion_absolute(&session.session, 0, x as f64, y as f64, Default::default())
                .await?;
        }
        DeviceEvent::Button { button, down } => {
            let code = match button {
                1 => 0x111,
                2 => 0x112,
                _ => 0x110,
            };
            let state = if down { KeyState::Pressed } else { KeyState::Released };
            session
                .proxy
                .notify_pointer_button(&session.session, code, state, Default::default())
                .await?;
        }
        DeviceEvent::Wheel { dx, dy } => {
            session
                .proxy
                .notify_pointer_axis(&session.session, dx as f64, dy as f64, Default::default())
                .await?;
        }
        DeviceEvent::Key { usage, down } => {
            if let Some(code) = keymap::evdev_from_hid(usage) {
                let state = if down { KeyState::Pressed } else { KeyState::Released };
                session
                    .proxy
                    .notify_keyboard_keycode(&session.session, code as i32, state, Default::default())
                    .await?;
            }
        }
    }
    Ok(())
}
