use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::message::AgentMessage;

/// Reconstruct a transcript from a sequence of event envelopes.
///
/// Extracts `MessageEnd` events, which carry the fully assembled `AgentMessage`.
/// The result is an ordered list of messages representing the conversation
/// as seen by the agent runtime.
pub fn replay_transcript(events: &[EventEnvelope]) -> Vec<AgentMessage> {
    events
        .iter()
        .filter_map(|envelope| {
            if let AgentEvent::MessageEnd { message, .. } = &envelope.event {
                Some(message.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Extract only assistant messages from a transcript.
pub fn assistant_messages(transcript: &[AgentMessage]) -> Vec<&AgentMessage> {
    transcript
        .iter()
        .filter(|m| m.role == lim_core::message::Role::Assistant)
        .collect()
}

/// Extract only user messages from a transcript.
pub fn user_messages(transcript: &[AgentMessage]) -> Vec<&AgentMessage> {
    transcript
        .iter()
        .filter(|m| m.role == lim_core::message::Role::User)
        .collect()
}

/// Count the number of tool calls across all messages in a transcript.
pub fn count_tool_calls(transcript: &[AgentMessage]) -> usize {
    transcript
        .iter()
        .map(|m| m.tool_calls().len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_core::event::{AgentEvent, EventEnvelope};
    use lim_core::id::{BranchId, SessionId, ToolCallId, TraceId};
    use lim_core::message::{AgentMessage, Role};

    fn make_message_end(msg: AgentMessage) -> EventEnvelope {
        let mid = msg.id;
        EventEnvelope::new(
            0,
            SessionId::new(),
            BranchId::new(),
            TraceId::new(),
            AgentEvent::MessageEnd {
                message_id: mid,
                message: msg,
            },
        )
    }

    #[test]
    fn replay_empty_transcript() {
        let events: Vec<EventEnvelope> = vec![];
        let transcript = replay_transcript(&events);
        assert!(transcript.is_empty());
    }

    #[test]
    fn replay_extracts_messages() {
        let user_msg = AgentMessage::user("Hello");
        let assistant_msg = AgentMessage::assistant(vec![ContentBlock::text("Hi there!")]);

        let events = vec![
            EventEnvelope::new(
                0,
                SessionId::new(),
                BranchId::new(),
                TraceId::new(),
                AgentEvent::AgentStart,
            ),
            make_message_end(user_msg),
            make_message_end(assistant_msg),
            EventEnvelope::new(
                3,
                SessionId::new(),
                BranchId::new(),
                TraceId::new(),
                AgentEvent::AgentEnd,
            ),
        ];

        let transcript = replay_transcript(&events);
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].role, Role::User);
        assert_eq!(transcript[1].role, Role::Assistant);
    }

    #[test]
    fn replay_with_tool_calls() {
        let tc_id = ToolCallId::new();
        let user_msg = AgentMessage::user("read the file");
        let assistant_msg = AgentMessage::assistant(vec![ContentBlock::tool_call(
            tc_id,
            "read",
            serde_json::json!({"path": "test.txt"}),
        )]);
        let tool_msg =
            AgentMessage::tool_result(tc_id, "file contents here", false);
        let final_msg =
            AgentMessage::assistant(vec![ContentBlock::text("The file says...")]);

        let events = vec![
            make_message_end(user_msg),
            make_message_end(assistant_msg),
            make_message_end(tool_msg),
            make_message_end(final_msg),
        ];

        let transcript = replay_transcript(&events);
        assert_eq!(transcript.len(), 4);
        assert_eq!(count_tool_calls(&transcript), 1);

        let asst = assistant_messages(&transcript);
        assert_eq!(asst.len(), 2);
    }

    #[test]
    fn replay_skips_non_message_events() {
        let events = vec![
            EventEnvelope::new(
                0,
                SessionId::new(),
                BranchId::new(),
                TraceId::new(),
                AgentEvent::AgentStart,
            ),
            EventEnvelope::new(
                1,
                SessionId::new(),
                BranchId::new(),
                TraceId::new(),
                AgentEvent::TurnStart {
                    turn_id: lim_core::id::TurnId::new(),
                },
            ),
            EventEnvelope::new(
                2,
                SessionId::new(),
                BranchId::new(),
                TraceId::new(),
                AgentEvent::AgentEnd,
            ),
        ];

        let transcript = replay_transcript(&events);
        assert!(transcript.is_empty());
    }
}
