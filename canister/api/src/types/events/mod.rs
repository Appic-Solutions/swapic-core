use crate::types::pool::IcpAccount;
use crate::types::sweep::{SweepAsset, SweepTarget};
use candid::{CandidType, Nat};
use serde::Deserialize;
use types::events::EventError as DomainEventError;
use types::{
    BlockNumber, ChainId, GasAmount, Nonce, QuoteHash, TokenAmount, TxHash, Wei, WeiPerGas,
};

pub type Hash32 = [u8; 32];

/// The wire error a field named `field` answers when its value is above `u128::MAX`.
fn too_large(field: &'static str) -> DomainEventError {
    DomainEventError::amount_too_large(field)
}

/// One entry of the log, kept forever. `index` counts from zero with no gap; `time_ns` is
/// IC time in nanoseconds.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub index: u64,
    pub time_ns: u64,
    pub payload: EventType,
}

/// What happened. A `Flow`, `Books` or `Pool` line carries its step as `step_bytes`, the
/// step's minicbor encoding, with `step` its name and `detail` a readable rendering.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum EventType {
    /// `json` is the public view of the config written: JSON of `Config` with every rpc url
    /// as `"***"`.
    ConfigChanged { json: String },
    /// Principals as text, so the line reads without a decoder.
    RolesChanged { quoter: String, watcher: String },
    /// A nonce was allocated to an outbound transaction, before the signature was asked
    /// for. `to` is an EIP-55 address.
    TxCreated {
        purpose: TxPurpose,
        chain_id: u64,
        nonce: u64,
        to: String,
        value_wei: Nat,
        data: Vec<u8>,
        gas_limit: Nat,
        max_fee_wei_per_gas: Nat,
        max_priority_fee_wei_per_gas: Nat,
        /// The gas action whose budget the transaction is held to (`relay_cctp`,
        /// `complete_swap`, `sweep`, ...).
        action: Option<String>,
    },
    /// An open transaction was re-sent at the same nonce with a higher fee, with the bytes
    /// that were broadcast.
    TxReplaced {
        purpose: TxPurpose,
        chain_id: u64,
        nonce: u64,
        max_fee_wei_per_gas: Nat,
        max_priority_fee_wei_per_gas: Nat,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    /// A nonce that was allocated and never signed for was spent by a zero-value
    /// self-transfer, so the chain is never left with a gap it cannot mine past.
    TxCancelled {
        chain_id: u64,
        nonce: u64,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    /// A transaction the canister signed reached its end: mined at the depth the outbox
    /// waits for (`tx_hash` the one that landed), or its nonce spent by a cancel.
    TxFinal {
        purpose: TxPurpose,
        chain_id: u64,
        nonce: u64,
        tx_hash: Hash32,
        outcome: TxOutcome,
    },
    /// A step of a flow: a gasless relay, or a CCTP finish.
    Flow {
        quote_hash: Hash32,
        step: String,
        detail: String,
        step_bytes: Vec<u8>,
    },
    /// A step of a chain's USDC books: a fee earned, a check that found a shortfall or a
    /// new surplus, a pause.
    Books {
        chain_id: u64,
        step: String,
        detail: String,
        step_bytes: Vec<u8>,
    },
    /// A step of the pool.
    Pool {
        step: String,
        detail: String,
        step_bytes: Vec<u8>,
    },
    /// A controller's sweep, its amount taken off the books it came from as it was signed
    /// (EVM) or asked of the ledger (ICP). `amount` is in the asset's smallest unit.
    Swept {
        id: u64,
        asset: SweepAsset,
        to: SweepTarget,
        amount: Nat,
        /// Of `amount`, what came off the pool ledger's fee line: `fees_base` for USDC on
        /// the pool chain, `fees_icp` for ckUSDC. Zero for native gas, ckETH and ICP.
        from_pool_fees: Nat,
        /// Of `amount`, what came off the chain books' `fee_income`. Zero but for USDC.
        from_fee_income: Nat,
    },
    /// A sweep that moved nothing (reverted, cancelled, or refused by the ledger): its
    /// amount is back on the books it came from.
    SweepReverted { id: u64, why: String },
}

/// How a transaction the canister signed ended.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxOutcome {
    Succeeded {
        block: u64,
    },
    Reverted {
        block: u64,
    },
    /// Its nonce was spent by a cancel: it never ran.
    Cancelled,
}

/// Why an outbound transaction exists.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxPurpose {
    /// A same-nonce zero-value self-transfer on that chain.
    Cancel(u64),
    /// A gasless order relayed, by its relay key.
    Relay(Hash32),
    /// A CCTP arrival finished, by its finish key.
    Finish(Hash32),
    /// A pool payout from the canister's Base pocket, by its payout id.
    PoolPayout(u64),
    /// An orphan sent back, by the hash of its credit key.
    PoolTransfer(Hash32),
    /// The canister's standing USDC allowance to Swapic.
    PoolApprove,
    /// A rebalance move's own transaction, by the move's id.
    PoolMove(u64),
    /// Eco's `cancelAndProve` of an unfilled intent, by the order id that published it.
    EcoRescue(Hash32),
    /// A controller's sweep, by its id.
    Sweep(u64),
    /// The canister's claim of its own deferred Across refunds on that chain.
    AcrossClaim(u64),
}

impl From<types::events::TxPurpose> for TxPurpose {
    fn from(purpose: types::events::TxPurpose) -> Self {
        use types::events::TxPurpose as Domain;
        match purpose {
            Domain::Cancel(chain_id) => Self::Cancel(chain_id.get()),
            Domain::Relay(hash) => Self::Relay(hash.into_bytes()),
            Domain::Finish(hash) => Self::Finish(hash.into_bytes()),
            Domain::PoolPayout(id) => Self::PoolPayout(id),
            Domain::PoolTransfer(key) => Self::PoolTransfer(key),
            Domain::PoolApprove => Self::PoolApprove,
            Domain::PoolMove(id) => Self::PoolMove(id),
            Domain::EcoRescue(order_id) => Self::EcoRescue(order_id),
            Domain::Sweep(id) => Self::Sweep(id),
            Domain::AcrossClaim(chain_id) => Self::AcrossClaim(chain_id.get()),
        }
    }
}

impl From<TxPurpose> for types::events::TxPurpose {
    fn from(purpose: TxPurpose) -> Self {
        match purpose {
            TxPurpose::Cancel(chain_id) => Self::Cancel(ChainId::new(chain_id)),
            TxPurpose::Relay(hash) => Self::Relay(QuoteHash::new(hash)),
            TxPurpose::Finish(hash) => Self::Finish(QuoteHash::new(hash)),
            TxPurpose::PoolPayout(id) => Self::PoolPayout(id),
            TxPurpose::PoolTransfer(key) => Self::PoolTransfer(key),
            TxPurpose::PoolApprove => Self::PoolApprove,
            TxPurpose::PoolMove(id) => Self::PoolMove(id),
            TxPurpose::EcoRescue(order_id) => Self::EcoRescue(order_id),
            TxPurpose::Sweep(id) => Self::Sweep(id),
            TxPurpose::AcrossClaim(chain_id) => Self::AcrossClaim(ChainId::new(chain_id)),
        }
    }
}

impl From<types::events::TxOutcome> for TxOutcome {
    fn from(outcome: types::events::TxOutcome) -> Self {
        use types::events::TxOutcome as Domain;
        match outcome {
            Domain::Succeeded { block } => Self::Succeeded { block: block.get() },
            Domain::Reverted { block } => Self::Reverted { block: block.get() },
            Domain::Cancelled => Self::Cancelled,
        }
    }
}

impl From<TxOutcome> for types::events::TxOutcome {
    fn from(outcome: TxOutcome) -> Self {
        match outcome {
            TxOutcome::Succeeded { block } => Self::Succeeded {
                block: BlockNumber::new(block),
            },
            TxOutcome::Reverted { block } => Self::Reverted {
                block: BlockNumber::new(block),
            },
            TxOutcome::Cancelled => Self::Cancelled,
        }
    }
}

impl From<types::Event> for Event {
    fn from(event: types::Event) -> Self {
        Self {
            index: event.index.get(),
            time_ns: event.timestamp.as_nanos(),
            payload: event.payload.into(),
        }
    }
}

impl From<types::EventType> for EventType {
    fn from(payload: types::EventType) -> Self {
        use types::EventType as Domain;
        match payload {
            Domain::ConfigChanged { json } => Self::ConfigChanged { json },
            Domain::RolesChanged { quoter, watcher } => Self::RolesChanged { quoter, watcher },
            Domain::TxCreated {
                purpose,
                chain_id,
                nonce,
                to,
                value,
                data,
                gas_limit,
                max_fee,
                max_priority_fee,
                action,
            } => Self::TxCreated {
                purpose: purpose.into(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                to: to.to_string(),
                value_wei: value.into(),
                data,
                gas_limit: gas_limit.into(),
                max_fee_wei_per_gas: max_fee.into(),
                max_priority_fee_wei_per_gas: max_priority_fee.into(),
                action: action.map(|action| action.name().to_string()),
            },
            Domain::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee,
                max_priority_fee,
                tx_hash,
                raw_tx,
            } => Self::TxReplaced {
                purpose: purpose.into(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                max_fee_wei_per_gas: max_fee.into(),
                max_priority_fee_wei_per_gas: max_priority_fee.into(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
            Domain::TxCancelled {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::TxCancelled {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
            Domain::TxFinal {
                purpose,
                chain_id,
                nonce,
                tx_hash,
                outcome,
            } => Self::TxFinal {
                purpose: purpose.into(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                tx_hash: tx_hash.into_bytes(),
                outcome: outcome.into(),
            },
            Domain::Flow { quote_hash, step } => Self::Flow {
                quote_hash: quote_hash.into_bytes(),
                step: step.name().to_string(),
                detail: detail_of(&step),
                step_bytes: step.to_bytes(),
            },
            Domain::Books { chain_id, step } => Self::Books {
                chain_id: chain_id.get(),
                step: step.name().to_string(),
                detail: detail_of(&step),
                step_bytes: step.to_bytes(),
            },
            Domain::Pool { step } => Self::Pool {
                step: step.name().to_string(),
                detail: detail_of(&step),
                step_bytes: step.to_bytes(),
            },
            Domain::Swept { sweep } => Self::Swept {
                id: sweep.id,
                asset: sweep.asset.into(),
                to: sweep.to.into(),
                amount: sweep.amount.into(),
                from_pool_fees: sweep.from_pool_fees.into(),
                from_fee_income: sweep.from_fee_income.into(),
            },
            Domain::SweepReverted { id, why } => Self::SweepReverted { id, why },
        }
    }
}

impl TryFrom<EventType> for types::EventType {
    type Error = DomainEventError;

    fn try_from(payload: EventType) -> Result<Self, Self::Error> {
        Ok(match payload {
            EventType::ConfigChanged { json } => Self::ConfigChanged { json },
            EventType::RolesChanged { quoter, watcher } => Self::RolesChanged { quoter, watcher },
            EventType::TxCreated {
                purpose,
                chain_id,
                nonce,
                to,
                value_wei,
                data,
                gas_limit,
                max_fee_wei_per_gas,
                max_priority_fee_wei_per_gas,
                action,
            } => Self::TxCreated {
                purpose: purpose.into(),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                to: to.parse().map_err(DomainEventError::not_an_address("to"))?,
                value: Wei::try_from(value_wei).map_err(|_| too_large("value_wei"))?,
                data,
                gas_limit: GasAmount::try_from(gas_limit).map_err(|_| too_large("gas_limit"))?,
                max_fee: WeiPerGas::try_from(max_fee_wei_per_gas)
                    .map_err(|_| too_large("max_fee_wei_per_gas"))?,
                max_priority_fee: WeiPerGas::try_from(max_priority_fee_wei_per_gas)
                    .map_err(|_| too_large("max_priority_fee_wei_per_gas"))?,
                action: action
                    .map(|name| {
                        types::config::GasAction::from_name(&name)
                            .ok_or(DomainEventError::NotAGasAction { field: "action" })
                    })
                    .transpose()?,
            },
            EventType::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee_wei_per_gas,
                max_priority_fee_wei_per_gas,
                tx_hash,
                raw_tx,
            } => Self::TxReplaced {
                purpose: purpose.into(),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                max_fee: WeiPerGas::try_from(max_fee_wei_per_gas)
                    .map_err(|_| too_large("max_fee_wei_per_gas"))?,
                max_priority_fee: WeiPerGas::try_from(max_priority_fee_wei_per_gas)
                    .map_err(|_| too_large("max_priority_fee_wei_per_gas"))?,
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
            EventType::TxCancelled {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::TxCancelled {
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
            EventType::TxFinal {
                purpose,
                chain_id,
                nonce,
                tx_hash,
                outcome,
            } => Self::TxFinal {
                purpose: purpose.into(),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                tx_hash: TxHash::new(tx_hash),
                outcome: outcome.into(),
            },
            EventType::Flow {
                quote_hash,
                step_bytes,
                ..
            } => Self::Flow {
                quote_hash: QuoteHash::new(quote_hash),
                step: types::FlowStep::from_bytes(&step_bytes).ok_or(
                    DomainEventError::NotAFlowStep {
                        field: "step_bytes",
                    },
                )?,
            },
            EventType::Books {
                chain_id,
                step_bytes,
                ..
            } => Self::Books {
                chain_id: ChainId::new(chain_id),
                step: types::books::BookStep::from_bytes(&step_bytes).ok_or(
                    DomainEventError::NotABookStep {
                        field: "step_bytes",
                    },
                )?,
            },
            EventType::Pool { step_bytes, .. } => Self::Pool {
                step: types::pool::PoolStep::from_bytes(&step_bytes).ok_or(
                    DomainEventError::NotAPoolStep {
                        field: "step_bytes",
                    },
                )?,
            },
            EventType::Swept {
                id,
                asset,
                to,
                amount,
                from_pool_fees,
                from_fee_income,
            } => Self::Swept {
                sweep: types::sweep::Sweep {
                    id,
                    asset: asset.into(),
                    to: sweep_to(to)?,
                    amount: TokenAmount::try_from(amount).map_err(|_| too_large("amount"))?,
                    from_pool_fees: TokenAmount::try_from(from_pool_fees)
                        .map_err(|_| too_large("from_pool_fees"))?,
                    from_fee_income: TokenAmount::try_from(from_fee_income)
                        .map_err(|_| too_large("from_fee_income"))?,
                },
            },
            EventType::SweepReverted { id, why } => Self::SweepReverted { id, why },
        })
    }
}

fn sweep_to(target: SweepTarget) -> Result<types::sweep::SweepTo, DomainEventError> {
    Ok(match target {
        SweepTarget::Evm { chain_id, to } => types::sweep::SweepTo::Evm {
            chain: ChainId::new(chain_id),
            to: to.parse().map_err(DomainEventError::not_an_address("to"))?,
        },
        SweepTarget::Icp(account) => types::sweep::SweepTo::Icp(icp_account(account)?),
    })
}

fn icp_account(account: IcpAccount) -> Result<types::pool::IcpAccount, DomainEventError> {
    let subaccount = account
        .subaccount
        .map(|bytes| <[u8; 32]>::try_from(bytes.as_slice()))
        .transpose()
        .map_err(|_| DomainEventError::NotAnAccount { field: "to" })?;
    Ok(types::pool::IcpAccount {
        owner: account.owner,
        subaccount,
    })
}

/// Why a wire event is not a domain event, naming the field at fault.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EventError {
    AmountTooLarge {
        field: String,
    },
    TextTooLong {
        field: String,
        len: u64,
    },
    /// The text in `field` is not an EVM address, and `reason` says which way.
    NotAnAddress {
        field: String,
        reason: EvmAddressError,
    },
    NotAFlowStep {
        field: String,
    },
    NotABookStep {
        field: String,
    },
    NotAGasAction {
        field: String,
    },
    NotAPoolStep {
        field: String,
    },
    /// The account in `field` has a subaccount that is not 32 bytes.
    NotAnAccount {
        field: String,
    },
}

/// Why a text is not an EVM address. Mirrors `types::evm::EvmAddressError` so a caller
/// replaying a log is told which of the four rules the text broke, rather than being told
/// something that is false for three of them.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EvmAddressError {
    NoPrefix,
    WrongLength { len: u64 },
    NotHex,
    BadChecksum,
}

impl From<types::evm::EvmAddressError> for EvmAddressError {
    fn from(error: types::evm::EvmAddressError) -> Self {
        use types::evm::EvmAddressError as Domain;
        match error {
            Domain::NoPrefix => Self::NoPrefix,
            Domain::WrongLength { len } => Self::WrongLength {
                len: crate::types::wire_len(len),
            },
            Domain::NotHex => Self::NotHex,
            Domain::BadChecksum => Self::BadChecksum,
        }
    }
}

impl From<types::events::EventError> for EventError {
    fn from(error: types::events::EventError) -> Self {
        use types::events::EventError as Domain;
        match error {
            Domain::AmountTooLarge { field } => Self::AmountTooLarge {
                field: field.to_string(),
            },
            Domain::TextTooLong { field, len } => Self::TextTooLong {
                field: field.to_string(),
                len: crate::types::wire_len(len),
            },
            Domain::NotAnAddress { field, reason } => Self::NotAnAddress {
                field: field.to_string(),
                reason: reason.into(),
            },
            Domain::NotAFlowStep { field } => Self::NotAFlowStep {
                field: field.to_string(),
            },
            Domain::NotABookStep { field } => Self::NotABookStep {
                field: field.to_string(),
            },
            Domain::NotAGasAction { field } => Self::NotAGasAction {
                field: field.to_string(),
            },
            Domain::NotAPoolStep { field } => Self::NotAPoolStep {
                field: field.to_string(),
            },
            Domain::NotAnAccount { field } => Self::NotAnAccount {
                field: field.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests;

/// A line's readable copy of its step, cut to [`MAX_DETAIL`] characters: `step_bytes` carry
/// the step whole, so the copy is for a human reading the log and never doubles a line's size
/// (audit C15).
fn detail_of(step: &impl std::fmt::Debug) -> String {
    let full = format!("{step:?}");
    match full.char_indices().nth(MAX_DETAIL) {
        None => full,
        Some((at, _)) => format!("{}...", &full[..at]),
    }
}

/// The most characters of a line's readable detail.
pub const MAX_DETAIL: usize = 512;
