//! Single integration-test binary for the crate: one link step instead of one per file.
//! Add a test file as `tests/it/<name>.rs` and declare it here.

mod cli;
mod config_file;
mod fleet;
mod hardening;
mod headless;
mod suite;
mod support;
mod ux;
mod ux_fixes;
