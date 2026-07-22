//! Uniform tab dispatch: a single `TabController::update(msg, ctx)` entry point
//! for every tab, so `main` routes all tab messages the same way instead of
//! threading a different mix of `connection` / `host` / `password` into each.
//!
//! Each tab keeps its existing handler as an inherent `update_impl(...)` (with
//! its own parameter shape); the trait impl just unpacks the shared
//! [`TabContext`] into whatever that tab needs. `main` builds one context via
//! `Ultimate64Browser::tab_context()` and calls `tab.update(msg, ctx)`.

use crate::remote_device::RemoteDevice;
use std::sync::Arc;
use std::sync::Mutex;

/// Everything a tab might need to talk to the device, gathered once per update.
/// Tabs disagree on which pieces they use — some want the bare `host`, some the
/// scheme-qualified `host_url`, some only the `connection` — so the context
/// carries all of them and each tab's trait impl picks what it needs.
#[derive(Clone)]
pub struct TabContext {
    /// Shared blocking REST client, when connected.
    pub connection: Option<Arc<Mutex<dyn RemoteDevice>>>,
    /// Bare host/IP, e.g. `"10.0.0.5"` (used by `api::run_prg`-style calls).
    pub host: Option<String>,
    /// Scheme-qualified host, e.g. `"http://10.0.0.5"` (config/profile REST).
    pub host_url: Option<String>,
    /// Device API password, if set.
    pub password: Option<String>,
}

/// Uniform update entry point implemented by every tab component.
pub trait TabController {
    type Message;
    fn update(&mut self, message: Self::Message, ctx: TabContext) -> iced::Task<Self::Message>;
}
