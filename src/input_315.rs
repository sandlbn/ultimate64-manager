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

/// The key combo that produces PETSCII `code`, for the keys that have no
/// printable character.
///
/// This exists because [`crate::petscii::byte_to_char`] must not be used for
/// this job: it is a *display* helper that folds every control code in
/// `0x00..=0x1F` to a space. Routing the keyboard through it turned RETURN
/// (13) into SPACE, so pressing Enter printed a space and the cursor moved one
/// to the right — the key appeared to work while doing something else entirely.
///
/// C64 layout notes: the second bank of function keys and the up/left cursor
/// directions are shifted forms, not keys of their own.
pub fn keys_for_petscii(code: u8) -> Option<&'static [&'static str]> {
    const RETURN: &[&str] = &["return"];
    const DEL: &[&str] = &["inst_del"];
    const INST: &[&str] = &["left_shift", "inst_del"];
    const HOME: &[&str] = &["clr_home"];
    const CLR: &[&str] = &["left_shift", "clr_home"];
    const DOWN: &[&str] = &["cursor_up_down"];
    const UP: &[&str] = &["left_shift", "cursor_up_down"];
    const RIGHT: &[&str] = &["cursor_left_right"];
    const LEFT: &[&str] = &["left_shift", "cursor_left_right"];
    const RUN_STOP: &[&str] = &["run_stop"];
    const F1: &[&str] = &["f1"];
    const F3: &[&str] = &["f3"];
    const F5: &[&str] = &["f5"];
    const F7: &[&str] = &["f7"];
    const F2: &[&str] = &["left_shift", "f1"];
    const F4: &[&str] = &["left_shift", "f3"];
    const F6: &[&str] = &["left_shift", "f5"];
    const F8: &[&str] = &["left_shift", "f7"];

    Some(match code {
        3 => RUN_STOP,
        13 | 141 => RETURN, // 141 is shift+RETURN
        17 => DOWN,
        19 => HOME,
        20 => DEL,
        29 => RIGHT,
        133 => F1,
        134 => F3,
        135 => F5,
        136 => F7,
        137 => F2,
        138 => F4,
        139 => F6,
        140 => F8,
        145 => UP,
        147 => CLR,
        148 => INST,
        157 => LEFT,
        _ => return None,
    })
}

/// The events that reproduce one PETSCII byte on the C64 keyboard.
///
/// Control codes are mapped by [`keys_for_petscii`]; everything else goes
/// through the printable-character table. An unmappable byte yields no events
/// rather than a wrong key.
pub fn events_for_petscii(code: u8) -> Vec<InputEvent> {
    if let Some(keys) = keys_for_petscii(code) {
        return vec![InputEvent::Keyboard {
            inputs: keys.iter().map(|k| k.to_string()).collect(),
            transition: Transition::Tap,
        }];
    }
    // Printable range only — never fold a control code to a space.
    let ch = match code {
        0x20..=0x7e => code as char,
        _ => return Vec::new(),
    };
    type_text_events(&ch.to_string())
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

/// What the device has been asked to hold, per port.
pub type PortState = Vec<(u8, BTreeSet<JoyInput>)>;

/// A batch waiting to go out, plus the state it establishes once accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInput {
    pub events: Vec<InputEvent>,
    /// Commit this with [`InputController::commit`] *after* the device accepts
    /// the batch — never before.
    pub establishes: PortState,
}

/// Tracks what the pad wants versus what the device has confirmed, so only
/// changes are sent and a failed send corrects itself.
///
/// The split matters. Recording a press the moment it is sent means a request
/// that times out leaves this side believing the device holds something it
/// does not, and nothing ever corrects it — the direction stays dead until it
/// is moved away and back. Holding `confirmed` until the device answers makes
/// the next poll re-send the same difference, so a dropped request heals on its
/// own within a frame.
#[derive(Debug, Default)]
pub struct InputController {
    desired: HashMap<u8, BTreeSet<JoyInput>>,
    confirmed: HashMap<u8, BTreeSet<JoyInput>>,
}

impl InputController {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the device is believed to be holding — what the UI should show.
    pub fn held(&self, port: u8) -> BTreeSet<JoyInput> {
        self.confirmed.get(&port).cloned().unwrap_or_default()
    }

    /// Whether nothing is held or wanted.
    pub fn is_idle(&self) -> bool {
        self.desired.values().all(|h| h.is_empty()) && self.confirmed.values().all(|h| h.is_empty())
    }

    /// Record where the pad is now. Cheap and lossless: call it every frame.
    pub fn set_desired(&mut self, port: u8, inputs: BTreeSet<JoyInput>) {
        self.desired.insert(port, inputs);
    }

    /// The difference the device still needs, or `None` when it is up to date.
    ///
    /// Only the difference is sent, so a steady direction costs nothing at all;
    /// and because this is recomputed from `confirmed` each time, a burst of
    /// movement during an in-flight request collapses into one batch rather
    /// than a queue of stale ones.
    pub fn pending(&self) -> Option<PendingInput> {
        let mut events = Vec::new();
        let mut establishes = Vec::new();

        let mut ports: Vec<u8> = self.desired.keys().copied().collect();
        ports.sort_unstable();
        for port in ports {
            let want = self.desired.get(&port).cloned().unwrap_or_default();
            let have = self.held(port);
            if want == have {
                continue;
            }
            let to_release: Vec<JoyInput> = have.difference(&want).copied().collect();
            let to_press: Vec<JoyInput> = want.difference(&have).copied().collect();
            // Release first: pressing the opposite direction before letting the
            // old one go would briefly show both.
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
            establishes.push((port, want));
        }

        if events.is_empty() {
            None
        } else {
            Some(PendingInput {
                events,
                establishes,
            })
        }
    }

    /// Accept a batch the device confirmed.
    pub fn commit(&mut self, establishes: PortState) {
        for (port, state) in establishes {
            self.confirmed.insert(port, state);
        }
    }

    /// Drop everything, locally and on the device.
    ///
    /// Worth sending whenever control is handed back — closing the panel,
    /// losing the pad, leaving the tab — or a held direction stays stuck on the
    /// device with nothing left to release it.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let had_any = !self.is_idle();
        self.desired.clear();
        self.confirmed.clear();
        if had_any {
            vec![InputEvent::ReleaseAll]
        } else {
            Vec::new()
        }
    }

    /// Forget what the device was holding without sending anything — for when
    /// it has already dropped everything itself, as it does on reset.
    pub fn forget(&mut self) {
        self.confirmed.clear();
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

    fn commit_all(c: &mut InputController) {
        if let Some(p) = c.pending() {
            c.commit(p.establishes);
        }
    }

    #[test]
    fn first_press_emits_press_only() {
        let mut c = InputController::new();
        c.set_desired(2, set(&[JoyInput::Up]));
        let p = c.pending().expect("a new direction must be sent");
        assert_eq!(p.events.len(), 1);
        assert!(matches!(
            &p.events[0],
            InputEvent::Joystick { port: 2, transition: Transition::Press, inputs } if inputs == &vec![JoyInput::Up]
        ));
    }

    /// The property that makes this playable: holding a direction costs no
    /// further requests once the device has confirmed it.
    #[test]
    fn an_unchanged_position_emits_nothing_once_confirmed() {
        let mut c = InputController::new();
        c.set_desired(2, set(&[JoyInput::Up, JoyInput::Fire]));
        commit_all(&mut c);
        for _ in 0..100 {
            c.set_desired(2, set(&[JoyInput::Up, JoyInput::Fire]));
            assert!(
                c.pending().is_none(),
                "a steady stick must not generate traffic"
            );
        }
    }

    #[test]
    fn a_change_emits_release_then_press_for_only_the_difference() {
        let mut c = InputController::new();
        c.set_desired(2, set(&[JoyInput::Up, JoyInput::Fire]));
        commit_all(&mut c);
        c.set_desired(2, set(&[JoyInput::Down, JoyInput::Fire]));
        let p = c.pending().expect("the change must be sent");
        assert_eq!(p.events.len(), 2, "one release and one press");
        assert!(matches!(
            &p.events[0],
            InputEvent::Joystick { transition: Transition::Release, inputs, .. } if inputs == &vec![JoyInput::Up]
        ));
        assert!(matches!(
            &p.events[1],
            InputEvent::Joystick { transition: Transition::Press, inputs, .. } if inputs == &vec![JoyInput::Down]
        ));
    }

    /// Nothing is believed held until the device says so. A send that is never
    /// committed (it timed out) must leave the difference outstanding.
    #[test]
    fn state_is_only_believed_after_the_device_confirms_it() {
        let mut c = InputController::new();
        c.set_desired(2, set(&[JoyInput::Left]));
        assert!(c.pending().is_some());
        assert!(c.held(2).is_empty(), "nothing is held until confirmed");

        // Pretend the request failed: no commit.
        assert!(c.pending().is_some(), "the difference is still outstanding");

        let p = c.pending().unwrap();
        c.commit(p.establishes);
        assert_eq!(c.held(2), set(&[JoyInput::Left]));
        assert!(c.pending().is_none());
    }

    /// A failed send must not be replayed. In a game the stick has already
    /// moved on, so the retry has to carry the *current* position.
    #[test]
    fn a_retry_sends_the_newest_position_not_the_stale_one() {
        let mut c = InputController::new();
        c.set_desired(2, set(&[JoyInput::Left]));
        let stale = c.pending().expect("first batch");
        // That request fails — never committed. Meanwhile the stick moves.
        c.set_desired(2, set(&[JoyInput::Right]));

        let fresh = c.pending().expect("retry");
        assert_ne!(fresh.events, stale.events, "must not replay the old batch");
        assert!(
            matches!(
                &fresh.events[0],
                InputEvent::Joystick { transition: Transition::Press, inputs, .. }
                    if inputs == &vec![JoyInput::Right]
            ),
            "retry must carry the current direction, got {:?}",
            fresh.events
        );
    }

    /// Several changes during one in-flight request collapse into a single
    /// follow-up rather than a queue of stale batches.
    #[test]
    fn rapid_movement_collapses_into_one_batch() {
        let mut c = InputController::new();
        for dir in [
            JoyInput::Up,
            JoyInput::Down,
            JoyInput::Left,
            JoyInput::Right,
        ] {
            c.set_desired(2, set(&[dir]));
        }
        let p = c.pending().expect("one batch");
        assert_eq!(p.events.len(), 1, "only the final position matters");
        assert!(matches!(
            &p.events[0],
            InputEvent::Joystick { inputs, .. } if inputs == &vec![JoyInput::Right]
        ));
    }

    #[test]
    fn releasing_everything_on_a_port_emits_a_release_for_it() {
        let mut c = InputController::new();
        c.set_desired(1, set(&[JoyInput::Left]));
        commit_all(&mut c);
        c.set_desired(1, BTreeSet::new());
        let p = c.pending().expect("the release must be sent");
        assert!(matches!(
            &p.events[0],
            InputEvent::Joystick {
                transition: Transition::Release,
                ..
            }
        ));
        c.commit(p.establishes);
        assert!(c.held(1).is_empty());
    }

    #[test]
    fn ports_are_tracked_independently() {
        let mut c = InputController::new();
        c.set_desired(1, set(&[JoyInput::Left]));
        c.set_desired(2, set(&[JoyInput::Right]));
        commit_all(&mut c);
        assert_eq!(c.held(1), set(&[JoyInput::Left]));
        assert_eq!(c.held(2), set(&[JoyInput::Right]));
        c.set_desired(1, BTreeSet::new());
        commit_all(&mut c);
        assert_eq!(
            c.held(2),
            set(&[JoyInput::Right]),
            "the other port is untouched"
        );
    }

    #[test]
    fn release_all_clears_state_and_is_a_no_op_when_idle() {
        let mut c = InputController::new();
        assert!(c.release_all().is_empty(), "nothing held, nothing to send");
        c.set_desired(2, set(&[JoyInput::Fire]));
        let evs = c.release_all();
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], InputEvent::ReleaseAll));
        assert!(c.is_idle());
        assert!(c.release_all().is_empty(), "already released");
    }
}

#[cfg(test)]
mod petscii_key_tests {
    use super::*;

    fn keys_of(evs: &[InputEvent]) -> Vec<String> {
        match evs.first() {
            Some(InputEvent::Keyboard { inputs, .. }) => inputs.clone(),
            _ => Vec::new(),
        }
    }

    /// The regression this fixes: Enter is PETSCII 13, which the display
    /// helper folded to a space. The C64 then printed a space and the cursor
    /// stepped one to the right — the key looked like it worked.
    #[test]
    fn enter_sends_return_and_never_space() {
        let evs = events_for_petscii(13);
        assert_eq!(keys_of(&evs), vec!["return".to_string()]);
        assert!(
            !keys_of(&evs).contains(&"space".to_string()),
            "RETURN must not degrade to SPACE"
        );
    }

    /// Every control code must map to its own key, not collapse to space.
    /// `byte_to_char` returns ' ' for all of 0x00..=0x1F, which is why these
    /// are listed out explicitly.
    #[test]
    fn control_codes_map_to_their_own_keys() {
        for (code, expected) in [
            (3u8, vec!["run_stop"]),
            (13, vec!["return"]),
            (17, vec!["cursor_up_down"]),
            (19, vec!["clr_home"]),
            (20, vec!["inst_del"]),
            (29, vec!["cursor_left_right"]),
            (145, vec!["left_shift", "cursor_up_down"]),
            (147, vec!["left_shift", "clr_home"]),
            (148, vec!["left_shift", "inst_del"]),
            (157, vec!["left_shift", "cursor_left_right"]),
        ] {
            assert_eq!(keys_of(&events_for_petscii(code)), expected, "code {code}");
        }
    }

    /// F2/F4/F6/F8 are shifted forms of F1/F3/F5/F7 on a C64.
    #[test]
    fn function_keys_use_shift_for_the_second_bank() {
        assert_eq!(keys_of(&events_for_petscii(133)), vec!["f1"]);
        assert_eq!(keys_of(&events_for_petscii(134)), vec!["f3"]);
        assert_eq!(keys_of(&events_for_petscii(135)), vec!["f5"]);
        assert_eq!(keys_of(&events_for_petscii(136)), vec!["f7"]);
        assert_eq!(keys_of(&events_for_petscii(137)), vec!["left_shift", "f1"]);
        assert_eq!(keys_of(&events_for_petscii(140)), vec!["left_shift", "f7"]);
    }

    #[test]
    fn printable_characters_still_work() {
        assert_eq!(keys_of(&events_for_petscii(b'a')), vec!["a"]);
        assert_eq!(keys_of(&events_for_petscii(b' ')), vec!["space"]);
        assert_eq!(keys_of(&events_for_petscii(b'1')), vec!["1"]);
        assert_eq!(keys_of(&events_for_petscii(b'!')), vec!["left_shift", "1"]);
    }

    /// Every combo must name keys the firmware accepts, or it rejects the whole
    /// batch and the keypress silently vanishes.
    #[test]
    fn every_petscii_mapping_names_valid_keys() {
        for code in 0u8..=255 {
            for k in keys_of(&events_for_petscii(code)) {
                assert!(is_valid_key(&k), "code {code} produced invalid key {k:?}");
            }
        }
    }

    /// An unmappable control code must produce nothing rather than a wrong key.
    #[test]
    fn unmapped_control_codes_send_nothing() {
        // 5 is "white text" — a colour code, not a key.
        assert!(events_for_petscii(5).is_empty());
        assert!(events_for_petscii(18).is_empty()); // RVS ON
    }
}
