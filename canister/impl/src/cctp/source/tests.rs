//! On Circle's real attested message (`samples/cctp_base_to_arbitrum.json`, burn tx
//! 0x90cc...da0c) and the source receipt that sent it, as Base's transmitter emits a message:
//! the four fields the attestation service fills in blank. No real source receipt of that burn
//! was captured, so its `MessageSent` is the attested message with those four fields zeroed,
//! which is what CCTP v2 emits.

use super::*;
use crate::pool::arrival::Log;
use types::{BlockNumber, Wei};

fn attested() -> BurnMessage {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("../../../samples/cctp_base_to_arbitrum.json")).unwrap();
    let message = hex::decode(
        json["iris"]["message"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    BurnMessage::parse(&message).unwrap()
}

/// `message` as the source chain's transmitter emitted it.
fn emitted(message: &BurnMessage) -> BurnMessage {
    let mut emitted = message.clone();
    emitted.nonce = [0; 32];
    emitted.finality_threshold_executed = 0;
    emitted.body.fee_executed = TokenAmount::ZERO;
    emitted.body.expiration_block = BlockNumber::new(0);
    emitted
}

const CANISTER: EvmAddress = EvmAddress::new([0xca; 20]);

/// The destination's least network fee for the arrival charged for.
const NEEDED: TokenAmount = TokenAmount::new(60_000);

/// Base as the sample's burn left it: its USDC, its transmitter, and the burn's own sender as
/// the Runner.
fn base() -> EvmChain {
    let message = attested();
    let word = |word: [u8; 32]| swapic::evm_of_word(word).unwrap();
    EvmChain {
        swapic: EvmAddress::new([0x51; 20]),
        runner: word(message.body.message_sender),
        cctp_receiver: EvmAddress::new([0x52; 20]),
        usdc: word(message.body.burn_token),
        token_messenger: word(message.sender),
        message_transmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"
            .parse()
            .unwrap(),
        gas_limit: types::config::PerAction::all(types::GasAmount::from(300_000_u64)),
        gas_budget: types::config::PerAction::all(Wei::ZERO),
        min_network_fee: types::config::PerAction::all(TokenAmount::ZERO),
    }
}

fn log(index: u64, address: EvmAddress, topics: Vec<[u8; 32]>, data: Vec<u8>) -> Log {
    Log {
        index,
        address,
        topics,
        data,
        block_time: None,
    }
}

/// The source receipt: the Runner's fee step to the canister (`fee`, none for zero), then
/// the transmitter's `MessageSent` of `sent`.
fn receipt(sent: &BurnMessage, fee: u64) -> Receipt {
    use alloy_sol_types::SolEvent;
    let chain = base();
    let mut logs = vec![];
    if fee > 0 {
        logs.push(log(
            1,
            chain.usdc,
            vec![
                topics::transfer(),
                chain.runner.to_word(),
                CANISTER.to_word(),
            ],
            TokenAmount::from(fee).to_be_bytes().to_vec(),
        ));
    }
    let data = swapic::MessageSent {
        message: sent.encode().into(),
    }
    .encode_data();
    logs.push(log(
        4,
        chain.message_transmitter,
        vec![topics::message_sent()],
        data,
    ));
    Receipt {
        tx: TxHash::new([0x90; 32]),
        block: BlockNumber::new(52_390_157),
        success: true,
        logs,
    }
}

/// The attested message is the emitted one but for the four fields Circle fills in.
#[test]
fn the_attested_message_is_the_one_the_source_sent() {
    let message = attested();
    assert_ne!(message.nonce, [0; 32], "the sample's nonce is filled in");
    assert!(message.attests(&emitted(&message)));
    let mut other = emitted(&message);
    other.body.amount = TokenAmount::from(1_u32);
    assert!(!message.attests(&other));
    let mut other = emitted(&message);
    other.body.hook_data.push(0);
    assert!(!message.attests(&other));
    let mut other = emitted(&message);
    other.destination_caller = [7; 32];
    assert!(!message.attests(&other));
}

/// M-06: a finish's source sent this message and paid the canister fee steps worth the
/// arrival's least network fee; a source with no fee step or less, another message, another
/// emitter, or a revert proves nothing.
#[test]
fn a_source_proves_its_message_and_its_fee() {
    let message = attested();
    let chain = base();
    assert_eq!(
        proves(
            &receipt(&emitted(&message), 166_275),
            &message,
            &chain,
            CANISTER,
            NEEDED
        ),
        Ok(())
    );
    assert_eq!(
        proves(
            &receipt(&emitted(&message), 0),
            &message,
            &chain,
            CANISTER,
            NEEDED
        ),
        Err(FinishError::Source("paid the canister no fee step"))
    );
    let mut other = emitted(&message);
    other.body.amount = TokenAmount::from(1_u32);
    assert_eq!(
        proves(
            &receipt(&other, 166_275),
            &message,
            &chain,
            CANISTER,
            NEEDED
        ),
        Err(FinishError::Source("did not send this message"))
    );
    let mut forged = receipt(&emitted(&message), 166_275);
    forged.logs[1].address = EvmAddress::new([0x66; 20]);
    assert_eq!(
        proves(&forged, &message, &chain, CANISTER, NEEDED),
        Err(FinishError::Source("did not send this message"))
    );
    // a fee to someone else is no fee to the canister
    assert!(proves(
        &receipt(&emitted(&message), 166_275),
        &message,
        &chain,
        EvmAddress::new([0x01; 20]),
        NEEDED
    )
    .is_err());
    // a fee step of one unit is no fee for the arrival (M-06's own story)
    assert_eq!(
        proves(
            &receipt(&emitted(&message), 1),
            &message,
            &chain,
            CANISTER,
            NEEDED
        ),
        Err(FinishError::Source(
            "paid the canister less than the arrival's least network fee"
        ))
    );
    let mut reverted = receipt(&emitted(&message), 166_275);
    reverted.success = false;
    assert_eq!(
        proves(&reverted, &message, &chain, CANISTER, NEEDED),
        Err(FinishError::Source("reverted"))
    );
}
