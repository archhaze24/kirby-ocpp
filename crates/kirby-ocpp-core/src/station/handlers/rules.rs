//! Applies response rules (see `station::response_rules`) to an inbound CALL before its handler.

use std::time::Duration;

use super::StationActor;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::ReplyHandle;
use crate::station::event::LogLevel;
use crate::station::response_rules::{Behavior, ResponseRule, RuleList, reject_payload};

impl StationActor {
    /// Looks up a rule for `action` and applies it.
    ///
    /// Returns the handle for the normal handler, or `None` when the rule dealt with the CALL
    /// (replied, dropped the connection or ignored it); the handler must then not run. Only
    /// `delay` returns a handle: one whose reply is held back.
    pub(super) fn apply_response_rule(
        &mut self,
        action: &str,
        reply: ReplyHandle,
    ) -> Option<ReplyHandle> {
        let Some(behavior) = self.state.response_rules.take_rule(action) else {
            return Some(reply);
        };
        let left = self
            .state
            .response_rules
            .iter()
            .find(|r: &&ResponseRule| r.action == action)
            .and_then(|r| r.remaining)
            .map_or_else(String::new, |n| format!(", {n} more"));
        self.log(
            LogLevel::Warn,
            format!("response rule: {action} -> {behavior}{left}"),
        );
        match behavior {
            Behavior::Delay { ms } => Some(reply.delayed(Duration::from_millis(ms))),
            Behavior::Reject => {
                let result = match reject_payload(action) {
                    Some(payload) => reply.accept_value(payload),
                    None => reply.reject(
                        ErrorCode::NotSupported,
                        format!("{action} rejected by a response rule"),
                    ),
                };
                self.log_rule_reply_error(action, result);
                None
            }
            Behavior::Error { code, description } => {
                let text = description.unwrap_or_else(|| "response rule".to_owned());
                let result = reply.reject(code, text);
                self.log_rule_reply_error(action, result);
                None
            }
            Behavior::Drop => {
                // No reply at all (not even the InternalError fallback), then the close; the
                // normal reconnect lifecycle follows from the `Closed` event.
                reply.abandon();
                if let Some(connection) = &self.link.conn {
                    connection.close();
                }
                None
            }
            Behavior::Ignore => {
                reply.abandon();
                None
            }
        }
    }

    fn log_rule_reply_error(&self, action: &str, result: Result<(), crate::ocpp::ReplyError>) {
        if let Err(error) = result {
            self.log(
                LogLevel::Warn,
                format!("could not reply to {action}: {error}"),
            );
        }
    }
}
