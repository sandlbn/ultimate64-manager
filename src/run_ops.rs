//! Shared "run a disk on the device" logic.
//!
//! Autoloading a disk from BASIC is `RESET` → wait for the `READY.` prompt →
//! `LOAD"*",<dev>,1` → wait for the load to finish → `RUN`. The three call
//! sites that used to do this ([`crate::api::run_disk`], the local file browser,
//! and the Assembly64 browser) each hard-coded blind `sleep(3s)` + `sleep(5s)`
//! delays, which are simultaneously too long for fast loads and too short for
//! big multi-load disks. This module replaces those with adaptive polling of the
//! C64 screen RAM and is the single source of truth for the sequence.
//!
//! All functions here are blocking and must run inside `spawn_blocking`.

use std::time::{Duration, Instant};

use crate::remote_device::RemoteDevice;

/// C64 text screen RAM base address and length (40×25 = 1000 bytes).
const SCREEN_BASE: u16 = 0x0400;
const SCREEN_LEN: u16 = 1000;

/// Screen codes for `READY.` — R E A D Y . (uppercase screen-code set).
const READY_CODES: [u8; 6] = [18, 5, 1, 4, 25, 46];

/// Upper bound on how long to wait for BASIC to reach the `READY.` prompt after
/// a reset. Real boot is ~2 s; the cap only matters if screen reads keep failing.
const READY_TIMEOUT: Duration = Duration::from_secs(8);

/// Upper bound on how long to wait for a `LOAD` to complete before issuing `RUN`
/// anyway. Generous because large disks legitimately take tens of seconds.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);

/// Fallback delay used only when the screen can't be read at all (e.g. a
/// firmware that rejects the memory read) — mirrors the old fixed behavior.
const FALLBACK_BOOT: Duration = Duration::from_secs(3);

/// Interval between screen polls.
const POLL: Duration = Duration::from_millis(200);

/// Count non-overlapping-enough occurrences of `READY.` in a screen snapshot.
fn ready_count(screen: &[u8]) -> usize {
    screen
        .windows(READY_CODES.len())
        .filter(|w| *w == READY_CODES)
        .count()
}

/// Poll screen RAM until `READY.` appears at least `min_count` times, or the
/// deadline passes. Returns `Some(true)` if the target was reached, `Some(false)`
/// if it timed out but screen reads worked, and `None` if screen reads never
/// succeeded (so the caller can apply a time-based fallback).
fn wait_for_ready(conn: &dyn RemoteDevice, min_count: usize, timeout: Duration) -> Option<bool> {
    let deadline = Instant::now() + timeout;
    let mut ever_read = false;
    loop {
        if let Ok(screen) = conn.read_mem(SCREEN_BASE, SCREEN_LEN) {
            ever_read = true;
            if ready_count(&screen) >= min_count {
                return Some(true);
            }
        }
        if Instant::now() >= deadline {
            return if ever_read { Some(false) } else { None };
        }
        std::thread::sleep(POLL);
    }
}

/// Reset the machine and autoload the disk currently mounted on `device_num`
/// (`"8"` or `"9"`): `RESET` → wait for `READY.` → `LOAD"*",<dev>,1` → wait for
/// the load to finish → `RUN`. Timing is adaptive; fixed sleeps are used only as
/// a fallback when screen RAM can't be read.
pub fn autoload_mounted_disk(conn: &dyn RemoteDevice, device_num: &str) -> Result<(), String> {
    autoload_with(conn, device_num, READY_TIMEOUT, LOAD_TIMEOUT)
}

/// Core of [`autoload_mounted_disk`] with injectable timeouts (tests pass tiny
/// values so they don't wait out the real multi-second caps).
fn autoload_with(
    conn: &dyn RemoteDevice,
    device_num: &str,
    ready_timeout: Duration,
    load_timeout: Duration,
) -> Result<(), String> {
    conn.reset().map_err(|e| format!("Reset failed: {}", e))?;

    // Wait for the boot `READY.` prompt, then note how many are on screen so we
    // can detect a *fresh* one after the load.
    let baseline = match wait_for_ready(conn, 1, ready_timeout) {
        Some(_) => ready_count(&conn.read_mem(SCREEN_BASE, SCREEN_LEN).unwrap_or_default()),
        None => {
            // Screen unreadable — fall back to the old fixed delay.
            std::thread::sleep(FALLBACK_BOOT);
            0
        }
    };

    let load_cmd = format!("load\"*\",{},1\n", device_num);
    conn.type_text(&load_cmd)
        .map_err(|e| format!("Type LOAD failed: {}", e))?;

    // Wait for the load to finish: a fresh `READY.` beyond the boot one. If the
    // screen can't be read, fall back to a fixed wait so RUN isn't sent mid-load.
    if wait_for_ready(conn, baseline + 1, load_timeout).is_none() {
        std::thread::sleep(Duration::from_secs(5));
    }

    conn.type_text("run\n")
        .map_err(|e| format!("Type RUN failed: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_count_matches_screen_codes() {
        let mut screen = vec![0x20u8; SCREEN_LEN as usize];
        assert_eq!(ready_count(&screen), 0);
        screen[80..86].copy_from_slice(&READY_CODES);
        assert_eq!(ready_count(&screen), 1);
        screen[200..206].copy_from_slice(&READY_CODES);
        assert_eq!(ready_count(&screen), 2);
    }

    #[test]
    fn autoload_resets_loads_and_runs() {
        use crate::remote_device::mock::MockDevice;
        // MockDevice's read_mem returns a fill byte, never READY, so both ready
        // polls hit the Some(false) timeout path. Tiny timeouts keep the test
        // fast; we assert the reset → LOAD → RUN ordering is preserved.
        let dev = MockDevice::new();
        let handle = dev.calls.clone();
        autoload_with(
            &dev,
            "8",
            Duration::from_millis(30),
            Duration::from_millis(30),
        )
        .unwrap();
        let calls = handle.lock().unwrap().clone();
        assert_eq!(calls.first().map(String::as_str), Some("reset"));
        let load_pos = calls
            .iter()
            .position(|c| c.contains("type_text") && c.contains("load"))
            .expect("LOAD issued");
        let run_pos = calls
            .iter()
            .position(|c| c == "type_text(\"run\\n\")")
            .expect("RUN issued");
        assert!(load_pos < run_pos, "LOAD must precede RUN");
    }
}
