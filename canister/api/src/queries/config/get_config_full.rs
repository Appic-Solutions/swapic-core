use crate::types::config::Config;
use crate::types::errors::GuardError;

pub type Args = ();
pub type Response = Result<Config, GuardError>;
