//! Scripted replies on the 2.0.1 station: answers armed by a scenario `reply` step for the next
//! inbound CALLs of one action (the 1.6 counterpart is `station::scripted_reply`).
//!
//! The commands exist and answer `NotImplemented`; arming and consuming replies is not
//! written yet.

use super::actor::Station2Actor;
use super::types::Reply2;
use crate::error::StationError;
use crate::station::scripted_reply::ScriptedReply;

impl Station2Actor {
    /// `Command2::ArmScriptedReply`.
    #[allow(
        clippy::needless_pass_by_value,
        clippy::unused_self,
        reason = "the reply list on the actor takes ownership once it is implemented"
    )]
    pub(super) fn cmd_arm_scripted_reply(&mut self, _scripted: ScriptedReply, reply: Reply2) {
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(Err(StationError::NotImplemented {
            command: "arm_scripted_reply",
        }));
    }

    /// `Command2::ClearScriptedReplies`.
    #[allow(
        clippy::unused_self,
        reason = "the reply list on the actor is read here once it is implemented"
    )]
    pub(super) fn cmd_clear_scripted_replies(&mut self, reply: Reply2) {
        let _ = reply.send(Err(StationError::NotImplemented {
            command: "clear_scripted_replies",
        }));
    }
}
