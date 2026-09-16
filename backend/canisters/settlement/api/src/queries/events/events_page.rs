use crate::types::events::EventEnvelope;

/// Positional, as the endpoint takes them: `(start, len)`.
pub type Args = (u64, u64);
pub type Response = Vec<EventEnvelope>;
