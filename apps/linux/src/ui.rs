//! GTK 4 panel. GNOME and KDE draw this with the system toolkit.

use gtk::cairo::Context;
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk::{Align, Application, ApplicationWindow, Box, Button, DrawingArea, Label, Orientation};
use multicontrol_core::{IpcEvent, IpcRequest, PairingView, PermissionState, Screen, StateSnapshot};
use multicontrol_shell::Client;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub fn main() {
    let state = Arc::new(Mutex::new(empty_state()));
    let arranged = Rc::new(Cell::new(false));
    let client = Client::spawn({
        let state = Arc::clone(&state);
        move |event| {
            if let IpcEvent::State { snapshot } = event {
                *state.lock().expect("state") = snapshot;
            }
        }
    });
    let app = Application::builder()
        .application_id("com.multicontrol.Panel")
        .build();
    let client = Rc::new(client);
    app.connect_activate(move |app| build(app, client.clone(), state.clone(), arranged.clone()));
    app.run();
}

fn build(app: &Application, client: Rc<Client>, state: Arc<Mutex<StateSnapshot>>, arranged: Rc<Cell<bool>>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Multi Control")
        .default_width(340)
        .default_height(520)
        .build();
    let root = Box::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let name = Label::new(Some("Multi Control"));
    name.set_halign(Align::Start);
    let status = Label::new(Some("Connecting…"));
    status.set_halign(Align::Start);
    let pause = Button::with_label("Pause");
    let add = Button::with_label("Add");
    let arrange = Button::with_label("Arrange");
    let actions = Box::new(Orientation::Horizontal, 8);
    actions.append(&pause);
    actions.append(&add);
    actions.append(&arrange);

    let list = Box::new(Orientation::Vertical, 6);
    let canvas = DrawingArea::builder().content_width(680).content_height(300).build();
    let shelf = Box::new(Orientation::Horizontal, 6);
    let pairing = Label::new(None);
    pairing.set_wrap(true);
    let hint = Label::new(Some("Ctrl+Alt and an arrow jumps along the desk."));
    hint.set_halign(Align::Start);

    root.append(&name);
    root.append(&status);
    root.append(&actions);
    root.append(&pairing);
    root.append(&list);
    root.append(&canvas);
    root.append(&shelf);
    root.append(&hint);
    window.set_child(Some(&root));

    let redraw = {
        let state = Arc::clone(&state);
        let arranged = arranged.clone();
        let client_pause = client.clone();
        let client_add = client.clone();
        let list = list.clone();
        let shelf = shelf.clone();
        let canvas = canvas.clone();
        let window = window.clone();
        move || {
            let snapshot = state.lock().expect("state").clone();
            name.set_label(&snapshot.local_name);
            status.set_label(&if snapshot.paused {
                "Paused on this computer".into()
            } else {
                format!("Controlling {}", snapshot.focused_name)
            });
            pause.set_label(if snapshot.paused { "Resume" } else { "Pause" });
            arrange.set_label(if arranged.get() { "Done" } else { "Arrange" });
            pairing.set_label(&match &snapshot.pairing {
                Some(PairingView::ShowingCode { peer_name, code }) => {
                    format!("On {peer_name}, enter {code}")
                }
                Some(PairingView::EnterCode { peer_name }) => format!("{peer_name} wants to connect"),
                None => permission_note(&snapshot.permission).unwrap_or_default(),
            });
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            while let Some(child) = shelf.first_child() {
                shelf.remove(&child);
            }
            canvas.set_visible(arranged.get());
            shelf.set_visible(arranged.get());
            list.set_visible(!arranged.get());
            if arranged.get() {
                window.set_default_size(720, 640);
                for screen in snapshot.screens.iter().filter(|screen| !screen.placed) {
                    let button = Button::with_label(&screen.name);
                    let client = client_pause.clone();
                    let id = screen.id.0.clone();
                    let right = snapshot
                        .screens
                        .iter()
                        .filter(|other| other.placed)
                        .map(|other| other.x + other.width as i32)
                        .max()
                        .unwrap_or(0);
                    button.connect_clicked(move |_| {
                        client.send(IpcRequest::Place { id: id.clone(), x: right + 40, y: 0 });
                    });
                    shelf.append(&button);
                }
            } else {
                window.set_default_size(340, 520);
                let local = snapshot.screens.iter().find(|screen| screen.id.0 == snapshot.local_id);
                for peer in snapshot.discovered.iter().filter(|peer| peer.paired) {
                    let relation = snapshot
                        .screens
                        .iter()
                        .find(|screen| screen.id.0 == peer.id)
                        .and_then(|screen| local.map(|anchor| side(anchor, screen)))
                        .unwrap_or("Not placed");
                    let button = Button::with_label(&format!("{}   {relation}", peer.name));
                    let client = client_add.clone();
                    let id = peer.id.clone();
                    button.connect_clicked(move |_| {
                        client.send(IpcRequest::Jump { target: id.clone() });
                    });
                    list.append(&button);
                }
            }
            canvas.queue_draw();
        }
    };

    canvas.set_draw_func({
        let state = Arc::clone(&state);
        move |_, cr, width, height| draw_desk(cr, width, height, &state.lock().expect("state"))
    });

    pause.connect_clicked({
        let state = Arc::clone(&state);
        let client = client.clone();
        move |_| {
            let paused = !state.lock().expect("state").paused;
            client.send(IpcRequest::SetPaused { paused });
        }
    });
    add.connect_clicked({
        let state = Arc::clone(&state);
        let client = client.clone();
        move |_| {
            let snapshot = state.lock().expect("state");
            if let Some(peer) = snapshot.discovered.iter().find(|peer| !peer.paired) {
                client.send(IpcRequest::BeginAdd { peer_id: peer.id.clone() });
            }
        }
    });
    arrange.connect_clicked({
        let arranged = arranged.clone();
        let redraw = redraw.clone();
        move |_| {
            arranged.set(!arranged.get());
            redraw();
        }
    });

    // The daemon pushes state on its own thread. Polling keeps the GTK loop in charge.
    glib::timeout_add_local(std::time::Duration::from_millis(200), {
        let redraw = redraw.clone();
        move || {
            redraw();
            ControlFlow::Continue
        }
    });
    redraw();
    window.present();
}

fn draw_desk(cr: &Context, width: i32, height: i32, state: &StateSnapshot) {
    cr.set_source_rgb(0.95, 0.93, 0.89);
    cr.paint().ok();
    let placed: Vec<&Screen> = state.screens.iter().filter(|screen| screen.placed).collect();
    if placed.is_empty() {
        return;
    }
    let min_x = placed.iter().map(|screen| screen.x).min().unwrap_or(0);
    let min_y = placed.iter().map(|screen| screen.y).min().unwrap_or(0);
    let max_x = placed.iter().map(|screen| screen.x + screen.width as i32).max().unwrap_or(1);
    let max_y = placed.iter().map(|screen| screen.y + screen.height as i32).max().unwrap_or(1);
    let pad = 16.0;
    let scale = ((width as f64 - pad * 2.0) / (max_x - min_x).max(1) as f64)
        .min((height as f64 - pad * 2.0) / (max_y - min_y).max(1) as f64);
    for (index, screen) in placed.iter().enumerate() {
        let x = pad + (screen.x - min_x) as f64 * scale;
        let y = pad + (screen.y - min_y) as f64 * scale;
        let w = (screen.width as f64 * scale).max(72.0);
        let h = (screen.height as f64 * scale).max(48.0);
        let color = [(0.97, 0.84, 0.71), (0.81, 0.91, 0.85), (0.85, 0.87, 0.97)][index % 3];
        cr.set_source_rgb(color.0, color.1, color.2);
        cr.rectangle(x, y, w, h);
        cr.fill().ok();
        cr.set_source_rgb(0.17, 0.14, 0.12);
        cr.move_to(x + 8.0, y + 24.0);
        cr.show_text(&screen.name).ok();
    }
}

fn empty_state() -> StateSnapshot {
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

fn permission_note(permission: &PermissionState) -> Option<String> {
    match permission {
        PermissionState::Granted | PermissionState::Unknown => None,
        PermissionState::Denied => Some("Grant the portal dialog, then come back.".into()),
        PermissionState::Unsupported { reason } => Some(reason.clone()),
    }
}

fn side(anchor: &Screen, other: &Screen) -> &'static str {
    if !other.placed {
        return "Not placed";
    }
    let dx = other.x + other.width as i32 / 2 - (anchor.x + anchor.width as i32 / 2);
    let dy = other.y + other.height as i32 / 2 - (anchor.y + anchor.height as i32 / 2);
    if dx.abs() >= dy.abs() {
        if dx >= 0 { "Right" } else { "Left" }
    } else if dy >= 0 {
        "Below"
    } else {
        "Above"
    }
}
