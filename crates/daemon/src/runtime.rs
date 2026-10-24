use anyhow::{anyhow, Context, Result};
use multicontrol_core::{
    generate_pair_code, jump_hotkey, pair_codes_match, Cursor, Desk, Direction, DiscoveredPeer,
    InputDevice, InputKind, IpcEvent, IpcRequest, KeyState, MachineId, Message, MoveCoalescer,
    MoveOutcome, PairingView, PeerRecord, PermissionState, Screen, StateSnapshot,
};
use multicontrol_platform::{Captured, DeviceEvent, Endpoint, FakeHandle, HwKind, PlatCmd};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::identity::{self, Gate, Identity};

const PAIR_WINDOW: Duration = Duration::from_secs(120);
const FLUSH: Duration = Duration::from_millis(4);
const SWITCH_DELAY: Duration = Duration::from_millis(150);
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub struct DaemonOptions {
    pub home: PathBuf,
    pub fake_screen: Option<(u32, u32)>,
    pub session_port: u16,
    pub heartbeat: Duration,
    pub discover: bool,
}

impl DaemonOptions {
    pub fn production() -> Self {
        Self {
            home: multicontrol_core::data_dir(),
            fake_screen: None,
            session_port: multicontrol_core::DEFAULT_SESSION_PORT,
            heartbeat: Duration::from_secs(3),
            discover: true,
        }
    }
}

pub struct Running {
    pub session_port: u16,
    pub home: PathBuf,
    pub fake: Option<FakeHandle>,
    shutdown: watch::Sender<bool>,
}

impl Running {
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn ipc_path(home: &Path) -> PathBuf {
    multicontrol_core::ipc_endpoint(home)
}

pub async fn start(opts: DaemonOptions) -> Result<Running> {
    identity::install_crypto();
    std::fs::create_dir_all(&opts.home)?;
    let identity = Arc::new(identity::load_or_create(&opts.home)?);
    let mut disk = Disk::load(&opts.home.join("config.json")).unwrap_or_else(|| Disk {
        id: new_id(),
        name: computer_name(),
        peers: Vec::new(),
        screens: Vec::new(),
        layout_rev: 0,
        local_only: Vec::new(),
    });
    if disk.id.is_empty() {
        disk.id = new_id();
    }
    if disk.name.is_empty() {
        disk.name = computer_name();
    }

    let home = opts.home.clone();
    let endpoint = multicontrol_platform::launch(opts.fake_screen).map_err(|err| anyhow!(err))?;
    let listener = TcpListener::bind(("0.0.0.0", opts.session_port))
        .await
        .with_context(|| format!("bind session port {}", opts.session_port))?;
    let session_port = listener.local_addr()?.port();
    let pins = Arc::new(RwLock::new(
        disk.peers
            .iter()
            .map(|peer| peer.fingerprint.clone())
            .collect::<HashSet<_>>(),
    ));
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bus = Arc::new(Bus::default());
    let ipc = ipc_path(&home);
    #[cfg(unix)]
    if ipc.exists() {
        let _ = std::fs::remove_file(&ipc);
    }

    let (mut app, capture_rx, discover) = App::boot(
        opts,
        disk,
        Arc::clone(&identity),
        Arc::clone(&pins),
        endpoint,
        session_port,
        events_tx.clone(),
        Arc::clone(&bus),
        home.clone(),
    );
    let fake = app.fake_handle.take();
    spawn_capture(capture_rx, events_tx.clone());
    tokio::spawn(listen_loop(
        listener,
        identity,
        pins,
        events_tx.clone(),
        shutdown_rx.clone(),
    ));
    tokio::spawn(ipc_loop(ipc, events_tx, shutdown_rx.clone()));
    if discover {
        if let Err(err) = crate::discover::spawn(
            app.name.clone(),
            app.local_id.clone(),
            app.fingerprint.clone(),
            session_port,
            app.events.clone(),
        ) {
            log::warn!("discovery unavailable: {err}");
        }
    }
    tokio::spawn(owner_loop(app, events_rx, shutdown_rx));
    Ok(Running {
        session_port,
        home,
        fake,
        shutdown: shutdown_tx,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Disk {
    id: String,
    name: String,
    peers: Vec<PeerRecord>,
    screens: Vec<Screen>,
    layout_rev: u64,
    #[serde(default)]
    local_only: Vec<String>,
}

impl Disk {
    fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[derive(Default)]
struct Bus {
    subs: Mutex<Vec<mpsc::UnboundedSender<String>>>,
}

impl Bus {
    fn publish(&self, line: String) {
        let mut subs = self.subs.lock().expect("bus");
        subs.retain(|tx| tx.send(line.clone()).is_ok());
    }

    fn add(&self, tx: mpsc::UnboundedSender<String>) {
        self.subs.lock().expect("bus").push(tx);
    }
}

pub enum Hub {
    Captured(Captured),
    Connected(LiveLink),
    Peer { conn: u64, msg: Message },
    Closed { conn: u64 },
    DialFailed { key: String },
    Ipc(IpcRequest),
    Subscribe(mpsc::UnboundedSender<String>),
    Discovered {
        id: String,
        name: String,
        host: String,
        port: u16,
        fingerprint: String,
    },
    Forgot(String),
}

pub struct LiveLink {
    id: u64,
    tx: mpsc::UnboundedSender<Message>,
    fingerprint: String,
    addr: SocketAddr,
    pairing_dial: bool,
}

struct Link {
    tx: mpsc::UnboundedSender<Message>,
    fingerprint: String,
    addr: SocketAddr,
    peer_id: Option<String>,
    peer_name: Option<String>,
    width: u32,
    height: u32,
    listen_port: u16,
    last_rx: Instant,
}

struct PairOut {
    code: String,
    label: String,
    started: Instant,
    conn: Option<u64>,
}

struct PairIn {
    conn: u64,
    code: String,
    name: String,
    started: Instant,
}

struct Seen {
    id: String,
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    fullname: String,
    last_seen: Instant,
}

#[derive(Default)]
struct Track {
    physical: BTreeSet<u8>,
    echoed: BTreeSet<u8>,
}

impl Track {
    fn note_local(&mut self, value: u8, down: bool) {
        if down {
            self.physical.insert(value);
            self.echoed.insert(value);
        } else {
            self.physical.remove(&value);
            self.echoed.remove(&value);
        }
    }

    fn note_remote(&mut self, value: u8, down: bool) -> bool {
        let was = self.echoed.contains(&value);
        if down {
            self.physical.insert(value);
            self.echoed.insert(value);
            !was
        } else {
            self.physical.remove(&value);
            self.echoed.remove(&value);
            was
        }
    }

    fn handoff(&mut self) -> (Vec<u8>, Vec<u8>) {
        let release: Vec<u8> = self.echoed.iter().copied().collect();
        let press: Vec<u8> = self.physical.iter().copied().collect();
        self.echoed.clone_from(&self.physical);
        (release, press)
    }

    fn disconnect(&mut self) -> (Vec<u8>, Vec<u8>) {
        self.handoff()
    }
}

struct App {
    home: PathBuf,
    local_id: String,
    name: String,
    fingerprint: String,
    session_port: u16,
    desk: Desk,
    keys: KeyState,
    buttons: Track,
    cursor: Cursor,
    peers: HashMap<String, PeerRecord>,
    links: HashMap<u64, Link>,
    discovered: HashMap<String, Seen>,
    pairing_out: Option<PairOut>,
    pairing_in: Option<PairIn>,
    paused: bool,
    permission: PermissionState,
    recheck: Option<fn() -> PermissionState>,
    controlled_by: Option<String>,
    injected_keys: BTreeSet<u16>,
    injected_buttons: BTreeSet<u8>,
    layout_rev: u64,
    heartbeat: Duration,
    dialing: HashSet<String>,
    cmds: std::sync::mpsc::Sender<PlatCmd>,
    events: mpsc::UnboundedSender<Hub>,
    identity: Arc<Identity>,
    pins: Arc<RwLock<HashSet<String>>>,
    bus: Arc<Bus>,
    fake_handle: Option<FakeHandle>,
    dirty: bool,
    stop: bool,
    edge_hold: Option<EdgeHold>,
    local_only: Vec<String>,
    devices: Vec<InputDevice>,
    capture_sent: Option<(bool, Vec<String>)>,
}

struct EdgeHold {
    since: Instant,
    from: MachineId,
    cursor: Cursor,
    warped: bool,
}

impl App {
    fn boot(
        opts: DaemonOptions,
        disk: Disk,
        identity: Arc<Identity>,
        pins: Arc<RwLock<HashSet<String>>>,
        endpoint: Endpoint,
        session_port: u16,
        events: mpsc::UnboundedSender<Hub>,
        bus: Arc<Bus>,
        home: PathBuf,
    ) -> (Self, std::sync::mpsc::Receiver<Captured>, bool) {
        let Endpoint {
            events: capture_rx,
            cmds,
            permission,
            recheck_permission,
            size,
            fake,
        } = endpoint;
        let mut desk = Desk::new();
        for screen in disk.screens {
            desk.upsert(screen);
        }
        let local = MachineId::new(disk.id.clone());
        desk.ensure(local.clone(), disk.name.clone(), size.0.max(1), size.1.max(1));
        if !desk.get(&local).map(|s| s.placed).unwrap_or(false) {
            let _ = desk.place(&local, 0, 0);
        }
        let cursor = desk.jump_to(&local).expect("local screen");
        let peers = disk.peers.into_iter().map(|peer| (peer.id.clone(), peer)).collect();
        let app = Self {
            home,
            local_id: disk.id,
            name: disk.name,
            fingerprint: identity.fingerprint.clone(),
            session_port,
            desk,
            keys: KeyState::default(),
            buttons: Track::default(),
            cursor,
            peers,
            links: HashMap::new(),
            discovered: HashMap::new(),
            pairing_out: None,
            pairing_in: None,
            paused: false,
            permission,
            recheck: recheck_permission,
            controlled_by: None,
            injected_keys: BTreeSet::new(),
            injected_buttons: BTreeSet::new(),
            layout_rev: disk.layout_rev,
            heartbeat: opts.heartbeat,
            dialing: HashSet::new(),
            cmds,
            events,
            identity,
            pins,
            bus,
            fake_handle: fake,
            dirty: true,
            stop: false,
            edge_hold: None,
            local_only: disk.local_only,
            devices: Vec::new(),
            capture_sent: None,
        };
        (app, capture_rx, opts.discover)
    }

    fn handle(&mut self, event: Hub) {
        match event {
            Hub::Captured(captured) => self.on_captured(captured),
            Hub::Connected(live) => self.on_connected(live),
            Hub::Peer { conn, msg } => self.on_peer(conn, msg),
            Hub::Closed { conn } => self.drop_link(conn),
            Hub::DialFailed { key } => {
                self.dialing.remove(&key);
            }
            Hub::Ipc(request) => self.on_ipc(request),
            Hub::Subscribe(tx) => {
                if let Ok(line) = serde_json::to_string(&IpcEvent::State {
                    snapshot: self.snapshot(),
                }) {
                    let _ = tx.send(line);
                }
                self.bus.add(tx);
            }
            Hub::Discovered {
                id,
                name,
                host,
                port,
                fingerprint,
            } => {
                if id == self.local_id {
                    return;
                }
                self.discovered.insert(
                    id.clone(),
                    Seen {
                        id,
                        name,
                        host,
                        port,
                        fingerprint,
                        fullname: String::new(),
                        last_seen: Instant::now(),
                    },
                );
                self.dirty = true;
            }
            Hub::Forgot(fullname) => {
                self.discovered.retain(|_, seen| seen.fullname != fullname);
                self.dirty = true;
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(check) = self.recheck {
            let next = check();
            if next != self.permission {
                self.permission = next;
                self.dirty = true;
            }
        }
        if self
            .pairing_out
            .as_ref()
            .is_some_and(|pair| now.duration_since(pair.started) > PAIR_WINDOW)
        {
            if let Some(conn) = self.pairing_out.as_ref().and_then(|pair| pair.conn) {
                self.drop_link(conn);
            }
            self.pairing_out = None;
            self.dirty = true;
        }
        if self
            .pairing_in
            .as_ref()
            .is_some_and(|pair| now.duration_since(pair.started) > PAIR_WINDOW)
        {
            if let Some(pair) = self.pairing_in.take() {
                self.send_conn(pair.conn, Message::PairReject);
                self.drop_link(pair.conn);
            }
            self.dirty = true;
        }
        let stale: Vec<u64> = self
            .links
            .iter()
            .filter(|(_, link)| now.duration_since(link.last_rx) > self.heartbeat)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.drop_link(id);
        }
        for link in self.links.values() {
            let _ = link.tx.send(Message::Heartbeat);
        }
        self.discovered
            .retain(|_, seen| now.duration_since(seen.last_seen) < Duration::from_secs(20));
        let peers: Vec<PeerRecord> = self.peers.values().cloned().collect();
        for peer in peers {
            let connected = self
                .links
                .values()
                .any(|link| link.peer_id.as_deref() == Some(peer.id.as_str()));
            if connected || self.local_id.as_str() >= peer.id.as_str() {
                continue;
            }
            self.dial(peer.host, peer.port, None);
        }
    }

    fn on_captured(&mut self, captured: Captured) {
        if self.controlled_by.is_some() {
            return;
        }
        match captured {
            Captured::MouseDelta { dx, dy } => self.on_delta(dx, dy),
            Captured::Button { button, down } => self.on_button(button, down),
            Captured::Wheel { dx, dy } => {
                if self.is_remote() && !self.paused {
                    self.send_focus(Message::Wheel { dx, dy });
                }
            }
            Captured::Key { usage, down } => self.on_key(usage, down),
            Captured::Devices(listed) => self.on_devices(listed),
        }
    }

    fn on_delta(&mut self, dx: i32, dy: i32) {
        if self.paused || (dx == 0 && dy == 0) {
            return;
        }
        match self.desk.apply_delta(self.cursor.clone(), dx, dy) {
            MoveOutcome::Stay { cursor } => {
                let parked = self.edge_hold.as_ref().and_then(|hold| {
                    self.desk.edge_point(&hold.from, &hold.cursor)
                });
                let still_there = parked.as_ref().is_some_and(|edge| {
                    edge.screen == cursor.screen && edge.x == cursor.x && edge.y == cursor.y
                });
                if !still_there {
                    self.edge_hold = None;
                }
                self.cursor = cursor;
                if self.is_remote() {
                    self.send_move();
                }
            }
            MoveOutcome::Switch { cursor, from } => self.hold_edge(from, cursor),
        }
    }

    fn hold_edge(&mut self, from: MachineId, cursor: Cursor) {
        let same = self
            .edge_hold
            .as_ref()
            .is_some_and(|hold| hold.from == from && hold.cursor.screen == cursor.screen);
        if same {
            if let Some(hold) = self.edge_hold.as_mut() {
                hold.cursor = cursor;
            }
        } else {
            self.edge_hold = Some(EdgeHold {
                since: Instant::now(),
                from: from.clone(),
                cursor,
                warped: false,
            });
        }
        let Some(dest) = self.edge_hold.as_ref().map(|hold| hold.cursor.clone()) else {
            return;
        };
        let Some(parked) = self.desk.edge_point(&from, &dest) else {
            return;
        };
        self.cursor = parked;
        let already = self.edge_hold.as_ref().is_some_and(|hold| hold.warped);
        if !already && from.0 == self.local_id {
            let (x, y) = self.local_of(&self.cursor.clone());
            self.command(PlatCmd::Warp { x, y });
            if let Some(hold) = self.edge_hold.as_mut() {
                hold.warped = true;
            }
        }
    }

    fn poll_edge(&mut self) {
        let ready = self
            .edge_hold
            .as_ref()
            .is_some_and(|hold| hold.since.elapsed() >= SWITCH_DELAY);
        if !ready {
            return;
        }
        let Some(hold) = self.edge_hold.take() else {
            return;
        };
        self.switch_to(hold.from, hold.cursor);
    }

    fn on_button(&mut self, button: u8, down: bool) {
        if self.paused {
            self.buttons.note_local(button, down);
            return;
        }
        if self.is_remote() {
            if self.buttons.note_remote(button, down) {
                self.send_focus(Message::Button { button, down });
            }
        } else {
            self.buttons.note_local(button, down);
        }
    }

    fn on_key(&mut self, usage: u16, down: bool) {
        if down {
            let held = self.keys.physical().collect();
            if let Some(dir) = jump_hotkey(&held, usage, true) {
                self.jump_direction(dir);
                return;
            }
        }
        if self.paused {
            self.keys.note_local(usage, down);
            return;
        }
        if self.is_remote() {
            if self.keys.note_remote(usage, down) {
                self.send_focus(Message::Key { usage, down });
            }
        } else {
            self.keys.note_local(usage, down);
        }
    }

    fn jump_direction(&mut self, dir: Direction) {
        if self.paused {
            return;
        }
        if let Some(cursor) = self.desk.jump_direction(&self.cursor, dir) {
            let from = self.cursor.screen.clone();
            self.switch_to(from, cursor);
        }
    }

    fn switch_to(&mut self, from: MachineId, cursor: Cursor) {
        self.edge_hold = None;
        if cursor.screen == from {
            self.cursor = cursor;
            return;
        }
        if cursor.screen.0 != self.local_id && self.link_id(&cursor.screen.0).is_none() {
            return;
        }
        let keys = self.keys.handoff();
        let (button_up, button_down) = self.buttons.handoff();
        self.release_on(&from, &keys.release, &button_up);
        self.cursor = cursor;
        if self.cursor.screen.0 == self.local_id {
            if from.0 != self.local_id {
                self.send_peer(&from.0, Message::Leave);
            }
            self.press_on(&MachineId::new(self.local_id.clone()), &keys.press, &button_down);
            self.command(PlatCmd::Swallow(false));
            self.command(PlatCmd::HideCursor(false));
            let (x, y) = self.local_of(&self.cursor.clone());
            self.command(PlatCmd::Warp { x, y });
        } else {
            if from.0 == self.local_id {
                self.command(PlatCmd::HideCursor(true));
                self.command(PlatCmd::Swallow(true));
            } else {
                self.send_peer(&from.0, Message::Leave);
            }
            let (x, y) = self.local_of(&self.cursor.clone());
            self.send_focus(Message::Enter { x, y });
            self.press_on(&self.cursor.screen.clone(), &keys.press, &button_down);
        }
        self.dirty = true;
    }

    fn release_on(&mut self, target: &MachineId, keys: &[u16], buttons: &[u8]) {
        if target.0 == self.local_id {
            for usage in keys {
                self.inject(DeviceEvent::Key { usage: *usage, down: false });
            }
            for button in buttons {
                self.inject(DeviceEvent::Button { button: *button, down: false });
            }
        } else {
            if !keys.is_empty() {
                self.send_peer(&target.0, Message::ReleaseKeys { usages: keys.to_vec() });
            }
            for button in buttons {
                self.send_peer(&target.0, Message::Button { button: *button, down: false });
            }
        }
    }

    fn press_on(&mut self, target: &MachineId, keys: &[u16], buttons: &[u8]) {
        if target.0 == self.local_id {
            for usage in keys {
                self.inject(DeviceEvent::Key { usage: *usage, down: true });
            }
            for button in buttons {
                self.inject(DeviceEvent::Button { button: *button, down: true });
            }
        } else {
            if !keys.is_empty() {
                self.send_peer(&target.0, Message::PressKeys { usages: keys.to_vec() });
            }
            for button in buttons {
                self.send_peer(&target.0, Message::Button { button: *button, down: true });
            }
        }
    }

    fn on_connected(&mut self, live: LiveLink) {
        let key = format!("{}:{}", live.addr.ip(), live.addr.port());
        self.dialing.remove(&key);
        self.dialing
            .retain(|item| !item.ends_with(&format!(":{}", live.addr.port())));
        let conn = live.id;
        self.links.insert(
            conn,
            Link {
                tx: live.tx.clone(),
                fingerprint: live.fingerprint,
                addr: live.addr,
                peer_id: None,
                peer_name: None,
                width: 0,
                height: 0,
                listen_port: live.addr.port(),
                last_rx: Instant::now(),
            },
        );
        let _ = live.tx.send(self.hello());
        if live.pairing_dial {
            if let Some(pair) = self.pairing_out.as_mut().filter(|pair| pair.conn.is_none()) {
                pair.conn = Some(conn);
                let _ = live.tx.send(Message::Pair {
                    code: pair.code.clone(),
                });
            }
        }
        self.dirty = true;
    }

    fn on_peer(&mut self, conn: u64, msg: Message) {
        if let Some(link) = self.links.get_mut(&conn) {
            link.last_rx = Instant::now();
        } else {
            return;
        }
        match msg {
            Message::Hello {
                id,
                name,
                width,
                height,
                fingerprint,
                port,
            } => self.on_hello(conn, id, name, width, height, fingerprint, port),
            Message::Pair { code } => {
                let name = self
                    .links
                    .get(&conn)
                    .and_then(|link| link.peer_name.clone())
                    .unwrap_or_else(|| "A nearby computer".into());
                if self.pinned_conn(conn) {
                    return;
                }
                self.pairing_in = Some(PairIn {
                    conn,
                    code,
                    name,
                    started: Instant::now(),
                });
                self.dirty = true;
            }
            Message::PairOk => self.finish_outgoing_pair(conn),
            Message::PairReject => {
                if self.pairing_out.as_ref().and_then(|pair| pair.conn) == Some(conn) {
                    self.pairing_out = None;
                    self.drop_link(conn);
                    self.dirty = true;
                }
            }
            Message::Enter { x, y } => self.on_enter(conn, x, y),
            Message::Leave => self.on_leave(conn),
            Message::Move { x, y } => {
                if self.controlled_by_conn(conn) {
                    self.inject(DeviceEvent::MouseAbsolute { x, y });
                }
            }
            Message::Button { button, down } => {
                if self.controlled_by_conn(conn) {
                    if down {
                        self.injected_buttons.insert(button);
                    } else {
                        self.injected_buttons.remove(&button);
                    }
                    self.inject(DeviceEvent::Button { button, down });
                }
            }
            Message::Wheel { dx, dy } => {
                if self.controlled_by_conn(conn) {
                    self.inject(DeviceEvent::Wheel { dx, dy });
                }
            }
            Message::Key { usage, down } => {
                if self.controlled_by_conn(conn) {
                    self.note_injected_key(usage, down);
                    self.inject(DeviceEvent::Key { usage, down });
                }
            }
            Message::ReleaseKeys { usages } => {
                if self.controlled_by_conn(conn) {
                    for usage in usages {
                        self.note_injected_key(usage, false);
                        self.inject(DeviceEvent::Key { usage, down: false });
                    }
                }
            }
            Message::PressKeys { usages } => {
                if self.controlled_by_conn(conn) {
                    for usage in usages {
                        self.note_injected_key(usage, true);
                        self.inject(DeviceEvent::Key { usage, down: true });
                    }
                }
            }
            Message::Layout { rev, screens } => self.apply_layout(rev, screens),
            Message::Identify => self.flash_local(),
            Message::Heartbeat => {}
        }
    }

    fn on_hello(
        &mut self,
        conn: u64,
        id: String,
        name: String,
        width: u32,
        height: u32,
        fingerprint: String,
        port: u16,
    ) {
        let cert_fp = self
            .links
            .get(&conn)
            .map(|link| link.fingerprint.clone())
            .unwrap_or_default();
        if !cert_fp.is_empty() && fingerprint != cert_fp {
            self.drop_link(conn);
            return;
        }
        if id == self.local_id {
            self.drop_link(conn);
            return;
        }
        let duplicate = self.links.iter().any(|(other, link)| {
            *other != conn && link.peer_id.as_deref() == Some(id.as_str())
        });
        if let Some(link) = self.links.get_mut(&conn) {
            link.peer_id = Some(id.clone());
            link.peer_name = Some(name.clone());
            link.width = width;
            link.height = height;
            if port != 0 {
                link.listen_port = port;
            }
        }
        self.desk.ensure(MachineId::new(id.clone()), name, width, height);
        if duplicate {
            self.drop_link(conn);
            return;
        }
        if self.pinned_fingerprint(&fingerprint) && self.pairing_out.as_ref().and_then(|p| p.conn) != Some(conn)
        {
            self.remember_peer(conn);
        }
        self.dirty = true;
    }

    fn finish_outgoing_pair(&mut self, conn: u64) {
        if self.pairing_out.as_ref().and_then(|pair| pair.conn) != Some(conn) {
            return;
        }
        self.remember_peer(conn);
        self.pairing_out = None;
        self.bump_layout();
        self.dirty = true;
    }

    fn remember_peer(&mut self, conn: u64) {
        let Some(link) = self.links.get(&conn) else { return };
        let Some(id) = link.peer_id.clone() else { return };
        let record = PeerRecord {
            id: id.clone(),
            name: link.peer_name.clone().unwrap_or_else(|| id.clone()),
            fingerprint: link.fingerprint.clone(),
            host: link.addr.ip().to_string(),
            port: link.listen_port,
        };
        self.pins.write().expect("pins").insert(record.fingerprint.clone());
        self.peers.insert(id, record);
        self.save();
    }

    fn on_enter(&mut self, conn: u64, x: i32, y: i32) {
        if !self.pinned_conn(conn) || self.is_remote() {
            return;
        }
        let Some(peer) = self.links.get(&conn).and_then(|link| link.peer_id.clone()) else {
            return;
        };
        self.release_injected();
        self.controlled_by = Some(peer);
        self.command(PlatCmd::Swallow(false));
        self.command(PlatCmd::HideCursor(false));
        self.command(PlatCmd::Warp { x, y });
        self.dirty = true;
    }

    fn on_leave(&mut self, conn: u64) {
        let peer = self.links.get(&conn).and_then(|link| link.peer_id.clone());
        if self.controlled_by == peer {
            self.release_injected();
            self.controlled_by = None;
            self.dirty = true;
        }
    }

    fn apply_layout(&mut self, rev: u64, screens: Vec<Screen>) {
        if rev <= self.layout_rev {
            return;
        }
        self.layout_rev = rev;
        for screen in screens {
            if screen.id.0 == self.local_id {
                if let Some(local) = self.desk.get_mut(&screen.id) {
                    local.x = screen.x;
                    local.y = screen.y;
                    local.placed = true;
                }
            } else if self.peers.contains_key(&screen.id.0) {
                self.desk.upsert(screen);
            }
        }
        self.save();
        self.dirty = true;
    }

    fn on_ipc(&mut self, request: IpcRequest) {
        match request {
            IpcRequest::SetPaused { paused } => {
                self.paused = paused;
                if paused {
                    self.edge_hold = None;
                }
                if paused && self.is_remote() {
                    let home = self.home_cursor();
                    let from = self.cursor.screen.clone();
                    self.switch_to(from, home);
                }
                if paused {
                    self.command(PlatCmd::Swallow(false));
                    self.command(PlatCmd::HideCursor(false));
                }
                self.dirty = true;
            }
            IpcRequest::SetName { name } => {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    self.name = name.clone();
                    if let Some(screen) = self.desk.get_mut(&MachineId::new(self.local_id.clone())) {
                        screen.name = name;
                    }
                    self.bump_layout();
                }
            }
            IpcRequest::BeginAdd { peer_id } => {
                if let Some(seen) = self.discovered.get(&peer_id) {
                    let host = seen.host.clone();
                    let port = seen.port;
                    let label = seen.name.clone();
                    self.begin_pair(host, port, label);
                } else {
                    self.fail("That computer is no longer on the network.");
                }
            }
            IpcRequest::BeginAddAddress { host, port } => {
                let label = format!("{host}:{port}");
                self.begin_pair(host, port, label);
            }
            IpcRequest::SubmitCode { code } => self.submit_code(code),
            IpcRequest::CancelPair => {
                if let Some(pair) = self.pairing_out.take() {
                    if let Some(conn) = pair.conn {
                        self.drop_link(conn);
                    }
                }
                if let Some(pair) = self.pairing_in.take() {
                    self.send_conn(pair.conn, Message::PairReject);
                    self.drop_link(pair.conn);
                }
                self.dirty = true;
            }
            IpcRequest::Place { id, x, y } => {
                let mid = MachineId::new(id);
                if self.desk.place(&mid, x, y).is_some() {
                    self.bump_layout();
                }
            }
            IpcRequest::Unplace { id } => {
                if id != self.local_id && self.desk.unplace(&MachineId::new(id)) {
                    self.bump_layout();
                }
            }
            IpcRequest::Identify { id } => {
                if id == self.local_id {
                    self.flash_local();
                } else {
                    self.send_peer(&id, Message::Identify);
                }
            }
            IpcRequest::Jump { target } => {
                if self.paused {
                    return;
                }
                if let Some(cursor) = self.desk.jump_to(&MachineId::new(target)) {
                    let from = self.cursor.screen.clone();
                    self.switch_to(from, cursor);
                }
            }
            IpcRequest::OpenPermissionSettings => self.command(PlatCmd::OpenSettings),
            IpcRequest::RecheckPermission => {
                if let Some(check) = self.recheck {
                    self.permission = check();
                    self.dirty = true;
                }
            }
            IpcRequest::SetInputShared { id, shared } => {
                if shared {
                    self.local_only.retain(|kept| kept != &id);
                } else if !self.local_only.iter().any(|kept| kept == &id) {
                    self.local_only.push(id);
                }
                for device in &mut self.devices {
                    device.shared = !self.local_only.iter().any(|kept| kept == &device.id);
                }
                self.save();
                self.apply_capture();
                self.dirty = true;
            }
            IpcRequest::Shutdown => self.stop = true,
        }
    }

    fn begin_pair(&mut self, host: String, port: u16, label: String) {
        let code = generate_pair_code();
        self.pairing_out = Some(PairOut {
            code,
            label,
            started: Instant::now(),
            conn: None,
        });
        self.dial(host, port, self.pairing_out.as_ref().map(|pair| pair.code.clone()));
        self.dirty = true;
    }

    fn submit_code(&mut self, code: String) {
        let Some(pair) = self.pairing_in.clone_snapshot() else {
            self.fail("There is no pairing request to answer.");
            return;
        };
        if !pair_codes_match(&pair.code, &code) {
            self.send_conn(pair.conn, Message::PairReject);
            self.pairing_in = None;
            self.drop_link(pair.conn);
            self.fail("That code does not match.");
            return;
        }
        self.send_conn(pair.conn, Message::PairOk);
        self.remember_peer(pair.conn);
        self.pairing_in = None;
        self.bump_layout();
    }

    fn dial(&mut self, host: String, port: u16, code: Option<String>) {
        let key = format!("{host}:{port}");
        if !self.dialing.insert(key.clone()) {
            return;
        }
        let events = self.events.clone();
        let identity = Arc::clone(&self.identity);
        let pins = Arc::clone(&self.pins);
        let require_pin = code.is_none();
        tokio::spawn(async move {
            match connect_out(&host, port, identity, pins, require_pin, events.clone()).await {
                Ok(()) => {}
                Err(err) => {
                    log::info!("dial {host}:{port} failed: {err}");
                    let _ = events.send(Hub::DialFailed { key });
                }
            }
        });
        let _ = code;
    }

    fn drop_link(&mut self, conn: u64) {
        let Some(link) = self.links.remove(&conn) else { return };
        if self.controlled_by.as_deref() == link.peer_id.as_deref() {
            self.release_injected();
            self.controlled_by = None;
        }
        if link.peer_id.as_deref() == Some(self.cursor.screen.0.as_str()) {
            let keys = self.keys.disconnect();
            let (_, button_down) = self.buttons.disconnect();
            self.cursor = self.home_cursor();
            self.press_on(&MachineId::new(self.local_id.clone()), &keys.press, &button_down);
            self.command(PlatCmd::Swallow(false));
            self.command(PlatCmd::HideCursor(false));
            let (x, y) = self.local_of(&self.cursor.clone());
            self.command(PlatCmd::Warp { x, y });
        }
        if self.pairing_out.as_ref().and_then(|pair| pair.conn) == Some(conn) {
            self.pairing_out = None;
        }
        if self.pairing_in.as_ref().map(|pair| pair.conn) == Some(conn) {
            self.pairing_in = None;
        }
        self.dirty = true;
    }

    fn bump_layout(&mut self) {
        self.layout_rev = self.layout_rev.saturating_add(1);
        let msg = Message::Layout {
            rev: self.layout_rev,
            screens: self.desk.screens().to_vec(),
        };
        let ids: Vec<u64> = self
            .links
            .iter()
            .filter(|(_, link)| link.peer_id.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.send_conn(id, msg.clone());
        }
        self.save();
        self.dirty = true;
    }

    fn flash_local(&self) {
        let label = self
            .desk
            .screens()
            .iter()
            .position(|screen| screen.id.0 == self.local_id)
            .map(|index| (index + 1).to_string())
            .unwrap_or_else(|| self.name.clone());
        if let Ok(line) = serde_json::to_string(&IpcEvent::Identify { label: label.clone() }) {
            self.bus.publish(line);
        }
        self.command(PlatCmd::Identify(label));
    }

    fn hello(&self) -> Message {
        let screen = self.desk.get(&MachineId::new(self.local_id.clone()));
        Message::Hello {
            id: self.local_id.clone(),
            name: self.name.clone(),
            width: screen.map(|s| s.width).unwrap_or(1),
            height: screen.map(|s| s.height).unwrap_or(1),
            fingerprint: self.fingerprint.clone(),
            port: self.session_port,
        }
    }

    fn send_move(&mut self) {
        let (x, y) = self.local_of(&self.cursor.clone());
        self.send_focus(Message::Move { x, y });
    }

    fn send_focus(&mut self, msg: Message) {
        let id = self.cursor.screen.0.clone();
        self.send_peer(&id, msg);
    }

    fn send_peer(&mut self, peer: &str, msg: Message) {
        if let Some(conn) = self.link_id(peer) {
            self.send_conn(conn, msg);
        }
    }

    fn send_conn(&mut self, conn: u64, msg: Message) {
        if let Some(link) = self.links.get(&conn) {
            let _ = link.tx.send(msg);
        }
    }

    fn link_id(&self, peer: &str) -> Option<u64> {
        self.links
            .iter()
            .find(|(_, link)| link.peer_id.as_deref() == Some(peer))
            .map(|(id, _)| *id)
    }

    fn is_remote(&self) -> bool {
        self.cursor.screen.0 != self.local_id
    }

    fn pinned_conn(&self, conn: u64) -> bool {
        self.links
            .get(&conn)
            .map(|link| self.pinned_fingerprint(&link.fingerprint))
            .unwrap_or(false)
    }

    fn pinned_fingerprint(&self, fingerprint: &str) -> bool {
        self.pins.read().expect("pins").contains(fingerprint)
    }

    fn controlled_by_conn(&self, conn: u64) -> bool {
        let peer = self.links.get(&conn).and_then(|link| link.peer_id.clone());
        self.controlled_by.is_some() && self.controlled_by == peer
    }

    fn note_injected_key(&mut self, usage: u16, down: bool) {
        if down {
            self.injected_keys.insert(usage);
        } else {
            self.injected_keys.remove(&usage);
        }
    }

    fn release_injected(&mut self) {
        let keys: Vec<u16> = self.injected_keys.iter().copied().collect();
        let buttons: Vec<u8> = self.injected_buttons.iter().copied().collect();
        for usage in keys {
            self.inject(DeviceEvent::Key { usage, down: false });
        }
        for button in buttons {
            self.inject(DeviceEvent::Button { button, down: false });
        }
        self.injected_keys.clear();
        self.injected_buttons.clear();
    }

    fn home_cursor(&self) -> Cursor {
        self.desk
            .jump_to(&MachineId::new(self.local_id.clone()))
            .unwrap_or_else(|| self.cursor.clone())
    }

    fn local_of(&self, cursor: &Cursor) -> (i32, i32) {
        self.desk
            .local_point(&cursor.screen, cursor.x, cursor.y)
            .unwrap_or((0, 0))
    }

    fn inject(&self, event: DeviceEvent) {
        self.command(PlatCmd::Inject(event));
    }

    fn command(&self, cmd: PlatCmd) {
        let _ = self.cmds.send(cmd);
    }

    fn fail(&self, message: &str) {
        if let Ok(line) = serde_json::to_string(&IpcEvent::Error {
            message: message.into(),
        }) {
            self.bus.publish(line);
        }
    }

    fn save(&self) {
        Disk {
            id: self.local_id.clone(),
            name: self.name.clone(),
            peers: self.peers.values().cloned().collect(),
            screens: self.desk.screens().to_vec(),
            layout_rev: self.layout_rev,
            local_only: self.local_only.clone(),
        }
        .save(&self.home.join("config.json"));
    }

    fn on_devices(&mut self, listed: Vec<multicontrol_platform::HwDevice>) {
        self.devices = listed
            .into_iter()
            .map(|device| InputDevice {
                shared: !self.local_only.iter().any(|kept| kept == &device.id),
                id: device.id,
                name: device.name,
                kind: match device.kind {
                    HwKind::Keyboard => InputKind::Keyboard,
                    HwKind::Pointer => InputKind::Pointer,
                },
            })
            .collect();
        self.apply_capture();
        self.dirty = true;
    }

    fn apply_capture(&mut self) {
        let share_all = self.local_only.is_empty() || self.devices.is_empty();
        let ids = if share_all {
            Vec::new()
        } else {
            self.devices
                .iter()
                .filter(|device| device.shared)
                .map(|device| device.id.clone())
                .collect()
        };
        let next = (share_all, ids);
        if self.capture_sent.as_ref() == Some(&next) {
            return;
        }
        self.command(PlatCmd::SetShared {
            share_all: next.0,
            ids: next.1.clone(),
        });
        self.capture_sent = Some(next);
    }

    fn snapshot(&self) -> StateSnapshot {
        let mut discovered = Vec::new();
        let mut seen_ids = HashSet::new();
        for seen in self.discovered.values() {
            seen_ids.insert(seen.id.clone());
            discovered.push(DiscoveredPeer {
                id: seen.id.clone(),
                name: seen.name.clone(),
                host: seen.host.clone(),
                port: seen.port,
                fingerprint: seen.fingerprint.clone(),
                paired: self.peers.contains_key(&seen.id),
                online: true,
            });
        }
        for peer in self.peers.values() {
            if seen_ids.contains(&peer.id) {
                continue;
            }
            discovered.push(DiscoveredPeer {
                id: peer.id.clone(),
                name: peer.name.clone(),
                host: peer.host.clone(),
                port: peer.port,
                fingerprint: peer.fingerprint.clone(),
                paired: true,
                online: self.link_id(&peer.id).is_some(),
            });
        }
        discovered.sort_by(|a, b| a.name.cmp(&b.name));
        let focused_id = self.cursor.screen.0.clone();
        let focused_name = self
            .desk
            .get(&self.cursor.screen)
            .map(|screen| screen.name.clone())
            .unwrap_or_else(|| focused_id.clone());
        StateSnapshot {
            local_id: self.local_id.clone(),
            local_name: self.name.clone(),
            paused: self.paused,
            permission: self.permission.clone(),
            focused_id,
            focused_name,
            controlling_remote: self.is_remote() && !self.paused,
            screens: self.desk.screens().to_vec(),
            discovered,
            paired: {
                let mut paired: Vec<_> = self.peers.values().cloned().collect();
                paired.sort_by(|a, b| a.name.cmp(&b.name));
                paired
            },
            pairing: self.pairing_view(),
            inputs: self.devices.clone(),
        }
    }

    fn pairing_view(&self) -> Option<PairingView> {
        if let Some(pair) = &self.pairing_out {
            return Some(PairingView::ShowingCode {
                peer_name: pair.label.clone(),
                code: pair.code.clone(),
            });
        }
        self.pairing_in.as_ref().map(|pair| PairingView::EnterCode {
            peer_name: pair.name.clone(),
        })
    }

    fn publish(&self) {
        if let Ok(line) = serde_json::to_string(&IpcEvent::State {
            snapshot: self.snapshot(),
        }) {
            self.bus.publish(line);
        }
    }
}

trait CloneSnapshot {
    fn clone_snapshot(&self) -> Option<PairIn>;
}

impl CloneSnapshot for Option<PairIn> {
    fn clone_snapshot(&self) -> Option<PairIn> {
        self.as_ref().map(|pair| PairIn {
            conn: pair.conn,
            code: pair.code.clone(),
            name: pair.name.clone(),
            started: pair.started,
        })
    }
}

async fn owner_loop(mut app: App, mut events: mpsc::UnboundedReceiver<Hub>, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(app.heartbeat.max(Duration::from_millis(50)) / 2);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut edge_clock = tokio::time::interval(Duration::from_millis(50));
    edge_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    app.publish();
    loop {
        if app.stop || *shutdown.borrow() {
            break;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => app.tick(),
            _ = edge_clock.tick() => app.poll_edge(),
            event = events.recv() => {
                let Some(event) = event else { break };
                app.handle(event);
            }
        }
        if app.dirty {
            app.publish();
            app.dirty = false;
        }
        if app.stop {
            break;
        }
    }
    let _ = app.cmds.send(PlatCmd::Shutdown);
}

fn spawn_capture(rx: std::sync::mpsc::Receiver<Captured>, tx: mpsc::UnboundedSender<Hub>) {
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            if tx.send(Hub::Captured(event)).is_err() {
                break;
            }
        }
    });
}

async fn listen_loop(
    listener: TcpListener,
    identity: Arc<Identity>,
    pins: Arc<RwLock<HashSet<String>>>,
    events: mpsc::UnboundedSender<Hub>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let Ok((stream, addr)) = accepted else { continue };
                let identity = Arc::clone(&identity);
                let pins = Arc::clone(&pins);
                let events = events.clone();
                tokio::spawn(async move {
                    if let Err(err) = accept_one(stream, addr, identity, pins, events.clone()).await {
                        log::info!("incoming connection closed: {err}");
                    }
                });
            }
        }
    }
}

async fn accept_one(
    stream: TcpStream,
    addr: SocketAddr,
    identity: Arc<Identity>,
    pins: Arc<RwLock<HashSet<String>>>,
    events: mpsc::UnboundedSender<Hub>,
) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let gate = Gate::new(pins, false);
    let seen = Arc::clone(&gate.seen);
    let config = identity::server_config(&identity, gate)?;
    let tls = TlsAcceptor::from(Arc::new(config)).accept(stream).await?;
    let fingerprint = seen.lock().expect("seen").clone().unwrap_or_default();
    finish_link(tls, addr, fingerprint, false, events).await
}

async fn connect_out(
    host: &str,
    port: u16,
    identity: Arc<Identity>,
    pins: Arc<RwLock<HashSet<String>>>,
    require_pin: bool,
    events: mpsc::UnboundedSender<Hub>,
) -> Result<()> {
    let stream = TcpStream::connect((host, port)).await?;
    let _ = stream.set_nodelay(true);
    let addr = stream.peer_addr()?;
    let gate = Gate::new(pins, require_pin);
    let seen = Arc::clone(&gate.seen);
    let config = identity::client_config(&identity, gate)?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from("multicontrol")?,
            stream,
        )
        .await?;
    let fingerprint = seen.lock().expect("seen").clone().unwrap_or_default();
    finish_link(tls, addr, fingerprint, !require_pin, events).await
}

async fn finish_link<S>(
    tls: S,
    addr: SocketAddr,
    fingerprint: String,
    pairing_dial: bool,
    events: mpsc::UnboundedSender<Hub>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let id = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let (read, write) = tokio::io::split(tls);
    tokio::spawn(write_loop(write, rx));
    let _ = events.send(Hub::Connected(LiveLink {
        id,
        tx,
        fingerprint,
        addr,
        pairing_dial,
    }));
    let events_read = events.clone();
    tokio::spawn(read_loop(read, events_read, id));
    Ok(())
}

async fn write_loop<W>(mut writer: W, mut rx: mpsc::UnboundedReceiver<Message>)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut coal = MoveCoalescer::new(FLUSH);
    let mut ticker = tokio::time::interval(FLUSH);
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                if let Message::Move { x, y } = msg {
                    if let Some((x, y)) = coal.push(x, y, Instant::now()) {
                        if write_msg(&mut writer, &Message::Move { x, y }).await.is_err() { break; }
                    }
                } else {
                    if let Some((x, y)) = coal.flush(Instant::now()) {
                        if write_msg(&mut writer, &Message::Move { x, y }).await.is_err() { break; }
                    }
                    if write_msg(&mut writer, &msg).await.is_err() { break; }
                }
            }
            _ = ticker.tick() => {
                if let Some((x, y)) = coal.poll(Instant::now()) {
                    if write_msg(&mut writer, &Message::Move { x, y }).await.is_err() { break; }
                }
            }
        }
    }
    let _ = writer.shutdown().await;
}

async fn read_loop<R>(read: R, events: mpsc::UnboundedSender<Hub>, conn: u64)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(read).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Ok(msg) = serde_json::from_str::<Message>(&line) {
                    if events.send(Hub::Peer { conn, msg }).is_err() {
                        break;
                    }
                }
            }
            _ => {
                let _ = events.send(Hub::Closed { conn });
                break;
            }
        }
    }
}

async fn write_msg<W>(writer: &mut W, msg: &Message) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

#[cfg(unix)]
async fn ipc_loop(
    path: PathBuf,
    events: mpsc::UnboundedSender<Hub>,
    mut shutdown: watch::Receiver<bool>,
) {
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            log::error!("ipc bind failed: {err}");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let events = events.clone();
                tokio::spawn(ipc_client(stream, events));
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
async fn ipc_client(stream: tokio::net::UnixStream, events: mpsc::UnboundedSender<Hub>) {
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let _ = events.send(Hub::Subscribe(tx));
    let mut lines = BufReader::new(read).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                let Ok(request) = serde_json::from_str::<IpcRequest>(&line) else { continue };
                let shutdown = matches!(request, IpcRequest::Shutdown);
                if events.send(Hub::Ipc(request)).is_err() { break; }
                if shutdown { break; }
            }
            msg = rx.recv() => {
                let Some(mut msg) = msg else { break };
                msg.push('\n');
                if write.write_all(msg.as_bytes()).await.is_err() { break; }
                if write.flush().await.is_err() { break; }
            }
        }
    }
}

#[cfg(windows)]
async fn ipc_loop(
    _path: PathBuf,
    events: mpsc::UnboundedSender<Hub>,
    mut shutdown: watch::Receiver<bool>,
) {
    let listener = match TcpListener::bind(("127.0.0.1", multicontrol_core::IPC_TCP_PORT)).await {
        Ok(listener) => listener,
        Err(err) => {
            log::error!("ipc bind failed: {err}");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let events = events.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                    let _ = events.send(Hub::Subscribe(tx));
                    let mut lines = BufReader::new(read).lines();
                    loop {
                        tokio::select! {
                            line = lines.next_line() => {
                                let Ok(Some(line)) = line else { break };
                                let Ok(request) = serde_json::from_str::<IpcRequest>(&line) else { continue };
                                if events.send(Hub::Ipc(request)).is_err() { break; }
                            }
                            msg = rx.recv() => {
                                let Some(mut msg) = msg else { break };
                                msg.push('\n');
                                if write.write_all(msg.as_bytes()).await.is_err() { break; }
                            }
                        }
                    }
                });
            }
        }
    }
}

fn new_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn computer_name() -> String {
    if let Ok(name) = std::env::var("MULTICONTROL_NAME") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    for cmd in [
        ("scutil", vec!["--get", "ComputerName"]),
        ("hostname", vec![]),
    ] {
        if let Ok(out) = std::process::Command::new(cmd.0).args(&cmd.1).output() {
            if let Ok(text) = String::from_utf8(out.stdout) {
                let text = text.trim();
                if !text.is_empty() && out.status.success() {
                    return text.to_string();
                }
            }
        }
    }
    "Computer".into()
}
