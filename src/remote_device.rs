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

#[cfg(test)]
pub(crate) mod mock {
    //! In-memory `RemoteDevice` test double. Records every call and serves
    //! canned data, so tab handlers and the async `api`/`music_ops` helpers can
    //! be exercised with no real device. Recorders are `Arc`-shared, so a test
    //! can clone a handle, move the device behind `Arc<Mutex<dyn RemoteDevice>>`,
    //! and still inspect what was called afterwards.

    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    #[allow(clippy::type_complexity)] // recorder handles; test-only
    pub struct MockDevice {
        pub calls: Arc<Mutex<Vec<String>>>,
        pub reads: Arc<Mutex<Vec<(u16, u16)>>>,
        pub writes: Arc<Mutex<Vec<(u16, Vec<u8>)>>>,
        /// Byte used to fill `read_mem` results.
        pub read_fill: u8,
        /// When true every operation returns an error.
        pub fail: bool,
    }

    impl Default for MockDevice {
        fn default() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                reads: Arc::new(Mutex::new(Vec::new())),
                writes: Arc::new(Mutex::new(Vec::new())),
                read_fill: 0xAA,
                fail: false,
            }
        }
    }

    impl MockDevice {
        pub fn new() -> Self {
            Self::default()
        }
        /// A failing device — every op returns `Err`.
        pub fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }
        /// Ordered list of recorded calls (e.g. `"read_mem(0,256)"`).
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        pub fn reads(&self) -> Vec<(u16, u16)> {
            self.reads.lock().unwrap().clone()
        }
        pub fn writes(&self) -> Vec<(u16, Vec<u8>)> {
            self.writes.lock().unwrap().clone()
        }
        fn record(&self, s: impl Into<String>) {
            self.calls.lock().unwrap().push(s.into());
        }
        fn guard(&self) -> Result<()> {
            if self.fail {
                Err(anyhow::anyhow!("mock failure"))
            } else {
                Ok(())
            }
        }
    }

    impl RemoteDevice for MockDevice {
        fn info(&self) -> Result<DeviceInfo> {
            self.record("info");
            self.guard()?;
            Ok(DeviceInfo {
                product: "MockU64".into(),
                firmware_version: "9.9".into(),
                fpga_version: "0".into(),
                core_version: None,
                hostname: "mock".into(),
                unique_id: None,
            })
        }
        fn drive_list(&self) -> Result<HashMap<String, Drive>> {
            self.record("drive_list");
            self.guard()?;
            Ok(HashMap::new())
        }
        fn read_mem(&self, address: u16, length: u16) -> Result<Vec<u8>> {
            self.record(format!("read_mem({address},{length})"));
            self.reads.lock().unwrap().push((address, length));
            self.guard()?;
            Ok(vec![self.read_fill; length as usize])
        }
        fn write_mem(&self, address: u16, data: &[u8]) -> Result<()> {
            self.record(format!("write_mem({address},{})", data.len()));
            self.writes.lock().unwrap().push((address, data.to_vec()));
            self.guard()
        }
        fn reset(&self) -> Result<()> {
            self.record("reset");
            self.guard()
        }
        fn reboot(&self) -> Result<()> {
            self.record("reboot");
            self.guard()
        }
        fn poweroff(&self) -> Result<()> {
            self.record("poweroff");
            self.guard()
        }
        fn menu(&self) -> Result<()> {
            self.record("menu");
            self.guard()
        }
        fn run_prg(&self, data: &[u8]) -> Result<()> {
            self.record(format!("run_prg({})", data.len()));
            self.guard()
        }
        fn run_crt(&self, data: &[u8]) -> Result<()> {
            self.record(format!("run_crt({})", data.len()));
            self.guard()
        }
        fn sid_play(&self, siddata: &[u8], songnr: Option<u8>) -> Result<()> {
            self.record(format!("sid_play({},{:?})", siddata.len(), songnr));
            self.guard()
        }
        fn mod_play(&self, moddata: &[u8]) -> Result<()> {
            self.record(format!("mod_play({})", moddata.len()));
            self.guard()
        }
        fn type_text(&self, s: &str) -> Result<()> {
            self.record(format!("type_text({:?})", s));
            self.guard()
        }
        fn mount_disk_image(
            &self,
            path: &Path,
            drive: String,
            mount_mode: MountMode,
            run: bool,
        ) -> Result<()> {
            self.record(format!(
                "mount_disk_image({:?},{},{:?},{})",
                path.file_name().unwrap_or_default(),
                drive,
                mount_mode,
                run
            ));
            self.guard()
        }
    }

    #[test]
    fn mock_records_and_serves() {
        let dev = MockDevice::new();
        assert_eq!(dev.read_mem(0x10, 4).unwrap(), vec![0xAA; 4]);
        dev.write_mem(0x20, &[1, 2, 3]).unwrap();
        dev.reset().unwrap();
        assert_eq!(dev.reads(), vec![(0x10, 4)]);
        assert_eq!(dev.writes(), vec![(0x20, vec![1, 2, 3])]);
        assert_eq!(
            dev.calls(),
            vec!["read_mem(16,4)", "write_mem(32,3)", "reset"]
        );
    }

    #[test]
    fn failing_mock_errors() {
        let dev = MockDevice::failing();
        assert!(dev.reset().is_err());
        assert!(dev.read_mem(0, 1).is_err());
    }

    #[test]
    fn mock_fits_the_connection_seam() {
        // The whole point of the trait: a mock drops into the exact slot the
        // real client occupies (`Arc<Mutex<dyn RemoteDevice>>`), including the
        // context handed to every tab.
        let conn: Arc<Mutex<dyn RemoteDevice>> = Arc::new(Mutex::new(MockDevice::new()));
        let ctx = crate::tab::TabContext {
            connection: Some(conn.clone()),
            host: Some("10.0.0.5".into()),
            host_url: Some("http://10.0.0.5".into()),
            password: None,
        };
        assert!(ctx.connection.is_some());
        conn.lock().unwrap().reset().unwrap();
    }
}
