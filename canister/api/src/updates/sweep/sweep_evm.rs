use crate::types::sweep::{SweepError, SweepEvmArgs, SweepEvmOk};

pub type Args = SweepEvmArgs;
pub type Response = Result<SweepEvmOk, SweepError>;
