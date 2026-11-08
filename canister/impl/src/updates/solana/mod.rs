//! The Solana doors (design 5.2). Each refuses before any outcall when the caller or the switches say
//! so, and reads the halt again after its last await.

use crate::cctp;
use crate::ed25519;
use crate::engine::flows::Busy;
use crate::guards::{require_controller, require_not_halted, require_watcher};
use crate::relay::Party;
use crate::solana::arrival::{build_complete, build_release, read_route, shards_of};
use crate::solana::cosign::{self, Context};
use crate::solana::keeper;
use crate::solana::tx::{compile_v0_from_payer, compile_v1_from_payer, record, sign_and_record, Compiled};
use crate::solana::upgrade;
use crate::solana::{deployment, now_s, rpc, screen, Deployment};
use crate::state::solana::hour_of;
use crate::state::Store;
use crate::storage::events::{append_event, read_state};
use crate::storage::{config, solana_keys};
use ic_cdk::update;
pub use settlement_api::types::solana::{
    FinishSolanaArgs, ProposeSolanaUpgradeArgs, ReleaseMisroutedArgs, RelaySolanaForwardArgs, SolanaAddresses,
    SolanaFinishAction, SolanaLifetime, SolanaProgramConfigArgs, SolanaReclaimItem, SweepSolanaFeesArgs,
};
use swapic_solana_wire::args::Config as ProgramConfig;
use swapic_solana_wire::base58;
use swapic_solana_wire::cctp::Message;
use swapic_solana_wire::consts::{SOLANA_DOMAIN, TOKEN_PROGRAM, USDC_MINT, WSOL_MINT};
use swapic_solana_wire::envelope::{read_release_terms, ForwarderHook, SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_wire::instructions;
use swapic_solana_wire::message::{wire_v0, Instruction, LookupTable};
use swapic_solana_wire::pda;
use types::cctp::BurnMessage;
use types::events::EventType;
use types::solana::{SolanaKeyRole, SolanaProposal, SolanaProposalStatus, SolanaPurpose, SolanaStep, SolanaTxVersion};
use types::{EvmAddress, QuoteHash, TokenAmount};

fn watcher() -> Result<(), String> {
    require_watcher().map_err(|e| format!("{e:?}"))?;
    require_not_halted().map_err(|e| format!("{e:?}"))
}

fn controller() -> Result<(), String> {
    require_controller().map_err(|e| format!("{e:?}"))
}

fn step(step: SolanaStep) -> Result<u64, String> {
    append_event(EventType::Solana { step }).map(|index| index.get()).map_err(|e| e.to_string())
}

/// Controller-only, once. Derives the payer key and the admin key, and answers both.
#[update]
pub async fn derive_solana_keys() -> Result<SolanaAddresses, String> {
    controller()?;
    let key_name = config::get().solana.ok_or("the canister has no Solana config")?.key_name;
    let (payer, admin) = match solana_keys::get() {
        Some(keys) => keys,
        None => {
            let payer = ed25519::public_key(SolanaKeyRole::Payer, &key_name).await?;
            let admin = ed25519::public_key(SolanaKeyRole::Admin, &key_name).await?;
            solana_keys::set(payer, admin);
            step(SolanaStep::KeysDerived { payer, admin })?;
            (payer, admin)
        }
    };
    Ok(SolanaAddresses { payer: base58::encode(&payer), admin: base58::encode(&admin) })
}

/// Controller-only. The proof of rollout step 4: a v1 memo transaction signed by the payer key, that
/// the watcher broadcasts. Whether it lands sets the arrival mode (design decision 8).
#[update]
pub async fn prove_solana_send(lifetime: SolanaLifetime) -> Result<u64, String> {
    controller()?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let d = deployment()?;
    let memo = Instruction {
        program: swapic_solana_wire::consts::b58("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"),
        accounts: vec![],
        data: b"swapic v1 send proof".to_vec(),
    };
    let compiled = compile_v1_from_payer(&d, &[memo], 10_000, &lifetime)?;
    sign_and_record(&d, SolanaPurpose::Proof, compiled, lifetime.last_valid_block_height).await
}

/// Watcher-only. Co-signs a gasless user burn as its fee payer, if every rule of design 5.2.4 passes.
/// Answers the id in `signed_txs`.
#[update]
pub async fn cosign_solana(tx: Vec<u8>, last_valid_block_height: u64) -> Result<u64, String> {
    // 1. the switches, before any outcall
    watcher()?;
    let d = deployment()?;
    if !d.config.enabled_gasless {
        return Err("gasless on Solana is off".to_string());
    }
    let settings = config::get();
    let now = now_s();
    let pool_address = match settings.pool_chain {
        Some(_) => crate::storage::ecdsa_address::get(),
        None => None,
    };
    let destination = |domain: u32| {
        let (chain, _) = settings.cctp_domains.0.iter().find(|(_, d)| d.get() == domain)?;
        settings.evm_chains.get(*chain)
    };
    // 2 to 7, and the parties of 8
    let passed = cosign::check(
        &tx,
        &Context { a: &d.a, config: &d.config, now_s: now, destination, pool_address, max_ahead_s: 7_200 },
    )?;
    screen(&passed.parties)?;

    // 9. the signer: nothing counts before its signature does
    let mut others = passed.signatures.iter().skip(1);
    for signer in &passed.signers {
        let signature = others.next().ok_or("a signature is missing")?;
        if !cosign::verify(signer, &passed.message, signature) {
            return Err("a signature does not verify".to_string());
        }
    }
    let hour = hour_of(now);
    let (signer, global) = read_state(|state| (state.solana_signer(&passed.user, hour), state.solana_global(hour)));
    cosign::within_limits(&d.config, signer, global)?;

    // 10. one co-sign per ref
    let _busy = Busy::take(QuoteHash::new(pda::keccak256(&[&b"swapic-solana-cosign"[..], &passed.r#ref].concat())))
        .ok_or("this ref is being co-signed")?;
    if let Some((hash, line, at_s, _)) = read_state(|state| state.solana_cosign_of_ref(&passed.r#ref)) {
        if let Some(line) = cosign::second_request((hash, line, at_s), &passed.message_hash, now)? {
            return Ok(line);
        }
    }

    // 11. the record, after the last await and the halt read again
    let signature = ed25519::sign(SolanaKeyRole::Payer, &d.config.key_name, passed.message.clone()).await?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let mut signatures = passed.signatures.clone();
    signatures[0] = signature;
    record(
        SolanaPurpose::Cosign { r#ref: passed.r#ref, signer: passed.user, message_hash: passed.message_hash },
        SolanaTxVersion::V0,
        signature,
        wire_v0(&signatures, &passed.message),
        last_valid_block_height,
    )
}

/// Watcher-only. A co-signed transaction landed and failed: the canister reads it itself, and only
/// then blocks the signer and charges the failure budget. The watcher's word decides nothing.
#[update]
pub async fn report_solana_failure(signature: Vec<u8>) -> Result<bool, String> {
    watcher()?;
    let d = deployment()?;
    let signature: [u8; 64] = signature.try_into().map_err(|_| "a signature is 64 bytes")?;
    let signer = read_state(|state| state.solana_cosign_signer(&signature)).ok_or("the canister did not co-sign this")?;
    let landed = rpc::transaction(d.config.sol_rpc, &signature).await?.ok_or("the transaction is not found")?;
    if !landed.failed {
        return Ok(false);
    }
    step(SolanaStep::CosignFailed { signer, signature })?;
    Ok(true)
}

/// The EVM chain a message came from, by its domain, and its config.
fn evm_source(message: &BurnMessage) -> Result<(types::ChainId, types::config::EvmChain), String> {
    let settings = config::get();
    settings
        .cctp_domains
        .0
        .iter()
        .find(|(_, domain)| domain.get() == message.source_domain)
        .and_then(|(chain, _)| Some((*chain, settings.evm_chains.get(*chain)?)))
        .ok_or_else(|| format!("no chain the canister signs on is CCTP domain {}", message.source_domain))
}

/// The source rules of an arrival, and whether its fee was paid: a Runner burn on a chain the canister
/// knows, proven from its receipt, or the canister's own pool payout (which pays no fee step).
async fn prove_arrival_source(
    parsed: &BurnMessage,
    src_tx_hash: &[u8],
    needed: TokenAmount,
) -> Result<(), String> {
    let (chain, source) = evm_source(parsed)?;
    if parsed.sender != source.token_messenger.to_word() {
        return Err("the message is not a token messenger burn".to_string());
    }
    if parsed.body.burn_token != source.usdc.to_word() {
        return Err("the burn is not the source chain's USDC".to_string());
    }
    if parsed.body.message_sender != source.runner.to_word() {
        return Err("the burn was not made by the source chain's Runner".to_string());
    }
    let source_tx = types::TxHash::new(src_tx_hash.try_into().map_err(|_| "src_tx_hash is 32 bytes")?);
    let own = read_state(|state| match state.store().pool_own_source(&source_tx) {
        Some(types::pool::OwnSource::Payout(id)) => Some(
            state
                .store()
                .pool_payout(id)
                .and_then(|payout| payout.order)
                .is_some_and(|pinned| cctp::own::burn_of(&pinned.order, parsed)),
        ),
        Some(_) => Some(false),
        None => None,
    });
    match own {
        Some(true) => return Ok(()),
        Some(false) => return Err("the message is not the burn of the canister's own order".to_string()),
        None => {}
    }
    let canister = crate::ecdsa::canister_address().await.map_err(|e| e.to_string())?;
    let receipt = cctp::source::read(chain, source_tx).await?;
    cctp::source::proves(&receipt, parsed, &source, canister, needed).map_err(|e| e.to_string())?;
    Ok(())
}

fn word_of_usdc(source: &types::config::EvmChain) -> [u8; 32] {
    source.usdc.to_word()
}

fn compile(d: &Deployment, instructions: &[Instruction], cu_limit: u32, lifetime: &SolanaLifetime) -> Result<Compiled, String> {
    match &d.config.arrival_table {
        Some(table) => {
            let table = LookupTable { address: table.address, entries: table.entries.clone() };
            let mut all = vec![
                instructions::compute_unit_limit(cu_limit.min(d.config.max_cu_limit)),
                instructions::compute_unit_price(d.config.max_cu_price.min(d.config.max_priority_lamports * 1_000_000 / u64::from(cu_limit.max(1)))),
            ];
            all.extend_from_slice(instructions);
            compile_v0_from_payer(d, &table, &all, lifetime)
        }
        None => compile_v1_from_payer(d, instructions, cu_limit, lifetime),
    }
}

/// Watcher-only. Finishes an arrival on Solana (design 5.2.7): answers the id in `signed_txs`, or
/// nothing when the message was already received, and then nothing more is ever signed for it.
#[update]
pub async fn finish_solana(args: FinishSolanaArgs) -> Result<Option<u64>, String> {
    // 1. the parse
    watcher()?;
    let d = deployment()?;
    if !d.config.enabled_arrivals {
        return Err("arrivals on Solana are off".to_string());
    }
    let parsed = BurnMessage::parse(&args.message).map_err(|e| e.to_string())?;
    let view = Message::new(&args.message).map_err(|e| format!("{e:?}"))?;
    let arrival = types::cctp::arrival_id(&parsed);
    if read_state(|state| state.solana_arrival_done(&arrival)) {
        return Ok(None);
    }
    // 3. the message's shape, before any read
    if parsed.destination_domain != SOLANA_DOMAIN || parsed.recipient != swapic_solana_wire::consts::TMM {
        return Err("the message is not a burn for Solana's token messenger".to_string());
    }
    let (caller, vault) = shards_of(&view, &d.a).ok_or("the message is not addressed to the program")?;
    if caller != vault {
        return Err("the caller and the vault are of different shards: release_solana_misrouted".to_string());
    }
    let hook = SolanaEnvelope::parse(view.hook()).map_err(|e| format!("the hook: {e:?}"))?;
    let now = now_s();
    let recipient = hook.recipient;
    // 6. the attempts, before any outcall
    let (completes, releases) =
        read_state(|state| (state.solana_complete_attempts(&arrival), state.solana_release_attempts(&arrival)));
    let wanted = match args.action {
        SolanaFinishAction::Complete => {
            if hook.action != SOLANA_ACTION_SWAP {
                return Err("only a Swap completes".to_string());
            }
            if now + d.config.complete_margin_s >= hook.deadline as u64 {
                return Err("past the completion margin: only a release".to_string());
            }
            if completes >= u64::from(d.config.max_complete_attempts) {
                return Err("the completes are spent: only a release".to_string());
            }
            SolanaPurpose::Complete { arrival }
        }
        SolanaFinishAction::Release => {
            if releases >= u64::from(d.config.max_release_attempts) {
                return Err("the releases are spent".to_string());
            }
            SolanaPurpose::Release { arrival }
        }
    };
    screen(&[Party::Solana(recipient)])?;

    // 2. already received: nothing is ever signed again for it (attack M5)
    let used = rpc::account(d.config.sol_rpc, &pda::used_nonce(&parsed.nonce), Some(0), 1024).await?;
    if used.is_some() {
        step(SolanaStep::ArrivalFinished { arrival })?;
        return Ok(None);
    }
    // 4. the fee, always the new-account rows (attack L1)
    prove_arrival_source(&parsed, &args.src_tx_hash, d.config.min_network_fee.arrival()).await?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;

    let _busy = Busy::take(QuoteHash::new(arrival)).ok_or("this arrival is being finished")?;
    let (_, source) = evm_source(&parsed)?;
    let source_token = word_of_usdc(&source);
    // 7 and 8. the route and the build
    let (all, cu_limit) = match args.action {
        SolanaFinishAction::Complete => {
            let route = args.route.as_ref().ok_or("a Complete names a route")?;
            let creates = usize::from(args.create_account) + usize::from(hook.mint_out == WSOL_MINT);
            let metas = read_route(route, &d.a, creates)?;
            let token_2022 = args.token_2022;
            (
                build_complete(
                    &d.a, &hook, caller, &parsed.nonce, parsed.source_domain, &source_token, &args.message,
                    &args.attestation, route, &metas, args.create_account, token_2022,
                ),
                args.cu_limit,
            )
        }
        SolanaFinishAction::Release => (
            build_release(
                &d.a, caller, vault, Some(&recipient), &parsed.nonce, parsed.source_domain, &source_token,
                &args.message, &args.attestation, args.create_account,
            ),
            args.cu_limit,
        ),
    };
    let compiled = compile(&d, &all, cu_limit, &args.lifetime)?;
    // 9. the record
    sign_and_record(&d, wanted, compiled, args.lifetime.last_valid_block_height).await.map(Some)
}

/// Controller-only. A message whose hook is unreadable or whose recipient is a program address (design
/// 4.8 step 5): `release` to Swapic's fee account, recording the order and the EVM payer to refund.
#[update]
pub async fn release_solana_misrouted(args: ReleaseMisroutedArgs) -> Result<Option<u64>, String> {
    controller()?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let d = deployment()?;
    let parsed = BurnMessage::parse(&args.message).map_err(|e| e.to_string())?;
    let view = Message::new(&args.message).map_err(|e| format!("{e:?}"))?;
    let (caller, vault) = shards_of(&view, &d.a).ok_or("the message is not addressed to the program")?;
    if let Some(terms) = read_release_terms(view.hook()) {
        if !d.a.is_swapic_account(&terms.recipient) {
            return Err("this hook names a recipient: finish_solana's release pays it".to_string());
        }
    }
    let order_ref: [u8; 32] = args.order_ref.try_into().map_err(|_| "order_ref is 32 bytes")?;
    let payer: EvmAddress = cctp::parse_address(&args.payer).ok_or("payer is not an EVM address")?;
    let arrival = types::cctp::arrival_id(&parsed);
    if rpc::account(d.config.sol_rpc, &pda::used_nonce(&parsed.nonce), Some(0), 1024).await?.is_some() {
        step(SolanaStep::ArrivalFinished { arrival })?;
        return Ok(None);
    }
    let (_, source) = evm_source(&parsed)?;
    let source_token = word_of_usdc(&source);
    let all = build_release(
        &d.a, caller, vault, None, &parsed.nonce, parsed.source_domain, &source_token, &args.message, &args.attestation, false,
    );
    let compiled = compile(&d, &all, 400_000, &args.lifetime)?;
    let id = sign_and_record(&d, SolanaPurpose::MisroutedRelease { arrival }, compiled, args.lifetime.last_valid_block_height).await?;
    step(SolanaStep::Misrouted { arrival, order_ref, payer: *payer.as_bytes() })?;
    Ok(Some(id))
}

/// Watcher-only. Reclaims the rent of burns the canister recorded, at least five days old.
#[update]
pub async fn reclaim_solana(items: Vec<SolanaReclaimItem>, lifetime: SolanaLifetime) -> Result<u64, String> {
    watcher()?;
    let d = deployment()?;
    if items.is_empty() || items.len() > keeper::MAX_RECLAIMS {
        return Err(format!("one to {} reclaims at a time", keeper::MAX_RECLAIMS));
    }
    let now = now_s();
    let mut refs = Vec::with_capacity(items.len());
    let mut all = Vec::with_capacity(items.len());
    for item in &items {
        let r#ref: [u8; 32] = item.r#ref.clone().try_into().map_err(|_| "a ref is 32 bytes")?;
        let burn = read_state(|state| state.solana_burn(&r#ref));
        keeper::reclaimable(burn, now)?;
        all.push(instructions::reclaim(&d.a, &d.a.canister, &d.a.event(&r#ref), &item.attestation, &item.destination_message));
        refs.push(r#ref);
    }
    let compiled = compile_v1_from_payer(&d, &all, 80_000 * all.len() as u32, &lifetime)?;
    sign_and_record(&d, SolanaPurpose::Reclaim { refs }, compiled, lifetime.last_valid_block_height).await
}

/// Watcher-only. Below `rent_float_low`, one System transfer from the payer key up to the target, at
/// most once an hour, from the canister's own balance read.
#[update]
pub async fn top_up_solana_rent(lifetime: SolanaLifetime) -> Result<Option<u64>, String> {
    watcher()?;
    let d = deployment()?;
    let rent = rpc::balance(d.config.sol_rpc, &d.a.rent.0).await?;
    let last = read_state(|state| state.solana_last_top_up());
    let Some(amount) = keeper::top_up(rent, d.config.rent_float_low, d.config.rent_float_target, last, now_s()) else {
        return Ok(None);
    };
    let compiled = compile_v1_from_payer(&d, &[instructions::system_transfer(&d.a.canister, &d.a.rent.0, amount)], 5_000, &lifetime)?;
    sign_and_record(&d, SolanaPurpose::TopUp, compiled, lifetime.last_valid_block_height).await.map(Some)
}

/// Watcher-only. A Forwarder message not delivered after 10 minutes, whose source paid `ForwardRelay`
/// or is the canister's own payout. The money goes to the mint recipient whoever submits.
#[update]
pub async fn relay_solana_forward(args: RelaySolanaForwardArgs) -> Result<Option<u64>, String> {
    watcher()?;
    let d = deployment()?;
    if !d.config.enabled_arrivals {
        return Err("arrivals on Solana are off".to_string());
    }
    let parsed = BurnMessage::parse(&args.message).map_err(|e| e.to_string())?;
    let view = Message::new(&args.message).map_err(|e| format!("{e:?}"))?;
    let arrival = types::cctp::arrival_id(&parsed);
    if parsed.destination_domain != SOLANA_DOMAIN || parsed.recipient != swapic_solana_wire::consts::TMM {
        return Err("the message is not a burn for Solana's token messenger".to_string());
    }
    // a Forwarder message has no caller: the program's messages cannot be received this way
    if parsed.destination_caller != [0u8; 32] {
        return Err("a relayed message has no destination caller".to_string());
    }
    let hook = ForwarderHook::parse(view.hook()).ok_or("the hook is not a Forwarder hook")?;
    if read_state(|state| state.solana_forward_attempts(&arrival)) >= keeper::MAX_FORWARD_TRIES {
        return Err("the relays are spent".to_string());
    }
    let owner = match hook {
        ForwarderHook::CreateFor(owner) => owner,
        ForwarderHook::Existing => {
            let account = rpc::account(d.config.sol_rpc, view.mint_recipient(), Some(64), 1024)
                .await?
                .ok_or("the token account is gone")?;
            account.data.get(32..64).and_then(|o| o.try_into().ok()).ok_or("the token account does not read")?
        }
    };
    if *view.mint_recipient() != pda::ata(&owner, &USDC_MINT, &TOKEN_PROGRAM) {
        return Err("a Forwarder burn mints to its owner's USDC account".to_string());
    }
    screen(&[Party::Solana(owner)])?;
    if rpc::account(d.config.sol_rpc, &pda::used_nonce(&parsed.nonce), Some(0), 1024).await?.is_some() {
        step(SolanaStep::ArrivalFinished { arrival })?;
        return Ok(None);
    }
    prove_arrival_source(&parsed, &args.src_tx_hash, d.config.min_network_fee.forward_relay).await?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let _busy = Busy::take(QuoteHash::new(arrival)).ok_or("this arrival is being finished")?;
    let (_, source) = evm_source(&parsed)?;
    let source_token = word_of_usdc(&source);
    let tail = swapic_solana_wire::instructions::ArrivalSource {
        nonce: &parsed.nonce,
        source_domain: parsed.source_domain,
        source_token: &source_token,
    };
    let all = [
        instructions::create_ata_idempotent(&d.a.canister, &owner, &USDC_MINT, &TOKEN_PROGRAM),
        instructions::receive_message(&d.a.canister, view.mint_recipient(), tail, &args.message, &args.attestation),
    ];
    let compiled = compile_v1_from_payer(&d, &all, 300_000, &args.lifetime)?;
    sign_and_record(&d, SolanaPurpose::ForwardRelay { arrival }, compiled, args.lifetime.last_valid_block_height).await.map(Some)
}

/// Controller-only, a money step Jeff runs. A Token transfer from the fee account, signed by the admin
/// key, with the payer key as fee payer.
#[update]
pub async fn sweep_solana_fees(args: SweepSolanaFeesArgs) -> Result<u64, String> {
    controller()?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let d = deployment()?;
    let to = base58::decode_pubkey(&args.to).ok_or("`to` is not a Solana address")?;
    let ix = instructions::token_transfer(&d.a.fee_account, &to, &d.a.admin, args.amount);
    let compiled = compile_v1_from_payer(&d, &[ix], 20_000, &args.lifetime)?;
    sign_and_record(&d, SolanaPurpose::Sweep, compiled, args.lifetime.last_valid_block_height).await
}

/// Controller-only, immediate: the program's settings, signed by the admin key. They move no money.
#[update]
pub async fn set_solana_program_config(args: SolanaProgramConfigArgs) -> Result<u64, String> {
    controller()?;
    let d = deployment()?;
    let program = ProgramConfig {
        paused: args.paused,
        event_keypair_allowed: args.event_keypair_allowed,
        bump: d.a.config.1,
        swap_domains: args.swap_domains,
        ck_domains: args.ck_domains,
        min_burn: args.min_burn,
        min_deadline_s: args.min_deadline_s,
        max_deadline_s: args.max_deadline_s,
    };
    let compiled = compile_v1_from_payer(&d, &[instructions::set_config(&d.a, &program)], 30_000, &args.lifetime)?;
    sign_and_record(&d, SolanaPurpose::Config, compiled, args.lifetime.last_valid_block_height).await
}

/// Controller-only. Publishes a build: a line every reader sees, and the watcher raises an urgent alert.
#[update]
pub fn propose_solana_upgrade(args: ProposeSolanaUpgradeArgs) -> Result<u64, String> {
    controller()?;
    let d = deployment()?;
    let id = read_state(|state| state.solana_proposal_count());
    let now = now_s();
    let proposal = SolanaProposal {
        id,
        buffer: base58::decode_pubkey(&args.buffer).ok_or("`buffer` is not a Solana address")?,
        build_hash: args.build_hash.try_into().map_err(|_| "build_hash is 32 bytes")?,
        source_commit: args.source_commit,
        proposed_at_s: now,
        not_before_s: now + d.config.upgrade_delay_s,
        status: SolanaProposalStatus::Waiting,
    };
    step(SolanaStep::UpgradeProposed { proposal })?;
    Ok(id)
}

/// Controller-only, at any time before execution.
#[update]
pub fn cancel_solana_upgrade(id: u64) -> Result<(), String> {
    controller()?;
    step(SolanaStep::UpgradeCancelled { id }).map(|_| ())
}

/// Controller-only. Signs exactly one loader `Upgrade`, once the delay has passed and the buffer holds
/// the published bytes under the admin key's authority.
#[update]
pub async fn execute_solana_upgrade(id: u64, lifetime: SolanaLifetime) -> Result<u64, String> {
    controller()?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let d = deployment()?;
    let proposal = read_state(|state| state.solana_live_proposal())
        .filter(|live| live.id == id)
        .ok_or("this proposal is not the live one")?;
    let now = now_s();
    if now < proposal.not_before_s {
        return Err("the delay has not passed".to_string());
    }
    let buffer = rpc::account(d.config.sol_rpc, &proposal.buffer, None, 2 * 1024 * 1024).await?.ok_or("the buffer is gone")?;
    let program_data_key = pda::find_program_address(&[&d.a.program[..]], &swapic_solana_wire::consts::UPGRADEABLE_LOADER).0;
    let program_data = rpc::account(d.config.sol_rpc, &program_data_key, Some(0), 1024).await?.ok_or("the program data account is gone")?;
    upgrade::check_execution(proposal.not_before_s, now, &proposal.build_hash, &buffer.owner, &buffer.data, &d.a.admin, program_data.space)?;
    let ix = instructions::loader_upgrade(&d.a.program, &proposal.buffer, &d.a.canister, &d.a.admin);
    let compiled = compile_v1_from_payer(&d, &[ix], 200_000, &lifetime)?;
    let line = sign_and_record(&d, SolanaPurpose::Upgrade { proposal: id }, compiled, lifetime.last_valid_block_height).await?;
    step(SolanaStep::UpgradeExecuted { id })?;
    Ok(line)
}

