use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ToolCallId};

/// A block of content within a message.
///
/// Messages are composed of multiple content blocks to support mixed content
/// (text + tool calls + images, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },

    /// Model thinking / reasoning content (not shown to user by default).
    Thinking { text: String },

    /// A tool call request from the model.
    ToolCall {
        id: ToolCallId,
        name: String,
        /// Raw JSON arguments. May be partial during streaming;
        /// validated and typed-deserialized only before tool execution.
        arguments: serde_json::Value,
    },

    /// An image (inline binary).
    Image {
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        mime_type: String,
    },

    /// Reference to an artifact produced during the session.
    ArtifactRef { artifact_id: ArtifactId, label: Option<String> },
}

impl ContentBlock {
    /// Create a text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Create a thinking content block.
    pub fn thinking(text: impl Into<String>) -> Self {
        Self::Thinking { text: text.into() }
    }

    /// Create a tool call content block.
    pub fn tool_call(
        id: ToolCallId,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::ToolCall { id, name: name.into(), arguments }
    }

    /// Returns `true` if this block is a tool call.
    pub fn is_tool_call(&self) -> bool {
        matches!(self, Self::ToolCall { .. })
    }

    /// If this is a tool call, return its ID and name.
    pub fn as_tool_call(&self) -> Option<(&ToolCallId, &str, &serde_json::Value)> {
        match self {
            Self::ToolCall { id, name, arguments } => Some((id, name.as_str(), arguments)),
            _ => None,
        }
    }
}

/// Base64 encoding/decoding for byte vectors in serde.
mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize bytes as a base64 string.
    ///
    /// # Errors
    /// Returns an error if the serializer fails to write the string.
    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let encoded = STANDARD.encode(data);
        serializer.serialize_str(&encoded)
    }

    /// Deserialize a base64 string into bytes.
    ///
    /// # Errors
    /// Returns an error if the input is not a valid base64 string.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block() {
        let block = ContentBlock::text("hello");
        assert!(!block.is_tool_call());
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
    }

    #[test]
    fn tool_call_block() {
        let id = ToolCallId::new();
        let block = ContentBlock::tool_call(id.clone(), "bash", serde_json::json!({"cmd": "ls"}));
        assert!(block.is_tool_call());
        let (tid, name, args) = block.as_tool_call().unwrap();
        assert_eq!(*tid, id);
        assert_eq!(name, "bash");
        assert_eq!(args["cmd"], "ls");
    }

    #[test]
    fn content_block_serde_roundtrip() {
        let block = ContentBlock::thinking("let me think...");
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        match deserialized {
            ContentBlock::Thinking { text } => assert_eq!(text, "let me think..."),
            _ => panic!("expected Thinking block"),
        }
    }

    #[test]
    fn image_block_base64_roundtrip() {
        let pixels = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]; // fake JPEG header
        let block =
            ContentBlock::Image { data: pixels.clone(), mime_type: "image/jpeg".to_string() };
        let json = serde_json::to_string(&block).unwrap();
        // Verify it's a base64 string, not a byte array
        assert!(json.contains("\"data\":\"/9j/4AAQ\""), "expected base64 string in JSON: {json}");
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        match deserialized {
            ContentBlock::Image { data, mime_type } => {
                assert_eq!(data, pixels);
                assert_eq!(mime_type, "image/jpeg");
            }
            _ => panic!("expected Image block"),
        }
    }
}
