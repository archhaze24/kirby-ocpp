//! Scripted replies on the 2.0.1 station: answers armed by a scenario `reply` step for the next
//! inbound CALLs of one action (the 1.6 counterpart is `station::scripted_reply`).
//!
//! An armed reply answers the CALL instead of the station's handler, so the handler does not
//! run and the station state does not change. The check comes right after the registration
//! gate, so a rejected registration still refuses CSMS CALLs. Replies are runtime-only: not
//! persisted and not part of the snapshot.

use std::time::Duration;

use serde_json::{Value, json};

use super::actor::Station2Actor;
use super::command::Outcome2;
use super::link::sanitize;
use super::types::Reply2;
use crate::error::StationError;
use crate::ocpp::ReplyError;
use crate::ocpp::responder::ReplyHandle;
use crate::ocpp::v2::registry;
use crate::station::LogLevel;
use crate::station::scripted_reply::{ScriptedAnswer, ScriptedReply};

/// The registry spelling of a CSMS-originated action, matched case-insensitively.
///
/// # Errors
/// The action is not an OCPP 2.0.1 action, or the CSMS cannot send it.
fn canonical_action(name: &str) -> Result<&'static str, StationError> {
    let name = name.trim();
    let entry = registry::iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            StationError::InvalidArgument(format!(
                "`{}` is not an OCPP 2.0.1 action",
                sanitize(name)
            ))
        })?;
    if entry.direction.csms_may_send() {
        Ok(entry.name)
    } else {
        Err(StationError::InvalidArgument(format!(
            "{} is sent by the station, not by the CSMS; only CSMS actions can be answered",
            entry.name
        )))
    }
}

impl Station2Actor {
    /// `Command2::ArmScriptedReply`: arms `scripted` after the ones already armed. The action
    /// name is canonicalized against the 2.0.1 registry.
    pub(super) fn cmd_arm_scripted_reply(&mut self, mut scripted: ScriptedReply, reply: Reply2) {
        let result = canonical_action(&scripted.action).and_then(|canonical| {
            canonical.clone_into(&mut scripted.action);
            let note = format!(
                "scripted reply armed: {} x{}",
                scripted.action, scripted.remaining
            );
            self.scripted_replies
                .arm(scripted)
                .map_err(|error| StationError::InvalidArgument(error.to_string()))?;
            self.log(LogLevel::Info, note);
            Ok(Outcome2::Done)
        });
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(result);
    }

    /// `Command2::ClearScriptedReplies`: removes every armed reply; the outcome lists the ones
    /// that were not used up.
    pub(super) fn cmd_clear_scripted_replies(&mut self, reply: Reply2) {
        let unused: Vec<Value> = self
            .scripted_replies
            .drain()
            .into_iter()
            .map(|armed| {
                json!({
                    "tag": armed.tag,
                    "action": armed.action,
                    "remaining": armed.remaining,
                })
            })
            .collect();
        if !unused.is_empty() {
            self.log(
                LogLevel::Info,
                format!("{} unused scripted reply(ies) cleared", unused.len()),
            );
        }
        let _ = reply.send(Ok(Outcome2::Response {
            payload: json!({ "unused": unused }),
        }));
    }

    /// Answers the CALL with an armed scripted reply.
    ///
    /// Returns the handle back when no reply is armed for `action`; `None` when the CALL was
    /// answered here (exactly once, possibly after the reply's delay).
    pub(super) fn apply_scripted_reply(
        &mut self,
        action: &str,
        reply: ReplyHandle,
    ) -> Option<ReplyHandle> {
        let Some((answer, delay_ms, left)) = self.scripted_replies.take(action) else {
            return Some(reply);
        };
        self.log(
            LogLevel::Warn,
            format!(
                "scripted reply: {} answered ({left} more)",
                sanitize(action)
            ),
        );
        let reply = if delay_ms > 0 {
            reply.delayed(Duration::from_millis(delay_ms))
        } else {
            reply
        };
        let result: Result<(), ReplyError> = match answer {
            ScriptedAnswer::Result { payload } => reply.accept_value(payload),
            ScriptedAnswer::Error {
                code,
                description,
                details,
            } => reply.reject_with_details(code, description, details),
        };
        if let Err(error) = result {
            self.log(
                LogLevel::Warn,
                format!("could not reply to {}: {error}", sanitize(action)),
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_canonicalized_and_checked_against_the_registry() {
        assert_eq!(canonical_action("reset").unwrap(), "Reset");
        assert_eq!(
            canonical_action(" requestStartTransaction ").unwrap(),
            "RequestStartTransaction"
        );
        // Both directions may send a DataTransfer.
        assert_eq!(canonical_action("DATATRANSFER").unwrap(), "DataTransfer");
        assert!(matches!(
            canonical_action("BootNotification"),
            Err(StationError::InvalidArgument(text)) if text.contains("station")
        ));
        assert!(matches!(
            canonical_action("Frobnicate"),
            Err(StationError::InvalidArgument(_))
        ));
        // A 1.6-only name is not a 2.0.1 action.
        assert!(canonical_action("RemoteStartTransaction").is_err());
    }
}
