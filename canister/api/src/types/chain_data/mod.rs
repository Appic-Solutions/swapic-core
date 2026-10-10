use candid::{CandidType, Nat};
use serde::Deserialize;
use types::chain_data::{ChainDataError as DomainError, ChainReading};
use types::numeric::{BlockNumber, WeiPerGas};

/// One chain as the watcher last saw it. There is no instant on it: the canister stamps
/// what it receives, so nothing a caller sends can date its data forward.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChainData {
    /// The head block the watcher saw.
    pub block: u64,
    /// The base fee of that block.
    pub base_fee_wei_per_gas: Nat,
    /// The tip to pay on top of the base fee.
    pub priority_fee_wei_per_gas: Nat,
}

/// A cached reading with the canister time it arrived at.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChainDataEntry {
    pub data: ChainData,
    pub pushed_at_ns: u64,
}

/// Why a pushed reading is not one this canister prices gas with.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ChainDataError {
    FeeTooLarge { field: String },
}

/// A fee as a 256-bit price, or the field that does not fit in one.
fn fee(value: Nat, field: &'static str) -> Result<WeiPerGas, DomainError> {
    WeiPerGas::try_from(value).map_err(|_| DomainError::FeeTooLarge { field })
}

impl TryFrom<ChainData> for ChainReading {
    type Error = DomainError;

    fn try_from(data: ChainData) -> Result<Self, Self::Error> {
        Ok(Self {
            block: BlockNumber::new(data.block),
            base_fee: fee(data.base_fee_wei_per_gas, "base_fee_wei_per_gas")?,
            priority_fee: fee(data.priority_fee_wei_per_gas, "priority_fee_wei_per_gas")?,
        })
    }
}

impl From<ChainReading> for ChainData {
    fn from(reading: ChainReading) -> Self {
        Self {
            block: reading.block.get(),
            base_fee_wei_per_gas: reading.base_fee.into(),
            priority_fee_wei_per_gas: reading.priority_fee.into(),
        }
    }
}

impl From<types::chain_data::ChainData> for ChainDataEntry {
    fn from(data: types::chain_data::ChainData) -> Self {
        Self {
            data: data.reading().into(),
            pushed_at_ns: data.pushed_at.as_nanos(),
        }
    }
}

impl From<DomainError> for ChainDataError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::FeeTooLarge { field } => Self::FeeTooLarge {
                field: field.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
