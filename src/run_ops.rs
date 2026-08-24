//! Shared "run a disk on the device" logic.
//!
//! **Preferred path — deterministic, no keyboard, no timing:** boot a disk by
//! DMA-loading its first directory program via `run_prg`, which is exactly
//! equivalent to `LOAD"*",8,1:RUN` (loads the first file to its embedded address
//! and runs it). The disk stays mounted, so multi-load games and disk-based
//! loaders still find the emulated drive.
//!
//! **Fallback — GCR (G64/…) or non-PRG-first disks** that can't be parsed to a
//! first PRG: drive BASIC by keyboard, but robustly — `RESET` → wait for the
//! *flashing-cursor* READY prompt (live zero-page flags, not a blind sleep) →
//! inject `LOAD"*",<dev>,1` ≤10 chars at a time, **polling the keyboard buffer
//! count back to 0 between chunks** so nothing is dropped → wait for the load →
//! `RUN`. No fixed delays anywhere.
//!
//! All functions here are blocking and must run inside `spawn_blocking`.

use std::time::{Duration, Instant};

use crate::remote_device::RemoteDevice;

/// C64 text screen RAM base address and length (40×25 = 1000 bytes).
const SCREEN_BASE: u16 = 0x0400;
const SCREEN_LEN: u16 = 1000;

/// Screen codes for `READY.` — R E A D Y . (uppercase screen-code set).
const READY_CODES: [u8; 6] = [18, 5, 1, 4, 25, 46];

// Zero-page flags that together mean "BASIC is idle at the flashing-cursor
// prompt and will accept keystrokes" (see 64MAP10 / C64-Wiki).
const LSTX: u16 = 0x00C5; // matrix code of the last key (must be reset before inject)
const NDX: u16 = 0x00C6; // keyboard buffer fill count (0 = empty)
const CURSOR_BLINK: u16 = 0x00CC; // 0 = cursor blinking / editor idle
const INPUT_SRC: u16 = 0x00D0; // 0 = pulling input from the keyboard
const KEYBUF: u16 = 0x0277; // 10-byte keyboard buffer queue

/// Cap waiting for the boot `READY.` on screen after a reset (generous for slow
/// reboots / big-disk mounts).
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap waiting for a `LOAD` to finish before issuing `RUN`.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);
/// Cap waiting for the zero-page ready flags to line up.
const READY_FLAGS_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap waiting for the KERNAL to drain one 10-byte keyboard chunk.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
/// Last-ditch delay used only when memory is entirely unreadable.
const FALLBACK_BOOT: Duration = Duration::from_secs(3);
/// Poll interval for every wait below.
const POLL: Duration = Duration::from_millis(80);

/// Count non-overlapping occurrences of `READY.` in a screen snapshot.
fn ready_count(screen: &[u8]) -> usize {
    screen
        .windows(READY_CODES.len())
        .filter(|w| *w == READY_CODES)
        .count()
}

/// Poll `read_mem(addr, len)` until `pred` holds, or the deadline passes.
/// `Some(true)` = matched, `Some(false)` = timed out but reads worked,
/// `None` = reads never succeeded (caller may apply a time-based fallback).
fn poll_mem(
    conn: &dyn RemoteDevice,
    addr: u16,
    len: u16,
    timeout: Duration,
    pred: impl Fn(&[u8]) -> bool,
) -> Option<bool> {
    let deadline = Instant::now() + timeout;
    let mut ever_read = false;
    loop {
        if let Ok(bytes) = conn.read_mem(addr, len) {
            ever_read = true;
            if pred(&bytes) {
                return Some(true);
            }
        }
        if Instant::now() >= deadline {
            return if ever_read { Some(false) } else { None };
        }
        std::thread::sleep(POLL);
    }
}

/// Wait until BASIC is idle at the flashing-cursor prompt and ready to take
/// keys: `$C6 (NDX)==0`, `$CC==0` (blinking), `$D0==0` (keyboard input source).
/// Reads the 11-byte span `$C6..=$D0` in one request.
fn wait_basic_ready(conn: &dyn RemoteDevice, timeout: Duration) -> Option<bool> {
    let cc = (CURSOR_BLINK - NDX) as usize; // 6
    let d0 = (INPUT_SRC - NDX) as usize; // 10
    poll_mem(conn, NDX, 11, timeout, move |b| {
        b.first() == Some(&0) && b.get(cc) == Some(&0) && b.get(d0) == Some(&0)
    })
}

/// PETSCII bytes for a command line; `\n`/`\r` become RETURN (`$0D`).
fn to_petscii(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            '\n' | '\r' => 0x0D,
            other => ultimate64::petscii::Petscii::from_str_lossy(&other.to_string())[0],
        })
        .collect()
}

/// Inject PETSCII into the keyboard buffer, ≤10 bytes per chunk, polling `$C6`
/// back to 0 between chunks so the KERNAL has consumed the previous keys before
/// more are queued. This is what keeps the LOAD line from being truncated —
/// no blind inter-chunk sleep.
fn inject_keys(conn: &dyn RemoteDevice, petscii: &[u8]) -> Result<(), String> {
    for chunk in petscii.chunks(10) {
        // Reset the last-key state + buffer count first. Verified on real
        // hardware: without clearing $C5 the KERNAL silently swallows the
        // injected keys (nothing reaches the screen); with it they type
        // correctly. This 2-byte write zeroes $C5 (LSTX) and $C6 (NDX).
        conn.write_mem(LSTX, &[0, 0])
            .map_err(|e| format!("keyboard reset failed: {}", e))?;
        conn.write_mem(KEYBUF, chunk)
            .map_err(|e| format!("keyboard write failed: {}", e))?;
        conn.write_mem(NDX, &[chunk.len() as u8])
            .map_err(|e| format!("keyboard count failed: {}", e))?;
        // Wait for the machine to drain this chunk before queueing the next. If
        // it never drains (not actually at a prompt) we stop waiting and move on
        // rather than hang.
        let _ = poll_mem(conn, NDX, 1, DRAIN_TIMEOUT, |b| b.first() == Some(&0));
    }
    Ok(())
}

/// Boot the (already-mounted) disk. Prefers the DMA path: parse the image and
/// DMA-load+run its first directory PRG via `run_prg`. Falls back to the
/// keyboard sequence for GCR / non-PRG-first disks (or when `image_bytes` is
/// `None`, e.g. the bytes couldn't be obtained).
pub fn boot_mounted_disk(
    conn: &dyn RemoteDevice,
    device_num: &str,
    image_bytes: Option<&[u8]>,
) -> Result<(), String> {
    if let Some(bytes) = image_bytes {
        if let Some((name, prg)) = crate::disk_image::extract_first_prg(bytes) {
            log::info!(
                "Disk boot: DMA-loading first PRG \"{}\" ({} bytes)",
                name,
                prg.len()
            );
            return conn
                .run_prg(&prg)
                .map_err(|e| format!("DMA run failed: {}", e));
        }
    }
    log::info!("Disk boot: no parseable first PRG — using keyboard LOAD\"*\"");
    autoload_mounted_disk(conn, device_num)
}

/// Reset the machine and autoload the disk mounted on `device_num` (`"8"`/`"9"`)
/// via the keyboard: `RESET` → flashing-cursor READY → `LOAD"*",<dev>,1` → wait
/// → `RUN`. Timing is entirely poll-driven; a fixed sleep is used only when
/// memory can't be read at all.
pub fn autoload_mounted_disk(conn: &dyn RemoteDevice, device_num: &str) -> Result<(), String> {
    autoload_with(
        conn,
        device_num,
        READY_TIMEOUT,
        LOAD_TIMEOUT,
        READY_FLAGS_TIMEOUT,
    )
}

/// Core of [`autoload_mounted_disk`] with injectable timeouts (tests pass tiny
/// values so they don't wait out the real multi-second caps).
fn autoload_with(
    conn: &dyn RemoteDevice,
    device_num: &str,
    ready_timeout: Duration,
    load_timeout: Duration,
    flags_timeout: Duration,
) -> Result<(), String> {
    conn.reset().map_err(|e| format!("Reset failed: {}", e))?;

    // Let the reset blank the previous screen (so a leftover READY. isn't
    // mistaken for the fresh prompt), then wait for the boot READY. to appear.
    let cleared = poll_mem(conn, SCREEN_BASE, SCREEN_LEN, ready_timeout, |s| {
        ready_count(s) == 0
    });
    let booted = poll_mem(conn, SCREEN_BASE, SCREEN_LEN, ready_timeout, |s| {
        ready_count(s) >= 1
    });

    // Confirm BASIC is genuinely idle at the flashing-cursor prompt and will
    // accept keys — this replaces the old blind settle.
    let ready = wait_basic_ready(conn, flags_timeout);
    if cleared.is_none() && booted.is_none() && ready.is_none() {
        // Memory entirely unreadable — last-ditch fixed delay.
        std::thread::sleep(FALLBACK_BOOT);
    }

    // READY. count now, so a fresh one after the load can be detected.
    let baseline = ready_count(&conn.read_mem(SCREEN_BASE, SCREEN_LEN).unwrap_or_default());

    inject_keys(conn, &to_petscii(&format!("load\"*\",{},1\r", device_num)))?;

    // Wait for the load to finish (a fresh READY. beyond the boot one), then
    // confirm the prompt is idle again before RUN.
    let _ = poll_mem(conn, SCREEN_BASE, SCREEN_LEN, load_timeout, |s| {
        ready_count(s) > baseline
    });
    let _ = wait_basic_ready(conn, flags_timeout);

    inject_keys(conn, &to_petscii("run\r"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_device::mock::MockDevice;

    #[test]
    fn ready_count_matches_screen_codes() {
        let mut screen = vec![0x20u8; SCREEN_LEN as usize];
        assert_eq!(ready_count(&screen), 0);
        screen[80..86].copy_from_slice(&READY_CODES);
        assert_eq!(ready_count(&screen), 1);
        screen[200..206].copy_from_slice(&READY_CODES);
        assert_eq!(ready_count(&screen), 2);
    }

    /// A minimal D64 with one PRG (load addr $0800 + one byte), first directory
    /// entry pointing at it — enough for `extract_first_prg` to succeed.
    fn one_prg_d64() -> Vec<u8> {
        use crate::disk_image::{build_blank_d64, ts_offset, ImageKind};
        let mut img = build_blank_d64("ONEFILE", "01 2A");
        let data_off = ts_offset(1, 0, ImageKind::D64).unwrap();
        let payload = [0x00u8, 0x08, 0x99]; // $0800 + one byte
        img[data_off] = 0;
        img[data_off + 1] = 2 + payload.len() as u8 - 1;
        img[data_off + 2..data_off + 2 + payload.len()].copy_from_slice(&payload);
        let dir_off = ts_offset(18, 1, ImageKind::D64).unwrap();
        img[dir_off] = 0;
        img[dir_off + 1] = 0xFF;
        img[dir_off + 2] = 0x82; // closed PRG
        img[dir_off + 3] = 1;
        img[dir_off + 4] = 0;
        img[dir_off + 5] = b'P';
        for b in img[dir_off + 6..dir_off + 21].iter_mut() {
            *b = 0xA0;
        }
        img
    }

    #[test]
    fn boot_dma_loads_first_prg_no_reset() {
        // Parseable D64 → DMA path: run_prg is used, no keyboard, no reset.
        let dev = MockDevice::new();
        let handle = dev.calls.clone();
        boot_mounted_disk(&dev, "8", Some(&one_prg_d64())).unwrap();
        let calls = handle.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.starts_with("run_prg(")),
            "expected DMA run_prg, got {:?}",
            calls
        );
        assert!(
            !calls.iter().any(|c| c == "reset"),
            "DMA path must not reset"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("write_mem")),
            "DMA path must not poke the keyboard buffer"
        );
    }

    #[test]
    fn boot_keyboard_fallback_injects_full_load_then_run() {
        // Non-parseable image (None) → keyboard fallback. read_fill = 0 so the
        // ready flags and buffer-drain polls succeed immediately.
        let dev = MockDevice {
            read_fill: 0,
            ..MockDevice::new()
        };
        let writes = dev.writes.clone();
        let calls = dev.calls.clone();
        // Tiny timeouts so the screen "wait for READY." polls don't stall.
        autoload_with(
            &dev,
            "8",
            Duration::from_millis(20),
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .unwrap();

        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.first().map(String::as_str), Some("reset"));

        // The bytes written to the keyboard buffer ($0277), in order, must be the
        // complete LOAD line followed by the RUN line — nothing dropped.
        let injected: Vec<u8> = writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(addr, _)| *addr == KEYBUF)
            .flat_map(|(_, data)| data.clone())
            .collect();
        let mut expected = to_petscii("load\"*\",8,1\r");
        expected.extend(to_petscii("run\r"));
        assert_eq!(injected, expected, "full LOAD then RUN must be injected");
    }

    #[test]
    fn to_petscii_maps_return() {
        assert_eq!(*to_petscii("\r").first().unwrap(), 0x0D);
        assert_eq!(*to_petscii("\n").first().unwrap(), 0x0D);
    }
}
