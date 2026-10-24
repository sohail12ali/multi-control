//! The arrange canvas. Coordinates inside the view grow downward (`isFlipped`).

use multicontrol_core::{IpcRequest, Screen, StateSnapshot};
use multicontrol_shell::Client;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSStringDrawing, NSTextAlignment, NSView,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;

pub struct DeskIvars {
    state: RefCell<StateSnapshot>,
    client: Client,
    drag: RefCell<Option<Drag>>,
}

struct Drag {
    id: String,
    origin_x: i32,
    origin_y: i32,
    grab_x: f64,
    grab_y: f64,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DeskIvars]
    #[name = "MCDeskView"]
    pub struct DeskView;

    impl DeskView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let background = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, 12.0, 12.0);
            NSColor::colorWithRed_green_blue_alpha(0.93, 0.94, 0.95, 1.0).setFill();
            background.fill();
            let state = self.ivars().state.borrow();
            let placed: Vec<&Screen> = state.screens.iter().filter(|screen| screen.placed).collect();
            if placed.is_empty() {
                return;
            }
            let (scale, ox, oy) = fit(&placed, bounds);
            let style: objc2::rc::Retained<NSMutableParagraphStyle> =
                unsafe { msg_send![NSMutableParagraphStyle::alloc(), init] };
            style.setAlignment(NSTextAlignment::Center);
            let font = NSFont::boldSystemFontOfSize(12.0);
            let ink = NSColor::colorWithRed_green_blue_alpha(0.16, 0.18, 0.22, 1.0);
            for (index, screen) in placed.iter().enumerate() {
                let rect = screen_rect(screen, scale, ox, oy);
                let color = COLORS[index % COLORS.len()];
                let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 8.0, 8.0);
                NSColor::colorWithRed_green_blue_alpha(color.0, color.1, color.2, 1.0).setFill();
                path.fill();
                if screen.id.0 == state.local_id {
                    NSColor::colorWithRed_green_blue_alpha(0.22, 0.45, 0.86, 1.0).setStroke();
                    path.setLineWidth(2.5);
                    path.stroke();
                }
                let title = if screen.id.0 == state.local_id {
                    format!("{}\nThis Mac", screen.name)
                } else {
                    screen.name.clone()
                };
                let text = NSString::from_str(&title);
                let text_rect = NSRect::new(
                    NSPoint::new(rect.origin.x + 8.0, rect.origin.y + rect.size.height / 2.0 - 16.0),
                    NSSize::new((rect.size.width - 16.0).max(8.0), 32.0),
                );
                let attrs = unsafe {
                    NSDictionary::from_slices(
                        &[NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName],
                        &[any(&*font), any(&*ink), any(&*style)],
                    )
                };
                unsafe {
                    text.drawInRect_withAttributes(text_rect, Some(&attrs));
                }
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let state = self.ivars().state.borrow();
            let placed: Vec<Screen> = state.screens.iter().filter(|screen| screen.placed).cloned().collect();
            drop(state);
            let bounds = self.bounds();
            let refs: Vec<&Screen> = placed.iter().collect();
            let (scale, ox, oy) = fit(&refs, bounds);
            for screen in &placed {
                let rect = screen_rect(screen, scale, ox, oy);
                if point.x >= rect.origin.x
                    && point.y >= rect.origin.y
                    && point.x < rect.origin.x + rect.size.width
                    && point.y < rect.origin.y + rect.size.height
                {
                    self.ivars().drag.replace(Some(Drag {
                        id: screen.id.0.clone(),
                        origin_x: screen.x,
                        origin_y: screen.y,
                        grab_x: point.x,
                        grab_y: point.y,
                    }));
                    break;
                }
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(drag) = self.ivars().drag.borrow().as_ref().map(|drag| Drag {
                id: drag.id.clone(),
                origin_x: drag.origin_x,
                origin_y: drag.origin_y,
                grab_x: drag.grab_x,
                grab_y: drag.grab_y,
            }) else {
                return;
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let state = self.ivars().state.borrow();
            let placed: Vec<Screen> = state.screens.iter().filter(|screen| screen.placed).cloned().collect();
            drop(state);
            let refs: Vec<&Screen> = placed.iter().collect();
            let (scale, _, _) = fit(&refs, self.bounds());
            if scale <= 0.0 {
                return;
            }
            let dx = ((point.x - drag.grab_x) / scale).round() as i32;
            let dy = ((point.y - drag.grab_y) / scale).round() as i32;
            self.ivars().state.borrow_mut().screens.iter_mut().for_each(|screen| {
                if screen.id.0 == drag.id {
                    screen.x = drag.origin_x + dx;
                    screen.y = drag.origin_y + dy;
                }
            });
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let Some(drag) = self.ivars().drag.borrow_mut().take() else { return };
            let state = self.ivars().state.borrow();
            if let Some(screen) = state.screens.iter().find(|screen| screen.id.0 == drag.id) {
                self.ivars().client.send(IpcRequest::Place {
                    id: drag.id,
                    x: screen.x,
                    y: screen.y,
                });
            }
        }
    }
);

const COLORS: [(f64, f64, f64); 5] = [
    (0.86, 0.92, 1.0),
    (0.86, 0.95, 0.89),
    (1.0, 0.93, 0.84),
    (0.93, 0.89, 0.98),
    (0.98, 0.89, 0.90),
];

impl DeskView {
    pub fn create(mtm: MainThreadMarker, client: Client) -> objc2::rc::Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DeskIvars {
            state: RefCell::new(empty()),
            client,
            drag: RefCell::new(None),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(680.0, 320.0));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_state(&self, state: StateSnapshot) {
        if self.ivars().drag.borrow().is_some() {
            return;
        }
        *self.ivars().state.borrow_mut() = state;
        self.setNeedsDisplay(true);
    }
}

fn empty() -> StateSnapshot {
    StateSnapshot {
        local_id: String::new(),
        local_name: String::new(),
        paused: false,
        permission: multicontrol_core::PermissionState::Unknown,
        focused_id: String::new(),
        focused_name: String::new(),
        controlling_remote: false,
        screens: Vec::new(),
        discovered: Vec::new(),
        paired: Vec::new(),
        pairing: None,
        inputs: Vec::new(),
    }
}

fn fit(screens: &[&Screen], bounds: NSRect) -> (f64, f64, f64) {
    let min_x = screens.iter().map(|screen| screen.x).min().unwrap_or(0);
    let min_y = screens.iter().map(|screen| screen.y).min().unwrap_or(0);
    let max_x = screens.iter().map(|screen| screen.x + screen.width as i32).max().unwrap_or(1);
    let max_y = screens.iter().map(|screen| screen.y + screen.height as i32).max().unwrap_or(1);
    let pad = 16.0;
    let scale = ((bounds.size.width - pad * 2.0) / (max_x - min_x).max(1) as f64)
        .min((bounds.size.height - pad * 2.0) / (max_y - min_y).max(1) as f64);
    (scale, pad - min_x as f64 * scale, pad - min_y as f64 * scale)
}

fn screen_rect(screen: &Screen, scale: f64, ox: f64, oy: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(ox + screen.x as f64 * scale, oy + screen.y as f64 * scale),
        NSSize::new((screen.width as f64 * scale).max(96.0), (screen.height as f64 * scale).max(64.0)),
    )
}

fn any(value: &impl objc2::Message) -> &AnyObject {
    unsafe { &*(value as *const _ as *const AnyObject) }
}
