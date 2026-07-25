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
/// a reset. Real boot is ~2 s, but a device that's mid-reboot or busy mounting a
/// large disk can take noticeably longer, so the cap is generous.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Upper bound on how long to wait for a `LOAD` to complete before issuing `RUN`
/// anyway. Generous because large disks legitimately take tens of seconds.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);

/// After the boot/load `READY.` appears, wait this long before typing so the
/// screen editor is actually accepting keystrokes. Without it the first
/// characters of `LOAD"*",8,1` can be dropped and only the tail (e.g. `,1`)
/// lands on screen.
const PROMPT_SETTLE: Duration = Duration::from_millis(600);

/// Fallback delay used only when the screen can't be read at all (e.g. a
/// firmware that rejects the memory read) — mirrors the old fixed behavior.
const FALLBACK_BOOT: Duration = Duration::from_secs(3);

/// Upper bound on how long to wait for the freshly-mounted disk to show up in
/// the drive list before resetting anyway. Normally satisfied in a poll or two.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between screen / drive-list polls.
const POLL: Duration = Duration::from_millis(200);

/// Which drive letter `device_num` (`"8"`/`"9"`) maps to in the drive list.
fn drive_key_for(device_num: &str) -> &'static str {
    if device_num == "9" {
        "b"
    } else {
        "a"
    }
}

/// Poll the drive list until the target drive reports a mounted image, so we
/// never reset the machine before the disk is actually in place. Same return
/// contract as [`poll_screen`]: `Some(true)` = confirmed, `Some(false)` = timed
/// out but the list was readable, `None` = the list never read.
fn wait_for_mount(conn: &dyn RemoteDevice, device_num: &str, timeout: Duration) -> Option<bool> {
    let want = drive_key_for(device_num);
    let deadline = Instant::now() + timeout;
    let mut ever_read = false;
    loop {
        if let Ok(list) = conn.drive_list() {
            ever_read = true;
            if list
                .iter()
                .any(|(name, d)| name.eq_ignore_ascii_case(want) && d.image_file.is_some())
            {
                return Some(true);
            }
        }
        if Instant::now() >= deadline {
            return if ever_read { Some(false) } else { None };
        }
        std::thread::sleep(POLL);
    }
}

/// Count non-overlapping-enough occurrences of `READY.` in a screen snapshot.
fn ready_count(screen: &[u8]) -> usize {
    screen
        .windows(READY_CODES.len())
        .filter(|w| *w == READY_CODES)
        .count()
}

/// Poll screen RAM until `pred` holds, or the deadline passes. Returns
/// `Some(true)` if `pred` was satisfied, `Some(false)` if it timed out but
/// screen reads worked, and `None` if screen reads never succeeded (so the
/// caller can apply a time-based fallback).
fn poll_screen(
    conn: &dyn RemoteDevice,
    timeout: Duration,
    pred: impl Fn(&[u8]) -> bool,
) -> Option<bool> {
    let deadline = Instant::now() + timeout;
    let mut ever_read = false;
    loop {
        if let Ok(screen) = conn.read_mem(SCREEN_BASE, SCREEN_LEN) {
            ever_read = true;
            if pred(&screen) {
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
/// (`"8"` or `"9"`): `RESET` → wait for the *fresh* `READY.` → `LOAD"*",<dev>,1`
/// → wait for the load to finish → `RUN`. Timing is adaptive; fixed sleeps are
/// used only as a fallback when screen RAM can't be read.
pub fn autoload_mounted_disk(conn: &dyn RemoteDevice, device_num: &str) -> Result<(), String> {
    autoload_with(
        conn,
        device_num,
        READY_TIMEOUT,
        LOAD_TIMEOUT,
        PROMPT_SETTLE,
        MOUNT_TIMEOUT,
    )
}

/// Core of [`autoload_mounted_disk`] with injectable timeouts + settle (tests
/// pass tiny values so they don't wait out the real multi-second caps).
fn autoload_with(
    conn: &dyn RemoteDevice,
    device_num: &str,
    ready_timeout: Duration,
    load_timeout: Duration,
    settle: Duration,
    mount_timeout: Duration,
) -> Result<(), String> {
    // Confirm the disk is actually mounted before touching reset — otherwise a
    // fast reset could beat the mount and the machine would boot to an empty or
    // stale drive. If the drive list can't be read at all, fall back to a short
    // fixed settle (the mount request itself already returned success).
    if wait_for_mount(conn, device_num, mount_timeout).is_none() {
        std::thread::sleep(Duration::from_millis(500));
    }

    conn.reset().map_err(|e| format!("Reset failed: {}", e))?;

    // Phase 1: wait for the reset to clear the previous screen. A `READY.` left
    // over from before the reset is still in screen RAM for a moment, so without
    // this we'd latch onto it and start typing while the machine is still
    // rebooting — which is exactly why only the tail of the LOAD line survives.
    let cleared = poll_screen(conn, ready_timeout, |s| ready_count(s) == 0);

    // Phase 2: wait for the fresh boot `READY.` prompt.
    let booted = poll_screen(conn, ready_timeout, |s| ready_count(s) >= 1);

    if cleared.is_none() && booted.is_none() {
        // Screen never readable — fall back to a fixed boot delay.
        std::thread::sleep(FALLBACK_BOOT);
    } else {
        // Let the editor settle so the whole LOAD line is accepted, not just its
        // last characters.
        std::thread::sleep(settle);
    }

    // Number of `READY.` now on screen, so we can detect a *fresh* one post-load.
    let baseline = ready_count(&conn.read_mem(SCREEN_BASE, SCREEN_LEN).unwrap_or_default());

    let load_cmd = format!("load\"*\",{},1\n", device_num);
    conn.type_text(&load_cmd)
        .map_err(|e| format!("Type LOAD failed: {}", e))?;

    // Wait for the load to finish: a fresh `READY.` beyond the boot one. If the
    // screen can't be read, fall back to a fixed wait so RUN isn't sent mid-load;
    // otherwise settle briefly so the RUN line is fully accepted too.
    match poll_screen(conn, load_timeout, |s| ready_count(s) > baseline) {
        None => std::thread::sleep(Duration::from_secs(5)),
        _ => std::thread::sleep(settle),
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
            Duration::from_millis(1),
            Duration::from_millis(30),
        )
        .unwrap();
        let calls = handle.lock().unwrap().clone();
        // Mount is confirmed (drive_list) before the machine is reset.
        let drive_pos = calls
            .iter()
            .position(|c| c == "drive_list")
            .expect("mount confirmed");
        let reset_pos = calls
            .iter()
            .position(|c| c == "reset")
            .expect("reset issued");
        let load_pos = calls
            .iter()
            .position(|c| c.contains("type_text") && c.contains("load"))
            .expect("LOAD issued");
        let run_pos = calls
            .iter()
            .position(|c| c == "type_text(\"run\\n\")")
            .expect("RUN issued");
        assert!(drive_pos < reset_pos, "mount confirmed before reset");
        assert!(reset_pos < load_pos, "reset must precede LOAD");
        assert!(load_pos < run_pos, "LOAD must precede RUN");
    }
}
