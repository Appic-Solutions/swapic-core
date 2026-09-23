use crate::types::events::Hash32;
use crate::types::swap::PausedSwapsPage;

/// Where the page starts: after this swap id, in swap id order, or at the first swap.
pub type Args = Option<Hash32>;
pub type Response = PausedSwapsPage;
