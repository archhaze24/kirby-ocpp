//! The event loop of the OCPP 2.x UI: keys, ticks, station events and effect execution.
//! Slice S2 of M6 fills it; it reuses [`crate::terminal::TerminalGuard`],
//! [`crate::shutdown_signal`], `crate::deliver`, `crate::resync_with`, `crate::Inbound`,
//! `crate::EVENT_QUEUE` and `crate::write_private`.

use kirby_ocpp_core::station2::Station2Handle;

use crate::{TuiError, TuiOptions};

/// Runs the UI for one OCPP 2.x station until the user quits. Does not shut the station
/// down; the caller does.
#[allow(
    clippy::unused_async,
    clippy::needless_pass_by_value,
    reason = "stub with the final signature; the loop of M6 slice S2 awaits and uses both arguments"
)]
pub async fn run2(handle: Station2Handle, options: TuiOptions) -> Result<(), TuiError> {
    let _ = (handle, options);
    Ok(())
}
