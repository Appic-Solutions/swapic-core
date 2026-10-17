//! The flows' half of the engine: every open flow's next step, in the same drive as the
//! swaps and under the same guard, so no two drives act on one flow.
//!
//! Each inter-canister call that can move money is bracketed by a `Begun` marker before it
//! and its result after it (see `types::flow`). A ledger call found in doubt is asked again
//! with the same deduplication key; a swap or a withdrawal found in doubt freezes for a
//! human. A flow a door is acting on right now is busy, and skipped.

use crate::ecdsa;
use crate::evm_reads::{self, Simulated, TransferRead};
use crate::guards::require_not_halted;
use crate::ic::{self, Account, CallError, Dedup};
use crate::state::Store;
use crate::storage::config;
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::flow_settings::{self, FlowSettings};
use crate::tx;
use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use candid::{Nat, Principal};
use std::cell::RefCell;
use std::collections::BTreeSet;
use types::events::TxPurpose;
use types::flow::{IcpToEvmQuote, Stage};
use types::router::{
    self, DexCall, EcoTerms, GaslessAuth, ReceiveWithAuthorization, SwapParams, ACTION_DELIVER,
    DOOR_AUTH_3009, RAIL_CCTP, RAIL_ECO, RAIL_LOCAL,
};
use types::{
    EventType, EvmAddress, Flow, FlowKind, FlowStatus, FlowStep, GasAmount, QuoteHash, Timestamp,
    TokenAmount, TxHash, Wei,
};

thread_local! {
    // Heap by necessity: the flows a door is in the middle of right now. An upgrade waits
    // for every outstanding call, so nothing in here survives one.
    static BUSY: RefCell<BTreeSet<QuoteHash>> = const { RefCell::new(BTreeSet::new()) };
}

/// Held while a door or the engine acts on one flow, released on drop, a trap included.
pub struct Busy(QuoteHash);

impl Busy {
    pub fn take(quote_hash: QuoteHash) -> Option<Self> {
        BUSY.with(|busy| busy.borrow_mut().insert(quote_hash))
            .then_some(Self(quote_hash))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|busy| busy.borrow_mut().remove(&self.0));
    }
}

/// Appends one step of `quote_hash`'s flow.
pub fn record(quote_hash: QuoteHash, step: FlowStep) -> Result<(), AppendError> {
    append_event(EventType::Flow { quote_hash, step }).map(drop)
}

pub fn now() -> Timestamp {
    Timestamp::from_nanos(ic_cdk::api::time())
}

pub fn principal(bytes: &[u8]) -> Option<Principal> {
    Principal::try_from_slice(bytes).ok()
}

pub fn amount(nat: &Nat) -> TokenAmount {
    TokenAmount::try_from(nat.clone()).unwrap_or(TokenAmount::MAX)
}

pub fn nat(amount: TokenAmount) -> Nat {
    Nat::from(amount)
}

/// The deduplication key of the call `flow` is in a begun stage for.
pub fn dedup(quote_hash: QuoteHash, flow: &Flow) -> Dedup {
    Dedup {
        memo: quote_hash.into_bytes(),
        created_at_nanos: flow.begun_at.unwrap_or_else(now).as_nanos(),
    }
}

/// Why a flow did not move this drive. Counted, never fatal: the next drive reads it again.
#[derive(Debug)]
pub enum FlowDriveError {
    Settings(&'static str),
    Call(CallError),
    Append(AppendError),
    Chain(String),
}

impl From<AppendError> for FlowDriveError {
    fn from(error: AppendError) -> Self {
        Self::Append(error)
    }
}

impl From<CallError> for FlowDriveError {
    fn from(error: CallError) -> Self {
        Self::Call(error)
    }
}

/// One pass over every open flow. Returns how many it acted on.
pub async fn drive_flows() -> usize {
    let open: Vec<(QuoteHash, Flow)> = read_state(|state| {
        state
            .store()
            .flows()
            .into_iter()
            .filter(|(_, flow)| !flow.status.is_closed())
            .collect()
    });
    let mut acted = 0;
    for (quote_hash, _) in open {
        if require_not_halted().is_err() {
            break;
        }
        let Some(_busy) = Busy::take(quote_hash) else {
            continue;
        };
        // the flow as it is now: a door may have moved it since the list was read
        let Some(flow) = read_state(|state| state.flow(&quote_hash)) else {
            continue;
        };
        match drive_flow(quote_hash, &flow).await {
            Ok(()) => acted += 1,
            // retried on the next drive; printed so an operator reading the replica's log
            // sees why a flow is standing still
            Err(error) => {
                ic_cdk::println!("flow {quote_hash} stays at {:?}: {error:?}", flow.status)
            }
        }
    }
    acted
}

async fn drive_flow(quote_hash: QuoteHash, flow: &Flow) -> Result<(), FlowDriveError> {
    match flow.kind {
        FlowKind::Relay | FlowKind::Finish => Ok(()),
        // the ICP flows are off until they move to ABI v3 (P11)
        FlowKind::IcpToEvm | FlowKind::EvmToIcp if !flow_settings::get().icp_enabled => Ok(()),
        FlowKind::IcpToEvm => icp_to_evm(quote_hash, flow).await,
        FlowKind::EvmToIcp => super::evm_to_icp::step(quote_hash, flow).await,
    }
}

pub fn freeze(quote_hash: QuoteHash, reason: &str) -> Result<(), FlowDriveError> {
    record(
        quote_hash,
        FlowStep::Frozen {
            reason: reason.to_string(),
        },
    )?;
    Ok(())
}

async fn icp_to_evm(quote_hash: QuoteHash, flow: &Flow) -> Result<(), FlowDriveError> {
    let quote = flow
        .icp_quote
        .clone()
        .ok_or(FlowDriveError::Settings("an ICP to EVM flow has no quote"))?;
    let settings = flow_settings::get();
    match flow.status {
        FlowStatus::Registered => {
            if now().as_secs() > quote.expires_at {
                record(quote_hash, FlowStep::Expired)?;
            }
            Ok(())
        }
        FlowStatus::Pulling => pull(quote_hash, flow, &quote).await.map(drop),
        FlowStatus::Pulled => {
            let ckusdc = settings
                .ckusdc_ledger
                .ok_or(FlowDriveError::Settings("no ckUSDC ledger"))?;
            if principal(&quote.src_ledger) == Some(ckusdc) {
                withdraw(quote_hash, flow, &settings).await
            } else {
                swap_to_ckusdc(quote_hash, flow, &quote, &settings).await
            }
        }
        FlowStatus::Swapped => withdraw(quote_hash, flow, &settings).await,
        FlowStatus::Swapping => freeze(quote_hash, "the ICPSwap call's outcome is unknown"),
        FlowStatus::Withdrawing => freeze(quote_hash, "the withdrawal's outcome is unknown"),
        FlowStatus::Withdrawn => check_landing(quote_hash, flow, &settings).await,
        FlowStatus::Landed => continue_on_ethereum(quote_hash, flow, &quote, &settings).await,
        FlowStatus::RefundDue | FlowStatus::Refunding => refund(quote_hash, flow, &quote).await,
        _ => Ok(()),
    }
}

/// The pull from the user's allowance that names the quote (spec 4b.1): `start_icp_swap`
/// makes it first, and the engine asks again with the same key when its outcome is in
/// doubt. Answers what was pulled.
pub async fn pull(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &IcpToEvmQuote,
) -> Result<TokenAmount, FlowDriveError> {
    let ledger = principal(&quote.src_ledger).ok_or(FlowDriveError::Settings("src_ledger"))?;
    let user = principal(&quote.user).ok_or(FlowDriveError::Settings("user"))?;
    let me = ic_cdk::api::id();
    let answer = ic::icrc2_transfer_from(
        ledger,
        quote_hash.into_bytes(),
        Account::of(user, None),
        Account::of(me, None),
        nat(quote.amount_in),
        dedup(quote_hash, flow),
    )
    .await;
    match answer {
        Ok(block_index) => {
            record(
                quote_hash,
                FlowStep::Pulled {
                    amount: quote.amount_in,
                    block_index,
                },
            )?;
            Ok(quote.amount_in)
        }
        Err(CallError::Refused(reason)) => {
            record(
                quote_hash,
                FlowStep::PullRefused {
                    reason: reason.clone(),
                },
            )?;
            Err(FlowDriveError::Call(CallError::Refused(reason)))
        }
        Err(error) => Err(FlowDriveError::Call(error)),
    }
}

/// ICPSwap, from the paid token to ckUSDC. The pool's own quote is read first, and a
/// quote below the floor refunds the user before anything is approved.
async fn swap_to_ckusdc(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &IcpToEvmQuote,
    settings: &FlowSettings,
) -> Result<(), FlowDriveError> {
    let pool = quote
        .pool
        .as_ref()
        .and_then(|bytes| principal(bytes))
        .ok_or(FlowDriveError::Settings("the quote names no pool"))?;
    let src = principal(&quote.src_ledger).ok_or(FlowDriveError::Settings("src_ledger"))?;
    let ckusdc = settings
        .ckusdc_ledger
        .ok_or(FlowDriveError::Settings("no ckUSDC ledger"))?;
    let (zero_for_one, fee_in, fee_out, swap_in) =
        match swap_terms(pool, src, ckusdc, flow.held).await {
            Ok(terms) => terms,
            Err(SwapRefusal::Refund(reason)) => {
                record(quote_hash, FlowStep::RefundStarted { reason })?;
                return Ok(());
            }
            Err(SwapRefusal::Retry(error)) => return Err(error),
        };
    let quoted = ic::pool_quote(pool, &nat(swap_in), zero_for_one).await?;
    let floor = quote.min_ckusdc;
    if amount(&quoted) < floor {
        record(
            quote_hash,
            FlowStep::RefundStarted {
                reason: format!("the pool quotes {quoted} ckUSDC, below the floor of {floor}"),
            },
        )?;
        return Ok(());
    }
    ic::icrc2_approve(src, pool, nat(swap_in) + fee_in.clone()).await?;
    record(quote_hash, FlowStep::Begun { stage: Stage::Swap })?;
    match ic::deposit_from_and_swap(
        pool,
        &nat(swap_in),
        zero_for_one,
        &nat(floor),
        fee_in,
        fee_out,
    )
    .await
    {
        Ok(out) => {
            record(
                quote_hash,
                FlowStep::Swapped {
                    ledger: ckusdc.as_slice().to_vec(),
                    amount: amount(&out),
                },
            )?;
            Ok(())
        }
        Err(error) => freeze(
            quote_hash,
            &format!("the ICPSwap swap did not answer: {error}"),
        ),
    }
}

/// Why a swap was not started.
pub enum SwapRefusal {
    /// The pool is not for this pair, or the amount does not cover the fees: give back.
    Refund(String),
    /// A read failed: try again.
    Retry(FlowDriveError),
}

/// The direction, the two fees, and the amount swapped out of `held` of `src`: what is
/// held less the approval's fee and the pull's.
pub async fn swap_terms(
    pool: Principal,
    src: Principal,
    dst: Principal,
    held: TokenAmount,
) -> Result<(bool, Nat, Nat, TokenAmount), SwapRefusal> {
    let metadata = ic::pool_metadata(pool)
        .await
        .map_err(|e| SwapRefusal::Retry(e.into()))?;
    let (src_text, dst_text) = (src.to_text(), dst.to_text());
    let zero_for_one = if metadata.token0.address == src_text && metadata.token1.address == dst_text
    {
        true
    } else if metadata.token1.address == src_text && metadata.token0.address == dst_text {
        false
    } else {
        return Err(SwapRefusal::Refund(
            "the pool does not trade this pair".to_string(),
        ));
    };
    let fee_in = ic::icrc1_fee(src)
        .await
        .map_err(|e| SwapRefusal::Retry(e.into()))?;
    let fee_out = ic::icrc1_fee(dst)
        .await
        .map_err(|e| SwapRefusal::Retry(e.into()))?;
    let fees = amount(&fee_in)
        .checked_add(amount(&fee_in))
        .unwrap_or(TokenAmount::MAX);
    let swap_in = held
        .checked_sub(fees)
        .filter(|left| *left > TokenAmount::ZERO)
        .ok_or_else(|| SwapRefusal::Refund("the amount does not cover the fees".to_string()))?;
    Ok((zero_for_one, fee_in, fee_out, swap_in))
}

/// The ck minter's `withdraw_erc20` of the held ckUSDC, to this canister's own Ethereum
/// address. The minter is approved first, for the amount and, when it charges gas in
/// ckETH, for the configured ckETH; a refusal before the burn refunds the user.
async fn withdraw(
    quote_hash: QuoteHash,
    flow: &Flow,
    settings: &FlowSettings,
) -> Result<(), FlowDriveError> {
    let ckusdc = settings
        .ckusdc_ledger
        .ok_or(FlowDriveError::Settings("no ckUSDC ledger"))?;
    let minter = settings
        .ck_minter
        .ok_or(FlowDriveError::Settings("no ck minter"))?;
    let mine = ecdsa::canister_address()
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
    let fee = amount(&ic::icrc1_fee(ckusdc).await?);
    let Some(withdraw) = flow
        .held
        .checked_sub(fee)
        .filter(|left| *left > TokenAmount::ZERO)
    else {
        record(
            quote_hash,
            FlowStep::RefundStarted {
                reason: "the ckUSDC does not cover the ledger fee".to_string(),
            },
        )?;
        return Ok(());
    };
    ic::icrc2_approve(ckusdc, minter, nat(withdraw)).await?;
    if let Some(cketh) = settings.cketh_ledger {
        ic::icrc2_approve(cketh, minter, nat(settings.cketh_fee_allowance)).await?;
    }
    record(
        quote_hash,
        FlowStep::Begun {
            stage: Stage::Withdraw,
        },
    )?;
    match ic::withdraw_erc20(minter, ckusdc, nat(withdraw), mine.to_string()).await {
        Ok(request) => {
            let withdrawal_id = u64::try_from(request.cketh_block_index.0.clone()).unwrap_or(0);
            record(
                quote_hash,
                FlowStep::Withdrawn {
                    withdrawal_id,
                    amount: withdraw,
                },
            )?;
            Ok(())
        }
        Err(CallError::Refused(reason)) => {
            record(quote_hash, FlowStep::RefundStarted { reason })?;
            Ok(())
        }
        Err(error) => freeze(
            quote_hash,
            &format!("the withdrawal did not answer: {error}"),
        ),
    }
}

/// Whether the withdrawal landed at this canister's Ethereum address, at depth: the
/// minter names the transaction, and its receipt is read for USDC's `Transfer` to this
/// address. What is credited is never more than the withdrawal burned.
async fn check_landing(
    quote_hash: QuoteHash,
    flow: &Flow,
    settings: &FlowSettings,
) -> Result<(), FlowDriveError> {
    let minter = settings
        .ck_minter
        .ok_or(FlowDriveError::Settings("no ck minter"))?;
    let usdc = settings
        .ethereum_usdc
        .ok_or(FlowDriveError::Settings("no Ethereum USDC"))?;
    let withdrawal_id = flow
        .withdrawal_id
        .ok_or(FlowDriveError::Settings("no withdrawal id"))?;
    let details = ic::withdrawal_status(minter, withdrawal_id).await?;
    let tx_text = details.iter().find_map(|detail| match &detail.status {
        ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Success {
            transaction_hash,
            ..
        }) => Some(Ok(transaction_hash.clone())),
        ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Reimbursed { .. })
        | ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::PendingReimbursement(_)) => {
            Some(Err(()))
        }
        _ => None,
    });
    let tx_text = match tx_text {
        None => return Ok(()),
        Some(Err(())) => return freeze(quote_hash, "the minter reimbursed the withdrawal"),
        Some(Ok(text)) => text,
    };
    let tx_hash = parse_tx_hash(&tx_text)
        .ok_or_else(|| FlowDriveError::Chain("the minter names no transaction hash".into()))?;
    let mine = ecdsa::canister_address()
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
    let depth = tx::confirmations(&config::get(), settings.ethereum)
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
    let read = evm_reads::transfer_to(settings.ethereum, tx_hash, usdc, mine, depth)
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
    require_not_halted().map_err(|_| FlowDriveError::Settings("halted"))?;
    match read {
        TransferRead::NotYet => Ok(()),
        TransferRead::NoTransfer => freeze(
            quote_hash,
            "the withdrawal's transaction moved no USDC to this canister",
        ),
        TransferRead::Landed { amount, .. } => {
            let credited = flow.withdrawn.map_or(amount, |burned| amount.min(burned));
            record(
                quote_hash,
                FlowStep::Landed {
                    chain_id: settings.ethereum,
                    tx_hash,
                    amount: credited,
                },
            )?;
            Ok(())
        }
    }
}

fn parse_tx_hash(text: &str) -> Option<TxHash> {
    let bytes = hex::decode(text.strip_prefix("0x").unwrap_or(text)).ok()?;
    Some(TxHash::new(bytes.try_into().ok()?))
}

/// The route the continuation takes, re-planned at landing against the user's ORIGINAL
/// terms (spec 4b.7): the quote's route when it can still be met, and otherwise a CCTP
/// Deliver of the USDC to the user's destination address.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    route: &SwapParams,
    mine: EvmAddress,
    usdc: EvmAddress,
    held: TokenAmount,
    now_s: u64,
    deadline_s: u64,
    paused: bool,
    failed_before: u32,
) -> Option<SwapParams> {
    let amount = U256::from_be_bytes(held.to_be_bytes());
    let deadline = now_s.saturating_add(deadline_s);
    let mut params = route.clone();
    params.payer = router::addr(mine);
    params.tokenIn = router::addr(usdc);
    params.amountIn = amount;
    params.deadline = U256::from(deadline);
    params.leg = DexCall {
        target: Address::ZERO,
        data: Bytes::new(),
    };
    params.legMinOut = amount;
    // the quote's network fee is withheld to this canister's own address, the operator
    params.networkFee = route.networkFee.min(amount);
    let net = amount.saturating_sub(params.networkFee);
    let route_holds = !paused
        && failed_before == 0
        && match route.rail {
            RAIL_ECO => {
                route.eco.routeDeadline > now_s.saturating_add(300) && net >= route.eco.amount
            }
            RAIL_CCTP => {
                route.dst.action != ACTION_DELIVER
                    || net.saturating_sub(route.maxFee) >= route.dst.minOut
            }
            RAIL_LOCAL => net >= route.dst.minOut,
            _ => false,
        };
    if route_holds {
        return Some(params);
    }
    // the fallback: the USDC itself, on CCTP, to the user's destination address
    let recipient = router::evm_recipient(route.dst.recipient)?;
    let max_fee = if route.rail == RAIL_CCTP {
        route.maxFee
    } else {
        U256::ZERO
    };
    params.rail = RAIL_CCTP;
    params.maxFee = max_fee;
    params.minFinality = if max_fee.is_zero() {
        2000
    } else {
        route.minFinality
    };
    params.eco = EcoTerms {
        salt: FixedBytes::ZERO,
        routeDeadline: 0,
        rewardDeadline: 0,
        amount: U256::ZERO,
        amountOut: U256::ZERO,
    };
    params.dst.action = ACTION_DELIVER;
    params.dst.recipient = router::recipient_word(recipient);
    params.dst.tokenOut = Address::ZERO;
    params.dst.minOut = net.saturating_sub(max_fee);
    params.dst.principal = FixedBytes::ZERO;
    params.dst.subaccount = FixedBytes::ZERO;
    params.dst.deadline = deadline.saturating_add(86_400);
    params.dst.swap = DexCall {
        target: Address::ZERO,
        data: Bytes::new(),
    };
    Some(params)
}

/// The landed USDC's ONE Ethereum transaction: `swapFor` from this canister's address,
/// paid with an EIP-3009 authorization the canister signs as the payer, so nothing stands
/// approved. Paused routers and expired or unmeetable routes fall back to a Deliver, and a
/// route that reverts in simulation is re-planned as one.
async fn continue_on_ethereum(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &IcpToEvmQuote,
    settings: &FlowSettings,
) -> Result<(), FlowDriveError> {
    if flow.failed_continuations >= 2 {
        return freeze(
            quote_hash,
            "the continuation and its fallback both reverted",
        );
    }
    let chain = settings.ethereum;
    let router_address = settings
        .router(chain)
        .ok_or(FlowDriveError::Settings("no router on Ethereum"))?;
    let usdc = settings
        .ethereum_usdc
        .ok_or(FlowDriveError::Settings("no Ethereum USDC"))?;
    let route = router::decode_params(&quote.route).ok_or(FlowDriveError::Settings(
        "the quote's route does not decode",
    ))?;
    let mine = ecdsa::canister_address()
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
    let paused = match evm_reads::eth_call(chain, mine, router_address, &router::paused_calldata())
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?
    {
        Simulated::Returned(bytes) => bytes.last().copied().unwrap_or(0) != 0,
        Simulated::Reverted(_) => true,
    };
    let separator =
        match evm_reads::eth_call(chain, mine, usdc, &router::domain_separator_calldata())
            .await
            .map_err(|e| FlowDriveError::Chain(e.to_string()))?
        {
            Simulated::Returned(bytes) if bytes.len() == 32 => {
                <[u8; 32]>::try_from(bytes).expect("BUG: checked to be 32 bytes")
            }
            _ => {
                return Err(FlowDriveError::Chain(
                    "USDC answers no domain separator".into(),
                ))
            }
        };
    let now_s = now().as_secs().get();
    let deadline_s = settings.continuation_deadline.as_secs();
    let mut failed = flow.failed_continuations;
    loop {
        let params = match plan(
            &route,
            mine,
            usdc,
            flow.eth_held,
            now_s,
            deadline_s,
            paused,
            failed,
        ) {
            Some(params) => params,
            None => return freeze(quote_hash, "no route meets the user's terms"),
        };
        let continuation_hash = router::quote_hash(&params, chain, router_address);
        let authorization = ReceiveWithAuthorization {
            from: router::addr(mine),
            to: router::addr(router_address),
            value: params.amountIn,
            validAfter: U256::ZERO,
            validBefore: params.deadline,
            nonce: FixedBytes(continuation_hash.into_bytes()),
        };
        let digest = router::receive_with_authorization_digest(separator, &authorization);
        let signature = ecdsa::sign_digest(digest)
            .await
            .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
        let auth = GaslessAuth {
            door: DOOR_AUTH_3009,
            validAfter: U256::ZERO,
            validBefore: params.deadline,
            nonce: U256::ZERO,
            deadline: U256::ZERO,
            signature: Bytes::from(signature),
            acceptance: Bytes::new(),
        };
        let data = router::swap_for_calldata(&params, &auth);
        let (simulated, estimate) =
            evm_reads::simulate_and_estimate(chain, mine, router_address, &data)
                .await
                .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
        if let Simulated::Reverted(message) = simulated {
            if params.dst.action == ACTION_DELIVER && params.rail == RAIL_CCTP && failed > 0 {
                return freeze(
                    quote_hash,
                    &format!("the fallback Deliver reverts in simulation: {message}"),
                );
            }
            // re-plan as the fallback
            failed = failed.max(1);
            continue;
        }
        require_not_halted().map_err(|_| FlowDriveError::Settings("halted"))?;
        let gas = gas_limit_for(estimate, settings);
        tx::create_and_send_with(
            TxPurpose::Continue(quote_hash),
            chain,
            router_address,
            Wei::ZERO,
            data,
            GasAmount::from(gas),
            // off until the ICP flows move to ABI v3 (P11), which gives them their action
            types::config::GasAction::RelayLocal,
            Some(tx::Detail::Continuation {
                action: params.dst.action,
                amount: flow.eth_held,
            }),
        )
        .await
        .map_err(|e| FlowDriveError::Chain(e.to_string()))?;
        return Ok(());
    }
}

/// The gas a continuation is given: the estimate with a margin, held to the configured
/// ceiling.
pub fn gas_limit_for(estimate: Option<u64>, settings: &FlowSettings) -> u64 {
    let ceiling = settings.max_gas_limit;
    match estimate {
        Some(estimate) => estimate
            .saturating_mul(13)
            .saturating_div(10)
            .saturating_add(50_000)
            .min(ceiling),
        None => ceiling,
    }
}

/// The held token back to the user's principal, less what the ledger charges, asked
/// again with the same key while its outcome is in doubt.
async fn refund(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &IcpToEvmQuote,
) -> Result<(), FlowDriveError> {
    let ledger = flow
        .held_ledger()
        .and_then(principal)
        .ok_or(FlowDriveError::Settings("the flow holds no ledger"))?;
    let user = principal(&quote.user).ok_or(FlowDriveError::Settings("user"))?;
    let fee = amount(&ic::icrc1_fee(ledger).await?);
    // the approvals a failed step made were charged to the canister's account, so the
    // refund leaves room for them
    let charges = fee.checked_add(fee).and_then(|two| two.checked_add(fee));
    let Some(back) = charges
        .and_then(|charges| flow.held.checked_sub(charges))
        .filter(|left| *left > TokenAmount::ZERO)
    else {
        return freeze(quote_hash, "what is held does not cover the refund's fees");
    };
    let flow = if flow.status == FlowStatus::RefundDue {
        record(
            quote_hash,
            FlowStep::Begun {
                stage: Stage::Refund,
            },
        )?;
        read_state(|state| state.flow(&quote_hash))
            .ok_or(FlowDriveError::Settings("the flow vanished"))?
    } else {
        flow.clone()
    };
    let block_index = ic::icrc1_transfer(
        ledger,
        None,
        Account::of(user, None),
        nat(back),
        dedup(quote_hash, &flow),
    )
    .await?;
    record(
        quote_hash,
        FlowStep::Refunded {
            amount: back,
            block_index,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::router::{Delivery, ACTION_SWAP};

    fn route(rail: u8, route_deadline: u64) -> SwapParams {
        SwapParams {
            r#ref: FixedBytes([1; 32]),
            payer: Address::ZERO,
            tokenIn: Address::ZERO,
            amountIn: U256::from(5_000_000),
            deadline: U256::ZERO,
            leg: DexCall {
                target: Address::ZERO,
                data: Bytes::new(),
            },
            legMinOut: U256::ZERO,
            feeBps: U256::ZERO,
            networkFee: U256::from(40_000),
            rail,
            dstChainId: 42161,
            maxFee: U256::from(5_000),
            minFinality: 1000,
            eco: EcoTerms {
                salt: FixedBytes([2; 32]),
                routeDeadline: route_deadline,
                rewardDeadline: route_deadline + 600,
                amount: U256::from(4_000_000),
                amountOut: U256::from(3_900_000),
            },
            dst: Delivery {
                action: ACTION_SWAP,
                recipient: router::recipient_word(EvmAddress::new([6; 20])),
                tokenOut: Address::from([7; 20]),
                minOut: U256::from(1),
                principal: FixedBytes::ZERO,
                subaccount: FixedBytes::ZERO,
                deadline: 0,
                swap: DexCall {
                    target: Address::ZERO,
                    data: Bytes::new(),
                },
            },
        }
    }

    fn planned(route: &SwapParams, paused: bool, failed: u32) -> SwapParams {
        let held = TokenAmount::from(4_990_000_u32);
        plan(
            route,
            EvmAddress::new([9; 20]),
            EvmAddress::new([8; 20]),
            held,
            1_000,
            1_800,
            paused,
            failed,
        )
        .expect("an EVM recipient always has a plan")
    }

    #[test]
    fn a_route_that_still_holds_is_kept_with_the_canister_as_payer() {
        let params = planned(&route(RAIL_CCTP, 0), false, 0);
        assert_eq!(params.dst.action, ACTION_SWAP);
        assert_eq!(params.payer, Address::from([9; 20]));
        assert_eq!(params.amountIn, U256::from(4_990_000));
        assert_eq!(params.networkFee, U256::from(40_000));
    }

    #[test]
    fn a_paused_router_an_expired_eco_route_or_a_revert_falls_back_to_a_deliver() {
        for (params, max_fee) in [
            (planned(&route(RAIL_CCTP, 0), true, 0), 5_000),
            (planned(&route(RAIL_ECO, 1_100), false, 0), 0),
            (planned(&route(RAIL_CCTP, 0), false, 1), 5_000),
        ] {
            assert_eq!(params.rail, RAIL_CCTP);
            assert_eq!(params.dst.action, ACTION_DELIVER);
            assert_eq!(
                params.dst.recipient,
                router::recipient_word(EvmAddress::new([6; 20]))
            );
            assert_eq!(params.dst.minOut, U256::from(4_990_000 - 40_000 - max_fee));
        }
    }

    #[test]
    fn a_live_eco_route_is_kept() {
        assert_eq!(planned(&route(RAIL_ECO, 100_000), false, 0).rail, RAIL_ECO);
    }
}
