//! Win32 tray window. GDI draws the desk; the daemon still owns the keyboard.

use multicontrol_core::{IpcEvent, IpcRequest, PairingView, PermissionState, Screen, StateSnapshot};
use multicontrol_shell::Client;
use std::sync::{Mutex, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    HBRUSH, HGDIOBJ, PAINTSTRUCT, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, LoadCursorW,
    PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MSG, SW_SHOW, WINDOW_EX_STYLE, WM_DESTROY,
    WM_LBUTTONUP, WM_PAINT, WNDCLASSW, WS_CAPTION, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

static CLIENT: OnceLock<Client> = OnceLock::new();
static STATE: OnceLock<Mutex<StateSnapshot>> = OnceLock::new();
static ARRANGED: Mutex<bool> = Mutex::new(false);
static HITS: Mutex<Vec<Hit>> = Mutex::new(Vec::new());

fn state() -> &'static Mutex<StateSnapshot> {
    STATE.get_or_init(|| Mutex::new(blank_state()))
}

struct Hit {
    rect: RECT,
    action: Action,
}

enum Action {
    Pause,
    Add,
    Arrange,
    Settings,
    Jump(String),
    Place(String),
    Submit,
    Cancel,
}

fn blank_state() -> StateSnapshot {
    StateSnapshot {
        local_id: String::new(),
        local_name: "Multi Control".into(),
        paused: false,
        permission: PermissionState::Unknown,
        focused_id: String::new(),
        focused_name: "this computer".into(),
        controlling_remote: false,
        screens: Vec::new(),
        discovered: Vec::new(),
        paired: Vec::new(),
        pairing: None,
    }
}

pub fn main() {
    let _ = CLIENT.set(Client::spawn(|event| {
        if let IpcEvent::State { snapshot } = event {
            *state().lock().expect("state") = snapshot;
        }
    }));
    unsafe {
        let instance = GetModuleHandleW(None).expect("module");
        let class = w!("MultiControlPanel");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance.into(),
            hIcon: Default::default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class,
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Multi Control"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            380,
            560,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .expect("window");
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            click(hwnd, x, y);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let state = state().lock().expect("state").clone();
    let arranged = *ARRANGED.lock().expect("arranged");
    let mut hits = Vec::new();
    let mut y = 16;
    text(dc, 16, y, client.right - 32, 24, &state.local_name);
    y += 28;
    let status = if state.paused {
        "Paused on this computer".into()
    } else {
        format!("Controlling {}", state.focused_name)
    };
    text(dc, 16, y, client.right - 32, 22, &status);
    y += 30;
    hits.push(button(dc, 16, y, 100, 28, if state.paused { "Resume" } else { "Pause" }, Action::Pause));
    hits.push(button(dc, 124, y, 80, 28, "Add", Action::Add));
    hits.push(button(
        dc,
        212,
        y,
        100,
        28,
        if arranged { "Done" } else { "Arrange" },
        Action::Arrange,
    ));
    y += 40;
    if let Some(note) = permission_note(&state.permission) {
        text(dc, 16, y, client.right - 32, 36, &note);
        y += 40;
        hits.push(button(dc, 16, y, 140, 28, "Open Settings", Action::Settings));
        y += 36;
    }
    match &state.pairing {
        Some(PairingView::ShowingCode { peer_name, code }) => {
            text(dc, 16, y, client.right - 32, 20, &format!("On {peer_name}, enter"));
            y += 22;
            text(dc, 16, y, client.right - 32, 32, code);
            y += 36;
            hits.push(button(dc, 16, y, 90, 28, "Cancel", Action::Cancel));
            y += 36;
        }
        Some(PairingView::EnterCode { peer_name }) => {
            text(dc, 16, y, client.right - 32, 20, &format!("{peer_name} wants to connect"));
            y += 24;
            hits.push(button(dc, 16, y, 110, 28, "Use last code", Action::Submit));
            y += 36;
        }
        None => {}
    }
    if arranged {
        let canvas = RECT { left: 16, top: y, right: client.right - 16, bottom: y + 280 };
        draw_desk(dc, canvas, &state);
        y = canvas.bottom + 8;
        for screen in state.screens.iter().filter(|screen| !screen.placed) {
            hits.push(button(dc, 16, y, 160, 28, &screen.name, Action::Place(screen.id.0.clone())));
            y += 34;
        }
    } else {
        let local = state.screens.iter().find(|screen| screen.id.0 == state.local_id);
        for peer in state.discovered.iter().filter(|peer| peer.paired) {
            let relation = state
                .screens
                .iter()
                .find(|screen| screen.id.0 == peer.id)
                .and_then(|screen| local.map(|anchor| side(anchor, screen)))
                .unwrap_or("Not placed");
            hits.push(button(
                dc,
                16,
                y,
                client.right - 32,
                32,
                &format!("{}   {relation}", peer.name),
                Action::Jump(peer.id.clone()),
            ));
            y += 38;
        }
    }
    text(dc, 16, client.bottom - 28, client.right - 32, 20, "Ctrl+Alt and an arrow jumps.");
    *HITS.lock().expect("hits") = hits;
    let _ = EndPaint(hwnd, &ps);
    let _ = (CREATESTRUCTW::default(), COLORREF::default(), SelectObject, HGDIOBJ::default());
}

unsafe fn button(dc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, w: i32, h: i32, label: &str, action: Action) -> Hit {
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let brush = CreateSolidBrush(COLORREF(0x00d8e6ef));
    FillRect(dc, &rect, brush);
    let _ = DeleteObject(brush.into());
    text(dc, x, y, w, h, label);
    Hit { rect, action }
}

unsafe fn text(dc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, w: i32, h: i32, value: &str) {
    let mut wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x001f242a));
    let _ = DrawTextW(dc, &mut wide, &mut rect, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
}

unsafe fn draw_desk(dc: windows::Win32::Graphics::Gdi::HDC, canvas: RECT, state: &StateSnapshot) {
    let brush = CreateSolidBrush(COLORREF(0x00e1ebf3));
    FillRect(dc, &canvas, brush);
    let _ = DeleteObject(brush.into());
    let placed: Vec<&Screen> = state.screens.iter().filter(|screen| screen.placed).collect();
    if placed.is_empty() {
        return;
    }
    let min_x = placed.iter().map(|screen| screen.x).min().unwrap_or(0);
    let min_y = placed.iter().map(|screen| screen.y).min().unwrap_or(0);
    let max_x = placed.iter().map(|screen| screen.x + screen.width as i32).max().unwrap_or(1);
    let max_y = placed.iter().map(|screen| screen.y + screen.height as i32).max().unwrap_or(1);
    let pad = 12;
    let scale = ((canvas.right - canvas.left - pad * 2) as f64 / (max_x - min_x).max(1) as f64)
        .min((canvas.bottom - canvas.top - pad * 2) as f64 / (max_y - min_y).max(1) as f64);
    for screen in placed {
        let left = canvas.left + pad + ((screen.x - min_x) as f64 * scale) as i32;
        let top = canvas.top + pad + ((screen.y - min_y) as f64 * scale) as i32;
        let rect = RECT {
            left,
            top,
            right: left + (screen.width as f64 * scale).max(64.0) as i32,
            bottom: top + (screen.height as f64 * scale).max(40.0) as i32,
        };
        let brush = CreateSolidBrush(COLORREF(if screen.id.0 == state.local_id { 0x006a8f2f } else { 0x00b5d7f7 }));
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush.into());
        text(dc, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, &screen.name);
    }
}

fn click(hwnd: HWND, x: i32, y: i32) {
    let hits = HITS.lock().expect("hits").clone_hits();
    let Some(hit) = hits.into_iter().find(|hit| contains(&hit.rect, x, y)) else { return };
    let client = CLIENT.get().expect("client");
    match hit.action {
        Action::Pause => {
            let paused = !state().lock().expect("state").paused;
            client.send(IpcRequest::SetPaused { paused });
        }
        Action::Add => {
            let state = state().lock().expect("state").clone();
            if let Some(peer) = state.discovered.iter().find(|peer| !peer.paired) {
                client.send(IpcRequest::BeginAdd { peer_id: peer.id.clone() });
            }
        }
        Action::Arrange => {
            let mut arranged = ARRANGED.lock().expect("arranged");
            *arranged = !*arranged;
        }
        Action::Settings => client.send(IpcRequest::OpenPermissionSettings),
        Action::Jump(id) => client.send(IpcRequest::Jump { target: id }),
        Action::Place(id) => {
            let state = state().lock().expect("state");
            let right = state.screens.iter().filter(|s| s.placed).map(|s| s.x + s.width as i32).max().unwrap_or(0);
            drop(state);
            client.send(IpcRequest::Place { id, x: right + 40, y: 0 });
        }
        Action::Submit | Action::Cancel => client.send(IpcRequest::CancelPair),
    }
    unsafe { let _ = InvalidateRect(Some(hwnd), None, true); }
}

fn contains(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

trait CloneHits {
    fn clone_hits(&self) -> Vec<Hit>;
}

impl CloneHits for Vec<Hit> {
    fn clone_hits(&self) -> Vec<Hit> {
        self.iter().map(|hit| Hit {
            rect: hit.rect,
            action: match &hit.action {
                Action::Pause => Action::Pause,
                Action::Add => Action::Add,
                Action::Arrange => Action::Arrange,
                Action::Settings => Action::Settings,
                Action::Jump(id) => Action::Jump(id.clone()),
                Action::Place(id) => Action::Place(id.clone()),
                Action::Submit => Action::Submit,
                Action::Cancel => Action::Cancel,
            },
        }).collect()
    }
}

fn permission_note(permission: &PermissionState) -> Option<String> {
    match permission {
        PermissionState::Granted | PermissionState::Unknown => None,
        PermissionState::Denied => Some("An elevated window ignores input unless Multi Control is elevated too.".into()),
        PermissionState::Unsupported { reason } => Some(reason.clone()),
    }
}

fn side(anchor: &Screen, other: &Screen) -> &'static str {
    if !other.placed { return "Not placed"; }
    let dx = other.x + other.width as i32 / 2 - (anchor.x + anchor.width as i32 / 2);
    let dy = other.y + other.height as i32 / 2 - (anchor.y + anchor.height as i32 / 2);
    if dx.abs() >= dy.abs() { if dx >= 0 { "Right" } else { "Left" } } else if dy >= 0 { "Below" } else { "Above" }
}

// Silence unused imports that the paint path references by type only on some SDKs.
#[allow(dead_code)]
fn _types(point: POINT) -> POINT { point }
