//! The gasless co-sign checker (design 5.2.4, 8b). The canister reads only the bytes it would sign:
//! the watcher is not trusted for any field. Every rule, in order; any miss signs nothing. The canister
//! does not parse the Jupiter route: the program bounds the result with `min_usdc`, refuses any Jupiter
//! instruction but the two routes, and the user is the route's authority.

#[cfg(test)]
mod tests;

use crate::relay::Party;
use swapic_solana_wire::args::{BurnArgs, Payload, RefSource};
use swapic_solana_wire::consts::COMPUTE_BUDGET_PROGRAM;
use swapic_solana_wire::message::{SignedV0, V0Message};
use swapic_solana_wire::pda::{keccak256, SwapicAddresses};
use types::config::{EvmChain, GasAction};
use types::solana::SolanaConfig;
use types::{EvmAddress, TokenAmount};

/// What the checker reads besides the bytes. `destination` answers a domain's chain.
pub struct Context<'a, F: Fn(u32) -> Option<EvmChain>> {
    pub a: &'a SwapicAddresses,
    pub config: &'a SolanaConfig,
    pub now_s: u64,
    pub destination: F,
    /// The pool chain's CCTP domain and the canister's address there: where a memo burn's deposit goes.
    pub pool: Option<(u32, EvmAddress)>,
}

/// A transaction that passed: what the door still has to do with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Passed {
    pub user: [u8; 32],
    pub r#ref: [u8; 32],
    pub message: Vec<u8>,
    pub message_hash: [u8; 32],
    pub signatures: Vec<[u8; 64]>,
    /// Who else must have signed: the user, and under the fallback the event keypair.
    pub signers: Vec<[u8; 32]>,
    pub parties: Vec<Party>,
}

const SET_COMPUTE_UNIT_LIMIT: u8 = 2;
const SET_COMPUTE_UNIT_PRICE: u8 = 3;
const SET_LOADED_DATA_LIMIT: u8 = 4;
const TWO_HOURS_S: u64 = 7_200;

fn le_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn le_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// Every rule that needs nothing but the bytes and the config: rules 2 to 7 and the parties of rule 8.
pub fn check<F: Fn(u32) -> Option<EvmChain>>(
    wire: &[u8],
    ctx: &Context<'_, F>,
) -> Result<Passed, String> {
    // 2. the format: v0, and nothing else
    let signed = SignedV0::parse(wire).map_err(|e| format!("not a signed transaction: {e:?}"))?;
    let message =
        V0Message::parse(signed.message).map_err(|e| format!("not a v0 message: {e:?}"))?;

    // 3. the signatures: the payer key first, then the user
    let signatures = message.header.required_signatures as usize;
    if signed.signatures.len() != signatures {
        return Err("the signature count is not the header's".to_string());
    }
    if signatures != 2 && signatures != 3 {
        return Err(format!("{signatures} signatures"));
    }
    if message.static_keys[0] != ctx.a.canister {
        return Err("the fee payer is not the canister's payer key".to_string());
    }
    let user = message.static_keys[1];
    if user == ctx.a.admin {
        return Err("the admin key is not a user".to_string());
    }

    // 4. the instructions: exactly these
    let (mut cu_limit, mut cu_price, mut loaded, mut burn) = (None, None, None, None);
    for ix in &message.instructions {
        let program = message.static_keys[ix.program_index as usize];
        if program == COMPUTE_BUDGET_PROGRAM {
            if !ix.accounts.is_empty() {
                return Err("a compute budget instruction names accounts".to_string());
            }
            match ix.data.first().copied() {
                Some(SET_COMPUTE_UNIT_LIMIT) if cu_limit.is_none() => {
                    cu_limit = Some(
                        ix.data
                            .get(1..)
                            .and_then(le_u32)
                            .ok_or("a bad compute limit")?,
                    );
                }
                Some(SET_COMPUTE_UNIT_PRICE) if cu_price.is_none() => {
                    cu_price = Some(
                        ix.data
                            .get(1..)
                            .and_then(le_u64)
                            .ok_or("a bad compute price")?,
                    );
                }
                Some(SET_LOADED_DATA_LIMIT) if loaded.is_none() => {
                    loaded = Some(
                        ix.data
                            .get(1..)
                            .and_then(le_u32)
                            .ok_or("a bad data limit")?,
                    );
                }
                _ => {
                    return Err("a compute budget instruction this door does not allow".to_string())
                }
            }
        } else if program == ctx.a.program {
            if burn.is_some() {
                return Err("two Swapic instructions".to_string());
            }
            burn = Some(ix);
        } else {
            return Err("an instruction of a program this door does not allow".to_string());
        }
    }
    let burn = burn.ok_or("no Swapic instruction")?;
    let cu_limit = cu_limit.ok_or("no compute limit")?;
    let cu_price = cu_price.ok_or("no compute price")?;
    if cu_limit > ctx.config.max_cu_limit {
        return Err(format!(
            "the compute limit {cu_limit} is above {}",
            ctx.config.max_cu_limit
        ));
    }
    if cu_price > ctx.config.max_cu_price {
        return Err(format!(
            "the compute price {cu_price} is above {}",
            ctx.config.max_cu_price
        ));
    }

    // 5. the accounts: the payer key is in its one slot, and in no other slot of any instruction
    if burn.accounts.first() != Some(&1) {
        return Err("the Swapic instruction's user is not the second signer".to_string());
    }
    if burn.accounts.get(2) != Some(&0) {
        return Err("the Swapic instruction's canister slot is not the payer key".to_string());
    }
    for ix in &message.instructions {
        for (slot, index) in ix.accounts.iter().enumerate() {
            if *index == 0 && !(std::ptr::eq(ix, burn) && slot == 2) {
                return Err("the payer key is named outside its slot".to_string());
            }
        }
    }

    // 6. the arguments
    let args = BurnArgs::decode(&burn.data).map_err(|e| format!("the Swapic arguments: {e:?}"))?;
    if !args.float {
        return Err("a co-signed burn spends the float".to_string());
    }
    if args.event_keypair != (signatures == 3) {
        return Err(
            "the third signature is the event keypair's, and only under its flag".to_string(),
        );
    }
    if args.event_keypair && !ctx.config.program_event_keypair_allowed {
        return Err("the event keypair fallback is off".to_string());
    }
    let mut signers = vec![user];
    if args.event_keypair {
        let event_index = *burn.accounts.get(7).ok_or("no event account")?;
        if event_index != 2 {
            return Err("the third signer is not the event account".to_string());
        }
        signers.push(message.static_keys[2]);
    }

    let destination = (ctx.destination)(args.domain as u32)
        .ok_or_else(|| format!("domain {} is not a chain Swapic has", args.domain))?;
    let recipient = EvmAddress::new(*args.recipient);
    if recipient == EvmAddress::ZERO
        || [
            destination.swapic,
            destination.runner,
            destination.cctp_receiver,
        ]
        .contains(&recipient)
    {
        return Err("the recipient is zero or one of Swapic's contracts".to_string());
    }
    let mut parties = vec![Party::Solana(user), Party::Evm(recipient)];
    let r#ref = match args.ref_source {
        RefSource::Direct(r) => *r,
        RefSource::Memo(memo) => {
            if ctx.pool != Some((args.domain as u32, recipient)) {
                return Err(
                    "a memo burn's destination is not the canister on the pool chain".to_string(),
                );
            }
            let terms = types::pool::memo::decode(memo).map_err(|e| format!("the memo: {e}"))?;
            // staking from Solana is not offered: a memo burn is a swap into ICP
            let types::pool::DepositKind::Swap { recipient, .. } = terms.kind else {
                return Err("a memo burn out of Solana is a swap".to_string());
            };
            parties.push(Party::Icp(recipient.owner));
            keccak256(memo)
        }
    };
    let gas_action = match args.payload {
        Payload::Deliver => GasAction::RelayDeliver,
        Payload::Swap { token_out, .. } => {
            if EvmAddress::new(*token_out) == destination.usdc {
                return Err("a Swap's tokenOut is the arriving USDC".to_string());
            }
            GasAction::CompleteSwap
        }
        Payload::CkDeposit { principal, .. } => {
            parties.push(Party::Icp(
                types::swapic::ck_principal(*principal)
                    .ok_or("a CkDeposit's principal does not read")?,
            ));
            GasAction::CompleteCk
        }
    };
    // a deadline at the edge cannot pass the canister and then fail on chain (attack M1)
    let earliest = ctx.now_s + ctx.config.program_min_deadline_s + ctx.config.deadline_margin_s;
    let latest = ctx.now_s + TWO_HOURS_S;
    if (args.deadline as u64) < earliest || (args.deadline as u64) > latest {
        return Err(format!(
            "the deadline {} is outside {earliest} to {latest}",
            args.deadline
        ));
    }
    if args.amount < ctx.config.gasless_min_usdc {
        return Err(format!(
            "a gasless burn must produce at least {} micro USDC",
            ctx.config.gasless_min_usdc
        ));
    }

    // 7. the fee
    let row = destination
        .min_network_fee
        .get(gas_action)
        .unwrap_or(TokenAmount::MAX);
    let needed = ctx
        .config
        .min_network_fee
        .gasless_burn
        .checked_add(row)
        .unwrap_or(TokenAmount::MAX);
    if TokenAmount::from(args.fee as u64) < needed {
        return Err(format!("the fee {} is below {needed:?}", args.fee));
    }
    let burned_at_least = args.amount.saturating_sub(args.fee as u64);
    if burned_at_least <= ctx.config.program_min_burn {
        return Err("what is left after the fee is not above the program's least burn".to_string());
    }
    if args.max_fee_deci_bps > ctx.config.max_fee_deci_bps {
        return Err(format!(
            "the max fee {} is above {}",
            args.max_fee_deci_bps, ctx.config.max_fee_deci_bps
        ));
    }
    let priority = (cu_limit as u128 * cu_price as u128).div_ceil(1_000_000);
    if priority > ctx.config.max_priority_lamports as u128 {
        return Err(format!(
            "the priority fee {priority} is above {}",
            ctx.config.max_priority_lamports
        ));
    }

    Ok(Passed {
        user,
        r#ref,
        message: signed.message.to_vec(),
        message_hash: super::sha256(signed.message),
        signatures: signed.signatures.clone(),
        signers,
        parties,
    })
}

/// Verifies a signer's Ed25519 signature over the message (rule 9): an invalid one counts for nothing.
pub fn verify(signer: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(signer) else {
        return false;
    };
    key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
        .is_ok()
}

/// Whether the limits let this signer through (rule 9), keyed on the verified signer.
pub fn within_limits(
    config: &SolanaConfig,
    signer: (bool, bool, u32),
    global: (u32, u32),
) -> Result<(), &'static str> {
    let (blocked, succeeded, count) = signer;
    let (total, failures) = global;
    if blocked {
        return Err("this signer's co-signed transaction landed and failed");
    }
    if count >= config.cosign_per_signer_per_hour {
        return Err("this signer's co-signs this hour are spent");
    }
    if total >= config.cosign_global_per_hour {
        return Err("the co-signs this hour are spent");
    }
    if failures >= config.cosign_failures_per_hour && !succeeded {
        return Err("the failure budget is spent: only a signer with a landed swap is served");
    }
    Ok(())
}

/// The first co-signature of a ref stands: the same message answers it, and another is refused until
/// 90 s after it (rule 10, one actor per swap).
pub fn second_request(
    existing: ([u8; 32], u64, u64),
    message_hash: &[u8; 32],
    now_s: u64,
) -> Result<Option<u64>, &'static str> {
    let (hash, line, at_s) = existing;
    if hash == *message_hash {
        return Ok(Some(line));
    }
    if now_s < at_s + 90 {
        return Err("this ref was co-signed with another message under 90 s ago");
    }
    Ok(None)
}
