//! Typed identifiers.
//!
//! The TypeScript implementation stored ids as `number | undefined` and tested
//! them for truthiness, so a CSMS-assigned `transactionId` or `reservationId`
//! of `0` was silently treated as "none". Here "no id" is always spelled
//! `Option<..>` and no id type has a niche that equals "absent": `0` is a valid
//! id everywhere.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Connector number. `0` addresses the charge point as a whole (OCPP 1.6 §3.x);
/// `1..` are physical connectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectorId(u32);

impl ConnectorId {
    /// Connector `0`: the charge point itself.
    pub const STATION: Self = Self(0);

    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// `true` for connector `0` (the charge point as a whole).
    pub const fn is_station(self) -> bool {
        self.0 == 0
    }
}

impl From<u32> for ConnectorId {
    fn from(id: u32) -> Self {
        Self(id)
    }
}

impl fmt::Display for ConnectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Reservation id as assigned by the CSMS in `ReserveNow` (OCPP integer, `0` is valid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReservationId(i32);

impl ReservationId {
    pub const fn new(id: i32) -> Self {
        Self(id)
    }

    pub const fn get(self) -> i32 {
        self.0
    }
}

impl fmt::Display for ReservationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Transaction id.
///
/// **Decision:** an enum, not a bare `i64`. A transaction has either a
/// CSMS-assigned id (`Csms`, an OCPP integer, `0` and negatives included) or,
/// while the charge point is offline and has started a transaction on its own,
/// a station-local placeholder (`Local`). The TS code encoded "local" as a
/// negative number (`-(Date.now()*100 + connector)`), which is a convention the
/// compiler cannot check and which collides with CSMSs that legitimately issue
/// negative ids. With the enum a local id can never reach the wire by
/// accident: [`TxId::wire`] returns `None` and [`TxId::require_wire`] is a
/// typed error for it, so a `StopTransaction`/`MeterValues` builder cannot be
/// written without handling the "not synced yet" case. After sync the actor
/// replaces `Local(_)` with `Csms(_)` on the connector state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxId {
    /// Id assigned by the CSMS in `StartTransaction.conf` (or `RemoteStart` flow).
    Csms(i32),
    /// Placeholder for a transaction started offline and not yet acknowledged.
    Local(u64),
}

/// A station-local [`TxId`] was used where a CSMS-assigned id is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("transaction {0} is local-only and has no CSMS transaction id yet")]
pub struct LocalTxIdError(pub TxId);

impl TxId {
    /// Wire value, if this id was assigned by the CSMS.
    pub const fn wire(self) -> Option<i32> {
        match self {
            Self::Csms(id) => Some(id),
            Self::Local(_) => None,
        }
    }

    /// Wire value or a typed error for local-only transactions.
    pub fn require_wire(self) -> Result<i32, LocalTxIdError> {
        self.wire().ok_or(LocalTxIdError(self))
    }

    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local(_))
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Csms(id) => id.fmt(f),
            Self::Local(id) => write!(f, "local-{id}"),
        }
    }
}

/// Maximum length of an OCPP `IdToken` (`CiString20Type`).
pub const ID_TAG_MAX_LEN: usize = 20;

/// An RFID / authorization token, validated to at most [`ID_TAG_MAX_LEN`]
/// characters (Unicode scalar values, matching JSON-schema `maxLength`).
///
/// Only the length bound is enforced: that is exactly what the bundled JSON
/// schemas check, so a value that passed schema validation never fails here.
/// Emptiness is allowed (the schema allows it); UIs that want a non-empty tag
/// must check [`IdTag::is_empty`] themselves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct IdTag(String);

/// Error returned by [`IdTag::new`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("idTag is {len} characters long, the maximum is {ID_TAG_MAX_LEN}")]
pub struct IdTagError {
    pub len: usize,
}

impl IdTag {
    pub fn new(value: impl Into<String>) -> Result<Self, IdTagError> {
        let value = value.into();
        let len = value.chars().count();
        if len > ID_TAG_MAX_LEN {
            return Err(IdTagError { len });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for IdTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IdTag {
    type Err = IdTagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for IdTag {
    type Error = IdTagError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for IdTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_tag_accepts_exactly_20_chars() {
        let tag = IdTag::new("a".repeat(20)).unwrap();
        assert_eq!(tag.as_str().len(), 20);
    }

    #[test]
    fn id_tag_rejects_21_chars() {
        let err = IdTag::new("a".repeat(21)).unwrap_err();
        assert_eq!(err, IdTagError { len: 21 });
    }

    #[test]
    fn id_tag_counts_characters_not_bytes() {
        // 20 two-byte chars: 40 bytes, still valid.
        assert!(IdTag::new("é".repeat(20)).is_ok());
        assert!(IdTag::new("é".repeat(21)).is_err());
    }

    #[test]
    fn id_tag_deserialization_validates() {
        let ok: IdTag = serde_json::from_str("\"ABC\"").unwrap();
        assert_eq!(ok.to_string(), "ABC");
        let long = format!("\"{}\"", "x".repeat(21));
        assert!(serde_json::from_str::<IdTag>(&long).is_err());
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"ABC\"");
    }

    #[test]
    fn zero_is_a_real_id() {
        assert_eq!(TxId::Csms(0).wire(), Some(0));
        assert_eq!(TxId::Csms(0).require_wire(), Ok(0));
        assert_eq!(ReservationId::new(0).get(), 0);
        assert_eq!(serde_json::to_string(&ReservationId::new(0)).unwrap(), "0");
    }

    #[test]
    fn negative_csms_ids_are_not_local() {
        let id = TxId::Csms(-7);
        assert!(!id.is_local());
        assert_eq!(id.wire(), Some(-7));
    }

    #[test]
    fn local_tx_id_has_no_wire_value() {
        let id = TxId::Local(42);
        assert!(id.is_local());
        assert_eq!(id.wire(), None);
        assert_eq!(id.require_wire(), Err(LocalTxIdError(id)));
        assert_eq!(id.to_string(), "local-42");
    }

    #[test]
    fn tx_id_serde_roundtrip_keeps_the_variant() {
        for id in [TxId::Csms(0), TxId::Csms(-1), TxId::Local(9)] {
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<TxId>(&json).unwrap(), id);
        }
    }

    #[test]
    fn connector_zero_is_the_station() {
        assert!(ConnectorId::STATION.is_station());
        assert!(!ConnectorId::new(1).is_station());
    }
}
