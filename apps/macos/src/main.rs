//! AppKit menu-bar panel. It does not see keystrokes; the daemon does.

mod desk;

use desk::DeskView;
use multicontrol_core::{IpcEvent, IpcRequest, PairingView, PermissionState, StateSnapshot};
use multicontrol_shell::Client;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezelStyle, NSButton,
    NSColor, NSControl, NSFont, NSPanel, NSScreen, NSStackView, NSStatusBar, NSStatusItem, NSTextField,
    NSUserInterfaceLayoutOrientation, NSVariableStatusItemLength, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSArray, NSInteger, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;

const COLLAPSED: (f64, f64) = (340.0, 500.0);
const EXPANDED: (f64, f64) = (720.0, 620.0);

fn main() {
    let mtm = MainThreadMarker::new().expect("the panel starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(mtm);
    let client = controller.ivars().client.clone();
    let ptr = objc2::rc::Retained::as_ptr(&controller) as usize;
    let _bridge = Client::spawn(move |event| deliver(ptr as *mut Controller, event));
    let _keep = client;

    let bar = NSStatusBar::systemStatusBar();
    let item = bar.statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(ns_string!("MC"));
        unsafe {
            button.setTarget(Some(unsafe { &*(ptr as *const AnyObject) }));
            button.setAction(Some(sel!(toggle:)));
        }
    }
    controller.ivars().status.replace(Some(item));
    controller.rebuild();

    app.run();
}

fn deliver(controller: *mut Controller, event: IpcEvent) {
    let boxed = Box::new(Delivery { controller, event });
    unsafe {
        dispatch_async_f(
            dispatch_get_main_queue(),
            Box::into_raw(boxed).cast(),
            Some(deliver_main),
        );
    }
}

struct Delivery {
    controller: *mut Controller,
    event: IpcEvent,
}

unsafe extern "C" fn close_flash(context: *mut c_void) {
    let raw = unsafe { Box::from_raw(context.cast::<usize>()) };
    if let Some(window) = unsafe { objc2::rc::Retained::<NSWindow>::from_raw(*raw as *mut NSWindow) } {
        window.orderOut(None);
    }
}

unsafe extern "C" fn deliver_main(context: *mut c_void) {
    let delivery = unsafe { Box::from_raw(context.cast::<Delivery>()) };
    if let Some(controller) = unsafe { delivery.controller.as_ref() } {
        controller.on_event(delivery.event);
    }
}

#[link(name = "System")]
unsafe extern "C" {
    fn dispatch_get_main_queue() -> *mut c_void;
    fn dispatch_async_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: Option<unsafe extern "C" fn(*mut c_void)>,
    );
}

struct ControllerIvars {
    client: Client,
    mtm: MainThreadMarker,
    panel: RefCell<Option<objc2::rc::Retained<NSPanel>>>,
    status: RefCell<Option<objc2::rc::Retained<NSStatusItem>>>,
    state: RefCell<StateSnapshot>,
    arranged: Cell<bool>,
    desk: RefCell<Option<objc2::rc::Retained<DeskView>>>,
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    #[name = "MCController"]
    struct Controller;

    impl Controller {
        #[unsafe(method(toggle:))]
        fn toggle(&self, _sender: Option<&AnyObject>) {
            self.toggle_panel();
        }

        #[unsafe(method(pause:))]
        fn pause(&self, _sender: Option<&AnyObject>) {
            let paused = !self.ivars().state.borrow().paused;
            self.ivars().client.send(IpcRequest::SetPaused { paused });
        }

        #[unsafe(method(arrange:))]
        fn arrange(&self, _sender: Option<&AnyObject>) {
            let next = !self.ivars().arranged.get();
            self.ivars().arranged.set(next);
            self.resize();
            self.rebuild();
        }

        #[unsafe(method(add:))]
        fn add(&self, _sender: Option<&AnyObject>) {
            let state = self.ivars().state.borrow().clone();
            if let Some(peer) = state.discovered.iter().find(|peer| !peer.paired) {
                self.ivars().client.send(IpcRequest::BeginAdd {
                    peer_id: peer.id.clone(),
                });
            }
        }

        #[unsafe(method(cancelPair:))]
        fn cancel_pair(&self, _sender: Option<&AnyObject>) {
            self.ivars().client.send(IpcRequest::CancelPair);
        }

        #[unsafe(method(submitCode:))]
        fn submit_code(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let field: &NSTextField = unsafe { &*(sender as *const AnyObject as *const NSTextField) };
            let code = field.stringValue().to_string();
            self.ivars().client.send(IpcRequest::SubmitCode { code });
        }

        #[unsafe(method(rename:))]
        fn rename(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let field: &NSTextField = unsafe { &*(sender as *const AnyObject as *const NSTextField) };
            let name = field.stringValue().to_string();
            self.ivars().client.send(IpcRequest::SetName { name });
        }

        #[unsafe(method(jump:))]
        fn jump(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let button: &NSButton = unsafe { &*(sender as *const AnyObject as *const NSButton) };
            let index = button.tag() as usize;
            let state = self.ivars().state.borrow();
            let peers: Vec<_> = state.discovered.iter().filter(|peer| peer.paired).collect();
            if let Some(peer) = peers.get(index) {
                self.ivars().client.send(IpcRequest::Jump {
                    target: peer.id.clone(),
                });
            }
        }

        #[unsafe(method(settings:))]
        fn settings(&self, _sender: Option<&AnyObject>) {
            self.ivars().client.send(IpcRequest::OpenPermissionSettings);
        }

        #[unsafe(method(placeShelf:))]
        fn place_shelf(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let button: &NSButton = unsafe { &*(sender as *const AnyObject as *const NSButton) };
            let name = button.title().to_string();
            let state = self.ivars().state.borrow();
            let Some(screen) = state.screens.iter().find(|screen| screen.name == name) else {
                return;
            };
            let right = state
                .screens
                .iter()
                .filter(|other| other.placed)
                .map(|other| other.x + other.width as i32)
                .max()
                .unwrap_or(0);
            self.ivars().client.send(IpcRequest::Place {
                id: screen.id.0.clone(),
                x: right + 40,
                y: 0,
            });
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> objc2::rc::Retained<Self> {
        let client = Client::spawn(|_| {});
        let this = Self::alloc(mtm).set_ivars(ControllerIvars {
            client,
            mtm,
            panel: RefCell::new(None),
            status: RefCell::new(None),
            state: RefCell::new(empty_state()),
            arranged: Cell::new(false),
            desk: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn on_event(&self, event: IpcEvent) {
        match event {
            IpcEvent::State { snapshot } => {
                *self.ivars().state.borrow_mut() = snapshot.clone();
                if let Some(desk) = self.ivars().desk.borrow().as_ref() {
                    desk.set_state(snapshot);
                }
                self.rebuild();
            }
            IpcEvent::Identify { label } => self.flash(&label),
            IpcEvent::Error { message } => {
                self.ivars().state.borrow_mut().focused_name = message;
                self.rebuild();
            }
        }
    }

    fn toggle_panel(&self) {
        let panel = self.panel();
        if panel.isVisible() {
            panel.orderOut(None);
        } else {
            self.place_under_status(&panel);
            panel.orderFrontRegardless();
        }
    }

    fn panel(&self) -> objc2::rc::Retained<NSPanel> {
        if let Some(panel) = self.ivars().panel.borrow().clone() {
            return panel;
        }
        let mtm = self.ivars().mtm;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(COLLAPSED.0, COLLAPSED.1));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::UtilityWindow
                | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(ns_string!("Multi Control"));
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setLevel(objc2_app_kit::NSStatusWindowLevel);
        self.ivars().panel.replace(Some(panel.clone()));
        panel
    }

    fn place_under_status(&self, panel: &NSPanel) {
        let Some(status) = self.ivars().status.borrow().clone() else { return };
        let Some(button) = status.button(self.ivars().mtm) else { return };
        let Some(window) = button.window() else { return };
        let anchor = window.frame();
        let size = panel.frame().size;
        panel.setFrameOrigin(NSPoint::new(
            anchor.origin.x + anchor.size.width / 2.0 - size.width / 2.0,
            anchor.origin.y - size.height - 6.0,
        ));
    }

    fn resize(&self) {
        let panel = self.panel();
        let (w, h) = if self.ivars().arranged.get() { EXPANDED } else { COLLAPSED };
        let origin = panel.frame().origin;
        panel.setContentSize(NSSize::new(w, h));
        panel.setFrameOrigin(origin);
    }

    fn rebuild(&self) {
        let panel = self.panel();
        let mtm = self.ivars().mtm;
        let state = self.ivars().state.borrow().clone();
        let mut rows: Vec<objc2::rc::Retained<NSView>> = Vec::new();

        let name = NSTextField::textFieldWithString(&ns(&state.local_name), mtm);
        name.setEditable(true);
        unsafe {
            name.setTarget(Some(self.as_object()));
            name.setAction(Some(sel!(rename:)));
        }
        rows.push(as_view(name.into_super()));

        let status = if state.paused {
            format!("Paused on this computer")
        } else {
            format!("Controlling {}", state.focused_name)
        };
        rows.push(as_view(label(&status, mtm).into_super()));

        let pause = button(if state.paused { "Resume" } else { "Pause" }, mtm);
        unsafe {
            pause.setTarget(Some(self.as_object()));
            pause.setAction(Some(sel!(pause:)));
        }
        rows.push(as_view(pause.into_super()));

        if let Some(note) = permission_note(&state.permission) {
            rows.push(as_view(label(&note, mtm).into_super()));
            let settings = button("Open Settings", mtm);
            unsafe {
                settings.setTarget(Some(self.as_object()));
                settings.setAction(Some(sel!(settings:)));
            }
            rows.push(as_view(settings.into_super()));
        }

        match &state.pairing {
            Some(PairingView::ShowingCode { peer_name, code }) => {
                rows.push(as_view(label(&format!("On {peer_name}, enter"), mtm).into_super()));
                let code_label = label(code, mtm);
                code_label.setFont(Some(&NSFont::boldSystemFontOfSize(28.0)));
                rows.push(as_view(code_label.into_super()));
                let cancel = button("Cancel", mtm);
                unsafe {
                    cancel.setTarget(Some(self.as_object()));
                    cancel.setAction(Some(sel!(cancelPair:)));
                }
                rows.push(as_view(cancel.into_super()));
            }
            Some(PairingView::EnterCode { peer_name }) => {
                rows.push(as_view(label(&format!("{peer_name} wants to connect"), mtm).into_super()));
                let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
                unsafe {
                    field.setTarget(Some(self.as_object()));
                    field.setAction(Some(sel!(submitCode:)));
                }
                rows.push(as_view(field.into_super()));
            }
            None => {}
        }

        if self.ivars().arranged.get() {
            let desk = self.desk_view(mtm);
            desk.set_state(state.clone());
            desk.setFrameSize(NSSize::new(if self.ivars().arranged.get() { 680.0 } else { 300.0 }, 320.0));
            rows.push(desk.into_super());
            for screen in state.screens.iter().filter(|screen| !screen.placed) {
                let chip = button(&screen.name, mtm);
                unsafe {
                    chip.setTarget(Some(self.as_object()));
                    chip.setAction(Some(sel!(placeShelf:)));
                    chip.setBezelStyle(NSBezelStyle::Recessed);
                }
                rows.push(as_view(chip.into_super()));
            }
        } else {
            let local = state.screens.iter().find(|screen| screen.id.0 == state.local_id);
            for (index, peer) in state.discovered.iter().filter(|peer| peer.paired).enumerate() {
                let relation = state
                    .screens
                    .iter()
                    .find(|screen| screen.id.0 == peer.id)
                    .and_then(|screen| local.map(|anchor| side(anchor, screen)))
                    .unwrap_or("Not placed");
                let row = button(&format!("{}   {relation}", peer.name), mtm);
                unsafe {
                    row.setTarget(Some(self.as_object()));
                    row.setAction(Some(sel!(jump:)));
                    row.setBezelStyle(NSBezelStyle::Recessed);
                    row.setTag(index as NSInteger);
                }
                rows.push(as_view(row.into_super()));
            }
        }

        let add = button("Add", mtm);
        let arrange = button(if self.ivars().arranged.get() { "Done" } else { "Arrange" }, mtm);
        unsafe {
            add.setTarget(Some(self.as_object()));
            add.setAction(Some(sel!(add:)));
            arrange.setTarget(Some(self.as_object()));
            arrange.setAction(Some(sel!(arrange:)));
        }
        rows.push(as_view(add.into_super()));
        rows.push(as_view(arrange.into_super()));
        rows.push(as_view(label("Ctrl+Alt and an arrow jumps along the desk.", mtm).into_super()));

        let refs: Vec<&NSView> = rows.iter().map(|view| view.as_ref()).collect();
        let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        panel.setContentView(Some(&stack));
    }

    fn desk_view(&self, mtm: MainThreadMarker) -> objc2::rc::Retained<DeskView> {
        if let Some(desk) = self.ivars().desk.borrow().clone() {
            return desk;
        }
        let desk = DeskView::create(mtm, self.ivars().client.clone());
        self.ivars().desk.replace(Some(desk.clone()));
        desk
    }

    fn flash(&self, label: &str) {
        let mtm = self.ivars().mtm;
        let screen = NSScreen::mainScreen(mtm);
        let frame = screen.map(|screen| screen.frame()).unwrap_or(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(800.0, 600.0),
        ));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setLevel(objc2_app_kit::NSStatusWindowLevel);
        window.setBackgroundColor(Some(&NSColor::colorWithRed_green_blue_alpha(0.18, 0.56, 0.42, 0.92)));
        let text = label_text(label, mtm);
        text.setFont(Some(&NSFont::boldSystemFontOfSize(180.0)));
        text.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        window.setContentView(Some(&text));
        window.orderFrontRegardless();
        let raw = objc2::rc::Retained::into_raw(window) as usize;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let boxed = Box::new(raw);
            unsafe {
                dispatch_async_f(dispatch_get_main_queue(), Box::into_raw(boxed).cast(), Some(close_flash));
            }
        });
    }

    fn as_object(&self) -> &AnyObject {
        unsafe { &*(self as *const Self as *const AnyObject) }
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
        PermissionState::Denied => {
            Some("Allow Accessibility for multicontrol-daemon, then come back.".into())
        }
        PermissionState::Unsupported { reason } => Some(reason.clone()),
    }
}

fn side(anchor: &multicontrol_core::Screen, other: &multicontrol_core::Screen) -> &'static str {
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

fn ns(text: &str) -> objc2::rc::Retained<NSString> {
    NSString::from_str(text)
}

fn label(text: &str, mtm: MainThreadMarker) -> objc2::rc::Retained<NSTextField> {
    NSTextField::labelWithString(&ns(text), mtm)
}

fn label_text(text: &str, mtm: MainThreadMarker) -> objc2::rc::Retained<NSTextField> {
    label(text, mtm)
}

fn as_view(view: objc2::rc::Retained<NSControl>) -> objc2::rc::Retained<NSView> {
    view.into_super()
}

fn button(title: &str, mtm: MainThreadMarker) -> objc2::rc::Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(&ns(title), None, None, mtm) }
}
