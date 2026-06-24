//! Agent loop — the core double-loop that drives LLM ↔ tool interactions.
//!
//! Outer loop: follow-up queue (new tasks appended after agent would stop).
//! Inner loop: steering queue (user interrupts) + assistant turn + tool calls.
