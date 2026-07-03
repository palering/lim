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
define_id!(ArtifactId, "Unique identifier for an artifact.");
define_id!(TraceId, "Trace identifier for correlating events across a run.");
define_id!(
    GoalId,
    "Unique identifier for a Loop Engineering goal (cross-session long-running task)."
);

/// Unique identifier for a tool call.
///
/// Unlike the other ID types (which wrap `Ulid`), `ToolCallId` wraps a
/// `String`. This is because tool call IDs must be able to carry
/// provider-supplied identifiers that are not Ulids — most notably
/// Anthropic's `toolu_...` IDs, which must be echoed back verbatim in
/// `tool_result` blocks. A Ulid newtype cannot hold such strings.
///
/// `new()` still generates a Ulid-based string for internal/mock providers,
/// preserving compatibility with the OpenAI and mock code paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Create a new random ID (Ulid-based, 26-char string).
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }

    /// Wrap an existing string identifier (e.g. Anthropic's `toolu_01abc...`).
    ///
    /// The caller is responsible for ensuring the string is a valid
    /// provider-side tool call identifier.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ToolCallId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for ToolCallId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

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

    #[test]
    fn tool_call_id_holds_arbitrary_string() {
        let id = ToolCallId::from_string("toolu_01ABCdefGHI");
        assert_eq!(id.as_str(), "toolu_01ABCdefGHI");
        assert_eq!(id.to_string(), "toolu_01ABCdefGHI");
    }

    #[test]
    fn tool_call_id_serde_preserves_string() {
        let id = ToolCallId::from_string("toolu_01ABCdefGHI");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"toolu_01ABCdefGHI\"");
        let deserialized: ToolCallId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn tool_call_id_from_str_accepts_anything() {
        let id: ToolCallId = "anything-at-all".parse().unwrap();
        assert_eq!(id.as_str(), "anything-at-all");
    }
}
