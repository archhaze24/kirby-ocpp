//! Offline queue and post-reconnect synchronization.
//!
//! TS reference: `src/station/offline-transaction-sync.ts`,
//! `src/station/transaction-message-retry.ts`.
//!
//! Only `StartTransaction` / `StopTransaction` are queued while offline (everything
//! else - StatusNotification, MeterValues, Heartbeat, Authorize - is dropped). After
//! BootNotification is `Accepted`, sync runs **before** meter rescheduling and status
//! re-sends: pending starts first, then stops with the real transaction id; a rejected
//! idTag drops the transaction and sends a `StopTransaction` with `DeAuthorized`. One
//! failing delivery must not abort the rest of the boot sequence. Retry policy:
//! `TransactionMessageAttempts` (default 1 = no retry) and `TransactionMessageRetryInterval`
//! for Start/Stop/MeterValues, triggered by CALLERROR, timeout or a schema-violating
//! response; retry sleeps are cancelled on disconnect. The queue length is exposed as
//! `Snapshot::pending_offline_messages`.
