//! Scripted replies: an answer prepared in advance for the next inbound CALLs of one action.
//!
//! A scenario's `reply` step arms one. The next matching CALL from the CSMS is answered with
//! the prepared CALLRESULT payload or CALLERROR instead of the station's own handler (the
//! handler does not run, so the station state does not change). A scripted reply takes
//! precedence over a response rule for the same action. Replies are runtime-only: they are
//! neither persisted nor part of the snapshot.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::response_rules::MAX_DELAY_MS;
use crate::ocpp::frame::ErrorCode;

/// Most replies that may be armed at once (a scenario that arms more is refused).
pub const MAX_ARMED: usize = 256;

/// Most CALLs one armed reply may answer.
pub const MAX_TIMES: u32 = 1000;

/// What the station sends back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScriptedAnswer {
    /// A CALLRESULT carrying this payload object as given (not changed, not schema-checked
    /// here; the scenario loader checks it where it can).
    Result { payload: Value },
    /// A CALLERROR.
    Error {
        code: ErrorCode,
        description: String,
        details: Value,
    },
}

/// One armed reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptedReply {
    /// Identifies the arming step (the scenario runner uses the step index), so the
    /// unused ones can be reported at the end.
    pub tag: u64,
    /// Canonical action name of the CSMS command this answers.
    pub action: String,
    pub answer: ScriptedAnswer,
    /// CALLs still to answer.
    pub remaining: u32,
    /// Hold the answer back this long (milliseconds, at most one hour).
    pub delay_ms: u64,
}

/// Why a reply was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmError {
    TooMany,
    BadTimes,
    BadDelay,
    BadPayload,
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany => write!(f, "at most {MAX_ARMED} replies may be armed at once"),
            Self::BadTimes => write!(f, "`times` must be between 1 and {MAX_TIMES}"),
            Self::BadDelay => write!(f, "`delay_ms` must be at most {MAX_DELAY_MS}"),
            Self::BadPayload => f.write_str("a result payload must be a JSON object"),
        }
    }
}

/// The armed replies of a station, oldest first.
#[derive(Debug, Default)]
pub struct ReplyList(Vec<ScriptedReply>);

impl ReplyList {
    /// Arms a reply after the ones already armed.
    ///
    /// # Errors
    /// [`ArmError`] when a field is out of range or the list is full.
    pub fn arm(&mut self, reply: ScriptedReply) -> Result<(), ArmError> {
        if reply.remaining == 0 || reply.remaining > MAX_TIMES {
            return Err(ArmError::BadTimes);
        }
        if reply.delay_ms > MAX_DELAY_MS {
            return Err(ArmError::BadDelay);
        }
        if matches!(&reply.answer, ScriptedAnswer::Result { payload } if !payload.is_object()) {
            return Err(ArmError::BadPayload);
        }
        if self.0.len() >= MAX_ARMED {
            return Err(ArmError::TooMany);
        }
        self.0.push(reply);
        Ok(())
    }

    /// Takes one use of the oldest reply for `action`: its answer and delay, and what is
    /// left of it. A used-up reply leaves the list.
    pub fn take(&mut self, action: &str) -> Option<(ScriptedAnswer, u64, u32)> {
        let index = self.0.iter().position(|r| r.action == action)?;
        let reply = self.0.get_mut(index)?;
        reply.remaining = reply.remaining.saturating_sub(1);
        let taken = (reply.answer.clone(), reply.delay_ms, reply.remaining);
        if reply.remaining == 0 {
            self.0.remove(index);
        }
        Some(taken)
    }

    /// Removes every reply and returns the ones that were never (fully) used.
    pub fn drain(&mut self) -> Vec<ScriptedReply> {
        std::mem::take(&mut self.0)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl super::actor::StationActor {
    /// Arms `reply` (the action name is checked and canonicalized like a response rule's).
    pub(super) fn arm_scripted_reply(
        &mut self,
        mut reply: ScriptedReply,
    ) -> Result<super::command::Outcome, crate::error::StationError> {
        let invalid = |text: String| crate::error::StationError::InvalidArgument(text);
        let canonical = super::response_rules::canonical_action(&reply.action)
            .map_err(|error| invalid(error.to_string()))?;
        canonical.clone_into(&mut reply.action);
        let note = format!(
            "scripted reply armed: {} x{}",
            reply.action, reply.remaining
        );
        self.scripted_replies
            .arm(reply)
            .map_err(|error| invalid(error.to_string()))?;
        self.log(super::event::LogLevel::Info, note);
        Ok(super::command::Outcome::Done)
    }

    /// Removes every armed reply; the outcome lists the ones not used up.
    pub(super) fn clear_scripted_replies(&mut self) -> super::command::Outcome {
        let unused: Vec<Value> = self
            .scripted_replies
            .drain()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "tag": r.tag,
                    "action": r.action,
                    "remaining": r.remaining,
                })
            })
            .collect();
        if !unused.is_empty() {
            self.log(
                super::event::LogLevel::Info,
                format!("{} unused scripted reply(ies) cleared", unused.len()),
            );
        }
        super::command::Outcome::Response {
            payload: serde_json::json!({ "unused": unused }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn reply(tag: u64, action: &str, remaining: u32) -> ScriptedReply {
        ScriptedReply {
            tag,
            action: action.to_owned(),
            answer: ScriptedAnswer::Result {
                payload: json!({"status": "Accepted"}),
            },
            remaining,
            delay_ms: 0,
        }
    }

    #[test]
    fn oldest_reply_of_the_action_is_used_first_and_leaves_when_spent() {
        let mut list = ReplyList::default();
        list.arm(reply(1, "Reset", 2)).unwrap();
        list.arm(reply(2, "Reset", 1)).unwrap();
        list.arm(reply(3, "UnlockConnector", 1)).unwrap();
        assert_eq!(list.take("Reset").unwrap().2, 1);
        assert_eq!(list.take("Reset").unwrap().2, 0);
        assert_eq!(list.take("Reset").unwrap().2, 0);
        assert!(list.take("Reset").is_none());
        assert_eq!(list.len(), 1);
        let left = list.drain();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].tag, 3);
        assert!(list.is_empty());
    }

    #[test]
    fn out_of_range_replies_are_refused() {
        let mut list = ReplyList::default();
        assert_eq!(list.arm(reply(1, "Reset", 0)), Err(ArmError::BadTimes));
        assert_eq!(
            list.arm(reply(1, "Reset", MAX_TIMES + 1)),
            Err(ArmError::BadTimes)
        );
        let mut slow = reply(1, "Reset", 1);
        slow.delay_ms = MAX_DELAY_MS + 1;
        assert_eq!(list.arm(slow), Err(ArmError::BadDelay));
        let mut text = reply(1, "Reset", 1);
        text.answer = ScriptedAnswer::Result {
            payload: json!("x"),
        };
        assert_eq!(list.arm(text), Err(ArmError::BadPayload));
        for tag in 0..MAX_ARMED as u64 {
            list.arm(reply(tag, "Reset", 1)).unwrap();
        }
        assert_eq!(list.arm(reply(0, "Reset", 1)), Err(ArmError::TooMany));
    }
}
