use crate::types::sweep::{SweepError, SweepIcpArgs, SweepIcpOk};

pub type Args = SweepIcpArgs;
pub type Response = Result<SweepIcpOk, SweepError>;
