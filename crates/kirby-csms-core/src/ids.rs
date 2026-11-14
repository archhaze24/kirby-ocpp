//! Station identity rules that depend on the negotiated protocol.

use kirby_ocpp_core::ocpp::Protocol;

/// The longest station id OCPP 2.0.1 allows, in characters (OCPP-J §3.1.1).
const MAX_ID_CHARS_V201: usize = 48;

/// Checks a station id (already free of control characters and at most 128 bytes) against the
/// rule of the protocol the session negotiated.
///
/// OCPP 2.0.1 ids are at most 48 characters of `identifierString` without the colon, which is
/// the Basic Auth separator. Every other protocol keeps the protocol-free rule of
/// `station_id_from_path`.
pub(crate) fn check_station_id_for(protocol: Protocol, id: &str) -> Result<(), &'static str> {
    match protocol {
        Protocol::V201 => {
            if id.is_empty() {
                Err("station id is empty")
            } else if id.chars().count() > MAX_ID_CHARS_V201 {
                Err("station id is longer than 48 characters")
            } else if !id.chars().all(is_identifier_char) {
                Err("station id has a character outside a-z A-Z 0-9 * - _ = + | @ .")
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '*' | '-' | '_' | '=' | '+' | '|' | '@' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v201_accepts_identifier_characters_up_to_48() {
        for id in ["CP-1", "a.b_c=d+e|f@g*h", "0", &"x".repeat(48)] {
            assert!(check_station_id_for(Protocol::V201, id).is_ok(), "{id}");
        }
    }

    #[test]
    fn v201_refuses_long_empty_colon_and_other_characters() {
        assert!(check_station_id_for(Protocol::V201, &"x".repeat(49)).is_err());
        assert!(check_station_id_for(Protocol::V201, "").is_err());
        for id in ["CP:1", "CP 1", "CP/1", "CP#1", "CPé", "CP%1", "CP,1"] {
            assert!(check_station_id_for(Protocol::V201, id).is_err(), "{id}");
        }
    }

    #[test]
    fn v201_counts_characters_not_bytes() {
        // 25 two-byte characters are under the byte cap of the other protocols but not valid.
        assert!(check_station_id_for(Protocol::V201, &"é".repeat(25)).is_err());
        // A long run of multi-byte characters is refused by character class, never by panic.
        assert!(check_station_id_for(Protocol::V201, &"é".repeat(60)).is_err());
    }

    #[test]
    fn other_protocols_are_unchanged() {
        let long = "a:b ".repeat(30);
        for protocol in [Protocol::V16, Protocol::V21] {
            assert!(check_station_id_for(protocol, &long).is_ok());
        }
    }
}
