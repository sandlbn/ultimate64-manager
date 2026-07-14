//! Large view methods split out of `main.rs` into cohesive files. Each adds
//! `pub(crate)` methods to `impl crate::Ultimate64Browser`.

mod dialogs;
mod dual_pane;
mod settings;
mod status_bar;
