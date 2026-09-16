use crate::types::config::Config;
use crate::types::errors::SetConfigError;

pub type Args = Config;
pub type Response = Result<(), SetConfigError>;
