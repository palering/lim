use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ulid::Ulid;

/// Generate a newtype ID wrapper around Ulid.
macro_rules! define_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            /// Create a new random ID.
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            /// Create from a raw Ulid value.
            pub fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// Get the inner Ulid.
            pub fn as_ulid(&self) -> &Ulid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self)
            }
        }
    };
}

define_id!(SessionId, "Unique identifier for a session.");
define_id!(BranchId, "Unique identifier for a conversation branch.");
define_id!(TurnId, "Unique identifier for a single turn in the agent loop.");
define_id!(MessageId, "Unique identifier for a message.");
define_id!(ToolCallId, "Unique identifier for a tool call.");
define_id!(ArtifactId, "Unique identifier for an artifact.");
define_id!(TraceId, "Trace identifier for correlating events across a run.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip() {
        let id = SessionId::new();
        let s = id.to_string();
        let parsed: SessionId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn id_display_not_empty() {
        let id = MessageId::new();
        assert!(!id.to_string().is_empty());
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = ToolCallId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: ToolCallId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn ids_are_unique() {
        let a = BranchId::new();
        let b = BranchId::new();
        assert_ne!(a, b);
    }
}
