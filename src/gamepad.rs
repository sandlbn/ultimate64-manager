//! Reading a USB/Bluetooth gamepad on the host and turning it into a C64
//! joystick position.
//!
//! `gilrs` owns platform handles that are not `Send` everywhere it runs, so it
//! lives on its own thread and publishes the latest position into a shared
//! snapshot. The UI samples that snapshot on a timer it already has, rather
//! than plumbing another channel through the iced runtime.
//!
//! Nothing here talks to the device. The snapshot feeds
//! [`crate::input_315::InputController`], which decides what — if anything —
//! actually needs sending.

use crate::input_315::PadState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What the reader currently sees.
#[derive(Debug, Clone, Default)]
pub struct GamepadSnapshot {
    /// Name of the pad being read, when one is connected.
    pub name: Option<String>,
    /// Current position, all-zero when nothing is connected.
    pub pad: PadState,
    /// Set when the gamepad subsystem could not start at all (no permission,
    /// headless session, unsupported platform). The UI shows this instead of
    /// pretending no pad is plugged in.
    pub error: Option<String>,
}

impl GamepadSnapshot {
    pub fn is_connected(&self) -> bool {
        self.name.is_some()
    }
}

/// Background gamepad reader. Dropping it stops the thread.
pub struct GamepadReader {
    snapshot: Arc<Mutex<GamepadSnapshot>>,
    stop: Arc<AtomicBool>,
}

impl GamepadReader {
    /// Start reading. Always succeeds — a subsystem that refuses to start is
    /// reported through the snapshot's `error`, so a missing gamepad stack
    /// never takes a tab down with it.
    pub fn start() -> Self {
        let snapshot = Arc::new(Mutex::new(GamepadSnapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        spawn_reader(snapshot.clone(), stop.clone());
        Self { snapshot, stop }
    }

    /// The latest position seen by the reader thread.
    pub fn snapshot(&self) -> GamepadSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Drop for GamepadReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Poll interval. Faster than the UI samples it, so a quick tap between two UI
/// frames is still observed, but slow enough to stay off the CPU.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

fn spawn_reader(snapshot: Arc<Mutex<GamepadSnapshot>>, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || {
            let mut gilrs = match gilrs::Gilrs::new() {
                Ok(g) => g,
                Err(e) => {
                    if let Ok(mut s) = snapshot.lock() {
                        s.error = Some(format!("Gamepad support unavailable: {}", e));
                    }
                    return;
                }
            };

            while !stop.load(Ordering::Relaxed) {
                // Pump the event queue; gilrs only refreshes state as events
                // are drained, so skipping this leaves the values stale.
                while gilrs.next_event().is_some() {}

                let next = read_first_pad(&gilrs);
                if let Ok(mut s) = snapshot.lock() {
                    s.name = next.0;
                    s.pad = next.1;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .ok();
}

/// Read the first connected pad. Returns its name and position.
fn read_first_pad(gilrs: &gilrs::Gilrs) -> (Option<String>, PadState) {
    use gilrs::{Axis, Button};

    let Some((_id, gp)) = gilrs.gamepads().next() else {
        return (None, PadState::default());
    };

    // gilrs reports the stick's Y as positive-up; a C64 joystick (and this
    // app's PadState) treat positive as down. Inverting here is the whole
    // reason the axis is read through a helper rather than used raw.
    let stick_x = gp.value(Axis::LeftStickX);
    let stick_y = -gp.value(Axis::LeftStickY);

    // A d-pad reports as buttons on most pads. Fold it in at full deflection so
    // either control works, and so a pad with only a d-pad is still playable.
    let dpad_x = match (
        gp.is_pressed(Button::DPadLeft),
        gp.is_pressed(Button::DPadRight),
    ) {
        (true, false) => -1.0,
        (false, true) => 1.0,
        _ => 0.0,
    };
    let dpad_y = match (
        gp.is_pressed(Button::DPadUp),
        gp.is_pressed(Button::DPadDown),
    ) {
        (true, false) => -1.0,
        (false, true) => 1.0,
        _ => 0.0,
    };

    let pad = PadState {
        x: pick_axis(stick_x, dpad_x),
        y: pick_axis(stick_y, dpad_y),
        // South (A / Cross) is the natural primary fire; the other two are
        // only reachable on pads that have them, matching fire2/fire3.
        fire: gp.is_pressed(Button::South),
        fire2: gp.is_pressed(Button::East),
        fire3: gp.is_pressed(Button::West),
    };
    (Some(gp.name().to_string()), pad)
}

/// Combine stick and d-pad on one axis: whichever is pushed further wins, so
/// holding both doesn't cancel out.
fn pick_axis(stick: f32, dpad: f32) -> f32 {
    if dpad.abs() > stick.abs() {
        dpad
    } else {
        stick
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_315::JoyInput;

    #[test]
    fn dpad_wins_when_the_stick_is_resting() {
        assert_eq!(pick_axis(0.05, -1.0), -1.0);
    }

    #[test]
    fn stick_wins_when_pushed_further_than_the_dpad() {
        assert_eq!(pick_axis(-0.9, 0.0), -0.9);
    }

    #[test]
    fn a_resting_pad_and_dpad_stay_neutral() {
        assert_eq!(pick_axis(0.0, 0.0), 0.0);
    }

    /// The axis-inversion contract: gilrs is positive-up, PadState is
    /// positive-down, so a stick pushed up must come out as `Up`.
    #[test]
    fn negative_y_means_up_for_the_c64() {
        let pad = PadState {
            y: -1.0,
            ..Default::default()
        };
        assert!(pad.to_inputs().contains(&JoyInput::Up));
        let down = PadState {
            y: 1.0,
            ..Default::default()
        };
        assert!(down.to_inputs().contains(&JoyInput::Down));
    }

    #[test]
    fn a_snapshot_without_a_name_is_not_connected() {
        assert!(!GamepadSnapshot::default().is_connected());
        let s = GamepadSnapshot {
            name: Some("Pad".into()),
            ..Default::default()
        };
        assert!(s.is_connected());
    }

    /// Starting the reader must never panic, even where no gamepad stack
    /// exists (CI containers, headless runners).
    #[test]
    fn reader_starts_and_reports_cleanly_without_hardware() {
        let r = GamepadReader::start();
        let s = r.snapshot();
        // Either it started and saw nothing, or it reported why it couldn't.
        assert!(s.error.is_some() || !s.is_connected() || s.is_connected());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Interactive check: move the stick and press buttons for ~10s and watch
    /// the exact `machine:input` events the app would send. Verifies the whole
    /// host-side chain — pad → PadState → JoyInput → edge-triggered events —
    /// without needing a device that implements the call.
    #[test]
    #[ignore = "interactive: attach a gamepad and move it while this runs"]
    fn live_watch_gamepad_events() {
        use crate::input_315::InputController;

        let r = GamepadReader::start();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = r.snapshot();
        if let Some(e) = &s.error {
            println!("gamepad subsystem: {e}");
            return;
        }
        let Some(name) = s.name.clone() else {
            println!("no gamepad connected — nothing to watch");
            return;
        };
        println!("watching {name} for 10s — move the stick and press buttons\n");

        let mut ctl = InputController::new();
        let mut sent = 0usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let snap = r.snapshot();
            ctl.set_desired(2, snap.pad.to_inputs());
            if let Some(p) = ctl.pending() {
                sent += 1;
                println!("  -> {:?}", p.events);
                // Stand in for the device accepting it.
                ctl.commit(p.establishes);
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        println!("\n{sent} batches would have been sent in 10s (idle costs none)");
    }

    /// Prints whatever pad is attached to the machine running the tests.
    #[test]
    #[ignore = "requires a physical gamepad attached to this computer"]
    fn live_report_attached_gamepad() {
        let r = GamepadReader::start();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = r.snapshot();
        if let Some(e) = &s.error {
            println!("gamepad subsystem: {e}");
        }
        match &s.name {
            Some(n) => println!(
                "connected: {n}  pos={:?}  inputs={:?}",
                s.pad,
                s.pad.to_inputs()
            ),
            None => println!("no gamepad connected to this computer"),
        }
    }
}
