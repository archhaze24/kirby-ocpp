//! Single integration-test binary for the crate: one link step instead of one per file.
//! Add a test file as `tests/it/<name>.rs` and declare it here.

mod fleet;
mod scenario_reply;
mod scenario_runner;
mod station_cancel_call;
mod station_csms_calls;
mod station_features;
mod station_lifecycle;
mod station_local_edits;
mod station_log_trigger;
mod station_persistence;
mod station_power;
mod station_profile_fallback;
mod station_response_rules;
mod station_retry;
mod station_review_fixes;
mod station_root_rotation;
mod station_sampling;
mod station_security;
mod station_signed_firmware;
mod station_status;
mod station_support;
mod station_tls_security;
mod station_transactions;
mod ws_calls;
mod ws_connect;
mod ws_support;
mod ws_tls;
