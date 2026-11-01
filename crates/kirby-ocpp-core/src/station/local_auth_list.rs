//! Local authorization list (`SendLocalList`, `GetLocalListVersion`).
//!
//! TS reference: `src/station/local-auth-list.ts`.
//!
//! Check order to preserve: feature enabled, max length, differential version
//! `<=` current, then apply. Differential entries without `idTagInfo` delete;
//! a full update clears first. Fixes: `LocalAuthListMaxLength` enforced on the
//! *resulting* size; `GetLocalListVersion` returns -1 when the feature is off.
