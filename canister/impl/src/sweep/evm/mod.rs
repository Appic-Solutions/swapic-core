//! An EVM sweep: native gas, or booked USDC fee income, from the canister's address on a
//! chain. Its line (`Swept`) and the nonce its transaction takes are written in one message
//! with no await between them; its amount comes back on its books if the transaction
//! reverts or its nonce is cancelled.

use crate::guards::{require_controller, require_not_halted};
use crate::state::Store;
use crate::storage::events::{append_event, append_event_at, read_state};
use crate::storage::{chain_data, config, outbox};
use crate::tx;
use candid::Nat;
use serde_json::{json, Value};
use settlement_api::types::sweep::{SweepError, SweepEvmArgs, SweepEvmOk};
use types::config::GasAction;
use types::events::TxPurpose;
use types::sweep::{Sweep, SweepAsset, SweepTo};
use types::{ChainId, EventType, EvmAddress, Timestamp, TokenAmount, Wei};

/// Takes sweep `id`'s amount back onto its books: it moved nothing. A line the log refuses
/// leaves the books lower than the balance, a surplus, which pauses nothing.
pub fn taken_back(id: u64, why: &str, now: Timestamp) {
    let line = EventType::SweepReverted {
        id,
        why: why.to_string(),
    };
    if let Err(error) = append_event_at(line, now) {
        crate::pool::log(&format!("the sweep {id} was not taken back: {error}"));
    }
}

/// What a USDC fee sweep of `amount` on `chain` takes off which line: the pool's Base fee
/// line first on the pool chain, then the chain books' fee income. None past what they
/// spare together, with what that is.
pub fn fee_parts(
    amount: TokenAmount,
    from_pool: TokenAmount,
    fee_income: TokenAmount,
) -> Result<(TokenAmount, TokenAmount), TokenAmount> {
    let pool = amount.min(from_pool);
    let rest = amount.checked_sub(pool).unwrap_or(TokenAmount::ZERO);
    if rest > fee_income {
        return Err(from_pool
            .checked_add(fee_income)
            .unwrap_or(TokenAmount::MAX));
    }
    Ok((pool, rest))
}

/// The canister's native balance on `chain`, as one provider answers it.
async fn native_balance(chain: ChainId, canister: EvmAddress) -> Result<Wei, String> {
    let answers = crate::rpc::rpc_batch(
        chain,
        &[(
            "eth_getBalance",
            json!([crate::rpc::hex0x(canister.as_bytes()), "latest"]),
        )],
        1_024,
    )
    .await
    .map_err(|error| format!("the balance read: {:?}", error.kind()))?;
    answers
        .first()
        .and_then(Value::as_str)
        .and_then(|text| Wei::from_str_hex(text.strip_prefix("0x")?).ok())
        .ok_or_else(|| "the balance read answered no amount".to_string())
}

/// `sweep_evm`'s body.
pub async fn sweep(args: SweepEvmArgs) -> Result<SweepEvmOk, SweepError> {
    require_controller().map_err(SweepError::Guard)?;
    require_not_halted().map_err(SweepError::Guard)?;
    let invalid = |why: &str| SweepError::Invalid(why.to_string());
    let chain = ChainId::new(args.chain_id);
    let asset = SweepAsset::from(args.asset);
    let amount = TokenAmount::from_canonical_nat(args.amount.clone())
        .ok_or_else(|| invalid("amount is wider than 256 bits"))?;
    if amount == TokenAmount::ZERO {
        return Err(invalid("amount must be above zero"));
    }
    let to: EvmAddress = args
        .to
        .parse()
        .map_err(|_| invalid("to is not an EVM address"))?;
    let config = config::get();
    let evm = config
        .evm_chains
        .get(chain)
        .ok_or_else(|| invalid("the canister signs nothing on that chain"))?;
    let gas_limit = evm
        .gas_limit
        .get(GasAction::Sweep)
        .ok_or_else(|| invalid("the chain has no sweep gas row"))?;
    let usdc = evm.usdc;
    let canister = crate::ecdsa::canister_address()
        .await
        .map_err(|error| SweepError::Unavailable(error.to_string()))?;
    if to == canister || to == EvmAddress::ZERO {
        return Err(invalid(
            "to is the canister's own address or the zero address",
        ));
    }
    if crate::storage::sanctions::is_sanctioned(&types::Address::from(to)) {
        return Err(SweepError::Sanctioned);
    }
    crate::gas::ensure_fresh(chain)
        .await
        .map_err(|failure| SweepError::Unavailable(format!("the gas price: {failure:?}")))?;
    let balance = match asset {
        SweepAsset::Native => Some(
            native_balance(chain, canister)
                .await
                .map_err(SweepError::Unavailable)?,
        ),
        _ => None,
    };
    // from here to the nonce no await: the line and the allocation are one message
    require_not_halted().map_err(SweepError::Guard)?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let (target, value, data, from_pool_fees, from_fee_income) = match (asset, balance) {
        (SweepAsset::Native, Some(balance)) => {
            if outbox::chains().contains(&chain) {
                return Err(SweepError::Busy(format!(
                    "a transaction of the canister on chain {chain} is still open: it may need gas"
                )));
            }
            let gas = chain_data::fresh(chain, now)
                .and_then(|reading| reading.fees())
                .and_then(|fees| fees.worst_cost(gas_limit))
                .ok_or_else(|| SweepError::Unavailable("no gas price to cost the sweep".into()))?;
            let available = balance.checked_sub(gas).unwrap_or(Wei::ZERO);
            let value = Wei::from_be_bytes(amount.to_be_bytes());
            if value > available {
                return Err(SweepError::AboveBooks {
                    available: Nat::from(TokenAmount::from_be_bytes(available.to_be_bytes())),
                });
            }
            (to, value, Vec::new(), TokenAmount::ZERO, TokenAmount::ZERO)
        }
        (SweepAsset::FeeUsdc, _) => {
            let (from_pool, fee_income) = read_state(|state| {
                let from_pool = if config.pool_chain == Some(chain) {
                    super::spare_fees_base(state)
                } else {
                    TokenAmount::ZERO
                };
                (from_pool, state.book(&chain).fee_income)
            });
            let (pool, rest) = fee_parts(amount, from_pool, fee_income).map_err(|available| {
                SweepError::AboveBooks {
                    available: Nat::from(available),
                }
            })?;
            let data = types::swapic::transfer_calldata(to, amount.to_u256());
            (usdc, Wei::ZERO, data, pool, rest)
        }
        _ => return Err(invalid("not an EVM asset")),
    };
    let id = read_state(|state| state.store().sweep_count());
    let sweep = Sweep {
        id,
        asset,
        to: SweepTo::Evm { chain, to },
        amount,
        from_pool_fees,
        from_fee_income,
    };
    append_event(EventType::Swept { sweep })
        .map_err(|error| SweepError::Unavailable(format!("the sweep's line: {error}")))?;
    let purpose = TxPurpose::Sweep(id);
    match tx::create_and_send(
        purpose,
        chain,
        target,
        value,
        data,
        gas_limit,
        GasAction::Sweep,
    )
    .await
    {
        Ok(signed) => Ok(SweepEvmOk {
            id,
            tx_id: signed.id.get(),
        }),
        Err(error) => {
            // a nonce handed out and never signed for is the stranded cancel's, which takes
            // the sweep back; with none, nothing will go out and it comes back now
            let allocated = purpose.quote_hash().is_some_and(|key| {
                read_state(|state| state.store().unsigned_nonce_of(&key).is_some())
            }) || outbox::find(purpose).is_some();
            if !allocated {
                taken_back(id, &error.to_string(), now);
            }
            Err(SweepError::Unavailable(error.to_string()))
        }
    }
}
