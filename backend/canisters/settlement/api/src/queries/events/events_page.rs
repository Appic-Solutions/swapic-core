use crate::types::events::Event;

/// Positional, as the endpoint takes them: `(start, len)`.
pub type Args = (u64, u64);
pub type Response = Vec<Event>;
