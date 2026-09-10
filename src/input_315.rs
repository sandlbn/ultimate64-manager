//! Driving the C64 keyboard and joysticks through the firmware 3.15
//! `machine:input` call.
//!
//! [`crate::api_315::send_input`] is the transport; this module is the state
//! that makes it usable interactively.
//!
//! # Why state, and not just "send what the pad is doing"
//!
//! Each call is a fresh HTTP request — the Ultimate's server sends
//! `Connection: close`, so there is no connection to keep warm — and holding a
//! direction is expressed as a `press` that lasts until a matching `release`.
//! Re-sending "up is held" every frame would therefore be both wrong and
//! unplayable.
//!
//! [`InputController`] keeps what is currently held and emits only the
//! difference, so a steady direction costs nothing and a change costs one small
//! request. Anything that has to reach the wire at 50 Hz does not belong here.

use crate::api_315::{InputEvent, JoyInput, Transition};
use std::collections::{BTreeSet, HashMap};

/// Every key name the firmware accepts, in the order the schema lists them
/// (the C64 matrix scan order, with `restore` appended — it hangs off NMI
/// rather than the matrix).
pub const KEY_NAMES: &[&str] = &[
    "inst_del",
    "return",
    "cursor_left_right",
    "f7",
    "f1",
    "f3",
    "f5",
    "cursor_up_down",
    "3",
    "w",
    "a",
    "4",
    "z",
    "s",
    "e",
    "left_shift",
    "5",
    "r",
    "d",
    "6",
    "c",
    "f",
    "t",
    "x",
    "7",
    "y",
    "g",
    "8",
    "b",
    "h",
    "u",
    "v",
    "9",
    "i",
    "j",
    "0",
    "m",
    "k",
    "o",
    "n",
    "plus",
    "p",
    "l",
    "minus",
    "period",
    "colon",
    "at",
    "comma",
    "pound",
    "star",
    "semicolon",
    "clr_home",
    "right_shift",
    "equals",
    "arrow_up",
    "slash",
    "1",
    "arrow_left",
    "ctrl",
    "2",
    "space",
    "commodore",
    "q",
    "run_stop",
    "restore",
];

/// Whether a name is one the firmware will accept. Used to keep a typo from
/// being sent as a batch the device rejects wholesale.
pub fn is_valid_key(name: &str) -> bool {
    KEY_NAMES.contains(&name)
}

/// The key (and whether shift is needed) that produces `c` on a C64 keyboard.
///
/// The shifted symbols follow the C64 layout, not ASCII — `"` is shift-2 and
/// `(` is shift-8, which is why this is a table rather than a calculation.
pub fn keys_for_char(c: char) -> Option<(&'static str, bool)> {
    let lower = c.to_ascii_lowercase();
    let key: &'static str = match lower {
        'a'..='z' => match lower {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            _ => "z",
        },
        '0'..='9' => match lower {
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            _ => "9",
        },
        ' ' => "space",
        '\n' | '\r' => "return",
        '+' => "plus",
        '-' => "minus",
        '.' => "period",
        ':' => "colon",
        '@' => "at",
        ',' => "comma",
        '*' => "star",
        ';' => "semicolon",
        '=' => "equals",
        '/' => "slash",
        '£' => "pound",
        '^' => "arrow_up",
        _ => return shifted_symbol(c),
    };
    // A letter typed as uppercase still uses the unshifted key: shift on a C64
    // selects the graphic, not the capital, in the default character set.
    Some((key, false))
}

/// Symbols that need shift, per the C64 layout.
fn shifted_symbol(c: char) -> Option<(&'static str, bool)> {
    let key = match c {
        '!' => "1",
        '"' => "2",
        '#' => "3",
        '$' => "4",
        '%' => "5",
        '&' => "6",
        '\'' => "7",
        '(' => "8",
        ')' => "9",
        '<' => "comma",
        '>' => "period",
        '?' => "slash",
        '[' => "colon",
        ']' => "semicolon",
        _ => return None,
    };
    Some((key, true))
}

/// Turn text into tap events, one per character.
///
/// Characters with no C64 key are skipped rather than failing the batch — the
/// firmware validates a batch as a whole and applies none of it on rejection,
/// so one stray character would otherwise silently drop the entire line.
/// Callers that need to know should check the returned count against the input
/// length.
pub fn type_text_events(text: &str) -> Vec<InputEvent> {
    text.chars()
        .filter_map(|c| {
            let (key, shift) = keys_for_char(c)?;
            let mut inputs = Vec::with_capacity(2);
            if shift {
                inputs.push("left_shift".to_string());
            }
            inputs.push(key.to_string());
            Some(InputEvent::Keyboard {
                inputs,
                transition: Transition::Tap,
            })
        })
        .collect()
}

/// A snapshot of a physical gamepad, normalised away from any particular
/// gamepad library so the mapping can be tested without one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadState {
    /// Stick/d-pad horizontal, -1.0 (left) to 1.0 (right).
    pub x: f32,
    /// Stick/d-pad vertical, -1.0 (up) to 1.0 (down).
    pub y: f32,
    pub fire: bool,
    pub fire2: bool,
    pub fire3: bool,
}

/// How far a stick must travel before it counts as a direction.
///
/// Analogue sticks rest near but not exactly at zero, so a naive `!= 0` test
/// leaves a direction permanently held. This is deliberately well clear of the
/// noise floor of a worn stick.
pub const STICK_DEADZONE: f32 = 0.5;

impl PadState {
    /// The C64 joystick inputs this pad position represents.
    ///
    /// Opposing directions are impossible on a real stick and confuse some
    /// games, so the dominant axis wins rather than both being sent.
    pub fn to_inputs(&self) -> BTreeSet<JoyInput> {
        let mut out = BTreeSet::new();
        if self.x <= -STICK_DEADZONE {
            out.insert(JoyInput::Left);
        } else if self.x >= STICK_DEADZONE {
            out.insert(JoyInput::Right);
        }
        if self.y <= -STICK_DEADZONE {
            out.insert(JoyInput::Up);
        } else if self.y >= STICK_DEADZONE {
            out.insert(JoyInput::Down);
        }
        if self.fire {
            out.insert(JoyInput::Fire);
        }
        if self.fire2 {
            out.insert(JoyInput::Fire2);
        }
        if self.fire3 {
            out.insert(JoyInput::Fire3);
        }
        out
    }
}

/// Tracks what is currently held so only changes are sent.
#[derive(Debug, Default)]
pub struct InputController {
    joy: HashMap<u8, BTreeSet<JoyInput>>,
}

impl InputController {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is currently held on `port`.
    pub fn held(&self, port: u8) -> BTreeSet<JoyInput> {
        self.joy.get(&port).cloned().unwrap_or_default()
    }

    /// Whether anything at all is held.
    pub fn is_idle(&self) -> bool {
        self.joy.values().all(|h| h.is_empty())
    }

    /// Move `port` to `desired`, returning only the events needed to get there.
    ///
    /// An unchanged position returns an empty vec, which callers should treat
    /// as "send nothing" — that is what keeps a held direction free.
    pub fn set_joystick(&mut self, port: u8, desired: BTreeSet<JoyInput>) -> Vec<InputEvent> {
        let held = self.joy.entry(port).or_default();
        let to_press: Vec<JoyInput> = desired.difference(held).copied().collect();
        let to_release: Vec<JoyInput> = held.difference(&desired).copied().collect();

        let mut events = Vec::new();
        if !to_release.is_empty() {
            events.push(InputEvent::Joystick {
                port,
                inputs: to_release,
                transition: Transition::Release,
            });
        }
        if !to_press.is_empty() {
            events.push(InputEvent::Joystick {
                port,
                inputs: to_press,
                transition: Transition::Press,
            });
        }
        *held = desired;
        events
    }

    /// Drop everything held, locally and on the device.
    ///
    /// Worth sending whenever control is handed back — closing the panel,
    /// losing the pad, leaving the tab — or a held direction stays stuck on the
    /// device with nothing left to release it.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        if self.is_idle() {
            self.joy.clear();
            return Vec::new();
        }
        self.joy.clear();
        vec![InputEvent::ReleaseAll]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[JoyInput]) -> BTreeSet<JoyInput> {
        items.iter().copied().collect()
    }

    #[test]
    fn every_mapped_key_name_is_one_the_firmware_accepts() {
        // Guards the hand-written table against a typo that would make the
        // firmware reject the whole batch.
        for c in "abcdefghijklmnopqrstuvwxyz0123456789 \n+-.:@,*;=/^".chars() {
            let (key, _) = keys_for_char(c).unwrap_or_else(|| panic!("no key for {c:?}"));
            assert!(is_valid_key(key), "{c:?} mapped to invalid key {key:?}");
        }
        for c in "!\"#$%&'()<>?[]".chars() {
            let (key, shift) = keys_for_char(c).unwrap_or_else(|| panic!("no key for {c:?}"));
            assert!(is_valid_key(key), "{c:?} mapped to invalid key {key:?}");
            assert!(shift, "{c:?} is a shifted symbol");
        }
        assert!(is_valid_key("restore"));
        assert_eq!(KEY_NAMES.len(), 65, "64 matrix keys plus restore");
    }

    #[test]
    fn unknown_characters_have_no_key() {
        assert!(keys_for_char('€').is_none());
        assert!(keys_for_char('~').is_none());
    }

    #[test]
    fn typing_emits_one_tap_per_character_with_shift_where_needed() {
        let evs = type_text_events("Hi!");
        assert_eq!(evs.len(), 3);
        match &evs[0] {
            InputEvent::Keyboard { inputs, transition } => {
                assert_eq!(inputs, &vec!["h".to_string()]);
                assert_eq!(*transition, Transition::Tap);
            }
            other => panic!("expected keyboard event, got {other:?}"),
        }
        match &evs[2] {
            InputEvent::Keyboard { inputs, .. } => {
                assert_eq!(inputs, &vec!["left_shift".to_string(), "1".to_string()]);
            }
            other => panic!("expected keyboard event, got {other:?}"),
        }
    }

    /// A character with no C64 key is skipped so it cannot take the rest of the
    /// line down with it — the firmware rejects a bad batch in full.
    #[test]
    fn typing_skips_unmappable_characters_instead_of_failing() {
        assert_eq!(type_text_events("a~b").len(), 2);
    }

    #[test]
    fn a_resting_stick_produces_no_direction() {
        let pad = PadState {
            x: 0.1,
            y: -0.2,
            ..Default::default()
        };
        assert!(
            pad.to_inputs().is_empty(),
            "drift inside the deadzone must not hold a direction"
        );
    }

    #[test]
    fn a_pushed_stick_maps_to_directions_and_buttons() {
        let pad = PadState {
            x: -1.0,
            y: -1.0,
            fire: true,
            ..Default::default()
        };
        assert_eq!(
            pad.to_inputs(),
            set(&[JoyInput::Left, JoyInput::Up, JoyInput::Fire])
        );
    }

    #[test]
    fn only_the_dominant_horizontal_direction_is_sent() {
        // A stick cannot be left and right at once; the sign decides.
        let left = PadState {
            x: -0.9,
            ..Default::default()
        }
        .to_inputs();
        assert!(left.contains(&JoyInput::Left) && !left.contains(&JoyInput::Right));
    }

    #[test]
    fn first_press_emits_press_only() {
        let mut c = InputController::new();
        let evs = c.set_joystick(2, set(&[JoyInput::Up]));
        assert_eq!(evs.len(), 1);
        assert!(matches!(
            &evs[0],
            InputEvent::Joystick { port: 2, transition: Transition::Press, inputs } if inputs == &vec![JoyInput::Up]
        ));
    }

    /// The property that makes this playable: holding a direction costs no
    /// further requests.
    #[test]
    fn an_unchanged_position_emits_nothing() {
        let mut c = InputController::new();
        c.set_joystick(2, set(&[JoyInput::Up, JoyInput::Fire]));
        for _ in 0..100 {
            assert!(
                c.set_joystick(2, set(&[JoyInput::Up, JoyInput::Fire]))
                    .is_empty(),
                "a steady stick must not generate traffic"
            );
        }
    }

    #[test]
    fn a_change_emits_release_then_press_for_only_the_difference() {
        let mut c = InputController::new();
        c.set_joystick(2, set(&[JoyInput::Up, JoyInput::Fire]));
        let evs = c.set_joystick(2, set(&[JoyInput::Down, JoyInput::Fire]));
        assert_eq!(evs.len(), 2, "one release and one press");
        // Release must precede press, or the device briefly sees both.
        assert!(matches!(
            &evs[0],
            InputEvent::Joystick { transition: Transition::Release, inputs, .. } if inputs == &vec![JoyInput::Up]
        ));
        assert!(matches!(
            &evs[1],
            InputEvent::Joystick { transition: Transition::Press, inputs, .. } if inputs == &vec![JoyInput::Down]
        ));
    }

    #[test]
    fn releasing_everything_on_a_port_emits_a_release_for_it() {
        let mut c = InputController::new();
        c.set_joystick(1, set(&[JoyInput::Left]));
        let evs = c.set_joystick(1, BTreeSet::new());
        assert_eq!(evs.len(), 1);
        assert!(matches!(
            &evs[0],
            InputEvent::Joystick {
                transition: Transition::Release,
                ..
            }
        ));
        assert!(c.held(1).is_empty());
    }

    #[test]
    fn ports_are_tracked_independently() {
        let mut c = InputController::new();
        c.set_joystick(1, set(&[JoyInput::Left]));
        c.set_joystick(2, set(&[JoyInput::Right]));
        assert_eq!(c.held(1), set(&[JoyInput::Left]));
        assert_eq!(c.held(2), set(&[JoyInput::Right]));
        // Changing one must not disturb the other.
        c.set_joystick(1, BTreeSet::new());
        assert_eq!(c.held(2), set(&[JoyInput::Right]));
    }

    #[test]
    fn release_all_clears_state_and_is_a_no_op_when_idle() {
        let mut c = InputController::new();
        assert!(c.release_all().is_empty(), "nothing held, nothing to send");
        c.set_joystick(2, set(&[JoyInput::Fire]));
        let evs = c.release_all();
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], InputEvent::ReleaseAll));
        assert!(c.is_idle());
        assert!(c.release_all().is_empty(), "already released");
    }
}
