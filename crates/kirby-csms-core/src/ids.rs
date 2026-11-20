//! Station identity rules that depend on the negotiated protocol.

use kirby_ocpp_core::ocpp::Protocol;

/// The longest station id OCPP 2.x allows, in characters (OCPP-J §3.1.1).
const MAX_ID_CHARS_V2: usize = 48;

/// Checks a station id (already free of control characters and at most 128 bytes) against the
/// rule of the protocol the session negotiated.
///
/// OCPP 2.0.1 and 2.1 ids are at most 48 characters of `identifierString` without the colon,
/// which is the Basic Auth separator. OCPP 1.6 keeps the protocol-free rule of
/// `station_id_from_path`.
pub(crate) fn check_station_id_for(protocol: Protocol, id: &str) -> Result<(), &'static str> {
    match protocol {
        Protocol::V201 | Protocol::V21 => {
            if id.is_empty() {
                Err("station id is empty")
            } else if id.chars().count() > MAX_ID_CHARS_V2 {
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
    fn v2_accepts_identifier_characters_up_to_48() {
        for protocol in [Protocol::V201, Protocol::V21] {
            for id in ["CP-1", "a.b_c=d+e|f@g*h", "0", &"x".repeat(48)] {
                assert!(check_station_id_for(protocol, id).is_ok(), "{id}");
            }
        }
    }

    #[test]
    fn v2_refuses_long_empty_colon_and_other_characters() {
        for protocol in [Protocol::V201, Protocol::V21] {
            assert!(check_station_id_for(protocol, &"x".repeat(49)).is_err());
            assert!(check_station_id_for(protocol, "").is_err());
            for id in ["CP:1", "CP 1", "CP/1", "CP#1", "CPé", "CP%1", "CP,1"] {
                assert!(check_station_id_for(protocol, id).is_err(), "{id}");
            }
        }
    }

    #[test]
    fn v2_counts_characters_not_bytes() {
        for protocol in [Protocol::V201, Protocol::V21] {
            // 25 two-byte characters are under the byte cap of 1.6 but not valid.
            assert!(check_station_id_for(protocol, &"é".repeat(25)).is_err());
            // A long run of multi-byte characters is refused by character class, never by panic.
            assert!(check_station_id_for(protocol, &"é".repeat(60)).is_err());
        }
    }

    #[test]
    fn ocpp_1_6_is_unchanged() {
        let long = "a:b ".repeat(30);
        assert!(check_station_id_for(Protocol::V16, &long).is_ok());
    }
}
