//! The canister finishes CCTP arrivals itself, design C (`finish_cctp`). The watcher hands
//! in the attested message, the source transaction that burned it, the stored recipe and
//! the gas limit the quoter charged for. The canister reads that source transaction itself
//! (`source`): it must have sent this message and paid the canister its fee step. The canister checks the message is a burn it may finish, decides
//! from the envelope's action and the clock what to call, and signs it once per arrival
//! (the burn, by its source domain and Circle's nonce, whatever Circle rewrites in its
//! message) and action, finishing at most one arrival per source transaction.
//!
//! The message must be a Swapic burn end to end: CCTP's token messenger on both ends (a
//! generic message whose body only looks like a burn is refused: anyone can have one
//! attested), the source chain's USDC, burned by the source chain's Runner, carrying a v3
//! envelope. A Swap or a CkDeposit is minted to the destination's CctpReceiver and called
//! only by it; a Deliver is minted to its envelope's recipient and received by anyone or by
//! the canister.
//!
//! The canister simulates only when `simulate_before_sign` is on: the watcher simulates
//! the call with the stored recipe before it calls here.

#[cfg(test)]
mod tests;

pub mod own;
pub mod source;

use crate::ecdsa;
use crate::engine::flows::Busy;
use crate::evm_reads::{self, Simulated};
use crate::guards::{require_not_halted, require_watcher};
use crate::relay::{envelope_parties, sanctioned_party, signing_gas, take_reading, Party};
use crate::state::{State, Store};
use crate::storage::config;
use crate::storage::events::read_state;
use crate::tx::{self, Detail};
use alloy_primitives::U256;
use settlement_api::types::chain_data::ChainData;
use thiserror::Error;
use types::cctp::{BurnMessage, MessageError};
use types::config::{ChainTable, EvmChain, GasAction};
use types::events::TxPurpose;
use types::flow::{finish_key, FinishAction};
use types::swapic::{self, Action, Envelope, EnvelopeError, Recipe};
use types::{CctpDomain, ChainId, EvmAddress, FlowStatus, QuoteHash, Timestamp, TxHash, Wei};

/// How long before the envelope's deadline the last completion is signed. A completion
/// that lands after the deadline reverts (`DeadlinePassed`), and between the signature and
/// the block there is the watcher's poll, the broadcast, and a fee that may wait for a
/// cheaper block.
pub const COMPLETE_MARGIN_S: u64 = 120;

/// How long after the envelope's deadline the first release is signed: `release` needs
/// the block's time past the deadline, and a chain's clock may trail the canister's.
pub const RELEASE_MARGIN_S: u64 = 60;

/// Why a finish was refused. Nothing was signed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FinishError {
    #[error(
        "a Swapic CCTP message is {expected} bytes, not {0}",
        expected = Envelope::CCTP_MESSAGE_LENGTH
    )]
    MessageLength(usize),
    #[error(transparent)]
    Message(#[from] MessageError),
    #[error("the message's envelope: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("src_tx_hash is {0} bytes, not 32")]
    SourceTxLength(usize),
    #[error("chain {0} is not one the canister signs on")]
    UnknownChain(ChainId),
    #[error("chain {0} has no CCTP domain")]
    NoDomain(ChainId),
    #[error("the message is for domain {message}, and chain {chain_id} is domain {chain}")]
    WrongDomain {
        message: u32,
        chain_id: ChainId,
        chain: u32,
    },
    #[error("the message is not a token messenger burn")]
    NotABurn,
    #[error("no chain the canister signs on is CCTP domain {0}")]
    UnknownSource(u32),
    #[error("the burn is not the source chain's USDC")]
    NotUsdc,
    #[error("the burn was not made by the source chain's Runner")]
    NotTheRunner,
    #[error("{0}")]
    Shape(&'static str),
    #[error("the envelope {0}")]
    Arrival(&'static str),
    #[error(
        "the envelope's deadline {deadline} is too close to act on at {now_s}: completed \
         before {complete_by}, released after {release_after}"
    )]
    NearDeadline {
        deadline: u64,
        now_s: u64,
        complete_by: u64,
        release_after: u64,
    },
    #[error("a Swap completes with a recipe")]
    NoRecipe,
    #[error("a CkDeposit completes with no recipe")]
    RecipeForCk,
    #[error("the recipe's {0} is not an EVM address")]
    RecipeAddress(&'static str),
    #[error(
        "source transaction {0} already finished another message: its one fee pays for one \
         arrival"
    )]
    SourceFinished(TxHash),
    #[error("{0} is sanctioned")]
    Sanctioned(Party),
    #[error("the source transaction {0}")]
    Source(&'static str),
}

/// What `finish_cctp` reads besides the message: the chains the canister signs on and
/// their CCTP domains, and the canister's own address.
pub struct Setup<'a> {
    pub chain_id: ChainId,
    pub evm_chains: &'a ChainTable<EvmChain>,
    pub domains: &'a ChainTable<CctpDomain>,
    pub canister: EvmAddress,
    /// The Solana deployment, when there is one: domain 5 as a source.
    pub solana: Option<&'a swapic_solana_wire::pda::SwapicAddresses>,
}

/// One finish, decided: what to call, where, under which gas action, and who the message
/// pays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub action: FinishAction,
    pub gas_action: GasAction,
    pub to: EvmAddress,
    pub envelope: Envelope,
    /// The message's mint recipient: the envelope's recipient for a Deliver, the
    /// CctpReceiver otherwise.
    pub mint_recipient: EvmAddress,
}

/// The chain a message was burned on, by its source domain, and its configuration.
pub fn source_of(message: &BurnMessage, setup: &Setup) -> Result<(ChainId, EvmChain), FinishError> {
    setup
        .domains
        .0
        .iter()
        .find(|(_, domain)| domain.get() == message.source_domain)
        .and_then(|(chain, _)| Some((*chain, setup.evm_chains.get(*chain)?)))
        .ok_or(FinishError::UnknownSource(message.source_domain))
}

/// The finish a message asks for at `now_s`, or why the canister will not finish it.
/// Pure, so every rule is a unit test.
pub fn decide(message: &BurnMessage, setup: &Setup, now_s: u64) -> Result<Plan, FinishError> {
    let chain_id = setup.chain_id;
    let destination = setup
        .evm_chains
        .get(chain_id)
        .ok_or(FinishError::UnknownChain(chain_id))?;
    let domain = setup
        .domains
        .get(chain_id)
        .ok_or(FinishError::NoDomain(chain_id))?;
    if message.destination_domain != domain.get() {
        return Err(FinishError::WrongDomain {
            message: message.destination_domain,
            chain_id,
            chain: domain.get(),
        });
    }
    if message.recipient != destination.token_messenger.to_word() {
        return Err(FinishError::NotABurn);
    }
    if message.source_domain == swapic_solana_wire::consts::SOLANA_DOMAIN {
        // only the program can burn as an `auth`, as only the Runner can burn as the Runner
        let solana = setup
            .solana
            .ok_or(FinishError::UnknownSource(message.source_domain))?;
        if message.sender != swapic_solana_wire::consts::TMM {
            return Err(FinishError::NotABurn);
        }
        if message.body.burn_token != swapic_solana_wire::consts::USDC_MINT {
            return Err(FinishError::NotUsdc);
        }
        if solana.auth_index(&message.body.message_sender).is_none() {
            return Err(FinishError::NotTheRunner);
        }
    } else {
        let (_, source) = source_of(message, setup)?;
        if message.sender != source.token_messenger.to_word() {
            return Err(FinishError::NotABurn);
        }
        if message.body.burn_token != source.usdc.to_word() {
            return Err(FinishError::NotUsdc);
        }
        if message.body.message_sender != source.runner.to_word() {
            return Err(FinishError::NotTheRunner);
        }
    }
    let envelope = Envelope::decode(&message.body.hook_data)?;
    let mint_recipient = swapic::evm_of_word(message.body.mint_recipient).ok_or(
        FinishError::Shape("the mint recipient is not an EVM address"),
    )?;
    if envelope.action == Action::Deliver {
        if mint_recipient != envelope.recipient {
            return Err(FinishError::Shape(
                "a Deliver is minted to its envelope's recipient",
            ));
        }
        if message.destination_caller != [0; 32]
            && message.destination_caller != setup.canister.to_word()
        {
            return Err(FinishError::Shape(
                "a Deliver is received by anyone or by the canister",
            ));
        }
        // minted to the user whenever it is received, before the deadline or after it
        return Ok(Plan {
            action: FinishAction::Receive,
            gas_action: GasAction::RelayDeliver,
            to: destination.message_transmitter,
            envelope,
            mint_recipient,
        });
    }
    let receiver = destination.cctp_receiver;
    if mint_recipient != receiver || message.destination_caller != receiver.to_word() {
        return Err(FinishError::Shape(
            "a Swap or a CkDeposit is minted to the CctpReceiver and called only by it",
        ));
    }
    let complete_by = envelope.deadline.saturating_sub(COMPLETE_MARGIN_S);
    let release_after = envelope.deadline.saturating_add(RELEASE_MARGIN_S);
    let (action, gas_action) = if now_s < complete_by {
        let gas_action = match envelope.action {
            Action::Swap => GasAction::CompleteSwap,
            _ => GasAction::CompleteCk,
        };
        (FinishAction::Complete, gas_action)
    } else if now_s > release_after {
        (FinishAction::Release, GasAction::Release)
    } else {
        return Err(FinishError::NearDeadline {
            deadline: envelope.deadline,
            now_s,
            complete_by,
            release_after,
        });
    };
    match action {
        FinishAction::Complete => fillable(&envelope, &destination)?,
        // a release pays the recipient the minted USDC, whatever else the envelope says
        _ if envelope.recipient == EvmAddress::ZERO => {
            return Err(FinishError::Arrival("releases to the zero address"))
        }
        _ => {}
    }
    Ok(Plan {
        action,
        gas_action,
        to: receiver,
        envelope,
        mint_recipient,
    })
}

/// Whether the receiver and the Runner will act on `envelope` (abi.md rule 10), checked
/// on every arrival, user-paid ones included: a completion they refuse reverts at the
/// canister's cost. The recipient is not zero and none of Swapic's contracts; a Swap's
/// tokenOut is not zero or the arriving USDC, and its minOut is above zero; minGas is set.
fn fillable(envelope: &Envelope, destination: &EvmChain) -> Result<(), FinishError> {
    let recipient = envelope.recipient;
    if recipient == EvmAddress::ZERO
        || [
            destination.swapic,
            destination.runner,
            destination.cctp_receiver,
        ]
        .contains(&recipient)
    {
        return Err(FinishError::Arrival(
            "pays zero or one of Swapic's own contracts",
        ));
    }
    if envelope.min_gas == 0 {
        return Err(FinishError::Arrival("sets no minGas"));
    }
    if envelope.action == Action::Swap
        && (envelope.token_out == EvmAddress::ZERO
            || envelope.token_out == destination.usdc
            || envelope.min_out == U256::ZERO)
    {
        return Err(FinishError::Arrival(
            "of a Swap names no tokenOut other than USDC, or no minOut",
        ));
    }
    Ok(())
}

/// The parties a finish pays, screened: none of them may be sanctioned.
/// - A delivery pays the envelope's recipient, who is its mint recipient as well.
/// - A completion pays the recipient, and a CkDeposit's completion credits the principal it
///   names, which must read as one.
/// - A release pays the recipient the minted USDC and credits no principal, so only the
///   recipient is screened, and a principal that does not read stops nothing.
pub fn screen(plan: &Plan) -> Result<(), FinishError> {
    let recipient = Party::Evm(plan.envelope.recipient);
    let parties = match plan.action {
        FinishAction::Receive => vec![recipient, Party::Evm(plan.mint_recipient)],
        FinishAction::Complete => envelope_parties(&plan.envelope).map_err(FinishError::Shape)?,
        FinishAction::Release => vec![recipient],
    };
    match sanctioned_party(&parties) {
        Some(party) => Err(FinishError::Sanctioned(party)),
        None => Ok(()),
    }
}

/// A recipe as the watcher sends it, its addresses read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GivenRecipe {
    pub target: EvmAddress,
    pub data: Vec<u8>,
    pub scaler: Option<EvmAddress>,
}

/// The recipe the call carries: the watcher's for a Swap's completion, none for a
/// CkDeposit's, and none for a call that takes none.
pub fn recipe_for(plan: &Plan, given: Option<GivenRecipe>) -> Result<Recipe, FinishError> {
    match (plan.action, plan.envelope.action, given) {
        (FinishAction::Complete, Action::Swap, Some(recipe))
            if recipe.target != EvmAddress::ZERO =>
        {
            Ok(Recipe {
                target: swapic::addr(recipe.target),
                data: recipe.data.into(),
                scaler: swapic::addr(recipe.scaler.unwrap_or(EvmAddress::ZERO)),
            })
        }
        (FinishAction::Complete, Action::Swap, _) => Err(FinishError::NoRecipe),
        (FinishAction::Complete, Action::CkDeposit, Some(recipe))
            if recipe.target != EvmAddress::ZERO
                || !recipe.data.is_empty()
                || recipe
                    .scaler
                    .is_some_and(|scaler| scaler != EvmAddress::ZERO) =>
        {
            Err(FinishError::RecipeForCk)
        }
        _ => Ok(swapic::no_recipe()),
    }
}

/// The calldata of `action` for this message.
pub fn calldata(
    action: FinishAction,
    message: &[u8],
    attestation: &[u8],
    recipe: Recipe,
) -> Vec<u8> {
    match action {
        FinishAction::Receive => swapic::receive_message_calldata(message, attestation),
        FinishAction::Complete => swapic::complete_cctp_calldata(message, attestation, recipe),
        FinishAction::Release => swapic::release_calldata(message, attestation),
    }
}

/// An address the watcher spelled, checksummed or in lower case.
pub fn parse_address(text: &str) -> Option<EvmAddress> {
    text.parse::<EvmAddress>().ok().or_else(|| {
        let bytes = hex::decode(text.strip_prefix("0x")?).ok()?;
        Some(EvmAddress::new(bytes.try_into().ok()?))
    })
}

/// The id of a finish of this message already signed that answers a call for `action`:
/// the action's own, and for a release the completion's too, unless that completion
/// reverted, which is what a release after it is for.
pub fn answered(action: FinishAction, arrival: [u8; 32]) -> Result<Option<u64>, String> {
    if action == FinishAction::Release {
        let complete = finish_key(arrival, FinishAction::Complete);
        if let Some(id) = tx::signed_once(complete)? {
            let reverted = read_state(|state| state.flow(&complete))
                .is_some_and(|flow| flow.status == FlowStatus::FinishFailed);
            if !reverted {
                return Ok(Some(id));
            }
        }
    }
    tx::signed_once(finish_key(arrival, action))
}

/// The id of any finish of this arrival already signed: its delivery, its release, or a
/// completion that did not revert (after a reverted one only a release follows). What a
/// call for an arrival the canister already finished answers, before anything else is
/// read, so neither the clock, nor the config, nor a later listing turns a finished
/// arrival into a refusal.
pub fn answered_arrival(arrival: [u8; 32]) -> Result<Option<u64>, String> {
    for action in [FinishAction::Receive, FinishAction::Release] {
        if let Some(id) = tx::signed_once(finish_key(arrival, action))? {
            return Ok(Some(id));
        }
    }
    let complete = finish_key(arrival, FinishAction::Complete);
    if let Some(id) = tx::signed_once(complete)? {
        let reverted = read_state(|state| state.flow(&complete))
            .is_some_and(|flow| flow.status == FlowStatus::FinishFailed);
        if !reverted {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Whether `source_tx` already finished as many arrivals other than this one as it paid for
/// (`paid`). One fee pays for one arrival and the fold holds the same count; this is the door's
/// read of the rule, before a nonce is spent on it.
pub fn finished_others<S: Store>(
    state: &State<S>,
    source_tx: TxHash,
    arrival: [u8; 32],
    paid: u32,
) -> bool {
    state.other_arrivals_of_source(&source_tx, arrival) >= paid.max(1)
}

/// The key two calls for one source transaction serialize on.
fn source_key(source_tx: TxHash) -> QuoteHash {
    let mut preimage = b"swapic-cctp-source".to_vec();
    preimage.extend_from_slice(source_tx.as_ref());
    QuoteHash::new(alloy_primitives::keccak256(preimage).0)
}

/// `finish_cctp`: answers the signed transaction's id in `signed_txs`. `reading` is the
/// destination chain's, the one the watcher priced the finish on.
pub async fn finish_cctp(
    chain_id: ChainId,
    message: &[u8],
    attestation: &[u8],
    recipe: Option<GivenRecipe>,
    src_tx_hash: &[u8],
    gas_limit: u64,
    reading: Option<ChainData>,
) -> Result<u64, String> {
    require_watcher().map_err(|e| format!("{e:?}"))?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    // the reading the watcher priced this call on: stored first, so the fee rule reads it
    // fresh; with none (or one too old) the canister reads the chain's gas itself, before
    // any check below
    take_reading(
        chain_id,
        reading,
        Timestamp::from_nanos(ic_cdk::api::time()),
    )?;
    crate::gas::ensure_fresh(chain_id)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))?;
    if message.len() != Envelope::CCTP_MESSAGE_LENGTH {
        return Err(FinishError::MessageLength(message.len()).to_string());
    }
    let parsed = BurnMessage::parse(message).map_err(|e| e.to_string())?;
    // a Solana source is named by its 64-byte signature, and keyed in the fold by a 32-byte hash of it
    let from_solana = parsed.source_domain == swapic_solana_wire::consts::SOLANA_DOMAIN;
    let solana_signature: Option<[u8; 64]> = if from_solana {
        Some(
            src_tx_hash
                .try_into()
                .map_err(|_| FinishError::SourceTxLength(src_tx_hash.len()).to_string())?,
        )
    } else {
        None
    };
    let source_tx = match &solana_signature {
        Some(signature) => crate::solana::source::source_key(signature),
        None => TxHash::new(
            src_tx_hash
                .try_into()
                .map_err(|_| FinishError::SourceTxLength(src_tx_hash.len()).to_string())?,
        ),
    };
    // an arrival already finished answers its finish, whatever came after it
    let arrival = types::cctp::arrival_id(&parsed);
    if let Some(id) = answered_arrival(arrival)? {
        return Ok(id);
    }
    // derived once by the deploy and read from stable memory after that
    let canister = ecdsa::canister_address().await.map_err(|e| e.to_string())?;
    let config = config::get();
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    let solana = crate::solana::deployment().ok();
    let setup = Setup {
        chain_id,
        evm_chains: &config.evm_chains,
        domains: &config.cctp_domains,
        canister,
        solana: solana.as_ref().map(|d| &d.a),
    };
    let mut plan = decide(&parsed, &setup, now_s).map_err(|e| e.to_string())?;
    // the canister's own transfer (a move's, a payout's) is finished with no fee step, only
    // from its own source transaction
    plan.gas_action =
        read_state(|state| own::finish_action(state, source_tx, &parsed, &plan, ic_cdk::api::id()))
            .map_err(|e| e.to_string())?;
    screen(&plan).map_err(|e| e.to_string())?;
    let recipe = recipe_for(&plan, recipe).map_err(|e| e.to_string())?;
    let destination = config
        .evm_chains
        .get(chain_id)
        .expect("BUG: decide found the chain in this table");
    let gas = signing_gas(chain_id, &destination, plan.gas_action, gas_limit)
        .map_err(|e| e.to_string())?;
    // the watcher's word is not enough: the source transaction itself shows this
    // burn and its fee step. The canister's own transfers are finished from its own
    // transactions only (`own`), and pay no fee step
    let mut paid = 1;
    // a burn out of Solana: one finish per `(domain 5, ref)`, its fee read from the program's instruction
    let mut solana_burn = None;
    if let (Some(signature), Some(d)) = (&solana_signature, &solana) {
        let needed = destination
            .min_network_fee
            .get(source::charged_action(&plan.envelope))
            .unwrap_or(types::TokenAmount::MAX);
        let landed = crate::solana::rpc::transaction(d.config.sol_rpc, signature)
            .await?
            .ok_or("the Solana transaction is not found at confirmed")?;
        let proved = crate::solana::source::proves(
            &landed,
            message,
            &d.a,
            needed,
            d.config.min_network_fee.gasless_burn,
        )?;
        paid = u32::MAX;
        solana_burn = Some((proved.r#ref, landed.block_time_s.unwrap_or(now_s)));
        require_not_halted().map_err(|e| format!("{e:?}"))?;
    } else if read_state(|state| state.store().pool_own_source(&source_tx)).is_none() {
        let (source_chain, source) = source_of(&parsed, &setup).map_err(|e| e.to_string())?;
        // the least the quoter charges for the arrival it charged for; an absent row asks
        // the most, as the relay's rule does
        let needed = destination
            .min_network_fee
            .get(source::charged_action(&plan.envelope))
            .unwrap_or(types::TokenAmount::MAX);
        let receipt = source::read(source_chain, source_tx).await?;
        paid = source::proves(&receipt, &parsed, &source, canister, needed)
            .map_err(|e| e.to_string())?;
        require_not_halted().map_err(|e| format!("{e:?}"))?;
    }

    let _busy =
        Busy::take(source_key(source_tx)).ok_or("the source transaction is being finished")?;
    let admit = || -> Result<Option<u64>, String> {
        if let Some(id) = answered(plan.action, arrival)? {
            return Ok(Some(id));
        }
        if read_state(|state| finished_others(state, source_tx, arrival, paid)) {
            return Err(FinishError::SourceFinished(source_tx).to_string());
        }
        if let Some((r#ref, _)) = &solana_burn {
            if read_state(|state| state.solana_finished_ref(r#ref)).is_some_and(|done| done != arrival) {
                return Err("this ref already finished another message".to_string());
            }
        }
        Ok(None)
    };
    if let Some(id) = admit()? {
        return Ok(id);
    }
    let data = calldata(plan.action, message, attestation, recipe);
    if config.simulate_before_sign {
        let (simulated, _) = evm_reads::simulate_and_estimate(chain_id, canister, plan.to, &data)
            .await
            // the kind only: an outcall's text can carry the provider's url and key
            .map_err(|e| format!("the simulation was not read: {:?}", e.kind()))?;
        if let Simulated::Reverted(_) = simulated {
            return Err(format!("{:?} reverts in simulation", plan.action));
        }
        // the halt and a concurrent finish can land while the read was out; from here to
        // the nonce's allocation there is no await
        require_not_halted().map_err(|e| format!("{e:?}"))?;
        if let Some(id) = admit()? {
            return Ok(id);
        }
    }
    // a reading that aged past its limit while the reads above were out is read again, not
    // refused; the halt and a concurrent finish are checked again after that read, and from
    // here to the nonce's allocation there is no await
    if crate::gas::refresh_if_aged(chain_id)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))?
    {
        require_not_halted().map_err(|e| format!("{e:?}"))?;
        if let Some(id) = admit()? {
            return Ok(id);
        }
    }
    if let Some((r#ref, burned_at_s)) = solana_burn {
        crate::storage::events::append_event(types::EventType::Solana {
            step: types::solana::SolanaStep::RefFinished { r#ref, arrival, burned_at_s },
        })
        .map_err(|e| e.to_string())?;
    }
    let key = finish_key(arrival, plan.action);
    let signed = tx::create_and_send_with(
        TxPurpose::Finish(key),
        chain_id,
        plan.to,
        Wei::ZERO,
        data,
        gas,
        plan.gas_action,
        Some(Detail::Finish {
            action: plan.action,
            arrival,
            source_tx,
            paid,
        }),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(signed.id.get())
}
