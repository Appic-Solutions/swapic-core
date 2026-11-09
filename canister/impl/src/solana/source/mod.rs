//! Domain 5 as a CCTP source (design 5.2.5, 8d): what a Solana transaction shows, read by the canister
//! itself before it finishes a burn out of Solana on an EVM chain. The fee is read from the program's
//! own instruction, and CCTP's `DepositForBurn` event in that instruction must match the attested
//! message field by field, so one paid transaction never stands as proof for another message with the
//! same ref (attack L4).

#[cfg(test)]
mod tests;

use super::rpc::LandedTx;
use swapic_solana_wire::args::{BurnArgs, RefSource};
use swapic_solana_wire::cctp::{word_to_u64, DepositForBurnEvent, Message};
use swapic_solana_wire::consts::USDC_MINT;
use swapic_solana_wire::message::{SignedV0, V0Message};
use swapic_solana_wire::pda::{keccak256, SwapicAddresses};
use types::TokenAmount;

/// What a proved transaction gives the finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proved {
    pub r#ref: [u8; 32],
    pub fee: u64,
    pub float: bool,
}

/// The transaction's key for the fold: the EVM path keys a source by a 32-byte hash.
pub fn source_key(signature: &[u8; 64]) -> types::TxHash {
    let mut preimage = b"swapic-solana-source".to_vec();
    preimage.extend_from_slice(signature);
    types::TxHash::new(keccak256(&preimage))
}

/// Whether `tx` is a source this attested message may be finished from, pure. `needed` is the least
/// fee of the arrival; `gasless` what a co-signed burn adds to it.
pub fn proves(
    tx: &LandedTx,
    message: &[u8],
    a: &SwapicAddresses,
    needed: TokenAmount,
    gasless: TokenAmount,
) -> Result<Proved, &'static str> {
    if tx.failed {
        return Err("the Solana transaction failed");
    }
    let attested = Message::new(message).map_err(|_| "the message is too short")?;
    let signed = SignedV0::parse(&tx.wire).map_err(|_| "the Solana transaction does not read")?;
    let parsed =
        V0Message::parse(signed.message).map_err(|_| "the Solana transaction is not v0")?;
    let envelope = attested.hook();

    for (position, ix) in parsed.instructions.iter().enumerate() {
        if parsed.static_keys[ix.program_index as usize] != a.program {
            continue;
        }
        let Ok(args) = BurnArgs::decode(&ix.data) else {
            continue;
        };
        let r#ref = match args.ref_source {
            RefSource::Direct(r) => *r,
            RefSource::Memo(memo) => keccak256(memo),
        };
        if args.envelope(&r#ref).encode().as_slice() != envelope {
            continue;
        }
        // the event CCTP wrote inside this very instruction
        let inner = tx
            .inner
            .iter()
            .find(|(index, _)| *index as usize == position)
            .ok_or("the program's instruction has no inner instructions")?;
        let matched = inner.1.iter().any(|(_, data)| {
            DepositForBurnEvent::parse(data)
                .is_some_and(|event| event_matches(&event, &attested, a))
        });
        if !matched {
            return Err("CCTP's burn event does not match the attested message");
        }
        let mut owed = needed;
        if args.float {
            owed = owed.checked_add(gasless).unwrap_or(TokenAmount::MAX);
        }
        if TokenAmount::from(args.fee as u64) < owed {
            return Err("the burn paid less than the arrival's least network fee");
        }
        return Ok(Proved {
            r#ref,
            fee: args.fee as u64,
            float: args.float,
        });
    }
    Err("the program did not burn this message in the transaction")
}

/// Every field the burn determined is the same in CCTP's own event and in the attested message.
fn event_matches(
    event: &DepositForBurnEvent<'_>,
    message: &Message<'_>,
    a: &SwapicAddresses,
) -> bool {
    event.burn_token == &USDC_MINT
        && event.burn_token == message.burn_token()
        && a.auth_index(event.depositor).is_some()
        && event.depositor == message.message_sender()
        && event.mint_recipient == message.mint_recipient()
        && event.destination_domain == message.destination_domain()
        && event.destination_token_messenger == message.recipient()
        && event.destination_caller == message.destination_caller()
        && Some(event.amount) == word_to_u64(message.amount_word())
        && Some(event.max_fee) == word_to_u64(message.max_fee_word())
        && event.hook == message.hook()
}
