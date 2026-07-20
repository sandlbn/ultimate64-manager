//! App-level (non-tab) message handlers, split out of `main.rs` to keep the
//! central `update()` a thin dispatcher. Each submodule adds an
//! `impl crate::Ultimate64Browser` block of `handle_*` methods; the match arms
//! in `main.rs` call into them. Submodules are descendants of the crate root,
//! so they can reach `Ultimate64Browser`'s private fields and the crate-level
//! `Message`/`Tab`/`UserMessage` types directly.

mod connection;
mod copy;
mod device;
mod dragdrop;
mod machine;
mod settings;
mod view;
mod window_modals;
