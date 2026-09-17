use crate::types::errors::GuardError;

pub type Args = ();
/// How many quotes the store held.
pub type Response = Result<u64, GuardError>;
