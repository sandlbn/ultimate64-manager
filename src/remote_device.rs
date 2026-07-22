//! `RemoteDevice` — the trait the tabs talk to instead of the concrete
//! `ultimate64::Rest` client.
//!
//! Every tab used to hold `Arc<Mutex<ultimate64::Rest>>`, hard-wiring the app to
//! the crate and making the tabs impossible to unit-test without a real device.
//! `RemoteDevice` abstracts exactly the blocking device operations the app
//! calls; `Rest` implements it by forwarding, and tests can supply a mock. The
//! connection is stored as `Arc<Mutex<dyn RemoteDevice>>` (see
//! [`crate::tab::TabContext`]).
//!
//! The trait mirrors `Rest`'s signatures verbatim (including `anyhow::Result`)
//! so the ~40 call sites — `conn.lock().unwrap().read_mem(..)` and friends —
//! stay byte-for-byte identical; only the stored *type* changed. `mount_disk_image`
//! takes a `&Path` rather than `Rest`'s generic `P: AsRef<Path>` so the trait
//! stays object-safe.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use ultimate64::drives::{Drive, MountMode};
use ultimate64::{DeviceInfo, Rest};

/// The blocking device operations the UI performs. `Send` so the boxed client
/// can be shared across `spawn_blocking` worker threads via `Arc<Mutex<_>>`.
pub trait RemoteDevice: Send {
    fn info(&self) -> Result<DeviceInfo>;
    fn drive_list(&self) -> Result<HashMap<String, Drive>>;
    fn read_mem(&self, address: u16, length: u16) -> Result<Vec<u8>>;
    fn write_mem(&self, address: u16, data: &[u8]) -> Result<()>;
    fn reset(&self) -> Result<()>;
    fn reboot(&self) -> Result<()>;
    fn poweroff(&self) -> Result<()>;
    fn menu(&self) -> Result<()>;
    fn run_prg(&self, data: &[u8]) -> Result<()>;
    fn run_crt(&self, data: &[u8]) -> Result<()>;
    fn sid_play(&self, siddata: &[u8], songnr: Option<u8>) -> Result<()>;
    fn mod_play(&self, moddata: &[u8]) -> Result<()>;
    fn type_text(&self, s: &str) -> Result<()>;
    fn mount_disk_image(
        &self,
        path: &Path,
        drive: String,
        mount_mode: MountMode,
        run: bool,
    ) -> Result<()>;
}

impl RemoteDevice for Rest {
    fn info(&self) -> Result<DeviceInfo> {
        Rest::info(self)
    }
    fn drive_list(&self) -> Result<HashMap<String, Drive>> {
        Rest::drive_list(self)
    }
    fn read_mem(&self, address: u16, length: u16) -> Result<Vec<u8>> {
        Rest::read_mem(self, address, length)
    }
    fn write_mem(&self, address: u16, data: &[u8]) -> Result<()> {
        Rest::write_mem(self, address, data)
    }
    fn reset(&self) -> Result<()> {
        Rest::reset(self)
    }
    fn reboot(&self) -> Result<()> {
        Rest::reboot(self)
    }
    fn poweroff(&self) -> Result<()> {
        Rest::poweroff(self)
    }
    fn menu(&self) -> Result<()> {
        Rest::menu(self)
    }
    fn run_prg(&self, data: &[u8]) -> Result<()> {
        Rest::run_prg(self, data)
    }
    fn run_crt(&self, data: &[u8]) -> Result<()> {
        Rest::run_crt(self, data)
    }
    fn sid_play(&self, siddata: &[u8], songnr: Option<u8>) -> Result<()> {
        Rest::sid_play(self, siddata, songnr)
    }
    fn mod_play(&self, moddata: &[u8]) -> Result<()> {
        Rest::mod_play(self, moddata)
    }
    fn type_text(&self, s: &str) -> Result<()> {
        Rest::type_text(self, s)
    }
    fn mount_disk_image(
        &self,
        path: &Path,
        drive: String,
        mount_mode: MountMode,
        run: bool,
    ) -> Result<()> {
        Rest::mount_disk_image(self, path, drive, mount_mode, run)
    }
}
