//! Single integration-test binary for the crate: one link step instead of one per file.
//! Add a test file as `tests/it/<name>.rs` and declare it here.

mod automation_sessions;
mod engine;
mod observe;
mod probe_close;
mod protocols;
mod redaction;
mod security;
mod state;
mod v201_engine;
mod v201_support;
