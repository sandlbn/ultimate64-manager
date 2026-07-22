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
