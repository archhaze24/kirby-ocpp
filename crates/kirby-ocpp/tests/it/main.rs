//! Single integration-test binary for the crate: one link step instead of one per file.
//! Add a test file as `tests/it/<name>.rs` and declare it here.

mod check;
mod cli;
mod config_file;
mod fleet;
mod hardening;
mod headless;
mod protocol2;
mod protocol21;
mod record;
mod state;
mod suite;
mod suite2;
mod support;
mod ux;
mod ux_fixes;
