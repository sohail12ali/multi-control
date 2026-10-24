//! AppKit menu-bar panel. It does not see keystrokes; the daemon does.

mod desk;

use desk::DeskView;
use multicontrol_core::{IpcEvent, IpcRequest, PairingView, PermissionState, StateSnapshot};
use dispatch2::DispatchQueue;
use multicontrol_shell::Client;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezelStyle, NSButton,
    NSButtonType, NSCellImagePosition, NSColor, NSControl, NSFocusRingType, NSFont, NSImage, NSLayoutAttribute, NSLayoutConstraint,
    NSLayoutConstraintOrientation, NSLayoutRelation, NSPanel, NSScreen, NSStackView,
    NSStackViewDistribution, NSStatusBar, NSStatusItem, NSTextAlignment, NSTextField,
    NSUserInterfaceLayoutOrientation, NSVariableStatusItemLength, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSArray, NSEdgeInsets, NSInteger, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex, OnceLock};

const COLLAPSED_WIDTH: f64 = 320.0;
const EXPANDED_WIDTH: f64 = 680.0;

fn main() {
    let mtm = MainThreadMarker::new().expect("the panel starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let slot: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
    let pending = Arc::new(Mutex::new(Vec::new()));
    let events = slot.clone();
    let queued = pending.clone();
    let client = Client::spawn(move |event| {
        let mut waiting = queued.lock().expect("event queue");
        if let Some(&ptr) = events.get() {
            drop(waiting);
            deliver(ptr, event);
        } else {
            waiting.push(event);
        }
    });
    let controller = Controller::new(mtm, client);
    let ptr = objc2::rc::Retained::as_ptr(&controller) as usize;
    let early = {
        let mut waiting = pending.lock().expect("event queue");
        let _ = slot.set(ptr);
        std::mem::take(&mut *waiting)
    };
    for event in early {
        deliver(ptr, event);
    }

    let bar = NSStatusBar::systemStatusBar();
    let item = bar.statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            ns_string!("rectangle.2.swap"),
            Some(ns_string!("Multi Control")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
            button.setImagePosition(NSCellImagePosition::ImageOnly);
        } else {
            button.setTitle(ns_string!("MC"));
        }
        unsafe {
            button.setTarget(Some(&*(ptr as *const AnyObject)));
            button.setAction(Some(sel!(toggle:)));
        }
    }
    controller.ivars().status.replace(Some(item));
    controller.rebuild();
    controller.reveal();

    app.run();
}

fn deliver(controller: usize, event: IpcEvent) {
    DispatchQueue::main().exec_async(move || {
        let controller = controller as *mut Controller;
        if let Some(controller) = unsafe { controller.as_ref() } {
            controller.on_event(event);
        }
    });
}

struct ControllerIvars {
    client: Client,
    mtm: MainThreadMarker,
    panel: RefCell<Option<objc2::rc::Retained<NSPanel>>>,
    status: RefCell<Option<objc2::rc::Retained<NSStatusItem>>>,
    state: RefCell<StateSnapshot>,
    arranged: Cell<bool>,
    view_set: Cell<bool>,
    adding: Cell<bool>,
    host_field: RefCell<Option<objc2::rc::Retained<NSTextField>>>,
    port_field: RefCell<Option<objc2::rc::Retained<NSTextField>>>,
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
            self.ivars().view_set.set(true);
            self.rebuild();
        }

        #[unsafe(method(showAdd:))]
        fn show_add(&self, _sender: Option<&AnyObject>) {
            self.ivars().adding.set(!self.ivars().adding.get());
            self.rebuild();
        }

        #[unsafe(method(pairFound:))]
        fn pair_found(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let button: &NSButton = unsafe { &*(sender as *const AnyObject as *const NSButton) };
            let index = button.tag() as usize;
            let state = self.ivars().state.borrow();
            let peers: Vec<_> = state.discovered.iter().filter(|peer| !peer.paired).collect();
            if let Some(peer) = peers.get(index) {
                let id = peer.id.clone();
                drop(state);
                self.ivars().adding.set(false);
                self.ivars().client.send(IpcRequest::BeginAdd { peer_id: id });
            }
        }

        #[unsafe(method(connectAddress:))]
        fn connect_address(&self, _sender: Option<&AnyObject>) {
            let host = self
                .ivars()
                .host_field
                .borrow()
                .as_ref()
                .map(|field| field.stringValue().to_string())
                .unwrap_or_default();
            let port_text = self
                .ivars()
                .port_field
                .borrow()
                .as_ref()
                .map(|field| field.stringValue().to_string())
                .unwrap_or_default();
            let host = host.trim().to_string();
            if host.is_empty() {
                return;
            }
            let port = port_text.trim().parse().unwrap_or(multicontrol_core::DEFAULT_SESSION_PORT);
            self.ivars().adding.set(false);
            self.ivars().client.send(IpcRequest::BeginAddAddress { host, port });
        }

        #[unsafe(method(toggleInput:))]
        fn toggle_input(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let button: &NSButton = unsafe { &*(sender as *const AnyObject as *const NSButton) };
            let index = button.tag() as usize;
            let shared = button.state() != 0;
            let id = self
                .ivars()
                .state
                .borrow()
                .inputs
                .get(index)
                .map(|device| device.id.clone());
            if let Some(id) = id {
                self.ivars().client.send(IpcRequest::SetInputShared { id, shared });
            }
        }

        #[unsafe(method(add:))]
        fn add(&self, _sender: Option<&AnyObject>) {
            self.ivars().adding.set(true);
            self.rebuild();
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
    fn new(mtm: MainThreadMarker, client: Client) -> objc2::rc::Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ControllerIvars {
            client,
            mtm,
            panel: RefCell::new(None),
            status: RefCell::new(None),
            state: RefCell::new(empty_state()),
            arranged: Cell::new(false),
            view_set: Cell::new(false),
            adding: Cell::new(false),
            host_field: RefCell::new(None),
            port_field: RefCell::new(None),
            desk: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn on_event(&self, event: IpcEvent) {
        match event {
            IpcEvent::State { snapshot } => {
                if !self.ivars().view_set.get() && snapshot.discovered.iter().any(|peer| peer.paired) {
                    self.ivars().arranged.set(true);
                }
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
            self.reveal();
        }
    }

    fn reveal(&self) {
        let panel = self.panel();
        self.place_under_status(&panel);
        panel.orderFrontRegardless();
        self.ivars().client.send(IpcRequest::RecheckPermission);
    }

    fn panel(&self) -> objc2::rc::Retained<NSPanel> {
        if let Some(panel) = self.ivars().panel.borrow().clone() {
            return panel;
        }
        let mtm = self.ivars().mtm;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(COLLAPSED_WIDTH, 240.0));
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
        panel.setTitlebarAppearsTransparent(true);
        panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
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

    fn rebuild(&self) {
        let mtm = self.ivars().mtm;
        let state = self.ivars().state.borrow().clone();
        let width = if self.ivars().arranged.get() { EXPANDED_WIDTH } else { COLLAPSED_WIDTH };
        let mut rows: Vec<objc2::rc::Retained<NSView>> = Vec::new();

        let name = NSTextField::textFieldWithString(&ns(&state.local_name), mtm);
        name.setEditable(true);
        name.setBezeled(false);
        name.setDrawsBackground(false);
        name.setFocusRingType(NSFocusRingType::None);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
        name.setTextColor(Some(&NSColor::labelColor()));
        unsafe {
            name.setTarget(Some(self.as_object()));
            name.setAction(Some(sel!(rename:)));
        }
        rows.push(as_view(name.into_super()));

        let status = if state.paused {
            "Paused. This keyboard stays on this computer.".into()
        } else if state.controlling_remote {
            format!("Controlling {}", state.focused_name)
        } else {
            format!("On {}", state.focused_name)
        };
        rows.push(as_view(caption(&status, width - 32.0, mtm).into_super()));

        if let Some(note) = permission_note(&state.permission) {
            rows.push(as_view(caption(&note, width - 32.0, mtm).into_super()));
            let settings = button("Open Settings", mtm);
            settings.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            unsafe {
                settings.setTarget(Some(self.as_object()));
                settings.setAction(Some(sel!(settings:)));
            }
            rows.push(hstack(vec![as_view(settings.into_super())], mtm).into_super());
        }

        match &state.pairing {
            Some(PairingView::ShowingCode { peer_name, code }) => {
                rows.push(as_view(
                    caption(&format!("On {peer_name}, enter this code"), width - 32.0, mtm).into_super(),
                ));
                let code_label = label(code, mtm);
                code_label.setAlignment(NSTextAlignment::Center);
                code_label.setFont(Some(&NSFont::boldSystemFontOfSize(32.0)));
                rows.push(as_view(code_label.into_super()));
                let cancel = button("Cancel", mtm);
                cancel.setBezelStyle(NSBezelStyle::AccessoryBarAction);
                unsafe {
                    cancel.setTarget(Some(self.as_object()));
                    cancel.setAction(Some(sel!(cancelPair:)));
                }
                rows.push(hstack(vec![as_view(cancel.into_super())], mtm).into_super());
            }
            Some(PairingView::EnterCode { peer_name }) => {
                rows.push(as_view(
                    caption(&format!("{peer_name} wants to connect"), width - 32.0, mtm).into_super(),
                ));
                let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
                field.setPlaceholderString(Some(ns_string!("Pairing code")));
                unsafe {
                    field.setTarget(Some(self.as_object()));
                    field.setAction(Some(sel!(submitCode:)));
                }
                rows.push(as_view(field.into_super()));
            }
            None => {}
        }

        if self.ivars().arranged.get() && !self.ivars().adding.get() {
            let desk = self.desk_view(mtm);
            desk.set_state(state.clone());
            pin_constant(any(&*desk), NSLayoutAttribute::Height, 300.0);
            rows.push(desk.into_super());
            let unplaced: Vec<_> = state.screens.iter().filter(|screen| !screen.placed).collect();
            if !unplaced.is_empty() {
                rows.push(as_view(caption("Not on the desk yet", width - 32.0, mtm).into_super()));
                let mut chips = Vec::new();
                for screen in unplaced {
                    let chip = button(&screen.name, mtm);
                    chip.setBezelStyle(NSBezelStyle::AccessoryBarAction);
                    unsafe {
                        chip.setTarget(Some(self.as_object()));
                        chip.setAction(Some(sel!(placeShelf:)));
                    }
                    chips.push(as_view(chip.into_super()));
                }
                rows.push(hstack(chips, mtm).into_super());
            }
        } else if self.ivars().adding.get() {
            rows.push(as_view(caption("Add a computer", width - 32.0, mtm).into_super()));
            let unpaired: Vec<_> = state.discovered.iter().filter(|peer| !peer.paired).collect();
            for (index, peer) in unpaired.iter().enumerate() {
                let pair = button(&format!("Pair {}", peer.name), mtm);
                pair.setBezelStyle(NSBezelStyle::AccessoryBarAction);
                unsafe {
                    pair.setTarget(Some(self.as_object()));
                    pair.setAction(Some(sel!(pairFound:)));
                    pair.setTag(index as NSInteger);
                }
                rows.push(as_view(pair.into_super()));
            }
            let host = NSTextField::textFieldWithString(ns_string!(""), mtm);
            host.setPlaceholderString(Some(ns_string!("Host name or address")));
            self.ivars().host_field.replace(Some(host.clone()));
            rows.push(as_view(host.into_super()));
            let port = NSTextField::textFieldWithString(&ns("47320"), mtm);
            port.setPlaceholderString(Some(ns_string!("Port")));
            self.ivars().port_field.replace(Some(port.clone()));
            rows.push(as_view(port.into_super()));
            let connect = button("Connect", mtm);
            let cancel = button("Cancel", mtm);
            unsafe {
                connect.setTarget(Some(self.as_object()));
                connect.setAction(Some(sel!(connectAddress:)));
                cancel.setTarget(Some(self.as_object()));
                cancel.setAction(Some(sel!(showAdd:)));
            }
            rows.push(hstack(vec![as_view(connect.into_super()), as_view(cancel.into_super())], mtm).into_super());
        } else {
            let local = state.screens.iter().find(|screen| screen.id.0 == state.local_id);
            rows.push(as_view(caption("Computers", width - 32.0, mtm).into_super()));
            if state.discovered.is_empty() {
                rows.push(as_view(
                    caption("No other computers yet. Add one with a host and port.", width - 32.0, mtm)
                        .into_super(),
                ));
            }
            let paired: Vec<_> = state.discovered.iter().filter(|peer| peer.paired).collect();
            for peer in &state.discovered {
                let relation = state
                    .screens
                    .iter()
                    .find(|screen| screen.id.0 == peer.id)
                    .and_then(|screen| local.map(|anchor| side(anchor, screen)))
                    .unwrap_or("Not placed");
                let presence = if peer.online { "Online" } else { "Offline" };
                let title = label(&peer.name, mtm);
                title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
                let detail = caption(
                    &format!("{presence} · {relation} · {}", short_fingerprint(&peer.fingerprint)),
                    180.0,
                    mtm,
                );
                let action = if peer.paired {
                    let jump = button("Jump", mtm);
                    jump.setBezelStyle(NSBezelStyle::AccessoryBarAction);
                    let index = paired.iter().position(|item| item.id == peer.id).unwrap_or(0);
                    unsafe {
                        jump.setTarget(Some(self.as_object()));
                        jump.setAction(Some(sel!(jump:)));
                        jump.setTag(index as NSInteger);
                    }
                    jump
                } else {
                    let pair = button("Pair", mtm);
                    pair.setBezelStyle(NSBezelStyle::AccessoryBarAction);
                    let index = state
                        .discovered
                        .iter()
                        .filter(|item| !item.paired)
                        .position(|item| item.id == peer.id)
                        .unwrap_or(0);
                    unsafe {
                        pair.setTarget(Some(self.as_object()));
                        pair.setAction(Some(sel!(pairFound:)));
                        pair.setTag(index as NSInteger);
                    }
                    pair
                };
                rows.push(
                    hstack(
                        vec![
                            as_view(title.into_super()),
                            spacer(mtm),
                            as_view(detail.into_super()),
                            as_view(action.into_super()),
                        ],
                        mtm,
                    )
                    .into_super(),
                );
            }
        }

        rows.push(as_view(caption("Inputs", width - 32.0, mtm).into_super()));
        if state.inputs.is_empty() {
            rows.push(as_view(
                caption("Every keyboard and mouse on this computer is shared.", width - 32.0, mtm).into_super(),
            ));
        }
        for (index, device) in state.inputs.iter().enumerate() {
            let toggle = button(&device.name, mtm);
            toggle.setButtonType(NSButtonType::Switch);
            toggle.setState(if device.shared { 1 } else { 0 });
            unsafe {
                toggle.setTarget(Some(self.as_object()));
                toggle.setAction(Some(sel!(toggleInput:)));
                toggle.setTag(index as NSInteger);
            }
            rows.push(as_view(toggle.into_super()));
        }

        let pause = button(if state.paused { "Resume" } else { "Pause" }, mtm);
        let add = button("Add", mtm);
        let arrange = button(if self.ivars().arranged.get() { "Done" } else { "Arrange" }, mtm);
        for action in [&pause, &add, &arrange] {
            action.setBezelStyle(NSBezelStyle::Push);
        }
        unsafe {
            pause.setTarget(Some(self.as_object()));
            pause.setAction(Some(sel!(pause:)));
            add.setTarget(Some(self.as_object()));
            add.setAction(Some(sel!(add:)));
            arrange.setTarget(Some(self.as_object()));
            arrange.setAction(Some(sel!(arrange:)));
        }
        let tools = hstack(
            vec![
                as_view(pause.into_super()),
                as_view(add.into_super()),
                as_view(arrange.into_super()),
            ],
            mtm,
        );
        tools.setDistribution(NSStackViewDistribution::FillEqually);
        rows.push(tools.into_super());
        rows.push(as_view(
            caption("Ctrl+Alt and an arrow jumps along the desk.", width - 32.0, mtm).into_super(),
        ));

        let refs: Vec<&NSView> = rows.iter().map(|view| view.as_ref()).collect();
        let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(NSLayoutAttribute::Width);
        stack.setDistribution(NSStackViewDistribution::EqualSpacing);
        stack.setSpacing(10.0);
        stack.setEdgeInsets(NSEdgeInsets { top: 14.0, left: 16.0, bottom: 14.0, right: 16.0 });
        pin_constant(any(&*stack), NSLayoutAttribute::Width, width);

        let panel = self.panel();
        panel.setContentView(Some(&stack));
        stack.layoutSubtreeIfNeeded();
        let height = stack.fittingSize().height.clamp(200.0, 760.0);
        panel.setContentSize(NSSize::new(width, height));
        if panel.isVisible() {
            self.place_under_status(&panel);
        }
    }

    fn desk_view(&self, mtm: MainThreadMarker) -> objc2::rc::Retained<DeskView> {
        if let Some(desk) = self.ivars().desk.borrow().clone() {
            return desk;
        }
        let desk = DeskView::create(mtm, self.ivars().client.clone());
        self.ivars().desk.replace(Some(desk.clone()));
        desk
    }

    fn flash(&self, text: &str) {
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
        let field = label(text, mtm);
        field.setFont(Some(&NSFont::boldSystemFontOfSize(180.0)));
        field.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        window.setContentView(Some(&field));
        window.orderFrontRegardless();
        let raw = objc2::rc::Retained::into_raw(window) as usize;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            DispatchQueue::main().exec_async(move || {
                if let Some(window) =
                    unsafe { objc2::rc::Retained::<NSWindow>::from_raw(raw as *mut NSWindow) }
                {
                    window.orderOut(None);
                }
            });
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
        inputs: Vec::new(),
    }
}

fn permission_note(permission: &PermissionState) -> Option<String> {
    match permission {
        PermissionState::Granted | PermissionState::Unknown => None,
        PermissionState::Denied => {
            Some("Accessibility is off. Allow Multi Control, then come back.".into())
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

fn short_fingerprint(fingerprint: &str) -> String {
    let compact: String = fingerprint.chars().filter(|ch| *ch != ':').take(8).collect();
    if compact.is_empty() {
        "unknown".into()
    } else {
        compact
    }
}

fn ns(text: &str) -> objc2::rc::Retained<NSString> {
    NSString::from_str(text)
}

fn label(text: &str, mtm: MainThreadMarker) -> objc2::rc::Retained<NSTextField> {
    NSTextField::labelWithString(&ns(text), mtm)
}

fn caption(text: &str, width: f64, mtm: MainThreadMarker) -> objc2::rc::Retained<NSTextField> {
    let field = label(text, mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field.setPreferredMaxLayoutWidth(width);
    field
}

fn as_view(view: objc2::rc::Retained<NSControl>) -> objc2::rc::Retained<NSView> {
    view.into_super()
}

fn button(title: &str, mtm: MainThreadMarker) -> objc2::rc::Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(&ns(title), None, None, mtm) }
}

fn hstack(
    views: Vec<objc2::rc::Retained<NSView>>,
    mtm: MainThreadMarker,
) -> objc2::rc::Retained<NSStackView> {
    let refs: Vec<&NSView> = views.iter().map(|view| view.as_ref()).collect();
    let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack.setSpacing(8.0);
    stack
}

fn spacer(mtm: MainThreadMarker) -> objc2::rc::Retained<NSView> {
    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(4.0, 4.0)),
    );
    view.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    view.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    view
}

fn pin_constant(view: &AnyObject, attribute: NSLayoutAttribute, constant: f64) {
    let constraint = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            view,
            attribute,
            NSLayoutRelation::Equal,
            None,
            NSLayoutAttribute::NotAnAttribute,
            1.0,
            constant,
        )
    };
    constraint.setActive(true);
}

fn any<T>(value: &T) -> &AnyObject {
    unsafe { &*(value as *const T as *const AnyObject) }
}
