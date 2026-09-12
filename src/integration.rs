//! Live integration tests against a REAL Ultimate64 on the network.
//!
//! `#[ignore]`d so normal `cargo test` / CI never runs them. Run explicitly:
//!
//! ```text
//! # all read-only tests (safe): info, status, memory, config, drives, …
//! U64_TEST_HOST=10.0.0.139 cargo test -- --ignored live_ --nocapture
//!
//! # with a network password on the device
//! U64_TEST_HOST=10.0.0.139 U64_TEST_PASSWORD=secret cargo test -- --ignored live_
//!
//! # include state-changing tests (reset/type/config-write/drive-reset)
//! U64_TEST_HOST=10.0.0.139 U64_TEST_DESTRUCTIVE=1 cargo test -- --ignored live_
//!
//! # reboot is extra-gated (offline ~30s — sabotages a combined run)
//! U64_TEST_HOST=10.0.0.139 U64_TEST_DESTRUCTIVE=1 U64_TEST_REBOOT=1 \
//!     cargo test -- --ignored live_reboot --nocapture
//! ```
//!
//! Off-network (host unset) every test no-ops (SKIP, passes).
//!
//! ## Robustness
//! * The Ultimate64's HTTP server chokes on concurrent connections, so every
//!   test takes a process-wide [`device_lock`] — they run one-at-a-time even
//!   under parallel `cargo test`.
//! * The crate's `reqwest::blocking` client keep-alives sockets the device may
//!   have closed, so device reads/config fetches go through [`retry`], which
//!   retries transient EOF / connection-reset errors.
//!
//! ## Why sync `#[test]`, not `#[tokio::test]`
//! `ultimate64::Rest` wraps a `reqwest::blocking::Client` that owns a runtime;
//! dropping it inside a task panics. We drive async fns via a throwaway
//! `block_on` and keep the last `Arc` ref in the sync test body.

use crate::remote_device::RemoteDevice;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

// ── Harness helpers ─────────────────────────────────────────────────────────

fn test_host() -> Option<String> {
    std::env::var("U64_TEST_HOST")
        .ok()
        .filter(|s| !s.is_empty())
}

fn test_password() -> Option<String> {
    std::env::var("U64_TEST_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

fn host_url(host: &str) -> String {
    format!("http://{}", host)
}

/// Serialize all live tests against the device — its HTTP server can't handle
/// concurrent connections. Poison-tolerant (a failing test shouldn't wedge the
/// rest). Held for the whole test via the returned guard.
fn device_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn connect(host: &str, password: Option<String>) -> Arc<Mutex<dyn RemoteDevice>> {
    let h = url::Host::parse(host).expect("U64_TEST_HOST is not a valid host/IP");
    let rest = ultimate64::Rest::new(&h, password).expect("failed to build Rest client");
    Arc::new(Mutex::new(rest))
}

fn block_on<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build test runtime")
        .block_on(fut)
}

/// Whether an error looks like the device dropping the connection — worth a
/// retry rather than a failure.
fn is_transient(e: &str) -> bool {
    let s = e.to_lowercase();
    [
        "end of file",
        "connection",
        "reset",
        "broken",
        "empty reply",
        "eof",
        "timed out",
    ]
    .iter()
    .any(|m| s.contains(m))
}

/// Retry a device operation a few times on transient connection errors.
fn retry<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut last = String::new();
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(250 * attempt as u64));
        }
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if is_transient(&e) => {
                eprintln!("  (transient, retry {}): {}", attempt + 1, e);
                last = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(format!("failed after retries: {}", last))
}

/// Read device memory through the app's real path, with transient-error retry.
fn read_mem(conn: &Arc<Mutex<dyn RemoteDevice>>, addr: u16, len: u32) -> Result<Vec<u8>, String> {
    retry(|| block_on(crate::api::read_memory_async(conn.clone(), addr, len)))
}

/// Run a blocking `RemoteDevice` call off the async context, with retry. `conn`
/// is cloned in, so the caller's retained ref stays the last one.
fn on_device<T: Send + 'static>(
    conn: &Arc<Mutex<dyn RemoteDevice>>,
    f: impl Fn(&dyn RemoteDevice) -> anyhow::Result<T> + Send + Sync + 'static,
) -> Result<T, String> {
    let f = Arc::new(f);
    retry(|| {
        let c = conn.clone();
        let f = f.clone();
        block_on(async move {
            tokio::task::spawn_blocking(move || f(&*c.lock().unwrap()).map_err(|e| e.to_string()))
                .await
                .unwrap()
        })
    })
}

macro_rules! host_or_skip {
    () => {
        match test_host() {
            Some(h) => h,
            None => {
                eprintln!("SKIP: set U64_TEST_HOST=<ip> to run live tests");
                return;
            }
        }
    };
}

macro_rules! require_flag {
    ($flag:literal, $why:literal) => {
        if !env_flag($flag) {
            eprintln!(concat!("SKIP: set ", $flag, "=1 to run ", $why));
            return;
        }
    };
}

// ── Read-only tests (need only U64_TEST_HOST) ───────────────────────────────

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_fetch_status() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    let status = retry(|| block_on(crate::fetch_status(conn.clone())).map_err(|e| e.to_string()))
        .expect("fetch_status failed — reachable? password correct?");
    assert!(status.connected, "device reported not connected");
    let info = status.device_info.expect("no device info returned");
    println!("Connected to: {}", info);
    assert!(!info.is_empty());
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_device_info() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    let info = on_device(&conn, |d| d.info()).expect("info() failed");
    println!(
        "product={} fw={} fpga={}",
        info.product, info.firmware_version, info.fpga_version
    );
    assert!(!info.product.is_empty());
    assert!(!info.firmware_version.is_empty());
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_drive_list() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    let drives = on_device(&conn, |d| d.drive_list()).expect("drive_list() failed");
    println!("drives: {:?}", drives.keys().collect::<Vec<_>>());
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_read_screen_ram() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    let screen = read_mem(&conn, 0x0400, 1000).expect("read screen RAM failed");
    assert_eq!(screen.len(), 1000, "expected a full 1000-byte screen read");
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_read_common_regions() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    let regions = [
        ("zero page", 0x0000u16, 256u32),
        ("stack", 0x0100, 256),
        ("BASIC ROM", 0xA000, 256),
        ("color RAM", 0xD800, 256),
        ("KERNAL ROM", 0xE000, 256),
    ];
    for (label, addr, len) in regions {
        let bytes = read_mem(&conn, addr, len)
            .unwrap_or_else(|e| panic!("read {} @ ${:04X} failed: {}", label, addr, e));
        assert_eq!(bytes.len(), len as usize, "{} short read", label);
        println!("{:<11} ${:04X}: {:02X?}…", label, addr, &bytes[..8]);
    }
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_read_full_64k() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    // Exercises read_memory_async's u16-length chunking on real hardware.
    let all = read_mem(&conn, 0, 65_536).expect("full 64K read failed");
    assert_eq!(all.len(), 65_536);
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_read_write_read_roundtrip() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let conn = connect(&host, test_password());
    // $02/$03 are unused zero-page scratch on a stock C64.
    let original = read_mem(&conn, 0x0002, 2).expect("initial read failed");
    retry(|| block_on(crate::api::write_byte_async(conn.clone(), 0x0002, 0x42)))
        .expect("write failed");
    let after = read_mem(&conn, 0x0002, 1).expect("read-back failed");
    assert_eq!(after, vec![0x42], "written byte did not read back");
    retry(|| {
        block_on(crate::api::write_byte_async(
            conn.clone(),
            0x0002,
            original[0],
        ))
    })
    .expect("restore failed");
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_config_list_categories() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let (url, pw) = (host_url(&host), test_password());
    let cats = retry(|| block_on(crate::config_api::fetch_categories(url.clone(), pw.clone())))
        .expect("fetch_categories failed");
    println!("{} config categories: {:?}", cats.len(), cats);
    assert!(!cats.is_empty(), "device returned no config categories");
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_config_load_every_category() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let (url, pw) = (host_url(&host), test_password());
    let cats = retry(|| block_on(crate::config_api::fetch_categories(url.clone(), pw.clone())))
        .expect("fetch_categories failed");
    let mut total = 0usize;
    for cat in &cats {
        let (name, items) = retry(|| {
            block_on(crate::config_api::fetch_category_items(
                url.clone(),
                cat.clone(),
                pw.clone(),
            ))
        })
        .unwrap_or_else(|e| panic!("category '{}' failed to load: {}", cat, e));
        println!("  {:<32} {} items", name, items.len());
        total += items.len();
    }
    println!("loaded {} categories, {} items total", cats.len(), total);
    assert!(total > 0, "no config items across any category");
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_config_item_details() {
    let host = host_or_skip!();
    let _serial = device_lock();
    let (url, pw) = (host_url(&host), test_password());
    let cats = retry(|| block_on(crate::config_api::fetch_categories(url.clone(), pw.clone())))
        .expect("fetch_categories failed");
    let cat = cats.first().expect("no categories").clone();
    let (_, items) = retry(|| {
        block_on(crate::config_api::fetch_category_items(
            url.clone(),
            cat.clone(),
            pw.clone(),
        ))
    })
    .expect("fetch_category_items failed");
    let item = items.first().expect("category has no items");
    let (name, details) = retry(|| {
        block_on(crate::config_api::fetch_item_details(
            url.clone(),
            cat.clone(),
            item.name.clone(),
            pw.clone(),
        ))
    })
    .expect("fetch_item_details failed");
    println!(
        "item '{}' current={:?} details={:?}",
        name, item.current_value, details
    );
}

#[test]
#[ignore = "requires a real Ultimate64; set U64_TEST_HOST=<ip>"]
fn live_debugreg_read() {
    let host = host_or_skip!();
    let _serial = device_lock();
    // U64-only endpoint; on a U2+ it legitimately errors, so this is lenient.
    match block_on(crate::api::read_debugreg_async(
        &host_url(&host),
        test_password().as_deref(),
    )) {
        Ok(v) => println!("debug register $D7FF = ${:02X}", v),
        Err(e) => println!("debugreg not available (expected on non-U64): {}", e),
    }
}

// ── State-changing tests (need U64_TEST_DESTRUCTIVE=1) ──────────────────────

#[test]
#[ignore = "DESTRUCTIVE: resets the C64. Set U64_TEST_HOST + U64_TEST_DESTRUCTIVE=1"]
fn live_reset_machine() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    block_on(crate::api::reset_machine_async(
        &host_url(&host),
        test_password().as_deref(),
    ))
    .expect("reset failed");
    println!("Reset sent to {}", host);
}

#[test]
#[ignore = "DESTRUCTIVE: types into the C64 keyboard buffer. Set U64_TEST_DESTRUCTIVE=1"]
fn live_type_text() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    // Harmless: types spaces + RETURN at whatever prompt is showing.
    block_on(crate::api::type_text_async(
        &host_url(&host),
        "  \n",
        test_password().as_deref(),
    ))
    .expect("type_text failed");
    println!("Typed test keystrokes to {}", host);
}

/// Guards the fix for the "Load timed out — device may be offline" reports:
/// `run_prg` answers only after the device has reset and DMA-loaded, which took
/// a fixed ~8s on an Ultimate II+ (fw 3.14) and ~1.9s on a C64 Ultimate
/// (fw 1.1.0). The old 5s cap sat between those two figures, so the II+ failed
/// every load while this machine passed — which is why it went unnoticed.
///
/// Asserts the real latency still fits inside REST_RUN_TIMEOUT_SECS, and prints
/// it so a firmware regression shows up as a number rather than a mystery.
#[test]
#[ignore = "DESTRUCTIVE: resets the C64 and runs a program. Set U64_TEST_DESTRUCTIVE=1"]
fn live_run_prg_completes_within_the_run_timeout() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");

    // `10 SYS 2061` plus real 6502 code at 2061 ($080D) that stamps a marker
    // into unused RAM. An earlier version of this test stopped at the BASIC
    // stub — but 2061 was then past the end of the payload, so it SYS'd into
    // uninitialised RAM and proved nothing. The marker is what makes this a
    // test of *execution* rather than of upload latency.
    //
    //   $0801  0B 08 0A 00 9E "2061" 00   line 10: SYS 2061
    //   $080B  00 00                      end of program
    //   $080D  A9 42     LDA #$42
    //          8D 00 C0  STA $C000
    //          A9 43     LDA #$43
    //          8D 01 C0  STA $C001
    //          60        RTS
    let prg: &[u8] = &[
        0x01, 0x08, // load address $0801
        0x0b, 0x08, 0x0a, 0x00, 0x9e, 0x32, 0x30, 0x36, 0x31, 0x00, // 10 SYS 2061
        0x00, 0x00, // end of BASIC program
        0xa9, 0x42, 0x8d, 0x00, 0xc0, // LDA #$42 : STA $C000
        0xa9, 0x43, 0x8d, 0x01, 0xc0, // LDA #$43 : STA $C001
        0x60, // RTS
    ];
    let conn = connect(&host, test_password());

    // Clear the marker bytes first, so a stale value can't fake a pass.
    on_device(&conn, |d| d.write_mem(0xC000, &[0x00, 0x00])).expect("failed to clear marker");
    let before = read_mem(&conn, 0xC000, 2).expect("marker read failed");
    assert_eq!(before, vec![0x00, 0x00], "marker did not clear");

    let started = std::time::Instant::now();
    block_on(crate::net_utils::run_blocking(
        crate::net_utils::REST_RUN_TIMEOUT_SECS,
        "Load",
        {
            let prg = prg.to_vec();
            let conn = conn.clone();
            move || {
                let c = conn.lock().unwrap();
                c.run_prg(&prg).map_err(|e| e.to_string())
            }
        },
    ))
    .expect("run_prg failed (or exceeded REST_RUN_TIMEOUT_SECS)");
    let elapsed = started.elapsed();

    println!(
        "run_prg on {} answered in {:.2}s",
        host,
        elapsed.as_secs_f32()
    );
    assert!(
        elapsed < Duration::from_secs(crate::net_utils::REST_RUN_TIMEOUT_SECS),
        "run_prg took {:?}, at or beyond the {}s budget",
        elapsed,
        crate::net_utils::REST_RUN_TIMEOUT_SECS
    );

    // The program must actually have executed, not merely been uploaded.
    std::thread::sleep(Duration::from_millis(1500));
    let marker = read_mem(&conn, 0xC000, 2).expect("marker read failed");
    assert_eq!(
        marker,
        vec![0x42, 0x43],
        "program was loaded but never ran (marker at $C000 is {marker:02X?})"
    );
    println!("marker $C000 = {marker:02X?} — program executed");
}

/// Exercises the port-64 `CMD_RUN_IMG` disk path end to end with a real 174848
/// byte d64 — the payload that crosses the 24-bit length field's 16-bit
/// boundary, and the transfer that previously had no timeout at all.
///
/// Needs a d64: set `U64_TEST_D64=/path/to/image.d64`.
#[test]
#[ignore = "DESTRUCTIVE: mounts and boots a disk. Set U64_TEST_DESTRUCTIVE=1 + U64_TEST_D64=<path>"]
fn live_315_mount_d64_over_port64() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");

    let Some(path) = std::env::var("U64_TEST_D64").ok().filter(|s| !s.is_empty()) else {
        eprintln!("SKIP: set U64_TEST_D64=<path to a .d64> to run the disk-boot test");
        return;
    };
    let image = std::fs::read(&path).expect("could not read U64_TEST_D64");
    println!("Booting {} ({} bytes) on {}", path, image.len(), host);

    let started = std::time::Instant::now();
    block_on(crate::port64::mount_disk_image(
        host.clone(),
        test_password(),
        image,
    ))
    .expect("port-64 CMD_MOUNT_IMG failed");
    println!(
        "CMD_MOUNT_IMG accepted in {:.2}s",
        started.elapsed().as_secs_f32()
    );

    // What this test can assert deterministically, and what it deliberately
    // cannot:
    //
    // The firmware mounts the image and resets the machine; the C64 then loads
    // the game through the *emulated 1541 at authentic speed*. Measured on this
    // device, a real game disk took ~60s to reach its title screen — and the
    // figure is game-dependent (a fastloader is much quicker, a plain KERNAL
    // loader much slower). So asserting "the game is on screen" by a fixed
    // deadline is inherently flaky: an earlier draft of this test waited 8s,
    // concluded the disk "did not boot", and was simply looking too early.
    //
    // Instead assert the two things the command is actually responsible for:
    // the image is mounted on drive A, and the machine was reset to BASIC ready
    // to load it. Whether the game finishes loading is the 1541's business.
    std::thread::sleep(Duration::from_secs(5));
    let conn = connect(&host, test_password());

    let drives = on_device(&conn, |d| d.drive_list()).expect("drive_list failed");
    let drive_a = drives.get("a").expect("no drive A reported");
    let mounted = format!("{:?}", drive_a);
    assert!(
        mounted.contains("tcpimage"),
        "drive A is not carrying the uploaded image: {mounted}"
    );
    println!("drive A mounted: {mounted}");

    // The machine must have been reset by the command — at this point it is at
    // the BASIC banner, loading. (Later it will leave the banner on its own.)
    let screen = read_mem(&conn, 0x0400, 1000).expect("screen read failed");
    let blank = screen.iter().all(|&b| b == 0x20);
    assert!(
        !blank,
        "screen is entirely blank — machine did not come back up"
    );
    println!("Image mounted and machine reset; the 1541 loads from here (~60s for this disk)");
}

// ── Firmware 3.15 API (auto-skips on older firmware) ────────────────────────

/// Read the device's firmware and build the capability gate from it. Returns
/// `None` (and prints why) when the device predates 3.15, so these tests are
/// safe to run against any host in the fleet.
fn caps_or_skip(host: &str) -> Option<crate::device_caps::DeviceCaps> {
    let conn = connect(host, test_password());
    let info = on_device(&conn, |d| d.info()).expect("info() failed");
    let caps = crate::device_caps::DeviceCaps::from_firmware(Some(&info.firmware_version));
    if !caps.has_315_api() {
        eprintln!(
            "SKIP: {} runs firmware {} — the 3.15 API needs 3.15 or newer",
            info.product, info.firmware_version
        );
        return None;
    }
    println!("{} on firmware {}", info.product, info.firmware_version);
    Some(caps)
}

#[test]
#[ignore = "requires an Ultimate on firmware 3.15+; set U64_TEST_HOST=<ip>"]
fn live_315_heap_stats() {
    let host = host_or_skip!();
    let _serial = device_lock();
    if caps_or_skip(&host).is_none() {
        return;
    }
    let h = block_on(crate::api_315::heap_stats(
        &host,
        test_password().as_deref(),
    ))
    .expect("heap_stats failed");
    println!(
        "heap: free {} / total {} ({:.0}% used), min ever free {}",
        h.free,
        h.total,
        h.used_fraction() * 100.0,
        h.min_ever_free
    );
    assert!(h.total > 0, "a device must report a heap total");
    assert!(h.free <= h.total, "free cannot exceed total");
    assert!(
        h.min_ever_free <= h.free,
        "low water mark cannot exceed current free"
    );
}

/// The menu screen is 2000 bytes when the menu is open, and a 404 (mapped to
/// `None`) when it is not — both are correct answers, so this asserts the
/// shape of whichever one comes back rather than forcing the menu open.
#[test]
#[ignore = "requires an Ultimate on firmware 3.15+; set U64_TEST_HOST=<ip>"]
fn live_315_menu_screen() {
    let host = host_or_skip!();
    let _serial = device_lock();
    if caps_or_skip(&host).is_none() {
        return;
    }
    match block_on(crate::api_315::menu_screen(
        &host,
        test_password().as_deref(),
    ))
    .expect("menu_screen failed")
    {
        Some(screen) => {
            assert_eq!(screen.codes.len(), crate::api_315::MENU_CELLS);
            assert_eq!(screen.colors.len(), crate::api_315::MENU_CELLS);
            // The menu buffer is ASCII, not C64 screen codes — 0x55 is "U".
            // 0x02 is the horizontal rule the menu draws separators with.
            let decode = |row: &[u8]| -> String {
                row.iter()
                    .map(|&c| match c {
                        0x20..=0x7e => c as char,
                        0x02 => '\u{2500}',
                        _ => ' ',
                    })
                    .collect()
            };
            println!("menu is open; row 0: {:?}", decode(screen.row(0)));
            assert!(
                screen.codes.iter().any(|&c| c != 0x20),
                "an open menu cannot be entirely blank"
            );
        }
        None => println!("menu is not on screen (404) — the documented way to ask"),
    }
}

#[test]
#[ignore = "requires an Ultimate on firmware 3.15+; set U64_TEST_HOST=<ip>"]
fn live_315_file_info_reports_missing_files_as_none() {
    let host = host_or_skip!();
    let _serial = device_lock();
    if caps_or_skip(&host).is_none() {
        return;
    }
    let missing = block_on(crate::api_315::file_info(
        &host,
        test_password().as_deref(),
        "/Temp/definitely-not-here-9f3a.d64",
    ))
    .expect("a missing file is not an error");
    assert_eq!(missing, None, "404 must map to None, not an Err");
    println!("missing file correctly reported as None");
}

/// Formats a real image on the device, then reads it back through `files:info`
/// to prove the bytes actually landed.
#[test]
#[ignore = "DESTRUCTIVE: writes a disk image to the device. Set U64_TEST_DESTRUCTIVE=1"]
fn live_315_create_disk_image_roundtrip() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    if caps_or_skip(&host).is_none() {
        return;
    }
    let dir = std::env::var("U64_TEST_DISK_DIR").unwrap_or_else(|_| "Temp".to_string());
    let path = format!("{}/u64mgr-selftest.d64", dir.trim_matches('/'));
    let pw = test_password();

    // The firmware refuses to overwrite and there is no REST delete, so a
    // repeat run legitimately finds the image already there. Treat that as the
    // same outcome rather than leaving a test that only passes once.
    match block_on(crate::api_315::create_disk_image(
        &host,
        pw.as_deref(),
        &path,
        crate::api_315::DiskKind::D64 { tracks: None },
        Some("SELFTEST"),
    )) {
        Ok(written) => {
            println!("created {} ({} bytes written)", path, written);
            assert_eq!(written, 174_848, "a 35-track D64 is 174848 bytes");
        }
        Err(e) if e.to_uppercase().contains("EXISTS") => {
            println!(
                "{} already existed from an earlier run — verifying it",
                path
            );
        }
        Err(e) => panic!("create_disk_image failed: {e}"),
    }

    let info = block_on(crate::api_315::file_info(&host, pw.as_deref(), &path))
        .expect("file_info failed")
        .expect("the image we just created must exist");
    println!("files:info -> {:?}", info);
    assert_eq!(info.size, 174_848, "a 35-track D64 is 174848 bytes on disk");
    assert_eq!(info.extension.to_uppercase(), "D64");
}

/// The regression guard for the bug this suite missed: a disk that mounts but
/// never boots.
///
/// The earlier version of the mount test asserted only that the image was
/// mounted and the machine had reset — both of which `CMD_RUN_IMG` does — so it
/// passed while every `.d64` sat at the BASIC banner forever. What actually
/// matters is that the machine *leaves* the banner.
///
/// Requires hardware where DMA `run_prg` reaches the machine. A cartridge
/// stacked inside an Ultimate 64 does not qualify: DMA never reaches the host
/// there, so this is opt-in via `U64_TEST_DISK_BOOT=1` rather than failing on
/// a rig that cannot pass it.
#[test]
#[ignore = "DESTRUCTIVE: boots a disk. Set U64_TEST_DESTRUCTIVE=1 U64_TEST_DISK_BOOT=1 U64_TEST_D64=<path>"]
fn live_disk_actually_boots_after_mount() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    require_flag!(
        "U64_TEST_DISK_BOOT",
        "the disk-boot check (needs working DMA)"
    );

    let Some(path) = std::env::var("U64_TEST_D64").ok().filter(|s| !s.is_empty()) else {
        eprintln!("SKIP: set U64_TEST_D64=<path to a .d64>");
        return;
    };
    let conn = connect(&host, test_password());
    let pw = test_password();

    block_on(crate::api::run_local_disk_async(
        &host,
        std::path::Path::new(&path),
        "a",
        pw.as_deref(),
        Some(conn.clone()),
    ))
    .expect("run_local_disk_async failed");

    // "COMMODORE 64 BASIC" in screen codes. Leaving this behind is the only
    // evidence the disk actually started.
    const BANNER: &[u8] = &[
        0x03, 0x0f, 0x0d, 0x0d, 0x0f, 0x04, 0x0f, 0x12, 0x05, 0x20, 0x36, 0x34, 0x20, 0x02, 0x01,
        0x13, 0x09, 0x03,
    ];
    // Where the VIC is actually fetching characters from. Reading $0400 alone is
    // not enough: a demo that relocates the screen leaves the stale BASIC banner
    // sitting there, so a fixed-address check reports "never booted" while the
    // demo is plainly running somewhere else.
    let screen_addr = |c: &Arc<Mutex<dyn RemoteDevice>>| -> u16 {
        let d018 = read_mem(c, 0xD018, 1).map(|v| v[0]).unwrap_or(0x15);
        let dd00 = read_mem(c, 0xDD00, 1).map(|v| v[0]).unwrap_or(0x97);
        let bank = (!dd00 & 3) as u16 * 16384;
        bank + ((d018 >> 4) & 0x0F) as u16 * 1024
    };

    for i in 1..=20 {
        std::thread::sleep(Duration::from_secs(2));
        let addr = screen_addr(&conn);
        let screen = read_mem(&conn, addr, 1000).expect("screen read failed");
        let at_banner = screen.windows(BANNER.len()).any(|w| w == BANNER);
        let blank = screen.iter().all(|&b| b == 0x20);
        // A relocated screen is itself proof something took the machine over.
        if addr != 0x0400 || (!at_banner && !blank) {
            println!("disk booted after ~{}s (screen at ${:04X})", i * 2, addr);
            return;
        }
    }
    panic!("disk never left the BASIC banner after 40s — it mounted but did not boot");
}

/// Cartridges answer 501 to `machine:input` — the firmware registers the call
/// but the hardware cannot drive keyboard or joystick lines. Callers rely on
/// that being `Unsupported` (so they fall back) rather than a hard failure,
/// so pin the behaviour on whichever product is under test.
#[test]
#[ignore = "requires an Ultimate on firmware 3.15+; set U64_TEST_HOST=<ip>"]
fn live_315_input_reports_hardware_support_honestly() {
    let host = host_or_skip!();
    let _serial = device_lock();
    if caps_or_skip(&host).is_none() {
        return;
    }
    // Releasing everything is the safest probe: it presses nothing.
    let events = [crate::api_315::InputEvent::ReleaseAll];
    match block_on(crate::api_315::send_input(
        &host,
        test_password().as_deref(),
        &events,
    )) {
        Ok(()) => println!("machine:input is supported on this hardware"),
        Err(crate::device_caps::CapError::Unsupported(why)) => {
            println!("machine:input unsupported here (expected on a cartridge): {why}");
        }
        Err(other) => panic!("unexpected failure from machine:input: {other}"),
    }
}

#[test]
#[ignore = "DESTRUCTIVE: resets drive A. Set U64_TEST_DESTRUCTIVE=1"]
fn live_drive_reset() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    block_on(crate::api::drive_reset_async(
        &host_url(&host),
        "a",
        test_password().as_deref(),
    ))
    .expect("drive reset failed");
    println!("Drive A reset on {}", host);
}

#[test]
#[ignore = "DESTRUCTIVE: writes a config value (echoes current back). Set U64_TEST_DESTRUCTIVE=1"]
fn live_config_write_roundtrip() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    let (url, pw) = (host_url(&host), test_password());
    let cats = retry(|| block_on(crate::config_api::fetch_categories(url.clone(), pw.clone())))
        .expect("fetch_categories failed");

    // First NON-network category with an item; write its current value straight
    // back (a no-op change that still exercises the save path). Network
    // categories intentionally drop the connection, so we skip them.
    for cat in cats {
        if crate::config_api::is_network_related_category(&cat) {
            continue;
        }
        let items = match retry(|| {
            block_on(crate::config_api::fetch_category_items(
                url.clone(),
                cat.clone(),
                pw.clone(),
            ))
        }) {
            Ok((_, items)) if !items.is_empty() => items,
            _ => continue,
        };
        let item = &items[0];
        let mut inner = HashMap::new();
        inner.insert(item.name.clone(), item.current_value.clone());
        let mut changes = HashMap::new();
        changes.insert(cat.clone(), inner);

        let res = block_on(crate::config_api::save_batch_changes(url, changes, pw))
            .expect("save_batch_changes failed");
        println!(
            "echoed {}/{} = {:?} → {}",
            cat, item.name, item.current_value, res
        );
        return;
    }
    panic!("found no non-network config item to exercise the write path");
}

#[test]
#[ignore = "VERY DESTRUCTIVE: reboots the device (offline ~30s). Set U64_TEST_DESTRUCTIVE=1 + U64_TEST_REBOOT=1"]
fn live_reboot_machine() {
    let host = host_or_skip!();
    let _serial = device_lock();
    require_flag!("U64_TEST_DESTRUCTIVE", "state-changing tests");
    require_flag!(
        "U64_TEST_REBOOT",
        "the reboot test (it sabotages a combined run)"
    );
    block_on(crate::api::reboot_machine_async(
        &host_url(&host),
        test_password().as_deref(),
    ))
    .expect("reboot failed");
    println!("Reboot sent to {}", host);
}

/// Drives the app's own key-injection path against the device and checks the
/// result is classified the way the UI depends on: a cartridge must report
/// `Unsupported` (so the controls retire) rather than a generic failure.
#[test]
#[ignore = "requires an Ultimate on firmware 3.15+; set U64_TEST_HOST=<ip>"]
fn live_315_named_key_injection_is_classified_correctly() {
    let host = host_or_skip!();
    let _serial = device_lock();
    if caps_or_skip(&host).is_none() {
        return;
    }
    // RUN/STOP is harmless at a BASIC prompt and in the menu alike.
    let events = [crate::api_315::InputEvent::Keyboard {
        inputs: vec!["run_stop".to_string()],
        transition: crate::api_315::Transition::Tap,
    }];
    match block_on(crate::api_315::send_input(
        &host,
        test_password().as_deref(),
        &events,
    )) {
        Ok(()) => println!("key injection works on this hardware"),
        Err(crate::device_caps::CapError::Unsupported(why)) => {
            println!("key injection unsupported here (expected on a cartridge): {why}");
            assert!(
                why.contains("Ultimate 64-class hardware"),
                "the UI latches on this exact wording: {why}"
            );
        }
        Err(other) => panic!("unexpected failure: {other}"),
    }
}
