//! Talks to the daemon. The native panels draw; this only moves messages.

use multicontrol_core::{data_dir, ipc_endpoint, IpcEvent, IpcRequest};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    requests: Sender<IpcRequest>,
}

impl Client {
    /// Starts the daemon if it is not already listening, then reads events on a background thread.
    pub fn spawn(on_event: impl Fn(IpcEvent) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("multicontrol-panel".into())
            .spawn(move || pump(rx, on_event))
            .expect("panel thread");
        Self { requests: tx }
    }

    pub fn send(&self, request: IpcRequest) {
        let _ = self.requests.send(request);
    }
}

fn pump(rx: mpsc::Receiver<IpcRequest>, on_event: impl Fn(IpcEvent)) {
    let path = ipc_endpoint(&data_dir());
    ensure_daemon(&path);
    let stream = match connect(&path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("Multi Control could not reach the daemon: {err}");
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone ipc"));
    let mut writer = stream;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Ok(event) = serde_json::from_str::<IpcEvent>(line.trim_end()) {
                    on_event(event);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        while let Ok(request) = rx.try_recv() {
            if let Ok(mut line) = serde_json::to_string(&request) {
                line.push('\n');
                if writer.write_all(line.as_bytes()).is_err() || writer.flush().is_err() {
                    return;
                }
            }
        }
    }
}

fn ensure_daemon(path: &std::path::Path) {
    if connect(path).is_ok() {
        return;
    }
    let binary = daemon_binary();
    if !binary.exists() {
        eprintln!("daemon binary not found at {}", binary.display());
        return;
    }
    let _ = Command::new(binary).spawn();
    for _ in 0..50 {
        if connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn daemon_binary() -> PathBuf {
    if let Ok(path) = std::env::var("MULTICONTROL_DAEMON") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) {
        "multicontrol-daemon.exe"
    } else {
        "multicontrol-daemon"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(name)
}

#[cfg(unix)]
fn connect(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(path)
}

#[cfg(windows)]
fn connect(path: &std::path::Path) -> std::io::Result<std::net::TcpStream> {
    std::net::TcpStream::connect(path.to_string_lossy().as_ref())
}
