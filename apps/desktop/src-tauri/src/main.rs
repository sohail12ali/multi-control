#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use multicontrol_core::{data_dir, ipc_endpoint, IpcRequest};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let ipc = spawn_ipc(handle);
            app.manage(Ipc(ipc));
            build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "panel" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![daemon_request, set_expanded])
        .run(tauri::generate_context!())
        .expect("Multi Control failed to start");
}

struct Ipc(Sender<String>);

#[tauri::command]
fn daemon_request(state: tauri::State<Ipc>, request: IpcRequest) -> Result<(), String> {
    let line = serde_json::to_string(&request).map_err(|err| err.to_string())?;
    state.0.send(line).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_expanded(window: tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    let size = if expanded {
        tauri::LogicalSize::new(760.0, 640.0)
    } else {
        tauri::LogicalSize::new(340.0, 520.0)
    };
    window.set_size(size).map_err(|err| err.to_string())?;
    let _ = window.center();
    Ok(())
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &quit])?;
    let mut builder = TrayIconBuilder::new().menu(&menu).on_menu_event(|app, event| {
        match event.id.as_ref() {
            "open" => show_panel(app),
            "pause" => {
                if let Some(ipc) = app.try_state::<Ipc>() {
                    let _ = ipc.0.send(r#"{"type":"set_paused","paused":true}"#.into());
                }
            }
            "quit" => {
                if let Some(ipc) = app.try_state::<Ipc>() {
                    let _ = ipc.0.send(r#"{"type":"shutdown"}"#.into());
                }
                app.exit(0);
            }
            _ => {}
        }
    });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn show_panel(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("panel") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn spawn_ipc(app: tauri::AppHandle) -> Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let path = ipc_endpoint(&data_dir());
        ensure_daemon(&path);
        match connect(&path) {
            Ok(stream) => pump(app, stream, rx),
            Err(err) => log::error!("daemon ipc: {err}"),
        }
    });
    tx
}

fn ensure_daemon(path: &std::path::Path) {
    if connect(path).is_ok() {
        return;
    }
    let binary = daemon_binary();
    if binary.exists() {
        let _ = Command::new(binary).spawn();
        for _ in 0..50 {
            if connect(path).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(40));
        }
    } else {
        log::error!("daemon binary not found at {}", binary.display());
    }
}

fn daemon_binary() -> PathBuf {
    if let Ok(path) = std::env::var("MULTICONTROL_DAEMON") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        let name = if cfg!(windows) { "multicontrol-daemon.exe" } else { "multicontrol-daemon" };
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    let name = if cfg!(windows) { "multicontrol-daemon.exe" } else { "multicontrol-daemon" };
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug").join(name)
}

#[cfg(unix)]
fn connect(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(path)
}

#[cfg(unix)]
fn pump(app: tauri::AppHandle, stream: std::os::unix::net::UnixStream, rx: mpsc::Receiver<String>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone ipc"));
    let mut writer = stream;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                handle_line(&app, line.trim_end());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        while let Ok(request) = rx.try_recv() {
            let _ = writeln!(writer, "{request}");
            let _ = writer.flush();
        }
    }
}

#[cfg(windows)]
fn connect(path: &std::path::Path) -> std::io::Result<std::net::TcpStream> {
    let addr = path.to_string_lossy();
    std::net::TcpStream::connect(addr.as_ref())
}

#[cfg(windows)]
fn pump(app: tauri::AppHandle, stream: std::net::TcpStream, rx: mpsc::Receiver<String>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone ipc"));
    let mut writer = stream;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => handle_line(&app, line.trim_end()),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        while let Ok(request) = rx.try_recv() {
            let _ = writeln!(writer, "{request}");
            let _ = writer.flush();
        }
    }
}

fn handle_line(app: &tauri::AppHandle, line: &str) {
    if let Some(window) = app.get_webview_window("panel") {
        let _ = window.emit("daemon", line);
    }
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
        if event.get("type").and_then(|value| value.as_str()) == Some("identify") {
            let label = event.get("label").and_then(|value| value.as_str()).unwrap_or("1");
            flash(app, label);
        }
    }
}

fn flash(app: &tauri::AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window("flash") {
        let _ = window.emit("flash", label);
        let _ = window.show();
    } else {
        let window = WebviewWindowBuilder::new(app, "flash", WebviewUrl::App("flash.html".into()))
            .title("Identify")
            .fullscreen(true)
            .always_on_top(true)
            .decorations(false)
            .transparent(true)
            .skip_taskbar(true)
            .build();
        if let Ok(window) = window {
            let _ = window.emit("flash", label);
        }
    }
    let app = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        if let Some(window) = app.get_webview_window("flash") {
            let _ = window.hide();
        }
    });
}
