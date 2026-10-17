//! The canister finishes CCTP arrivals itself (`finish_cctp`): the watcher hands in the
//! attested message, the canister decides from the hook data and the clock what to call,
//! simulates it, holds it to the gas rule and signs it once per message and action.
//!
//! Only Swapic's own burns are finished: the message must come from CCTP's token
//! messenger on both ends and burn the source chain's USDC, its burn must be sent by
//! Swapic on the source chain and carry Swapic's hook data, so the canister never pays gas
//! for a message that collected no `networkFee`. The outer check comes first: anyone can
//! have a generic message attested whose body looks like a Swapic burn.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::engine::flows::gas_limit_for;
use crate::engine::flows::Busy;
use crate::evm_reads::{self, Simulated};
use crate::guards::{require_not_halted, require_watcher};
use crate::state::Store;
use crate::storage::config;
use crate::storage::events::read_state;
use crate::storage::flow_settings::{self, FlowSettings};
use crate::tx::{self, Detail};
use alloy_primitives::{Address, Bytes};
use types::cctp::BurnMessage;
use types::config::GasAction;
use types::flow::{finish_key, FinishAction};
use types::router::{self, DexCall, HookData, ACTION_CK_DEPOSIT, ACTION_DELIVER};
use types::{ChainId, EvmAddress, GasAmount, QuoteHash, Timestamp, Wei};

/// How close to its deadline a completion that reverts is tried as a release instead: the
/// canister's clock and the chain's differ by a few seconds, and a block may land late.
const DEADLINE_MARGIN_S: u64 = 300;

/// One finish, decided: what to call, where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub action: FinishAction,
    pub to: EvmAddress,
    pub hook: HookData,
    /// The deadline the hook data names, for the release fallback.
    pub deadline_s: u64,
}

/// What `finish_cctp` reads besides the message: the chain's CCTP domain, every chain's
/// domain, USDC and Swapic, the token messenger and the message transmitter.
pub struct Setup<'a> {
    pub chain_id: ChainId,
    pub domains: &'a types::config::ChainTable<types::CctpDomain>,
    pub usdc: &'a types::config::ChainTable<EvmAddress>,
    pub settings: &'a FlowSettings,
    pub messenger: Option<EvmAddress>,
    pub transmitter: Option<EvmAddress>,
}

fn word(address: EvmAddress) -> [u8; 32] {
    address.to_word()
}

/// The finish a message asks for at `now_s`, or why the canister will not finish it.
/// Pure, so every rule is a unit test.
pub fn decide(message: &BurnMessage, setup: &Setup, now_s: u64) -> Result<Plan, String> {
    let chain_domain = setup
        .domains
        .get(setup.chain_id)
        .ok_or_else(|| format!("chain {} has no CCTP domain", setup.chain_id))?;
    if message.destination_domain != chain_domain.get() {
        return Err(format!(
            "the message is for domain {}, and chain {} is domain {}",
            message.destination_domain,
            setup.chain_id,
            chain_domain.get()
        ));
    }
    let messenger = setup.messenger.ok_or("no token messenger is configured")?;
    if message.sender != word(messenger) || message.recipient != word(messenger) {
        return Err("the message is not a token messenger burn".into());
    }
    let source = setup
        .domains
        .0
        .iter()
        .find(|(_, domain)| domain.get() == message.source_domain)
        .map(|(chain, _)| *chain)
        .ok_or_else(|| format!("no chain is CCTP domain {}", message.source_domain))?;
    let usdc = setup
        .usdc
        .get(source)
        .ok_or_else(|| format!("no USDC is configured on chain {source}"))?;
    if message.body.burn_token != word(usdc) {
        return Err("the burn is not USDC".into());
    }
    let source_swapic = setup
        .settings
        .router(source)
        .ok_or_else(|| format!("no Swapic is configured on chain {source}"))?;
    if message.body.message_sender != word(source_swapic) {
        return Err("the burn was not made by Swapic".into());
    }
    let hook = HookData::parse(&message.body.hook_data)
        .ok_or("the message carries no Swapic hook data")?;
    if hook.action == ACTION_DELIVER {
        if message.destination_caller != [0; 32] {
            return Err("a Deliver must leave its destination caller open".into());
        }
        let transmitter = setup
            .transmitter
            .ok_or("no message transmitter is configured")?;
        return Ok(Plan {
            action: FinishAction::Receive,
            to: transmitter,
            deadline_s: hook.deadline,
            hook,
        });
    }
    let swapic = setup
        .settings
        .router(setup.chain_id)
        .ok_or_else(|| format!("no Swapic is configured on chain {}", setup.chain_id))?;
    if message.body.mint_recipient != word(swapic) || message.destination_caller != word(swapic) {
        return Err("a Swap or a CkDeposit is minted to Swapic and called by it".into());
    }
    let action = if now_s < hook.deadline {
        FinishAction::Complete
    } else {
        FinishAction::Release
    };
    Ok(Plan {
        action,
        to: swapic,
        deadline_s: hook.deadline,
        hook,
    })
}

/// The DEX call a completion carries: the watcher's fresh swap for a Swap, nothing for a
/// CkDeposit.
pub fn completion_swap(
    hook: &HookData,
    swap: Option<(EvmAddress, Vec<u8>)>,
) -> Result<DexCall, String> {
    match (hook.action, swap) {
        (ACTION_CK_DEPOSIT, Some((target, _))) if target != EvmAddress::ZERO => {
            Err("a CkDeposit takes no swap".into())
        }
        (_, Some((target, data))) => Ok(DexCall {
            target: router::addr(target),
            data: Bytes::from(data),
        }),
        (_, None) => Ok(DexCall {
            target: Address::ZERO,
            data: Bytes::new(),
        }),
    }
}

/// The calldata of `action` for this message.
pub fn calldata(
    action: FinishAction,
    message: &[u8],
    attestation: &[u8],
    swap: DexCall,
) -> Vec<u8> {
    match action {
        FinishAction::Receive => router::receive_message_calldata(message, attestation),
        FinishAction::Complete => router::complete_cctp_calldata(message, attestation, swap),
        FinishAction::Release => router::release_calldata(message, attestation),
    }
}

/// An address the watcher spelled, checksummed or in lower case.
pub fn parse_address(text: &str) -> Option<EvmAddress> {
    text.parse::<EvmAddress>().ok().or_else(|| {
        let bytes = hex::decode(text.strip_prefix("0x")?).ok()?;
        Some(EvmAddress::new(bytes.try_into().ok()?))
    })
}

/// Where a finish stands under its key: signed already (its id), being signed, or not yet.
pub fn existing(key: QuoteHash) -> Result<Option<u64>, String> {
    read_state(|state| {
        if let Some(flow) = state.flow(&key) {
            return flow
                .signed_id
                .map(Some)
                .ok_or_else(|| "the finish has a flow but no signed transaction".to_string());
        }
        if state.store().has_unsigned_nonce_for(&key) {
            return Err("the finish is being signed".into());
        }
        Ok(None)
    })
}

/// `finish_cctp`: answers the signed transaction's id in `signed_txs`.
pub async fn finish_cctp(
    chain_id: ChainId,
    message: &[u8],
    attestation: &[u8],
    swap: Option<(String, Vec<u8>)>,
) -> Result<u64, String> {
    require_watcher().map_err(|e| format!("{e:?}"))?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let parsed = BurnMessage::parse(message).map_err(|e| e.to_string())?;
    let config = config::get();
    let settings = flow_settings::get();
    let setup = Setup {
        chain_id,
        domains: &config.cctp_domains,
        usdc: &config.usdc_addresses,
        settings: &settings,
        messenger: config.token_messenger,
        transmitter: config.message_transmitter,
    };
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    let plan = decide(&parsed, &setup, now_s)?;
    let swap = swap
        .map(|(target, data)| {
            parse_address(&target)
                .map(|target| (target, data))
                .ok_or_else(|| format!("the swap target {target} is not an address"))
        })
        .transpose()?;
    let dex = completion_swap(&plan.hook, swap)?;
    let message_hash = alloy_primitives::keccak256(message).0;

    let mut candidates = vec![plan.action];
    if plan.action == FinishAction::Complete
        && now_s.saturating_add(DEADLINE_MARGIN_S) >= plan.deadline_s
    {
        candidates.push(FinishAction::Release);
    }
    for action in &candidates {
        if let Some(id) = existing(finish_key(message_hash, *action))? {
            return Ok(id);
        }
    }
    let first = finish_key(message_hash, plan.action);
    let _busy = Busy::take(first).ok_or("the finish is being signed")?;
    let mine = ecdsa::canister_address().await.map_err(|e| e.to_string())?;
    let mut refusals = Vec::new();
    for action in candidates {
        let data = calldata(action, message, attestation, dex.clone());
        let (simulated, estimate) =
            evm_reads::simulate_and_estimate(chain_id, mine, plan.to, &data)
                .await
                .map_err(|e| e.to_string())?;
        if let Simulated::Reverted(message) = simulated {
            refusals.push(format!("{action:?} reverts in simulation: {message}"));
            continue;
        }
        // the halt and a concurrent finish can land while the reads were out; from here to
        // the nonce's allocation there is no await
        require_not_halted().map_err(|e| format!("{e:?}"))?;
        let key = finish_key(message_hash, action);
        if let Some(id) = existing(key)? {
            return Ok(id);
        }
        let gas = gas_limit_for(estimate, &settings);
        let signed = tx::create_and_send_with(
            types::events::TxPurpose::Finish(key),
            chain_id,
            plan.to,
            Wei::ZERO,
            data,
            GasAmount::from(gas),
            match action {
                FinishAction::Receive => GasAction::RelayDeliver,
                FinishAction::Complete if plan.hook.action == ACTION_CK_DEPOSIT => {
                    GasAction::CompleteCk
                }
                FinishAction::Complete => GasAction::CompleteSwap,
                FinishAction::Release => GasAction::Release,
            },
            Some(Detail::Finish {
                action,
                message_hash,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
        return Ok(signed.id.get());
    }
    Err(refusals.join("; "))
}
