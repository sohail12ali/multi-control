//! Platform keycode tables. Values are numeric so the maps compile on every OS.

use std::collections::HashMap;

fn table(pairs: &[(u16, u16)]) -> HashMap<u16, u16> {
    pairs.iter().copied().collect()
}

fn invert(map: &HashMap<u16, u16>) -> HashMap<u16, u16> {
    let mut out = HashMap::new();
    for (key, usage) in map {
        out.entry(*usage)
            .and_modify(|existing: &mut u16| {
                if *key > *existing {
                    *existing = *key;
                }
            })
            .or_insert(*key);
    }
    out
}

/// macOS virtual keycode → HID usage.
pub fn hid_from_mac(code: u16) -> Option<u16> {
    mac_to_hid().get(&code).copied()
}

pub fn mac_from_hid(usage: u16) -> Option<u16> {
    hid_to_mac().get(&usage).copied()
}

pub fn hid_from_vk(code: u16) -> Option<u16> {
    vk_to_hid().get(&code).copied()
}

pub fn vk_from_hid(usage: u16) -> Option<u16> {
    hid_to_vk().get(&usage).copied()
}

pub fn hid_from_evdev(code: u16) -> Option<u16> {
    evdev_to_hid().get(&code).copied()
}

pub fn evdev_from_hid(usage: u16) -> Option<u16> {
    hid_to_evdev().get(&usage).copied()
}

/// X11 keycodes are evdev + 8.
pub fn hid_from_x11(code: u16) -> Option<u16> {
    code.checked_sub(8).and_then(hid_from_evdev)
}

pub fn x11_from_hid(usage: u16) -> Option<u16> {
    evdev_from_hid(usage).map(|code| code + 8)
}

fn mac_to_hid() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        table(&[
            (0x00, 0x04), // A
            (0x0B, 0x05), // B
            (0x08, 0x06), // C
            (0x02, 0x07), // D
            (0x0E, 0x08), // E
            (0x03, 0x09), // F
            (0x05, 0x0A), // G
            (0x04, 0x0B), // H
            (0x22, 0x0C), // I
            (0x26, 0x0D), // J
            (0x28, 0x0E), // K
            (0x25, 0x0F), // L
            (0x2E, 0x10), // M
            (0x2D, 0x11), // N
            (0x1F, 0x12), // O
            (0x23, 0x13), // P
            (0x0C, 0x14), // Q
            (0x0F, 0x15), // R
            (0x01, 0x16), // S
            (0x11, 0x17), // T
            (0x20, 0x18), // U
            (0x09, 0x19), // V
            (0x0D, 0x1A), // W
            (0x07, 0x1B), // X
            (0x10, 0x1C), // Y
            (0x06, 0x1D), // Z
            (0x12, 0x1E), // 1
            (0x13, 0x1F),
            (0x14, 0x20),
            (0x15, 0x21),
            (0x17, 0x22), // 5
            (0x16, 0x23), // 6
            (0x1A, 0x24),
            (0x1C, 0x25),
            (0x19, 0x26),
            (0x1D, 0x27), // 0
            (0x24, 0x28), // return
            (0x35, 0x29), // esc
            (0x33, 0x2A), // delete / backspace
            (0x30, 0x2B), // tab
            (0x31, 0x2C), // space
            (0x1B, 0x2D), // -
            (0x18, 0x2E), // =
            (0x21, 0x2F), // [
            (0x1E, 0x30), // ]
            (0x2A, 0x31), // \
            (0x29, 0x33), // ;
            (0x27, 0x34), // '
            (0x32, 0x35), // `
            (0x2B, 0x36), // ,
            (0x2F, 0x37), // .
            (0x2C, 0x38), // /
            (0x39, 0x39), // caps
            (0x7A, 0x3A), // F1
            (0x78, 0x3B),
            (0x63, 0x3C),
            (0x76, 0x3D),
            (0x60, 0x3E),
            (0x61, 0x3F),
            (0x62, 0x40),
            (0x64, 0x41),
            (0x65, 0x42),
            (0x6D, 0x43),
            (0x67, 0x44),
            (0x6F, 0x45), // F12
            (0x73, 0x4A), // home
            (0x74, 0x4B), // page up
            (0x75, 0x4C), // forward delete
            (0x77, 0x4D), // end
            (0x79, 0x4E), // page down
            (0x7C, 0x4F), // right
            (0x7B, 0x50), // left
            (0x7D, 0x51), // down
            (0x7E, 0x52), // up
            (0x3B, 0xE0), // left ctrl
            (0x38, 0xE1), // left shift
            (0x3A, 0xE2), // left option
            (0x37, 0xE3), // left command
            (0x3E, 0xE4), // right ctrl
            (0x3C, 0xE5), // right shift
            (0x3D, 0xE6), // right option
            (0x36, 0xE7), // right command
        ])
    })
}

fn hid_to_mac() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| invert(mac_to_hid()))
}

fn vk_to_hid() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut pairs = vec![
            (0x0D, 0x28), // return
            (0x1B, 0x29),
            (0x08, 0x2A),
            (0x09, 0x2B),
            (0x20, 0x2C),
            (0xBD, 0x2D),
            (0xBB, 0x2E),
            (0xDB, 0x2F),
            (0xDD, 0x30),
            (0xDC, 0x31),
            (0xBA, 0x33),
            (0xDE, 0x34),
            (0xC0, 0x35),
            (0xBC, 0x36),
            (0xBE, 0x37),
            (0xBF, 0x38),
            (0x14, 0x39), // caps
            (0x2D, 0x49), // insert
            (0x24, 0x4A),
            (0x21, 0x4B),
            (0x2E, 0x4C),
            (0x23, 0x4D),
            (0x22, 0x4E),
            (0x27, 0x4F),
            (0x25, 0x50),
            (0x28, 0x51),
            (0x26, 0x52),
            (0xA2, 0xE0),
            (0xA0, 0xE1),
            (0xA4, 0xE2),
            (0x5B, 0xE3),
            (0xA3, 0xE4),
            (0xA1, 0xE5),
            (0xA5, 0xE6),
            (0x5C, 0xE7),
            (0x10, 0xE1),
            (0x11, 0xE0),
            (0x12, 0xE2),
        ];
        for i in 0..26 {
            pairs.push((0x41 + i, 0x04 + i));
        }
        for i in 0..9 {
            pairs.push((0x31 + i, 0x1E + i));
        }
        pairs.push((0x30, 0x27));
        for i in 0..12 {
            pairs.push((0x70 + i, 0x3A + i));
        }
        table(&pairs)
    })
}

fn hid_to_vk() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| invert(vk_to_hid()))
}

fn evdev_to_hid() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        table(&[
            (1, 0x29),   // esc
            (2, 0x1E),   // 1
            (3, 0x1F),
            (4, 0x20),
            (5, 0x21),
            (6, 0x22),
            (7, 0x23),
            (8, 0x24),
            (9, 0x25),
            (10, 0x26),
            (11, 0x27),  // 0
            (12, 0x2D),  // minus
            (13, 0x2E),  // equal
            (14, 0x2A),  // backspace
            (15, 0x2B),  // tab
            (16, 0x14),  // Q
            (17, 0x1A),  // W
            (18, 0x08),  // E
            (19, 0x15),  // R
            (20, 0x17),  // T
            (21, 0x1C),  // Y
            (22, 0x18),  // U
            (23, 0x0C),  // I
            (24, 0x12),  // O
            (25, 0x13),  // P
            (26, 0x2F),  // [
            (27, 0x30),  // ]
            (28, 0x28),  // enter
            (29, 0xE0),  // left ctrl
            (30, 0x04),  // A
            (31, 0x16),  // S
            (32, 0x07),  // D
            (33, 0x09),  // F
            (34, 0x0A),  // G
            (35, 0x0B),  // H
            (36, 0x0D),  // J
            (37, 0x0E),  // K
            (38, 0x0F),  // L
            (39, 0x33),  // ;
            (40, 0x34),  // '
            (41, 0x35),  // grave
            (42, 0xE1),  // left shift
            (43, 0x31),  // backslash
            (44, 0x1D),  // Z
            (45, 0x1B),  // X
            (46, 0x06),  // C
            (47, 0x19),  // V
            (48, 0x05),  // B
            (49, 0x11),  // N
            (50, 0x10),  // M
            (51, 0x36),  // comma
            (52, 0x37),  // dot
            (53, 0x38),  // slash
            (54, 0xE5),  // right shift
            (56, 0xE2),  // left alt
            (57, 0x2C),  // space
            (58, 0x39),  // caps
            (59, 0x3A),  // F1
            (60, 0x3B),
            (61, 0x3C),
            (62, 0x3D),
            (63, 0x3E),
            (64, 0x3F),
            (65, 0x40),
            (66, 0x41),
            (67, 0x42),
            (68, 0x43),  // F10
            (87, 0x44),  // F11
            (88, 0x45),  // F12
            (97, 0xE4),  // right ctrl
            (100, 0xE6), // right alt
            (102, 0x4A), // home
            (103, 0x52), // up
            (104, 0x4B),
            (105, 0x50), // left
            (106, 0x4F), // right
            (107, 0x4D),
            (108, 0x51), // down
            (109, 0x4E),
            (110, 0x49),
            (111, 0x4C),
            (125, 0xE3),
            (126, 0xE7),
        ])
    })
}

fn hid_to_evdev() -> &'static HashMap<u16, u16> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    MAP.get_or_init(|| invert(evdev_to_hid()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_modifiers_roundtrip() {
        assert_eq!(hid_from_mac(0x00), Some(0x04));
        assert_eq!(mac_from_hid(0x04), Some(0x00));
        assert_eq!(hid_from_vk(0x41), Some(0x04));
        assert_eq!(vk_from_hid(0xE0), Some(0xA2));
        assert_eq!(hid_from_evdev(30), Some(0x04));
        assert_eq!(evdev_from_hid(0x4F), Some(106));
        assert_eq!(hid_from_x11(30 + 8), Some(0x04));
        assert_eq!(x11_from_hid(0x04), Some(38));
    }
}
