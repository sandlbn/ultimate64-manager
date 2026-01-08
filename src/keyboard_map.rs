// keyboard_map.rs - PC to C64 keyboard mapping for remote control
//
// Maps PC keyboard input to C64 key codes for use with Ultimate64's
// keyboard buffer (same approach as type_text).
//
// Memory locations used:
//   $0277-$0280 (631-640) - Keyboard buffer (10 chars max)
//   $C6 (198) - Number of characters in keyboard buffer

#![allow(dead_code)]

use iced::keyboard::{Key, Modifiers};
use std::collections::HashMap;

/// C64 memory addresses for keyboard buffer
pub const KEYBUF_ADDR: u16 = 0x0277; // Keyboard buffer start (10 bytes)
pub const KEYBUF_COUNT: u16 = 0x00C6; // Number of chars in buffer

// Legacy addresses (kept for reference, but buffer approach is more reliable)
pub const LSTX_ADDR: u16 = 0x00C5; // Current key pressed (matrix code)
pub const SHFLAG_ADDR: u16 = 0x028D; // Shift/Ctrl/C= flags
pub const NO_KEY: u8 = 0x40; // No key pressed value

/// Modifier key flags for C64
#[derive(Debug, Clone, Copy, Default)]
pub struct C64Modifiers {
    pub shift: bool,
    pub commodore: bool,
    pub ctrl: bool,
}

impl C64Modifiers {
    pub fn to_byte(&self) -> u8 {
        let mut flags = 0u8;
        if self.shift {
            flags |= 0x01;
        }
        if self.commodore {
            flags |= 0x02;
        }
        if self.ctrl {
            flags |= 0x04;
        }
        flags
    }
}

/// A mapped C64 key - now uses PETSCII code for keyboard buffer
#[derive(Debug, Clone, Copy)]
pub struct C64Key {
    pub petscii: u8,     // PETSCII code to put in keyboard buffer
    pub matrix_code: u8, // Matrix code (for reference/legacy)
    pub modifiers: C64Modifiers,
}

impl C64Key {
    pub const fn new(petscii: u8, matrix_code: u8) -> Self {
        Self {
            petscii,
            matrix_code,
            modifiers: C64Modifiers {
                shift: false,
                commodore: false,
                ctrl: false,
            },
        }
    }

    pub const fn with_shift(petscii: u8, matrix_code: u8) -> Self {
        Self {
            petscii,
            matrix_code,
            modifiers: C64Modifiers {
                shift: true,
                commodore: false,
                ctrl: false,
            },
        }
    }

    pub const fn with_ctrl(petscii: u8, matrix_code: u8) -> Self {
        Self {
            petscii,
            matrix_code,
            modifiers: C64Modifiers {
                shift: false,
                commodore: false,
                ctrl: true,
            },
        }
    }

    pub const fn with_commodore(petscii: u8, matrix_code: u8) -> Self {
        Self {
            petscii,
            matrix_code,
            modifiers: C64Modifiers {
                shift: false,
                commodore: true,
                ctrl: false,
            },
        }
    }
}

/// C64 Keyboard Matrix Codes and PETSCII codes
///
/// The C64 keyboard is an 8x8 matrix. Each key has a unique code:
///   Code = Row * 8 + Column
///   $40 (64) = No key pressed
///
/// PETSCII codes are used for the keyboard buffer approach.
pub mod matrix {
    // Row 0
    pub const DEL: u8 = 0x00;
    pub const RETURN: u8 = 0x01;
    pub const CRSR_RIGHT: u8 = 0x02;
    pub const F7: u8 = 0x03;
    pub const F1: u8 = 0x04;
    pub const F3: u8 = 0x05;
    pub const F5: u8 = 0x06;
    pub const CRSR_DOWN: u8 = 0x07;

    // Row 1
    pub const KEY_3: u8 = 0x08;
    pub const W: u8 = 0x09;
    pub const A: u8 = 0x0A;
    pub const KEY_4: u8 = 0x0B;
    pub const Z: u8 = 0x0C;
    pub const S: u8 = 0x0D;
    pub const E: u8 = 0x0E;
    pub const LEFT_SHIFT: u8 = 0x0F;

    // Row 2
    pub const KEY_5: u8 = 0x10;
    pub const R: u8 = 0x11;
    pub const D: u8 = 0x12;
    pub const KEY_6: u8 = 0x13;
    pub const C: u8 = 0x14;
    pub const F: u8 = 0x15;
    pub const T: u8 = 0x16;
    pub const X: u8 = 0x17;

    // Row 3
    pub const KEY_7: u8 = 0x18;
    pub const Y: u8 = 0x19;
    pub const G: u8 = 0x1A;
    pub const KEY_8: u8 = 0x1B;
    pub const B: u8 = 0x1C;
    pub const H: u8 = 0x1D;
    pub const U: u8 = 0x1E;
    pub const V: u8 = 0x1F;

    // Row 4
    pub const KEY_9: u8 = 0x20;
    pub const I: u8 = 0x21;
    pub const J: u8 = 0x22;
    pub const KEY_0: u8 = 0x23;
    pub const M: u8 = 0x24;
    pub const K: u8 = 0x25;
    pub const O: u8 = 0x26;
    pub const N: u8 = 0x27;

    // Row 5
    pub const PLUS: u8 = 0x28;
    pub const P: u8 = 0x29;
    pub const L: u8 = 0x2A;
    pub const MINUS: u8 = 0x2B;
    pub const PERIOD: u8 = 0x2C;
    pub const COLON: u8 = 0x2D;
    pub const AT: u8 = 0x2E;
    pub const COMMA: u8 = 0x2F;

    // Row 6
    pub const POUND: u8 = 0x30;
    pub const ASTERISK: u8 = 0x31;
    pub const SEMICOLON: u8 = 0x32;
    pub const HOME: u8 = 0x33;
    pub const RIGHT_SHIFT: u8 = 0x34;
    pub const EQUALS: u8 = 0x35;
    pub const UP_ARROW: u8 = 0x36;
    pub const SLASH: u8 = 0x37;

    // Row 7
    pub const KEY_1: u8 = 0x38;
    pub const LEFT_ARROW: u8 = 0x39;
    pub const CTRL: u8 = 0x3A;
    pub const KEY_2: u8 = 0x3B;
    pub const SPACE: u8 = 0x3C;
    pub const COMMODORE: u8 = 0x3D;
    pub const Q: u8 = 0x3E;
    pub const RUN_STOP: u8 = 0x3F;

    pub const NO_KEY: u8 = 0x40;
}

/// PETSCII codes for keyboard buffer
pub mod petscii {
    // Control codes
    pub const RETURN: u8 = 13;
    pub const SPACE: u8 = 32;
    pub const DEL: u8 = 20; // Delete/Backspace
    pub const HOME: u8 = 19;
    pub const CLR: u8 = 147; // Clear screen (Shift+Home)
    pub const RUN_STOP: u8 = 3; // STOP

    // Cursor movement
    pub const CRSR_DOWN: u8 = 17;
    pub const CRSR_UP: u8 = 145; // Shift+Down
    pub const CRSR_RIGHT: u8 = 29;
    pub const CRSR_LEFT: u8 = 157; // Shift+Right

    // Function keys
    pub const F1: u8 = 133;
    pub const F2: u8 = 137;
    pub const F3: u8 = 134;
    pub const F4: u8 = 138;
    pub const F5: u8 = 135;
    pub const F6: u8 = 139;
    pub const F7: u8 = 136;
    pub const F8: u8 = 140;

    // Letters (uppercase PETSCII)
    pub const A: u8 = 65;
    pub const B: u8 = 66;
    pub const C: u8 = 67;
    pub const D: u8 = 68;
    pub const E: u8 = 69;
    pub const F: u8 = 70;
    pub const G: u8 = 71;
    pub const H: u8 = 72;
    pub const I: u8 = 73;
    pub const J: u8 = 74;
    pub const K: u8 = 75;
    pub const L: u8 = 76;
    pub const M: u8 = 77;
    pub const N: u8 = 78;
    pub const O: u8 = 79;
    pub const P: u8 = 80;
    pub const Q: u8 = 81;
    pub const R: u8 = 82;
    pub const S: u8 = 83;
    pub const T: u8 = 84;
    pub const U: u8 = 85;
    pub const V: u8 = 86;
    pub const W: u8 = 87;
    pub const X: u8 = 88;
    pub const Y: u8 = 89;
    pub const Z: u8 = 90;
}

/// Keyboard mapper for converting PC keys to C64 PETSCII codes
pub struct KeyboardMapper {
    /// Currently pressed keys (for tracking)
    pressed_keys: HashMap<String, C64Key>,
}

impl Default for KeyboardMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardMapper {
    pub fn new() -> Self {
        Self {
            pressed_keys: HashMap::new(),
        }
    }

    /// Map a PC key to C64 PETSCII code
    /// Returns Some(petscii_code) or None if key not mapped
    pub fn map_key(&self, key: &Key, modifiers: &Modifiers) -> Option<u8> {
        match key {
            Key::Character(c) => self.map_character(c, modifiers),
            Key::Named(named) => self.map_named_key(named, modifiers),
            _ => None,
        }
    }

    /// Map character keys to PETSCII
    fn map_character(&self, c: &str, _modifiers: &Modifiers) -> Option<u8> {
        let ch = c.chars().next()?;

        // Convert to uppercase PETSCII (C64 default mode)
        match ch.to_ascii_uppercase() {
            'A' => Some(petscii::A),
            'B' => Some(petscii::B),
            'C' => Some(petscii::C),
            'D' => Some(petscii::D),
            'E' => Some(petscii::E),
            'F' => Some(petscii::F),
            'G' => Some(petscii::G),
            'H' => Some(petscii::H),
            'I' => Some(petscii::I),
            'J' => Some(petscii::J),
            'K' => Some(petscii::K),
            'L' => Some(petscii::L),
            'M' => Some(petscii::M),
            'N' => Some(petscii::N),
            'O' => Some(petscii::O),
            'P' => Some(petscii::P),
            'Q' => Some(petscii::Q),
            'R' => Some(petscii::R),
            'S' => Some(petscii::S),
            'T' => Some(petscii::T),
            'U' => Some(petscii::U),
            'V' => Some(petscii::V),
            'W' => Some(petscii::W),
            'X' => Some(petscii::X),
            'Y' => Some(petscii::Y),
            'Z' => Some(petscii::Z),

            // Numbers (PETSCII same as ASCII)
            '0' => Some(48),
            '1' => Some(49),
            '2' => Some(50),
            '3' => Some(51),
            '4' => Some(52),
            '5' => Some(53),
            '6' => Some(54),
            '7' => Some(55),
            '8' => Some(56),
            '9' => Some(57),

            // Common symbols
            ' ' => Some(petscii::SPACE),
            '.' => Some(46),
            ',' => Some(44),
            ':' => Some(58),
            ';' => Some(59),
            '/' => Some(47),
            '=' => Some(61),
            '+' => Some(43),
            '-' => Some(45),
            '*' => Some(42),
            '@' => Some(64),
            '"' => Some(34),
            '\'' => Some(39),
            '!' => Some(33),
            '?' => Some(63),
            '#' => Some(35),
            '$' => Some(36),
            '%' => Some(37),
            '&' => Some(38),
            '(' => Some(40),
            ')' => Some(41),
            '<' => Some(60),
            '>' => Some(62),
            '[' => Some(91),
            ']' => Some(93),

            _ => None,
        }
    }

    /// Map named/special keys to PETSCII
    fn map_named_key(
        &self,
        named: &iced::keyboard::key::Named,
        _modifiers: &Modifiers,
    ) -> Option<u8> {
        use iced::keyboard::key::Named;

        match named {
            // Cursor keys
            Named::ArrowUp => Some(petscii::CRSR_UP),
            Named::ArrowDown => Some(petscii::CRSR_DOWN),
            Named::ArrowLeft => Some(petscii::CRSR_LEFT),
            Named::ArrowRight => Some(petscii::CRSR_RIGHT),

            // Function keys
            Named::F1 => Some(petscii::F1),
            Named::F2 => Some(petscii::F2),
            Named::F3 => Some(petscii::F3),
            Named::F4 => Some(petscii::F4),
            Named::F5 => Some(petscii::F5),
            Named::F6 => Some(petscii::F6),
            Named::F7 => Some(petscii::F7),
            Named::F8 => Some(petscii::F8),

            // Special keys
            Named::Enter => Some(petscii::RETURN),
            Named::Space => Some(petscii::SPACE),
            Named::Backspace => Some(petscii::DEL),
            Named::Delete => Some(petscii::DEL),
            Named::Home => Some(petscii::HOME),
            Named::End => Some(petscii::CLR), // CLR = Shift+Home
            Named::Escape => Some(petscii::RUN_STOP),
            Named::Insert => Some(148), // Insert mode
            Named::Tab => Some(9),      // Tab

            _ => None,
        }
    }

    /// Handle key press - returns PETSCII code to send
    pub fn key_down(&mut self, key: &Key, modifiers: &Modifiers) -> Option<u8> {
        let petscii = self.map_key(key, modifiers)?;

        // Track pressed key
        let key_id = format!("{:?}", key);
        self.pressed_keys.insert(key_id, C64Key::new(petscii, 0));

        Some(petscii)
    }

    /// Handle key release
    pub fn key_up(&mut self, key: &Key) {
        let key_id = format!("{:?}", key);
        self.pressed_keys.remove(&key_id);
    }

    /// Release all keys
    pub fn release_all(&mut self) {
        self.pressed_keys.clear();
    }

    /// Check if any keys are pressed
    pub fn has_keys_pressed(&self) -> bool {
        !self.pressed_keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_letter_mapping() {
        let mapper = KeyboardMapper::new();
        let mods = Modifiers::default();

        let key = Key::Character("a".into());
        let result = mapper.map_key(&key, &mods);
        assert_eq!(result, Some(petscii::A));
    }

    #[test]
    fn test_cursor_keys() {
        let mapper = KeyboardMapper::new();
        let mods = Modifiers::default();

        let key = Key::Named(iced::keyboard::key::Named::ArrowUp);
        let result = mapper.map_key(&key, &mods);
        assert_eq!(result, Some(petscii::CRSR_UP));

        let key = Key::Named(iced::keyboard::key::Named::ArrowDown);
        let result = mapper.map_key(&key, &mods);
        assert_eq!(result, Some(petscii::CRSR_DOWN));
    }

    #[test]
    fn test_function_keys() {
        let mapper = KeyboardMapper::new();
        let mods = Modifiers::default();

        let key = Key::Named(iced::keyboard::key::Named::F1);
        assert_eq!(mapper.map_key(&key, &mods), Some(petscii::F1));

        let key = Key::Named(iced::keyboard::key::Named::F3);
        assert_eq!(mapper.map_key(&key, &mods), Some(petscii::F3));
    }
}
