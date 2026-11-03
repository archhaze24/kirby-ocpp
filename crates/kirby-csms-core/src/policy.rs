//! WebSocket subprotocol negotiation policy and transaction id allocation.

/// How the central system answers the WebSocket subprotocol negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprotocolPolicy {
    /// Select `name` if the client offered it; refuse the upgrade (HTTP 400) otherwise.
    /// The default is `Require("ocpp1.6")`.
    Require(String),
    /// Always select `name`, even if the client did not offer it. Use this to test
    /// a charge point that must notice a protocol it did not ask for.
    Select(String),
    /// Accept the upgrade without selecting any subprotocol (no
    /// `Sec-WebSocket-Protocol` response header).
    Omit,
    /// Refuse every upgrade with HTTP 400.
    Reject,
}

impl Default for SubprotocolPolicy {
    fn default() -> Self {
        Self::Require("ocpp1.6".to_owned())
    }
}

impl SubprotocolPolicy {
    /// The subprotocol to select for a client that `offered` these (`None` selects
    /// nothing), or the reason the upgrade is refused.
    pub fn select(&self, offered: &[String]) -> Result<Option<String>, &'static str> {
        match self {
            Self::Require(name) if offered.iter().any(|offer| offer == name) => {
                Ok(Some(name.clone()))
            }
            Self::Require(_) => Err("required subprotocol was not offered"),
            Self::Select(name) => Ok(Some(name.clone())),
            Self::Omit => Ok(None),
            Self::Reject => Err("handshake rejected by configuration"),
        }
    }
}

/// How `StartTransaction` transaction ids are allocated by the default responder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionIds {
    /// Every `StartTransaction` gets this id (the TS harness uses 123).
    Fixed(i32),
    /// The first `StartTransaction` gets this id, each following one gets the next integer.
    Incrementing(i32),
}

impl Default for TransactionIds {
    fn default() -> Self {
        Self::Fixed(123)
    }
}

impl TransactionIds {
    /// A fresh allocator following this policy.
    pub fn allocator(self) -> TransactionIdAllocator {
        TransactionIdAllocator {
            policy: self,
            next: match self {
                Self::Fixed(id) | Self::Incrementing(id) => id,
            },
        }
    }
}

/// Hands out transaction ids according to a [`TransactionIds`] policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionIdAllocator {
    policy: TransactionIds,
    next: i32,
}

impl TransactionIdAllocator {
    /// The id for the next `StartTransaction`. Wraps instead of overflowing.
    pub fn allocate(&mut self) -> i32 {
        match self.policy {
            TransactionIds::Fixed(id) => id,
            TransactionIds::Incrementing(_) => {
                let id = self.next;
                self.next = self.next.wrapping_add(1);
                id
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_follows_policy() {
        let mut fixed = TransactionIds::Fixed(7).allocator();
        assert_eq!((fixed.allocate(), fixed.allocate()), (7, 7));
        let mut inc = TransactionIds::Incrementing(10).allocator();
        assert_eq!((inc.allocate(), inc.allocate()), (10, 11));
    }

    #[test]
    fn subprotocol_selection() {
        let offered = vec!["ocpp2.0.1".to_owned(), "ocpp1.6".to_owned()];
        let policy = SubprotocolPolicy::default();
        assert_eq!(policy.select(&offered), Ok(Some("ocpp1.6".to_owned())));
        assert!(policy.select(&[]).is_err());
        assert_eq!(SubprotocolPolicy::Omit.select(&[]), Ok(None));
        assert!(SubprotocolPolicy::Reject.select(&offered).is_err());
    }
}
