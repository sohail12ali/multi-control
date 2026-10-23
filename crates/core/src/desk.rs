//! Virtual desk: machines placed like Windows displays, cursor crossing only
//! where two edges actually touch.

use serde::{Deserialize, Serialize};

pub const SNAP_PX: i32 = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineId(pub String);

impl MachineId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A computer on the shared desk. Coordinates are global pixels.
/// Rectangles are half-open: `[x, x + width) × [y, y + height)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub id: MachineId,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub placed: bool,
}

impl Screen {
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && y >= self.y
            && x < self.right()
            && y < self.bottom()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub screen: MachineId,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    Stay { cursor: Cursor },
    Switch { cursor: Cursor, from: MachineId },
}

#[derive(Clone, Debug, Default)]
pub struct Desk {
    screens: Vec<Screen>,
}

impl Desk {
    pub fn new() -> Self {
        Self { screens: Vec::new() }
    }

    pub fn screens(&self) -> &[Screen] {
        &self.screens
    }

    pub fn get(&self, id: &MachineId) -> Option<&Screen> {
        self.screens.iter().find(|s| &s.id == id)
    }

    pub fn get_mut(&mut self, id: &MachineId) -> Option<&mut Screen> {
        self.screens.iter_mut().find(|s| &s.id == id)
    }

    pub fn upsert(&mut self, screen: Screen) {
        if let Some(existing) = self.get_mut(&screen.id) {
            existing.name = screen.name;
            existing.width = screen.width;
            existing.height = screen.height;
            existing.x = screen.x;
            existing.y = screen.y;
            existing.placed = screen.placed;
        } else {
            self.screens.push(screen);
        }
    }

    pub fn ensure(&mut self, id: MachineId, name: String, width: u32, height: u32) -> &Screen {
        if self.get(&id).is_none() {
            self.screens.push(Screen {
                id: id.clone(),
                name,
                x: 0,
                y: 0,
                width: width.max(1),
                height: height.max(1),
                placed: false,
            });
        } else if let Some(screen) = self.get_mut(&id) {
            screen.name = name;
            if width > 0 {
                screen.width = width;
            }
            if height > 0 {
                screen.height = height;
            }
        }
        self.get(&id).expect("just inserted")
    }

    pub fn screen_at(&self, x: i32, y: i32) -> Option<&Screen> {
        self.screens.iter().find(|s| s.placed && s.contains(x, y))
    }

    pub fn snap_position(&self, id: &MachineId, x: i32, y: i32) -> (i32, i32) {
        let Some(screen) = self.get(id) else {
            return (x, y);
        };
        let w = screen.width as i32;
        let h = screen.height as i32;
        let mut best_x: Option<(i32, i32)> = None;
        let mut best_y: Option<(i32, i32)> = None;
        for other in &self.screens {
            if !other.placed || &other.id == id {
                continue;
            }
            let ow = other.width as i32;
            let oh = other.height as i32;
            if ranges_near(y, h, other.y, oh) {
                for target in [other.x - w, other.right(), other.x, other.right() - w] {
                    let dist = (x - target).abs();
                    if dist <= SNAP_PX && best_x.map(|(d, _)| dist < d).unwrap_or(true) {
                        best_x = Some((dist, target));
                    }
                }
            }
            if ranges_near(x, w, other.x, ow) {
                for target in [other.y - h, other.bottom(), other.y, other.bottom() - h] {
                    let dist = (y - target).abs();
                    if dist <= SNAP_PX && best_y.map(|(d, _)| dist < d).unwrap_or(true) {
                        best_y = Some((dist, target));
                    }
                }
            }
        }
        let mut x = best_x.map(|(_, v)| v).unwrap_or(x);
        let mut y = best_y.map(|(_, v)| v).unwrap_or(y);
        for _ in 0..4 {
            let mut pushed = false;
            for other in &self.screens {
                if !other.placed || &other.id == id {
                    continue;
                }
                if let Some((nx, ny)) = push_out(x, y, w, h, other) {
                    x = nx;
                    y = ny;
                    pushed = true;
                }
            }
            if !pushed {
                break;
            }
        }
        (x, y)
    }

    pub fn place(&mut self, id: &MachineId, x: i32, y: i32) -> Option<(i32, i32)> {
        let (sx, sy) = self.snap_position(id, x, y);
        let screen = self.get_mut(id)?;
        screen.x = sx;
        screen.y = sy;
        screen.placed = true;
        Some((sx, sy))
    }

    pub fn unplace(&mut self, id: &MachineId) -> bool {
        if let Some(screen) = self.get_mut(id) {
            screen.placed = false;
            true
        } else {
            false
        }
    }

    pub fn apply_delta(&self, cursor: Cursor, dx: i32, dy: i32) -> MoveOutcome {
        let dx = dx.clamp(-100_000, 100_000);
        let dy = dy.clamp(-100_000, 100_000);
        if dx == 0 && dy == 0 {
            return MoveOutcome::Stay { cursor };
        }
        let from = cursor.screen.clone();
        let dest_x = (cursor.x as i64 + dx as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let dest_y = (cursor.y as i64 + dy as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;

        let mut current_id = cursor.screen.clone();
        let mut last_good = (cursor.x, cursor.y);
        let mut ended = (cursor.x, cursor.y);
        let mut switched = false;
        let mut stop = false;

        walk_line(cursor.x, cursor.y, dest_x, dest_y, |px, py| {
            if stop {
                return false;
            }
            let Some(screen) = self.get(&current_id).cloned() else {
                stop = true;
                return false;
            };
            if screen.contains(px, py) {
                last_good = (px, py);
                ended = (px, py);
                return true;
            }
            if let Some(next) = self.screen_at(px, py) {
                if next.id != current_id && shares_edge_step(&screen, next, last_good, (px, py)) {
                    current_id = next.id.clone();
                    switched = true;
                    last_good = (px, py);
                    ended = (px, py);
                    return true;
                }
            }
            ended = last_good;
            stop = true;
            false
        });

        let cursor = Cursor {
            screen: current_id.clone(),
            x: ended.0,
            y: ended.1,
        };
        if switched && current_id != from {
            MoveOutcome::Switch { cursor, from }
        } else {
            MoveOutcome::Stay { cursor }
        }
    }

    pub fn jump_direction(&self, cursor: &Cursor, dir: Direction) -> Option<Cursor> {
        let from = self.get(&cursor.screen)?.clone();
        if !from.placed {
            return None;
        }
        let mut best: Option<(i32, Cursor)> = None;
        for other in self.screens.iter().filter(|s| s.placed && s.id != from.id) {
            let Some(entry) = entry_for(dir, &from, other, cursor) else {
                continue;
            };
            let rank = match dir {
                Direction::Left | Direction::Right => (cursor.y - entry.y).abs(),
                Direction::Up | Direction::Down => (cursor.x - entry.x).abs(),
            };
            if best.as_ref().map(|(r, _)| rank < *r).unwrap_or(true) {
                best = Some((rank, entry));
            }
        }
        best.map(|(_, cursor)| cursor)
    }

    pub fn jump_to(&self, target: &MachineId) -> Option<Cursor> {
        let screen = self.get(target)?;
        let w = screen.width.max(1) as i32;
        let h = screen.height.max(1) as i32;
        Some(Cursor {
            screen: target.clone(),
            x: screen.x + w / 2,
            y: screen.y + h / 2,
        })
    }

    /// Local pixel on `screen` for a global point, clamped inside it.
    pub fn local_point(&self, screen: &MachineId, x: i32, y: i32) -> Option<(i32, i32)> {
        let screen = self.get(screen)?;
        let w = screen.width.max(1) as i32;
        let h = screen.height.max(1) as i32;
        let lx = (x - screen.x).clamp(0, w - 1);
        let ly = (y - screen.y).clamp(0, h - 1);
        Some((lx, ly))
    }

    pub fn side_label(&self, anchor: &MachineId, other: &MachineId) -> Option<&'static str> {
        let a = self.get(anchor)?;
        let b = self.get(other)?;
        if !a.placed || !b.placed {
            return None;
        }
        if a.right() == b.x && overlap_len(a.y, a.bottom(), b.y, b.bottom()) > 0 {
            return Some("Right");
        }
        if b.right() == a.x && overlap_len(a.y, a.bottom(), b.y, b.bottom()) > 0 {
            return Some("Left");
        }
        if a.bottom() == b.y && overlap_len(a.x, a.right(), b.x, b.right()) > 0 {
            return Some("Below");
        }
        if b.bottom() == a.y && overlap_len(a.x, a.right(), b.x, b.right()) > 0 {
            return Some("Above");
        }
        let cx = (b.x + b.right()) / 2 - (a.x + a.right()) / 2;
        let cy = (b.y + b.bottom()) / 2 - (a.y + a.bottom()) / 2;
        if cx.abs() >= cy.abs() {
            Some(if cx >= 0 { "Right" } else { "Left" })
        } else {
            Some(if cy >= 0 { "Below" } else { "Above" })
        }
    }
}

fn entry_for(dir: Direction, from: &Screen, other: &Screen, cursor: &Cursor) -> Option<Cursor> {
    let vertical = overlap_len(from.y, from.bottom(), other.y, other.bottom()) > 0
        && match dir {
            Direction::Right => from.right() == other.x,
            Direction::Left => other.right() == from.x,
            _ => false,
        };
    let horizontal = overlap_len(from.x, from.right(), other.x, other.right()) > 0
        && match dir {
            Direction::Down => from.bottom() == other.y,
            Direction::Up => other.bottom() == from.y,
            _ => false,
        };
    if !vertical && !horizontal {
        return None;
    }
    let w = other.width.max(1) as i32;
    let h = other.height.max(1) as i32;
    let x = cursor.x.clamp(other.x, other.x + w - 1);
    let y = cursor.y.clamp(other.y, other.y + h - 1);
    let (x, y) = match dir {
        Direction::Right => (other.x, y),
        Direction::Left => (other.right() - 1, y),
        Direction::Down => (x, other.y),
        Direction::Up => (x, other.bottom() - 1),
    };
    Some(Cursor {
        screen: other.id.clone(),
        x,
        y,
    })
}

fn shares_edge_step(from: &Screen, to: &Screen, prev: (i32, i32), next: (i32, i32)) -> bool {
    let sx = next.0 - prev.0;
    let sy = next.1 - prev.1;
    if sx.abs() + sy.abs() != 1 {
        return false;
    }
    if sx > 0 {
        return from.right() == to.x && to.contains(next.0, next.1);
    }
    if sx < 0 {
        return to.right() == from.x && to.contains(next.0, next.1);
    }
    if sy > 0 {
        return from.bottom() == to.y && to.contains(next.0, next.1);
    }
    if sy < 0 {
        return to.bottom() == from.y && to.contains(next.0, next.1);
    }
    false
}

fn ranges_near(a: i32, a_len: i32, b: i32, b_len: i32) -> bool {
    let a2 = a.saturating_add(a_len);
    let b2 = b.saturating_add(b_len);
    let gap = if a2 < b {
        b - a2
    } else if b2 < a {
        a - b2
    } else {
        0
    };
    gap <= SNAP_PX
}

fn overlap_len(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    (a1.min(b1) - a0.max(b0)).max(0)
}

fn overlaps(x: i32, y: i32, w: i32, h: i32, other: &Screen) -> bool {
    x < other.right() && x + w > other.x && y < other.bottom() && y + h > other.y
}

fn push_out(x: i32, y: i32, w: i32, h: i32, other: &Screen) -> Option<(i32, i32)> {
    if w <= 0 || h <= 0 || !overlaps(x, y, w, h, other) {
        return None;
    }
    let options = [
        (other.x - w - x, 0),
        (other.right() - x, 0),
        (0, other.y - h - y),
        (0, other.bottom() - y),
    ];
    let (dx, dy) = options
        .into_iter()
        .min_by_key(|(dx, dy)| dx.abs() + dy.abs())?;
    Some((x + dx, y + dy))
}

/// Supercover line. Diagonal steps are split so every visit is orthogonal.
fn walk_line(x0: i32, y0: i32, x1: i32, y1: i32, mut visit: impl FnMut(i32, i32) -> bool) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let nx = dx.abs();
    let ny = dy.abs();
    let sign_x = dx.signum();
    let sign_y = dy.signum();
    let mut px = x0;
    let mut py = y0;
    let mut ix = 0i32;
    let mut iy = 0i32;
    if !visit(px, py) {
        return;
    }
    while ix < nx || iy < ny {
        let cmp = (1 + 2 * ix) as i64 * ny as i64 - (1 + 2 * iy) as i64 * nx as i64;
        if cmp == 0 {
            px += sign_x;
            ix += 1;
            if !visit(px, py) {
                return;
            }
            py += sign_y;
            iy += 1;
            if iy <= ny && !visit(px, py) {
                return;
            }
        } else if cmp < 0 {
            px += sign_x;
            ix += 1;
            if !visit(px, py) {
                return;
            }
        } else {
            py += sign_y;
            iy += 1;
            if !visit(px, py) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: &str, x: i32, y: i32, w: u32, h: u32) -> Screen {
        Screen {
            id: MachineId::new(id),
            name: id.into(),
            x,
            y,
            width: w,
            height: h,
            placed: true,
        }
    }

    fn desk(items: Vec<Screen>) -> Desk {
        Desk { screens: items }
    }

    fn cursor(id: &str, x: i32, y: i32) -> Cursor {
        Cursor {
            screen: MachineId::new(id),
            x,
            y,
        }
    }

    #[test]
    fn stays_inside_one_screen() {
        let desk = desk(vec![screen("a", 0, 0, 100, 80)]);
        let outcome = desk.apply_delta(cursor("a", 10, 10), 5, -3);
        assert_eq!(
            outcome,
            MoveOutcome::Stay {
                cursor: cursor("a", 15, 7)
            }
        );
    }

    #[test]
    fn crosses_a_touching_right_edge() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 80),
            screen("b", 100, 0, 100, 80),
        ]);
        let outcome = desk.apply_delta(cursor("a", 98, 20), 5, 0);
        assert_eq!(
            outcome,
            MoveOutcome::Switch {
                from: MachineId::new("a"),
                cursor: cursor("b", 103, 20),
            }
        );
    }

    #[test]
    fn a_gap_stops_the_cursor() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 80),
            screen("c", 110, 0, 100, 80),
        ]);
        let outcome = desk.apply_delta(cursor("a", 50, 40), 100, 0);
        assert_eq!(
            outcome,
            MoveOutcome::Stay {
                cursor: cursor("a", 99, 40)
            }
        );
    }

    #[test]
    fn offset_edge_only_crosses_the_overlap() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 100),
            screen("b", 100, 40, 80, 80),
        ]);
        let blocked = desk.apply_delta(cursor("a", 99, 10), 3, 0);
        assert_eq!(
            blocked,
            MoveOutcome::Stay {
                cursor: cursor("a", 99, 10)
            }
        );
        let crossed = desk.apply_delta(cursor("a", 99, 50), 3, 0);
        assert_eq!(
            crossed,
            MoveOutcome::Switch {
                from: MachineId::new("a"),
                cursor: cursor("b", 102, 50),
            }
        );
    }

    #[test]
    fn corner_touch_does_not_cross() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 100),
            screen("c", 100, 100, 100, 100),
        ]);
        let outcome = desk.apply_delta(cursor("a", 99, 99), 4, 4);
        assert!(matches!(outcome, MoveOutcome::Stay { .. }));
        if let MoveOutcome::Stay { cursor } = outcome {
            assert_eq!(cursor.screen.0, "a");
        }
    }

    #[test]
    fn fast_move_can_pass_through_to_a_third_screen() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 40),
            screen("b", 100, 0, 100, 40),
            screen("c", 200, 0, 100, 40),
        ]);
        let outcome = desk.apply_delta(cursor("a", 90, 10), 150, 0);
        assert_eq!(
            outcome,
            MoveOutcome::Switch {
                from: MachineId::new("a"),
                cursor: cursor("c", 240, 10),
            }
        );
    }

    #[test]
    fn crosses_up_and_down() {
        let desk = desk(vec![
            screen("a", 0, 100, 60, 50),
            screen("b", 0, 0, 60, 100),
        ]);
        let up = desk.apply_delta(cursor("a", 10, 100), 0, -2);
        assert_eq!(
            up,
            MoveOutcome::Switch {
                from: MachineId::new("a"),
                cursor: cursor("b", 10, 98),
            }
        );
    }

    #[test]
    fn jump_follows_the_touching_side() {
        let desk = desk(vec![
            screen("a", 0, 0, 100, 100),
            screen("b", 100, 20, 50, 40),
        ]);
        let landed = desk
            .jump_direction(&cursor("a", 10, 90), Direction::Right)
            .expect("neighbor");
        assert_eq!(landed.screen.0, "b");
        assert_eq!(landed.x, 100);
        assert!(landed.y >= 20 && landed.y < 60);
        assert!(desk
            .jump_direction(&cursor("a", 10, 10), Direction::Left)
            .is_none());
    }

    #[test]
    fn snap_pulls_a_near_edge_flush_and_separates_overlap() {
        let mut desk = desk(vec![screen("a", 0, 0, 100, 100)]);
        desk.upsert(Screen {
            id: MachineId::new("b"),
            name: "b".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            placed: false,
        });
        let (x, y) = desk.place(&MachineId::new("b"), 90, 0).unwrap();
        assert_eq!((x, y), (100, 0));
        let (x, y) = desk.place(&MachineId::new("b"), 40, 40).unwrap();
        assert!(!overlaps(x, y, 100, 100, desk.get(&MachineId::new("a")).unwrap()));
    }

    #[test]
    fn side_labels_match_placement() {
        let mut desk = desk(vec![screen("a", 0, 0, 100, 100)]);
        desk.upsert(screen("b", 100, 0, 80, 60));
        assert_eq!(
            desk.side_label(&MachineId::new("a"), &MachineId::new("b")),
            Some("Right")
        );
    }
}
