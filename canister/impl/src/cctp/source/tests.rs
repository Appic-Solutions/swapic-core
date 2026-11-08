//! On Circle's real attested message (`samples/cctp_base_to_arbitrum.json`, burn tx
//! 0x90cc...da0c) and the REAL source receipt that sent it
//! (`samples/cctp_base_to_arbitrum_source_receipt.json`: Base block 52,390,157, status 1, 17
//! logs, one `MessageSent`, read read-only for the final review round): Base's transmitter
//! emitted the message with the four fields the attestation service fills in blank. That
//! burn was no canister relay, so its source paid the canister no fee step: the tests that
//! need one add the Runner's fee transfer to the real logs.

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
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
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

/// The real source receipt of the sample's burn.
fn real() -> Receipt {
    let json: serde_json::Value = serde_json::from_str(include_str!(
        "../../../samples/cctp_base_to_arbitrum_source_receipt.json"
    ))
    .unwrap();
    Receipt::parse(&json).unwrap()
}

/// The real receipt with the Runner's fee step to the canister (`fee`) among its logs.
fn with_fee(mut receipt: Receipt, fee: u64) -> Receipt {
    let chain = base();
    receipt.logs.push(log(
        1_000,
        chain.usdc,
        vec![
            topics::transfer(),
            chain.runner.to_word(),
            CANISTER.to_word(),
        ],
        TokenAmount::from(fee).to_be_bytes().to_vec(),
    ));
    receipt
}

/// What Base's transmitter emitted in the real receipt.
fn sent_in(receipt: &Receipt) -> Vec<BurnMessage> {
    let chain = base();
    receipt
        .logs
        .iter()
        .filter(|log| {
            log.address == chain.message_transmitter
                && log.topics.first() == Some(&topics::message_sent())
        })
        .filter_map(|log| swapic::read_message_sent(&log.topics, &log.data))
        .map(|bytes| BurnMessage::parse(&bytes).unwrap())
        .collect()
}

/// The real source sent exactly the attested message with the four fields Circle fills in
/// blank: the derivation the earlier tests used equals what Base emitted.
#[test]
fn the_real_source_sent_the_attested_message_with_the_four_fields_blank() {
    let receipt = real();
    assert!(receipt.success);
    assert_eq!(receipt.block, BlockNumber::new(52_390_157));
    assert_eq!(receipt.logs.len(), 17);
    let message = attested();
    assert_eq!(sent_in(&receipt), vec![emitted(&message)]);
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

/// M-06, on the real receipt: a finish's source sent this message and paid the canister fee
/// steps worth the arrival's least network fee. The real burn, no canister relay, paid no fee
/// step; a fee step of one unit, a fee to someone else, another message, another emitter, or
/// a revert proves nothing either.
#[test]
fn a_source_proves_its_message_and_its_fee() {
    let message = attested();
    let chain = base();
    let proved = |receipt: &Receipt, message: &BurnMessage, canister: EvmAddress| {
        proves(receipt, message, &chain, canister, NEEDED).map(drop)
    };
    assert_eq!(
        proved(&with_fee(real(), 166_275), &message, CANISTER),
        Ok(())
    );
    assert_eq!(
        proved(&real(), &message, CANISTER),
        Err(FinishError::Source("paid the canister no fee step"))
    );
    assert_eq!(
        proved(&with_fee(real(), 1), &message, CANISTER),
        Err(FinishError::Source(
            "paid the canister less than the arrival's least network fee"
        ))
    );
    // a fee to someone else is no fee to the canister
    assert!(proved(
        &with_fee(real(), 166_275),
        &message,
        EvmAddress::new([0x01; 20])
    )
    .is_err());
    let mut other = message.clone();
    other.body.amount = TokenAmount::from(1_u32);
    assert_eq!(
        proved(&with_fee(real(), 166_275), &other, CANISTER),
        Err(FinishError::Source("did not send this message"))
    );
    let mut forged = with_fee(real(), 166_275);
    for log in forged.logs.iter_mut() {
        if log.address == chain.message_transmitter {
            log.address = EvmAddress::new([0x66; 20]);
        }
    }
    assert_eq!(
        proved(&forged, &message, CANISTER),
        Err(FinishError::Source("did not send this message"))
    );
    let mut reverted = with_fee(real(), 166_275);
    reverted.success = false;
    assert_eq!(
        proved(&reverted, &message, CANISTER),
        Err(FinishError::Source("reverted"))
    );
}

/// A receipt of `logs`, in order: an order's start (`Order`), a fee step to the canister
/// (`Fee`) or the burn of the real message (`Burn`).
#[derive(Clone, Copy)]
enum Step {
    Order,
    Fee(u64),
    Burn,
}

fn run_of(steps: &[Step]) -> Receipt {
    let chain = base();
    let burn = real()
        .logs
        .into_iter()
        .find(|log| {
            log.address == chain.message_transmitter
                && log.topics.first() == Some(&topics::message_sent())
        })
        .unwrap();
    let logs = steps
        .iter()
        .enumerate()
        .map(|(at, step)| {
            let index = at as u64 + 1;
            match step {
                Step::Order => log(index, chain.swapic, vec![topics::order_started()], vec![]),
                Step::Fee(fee) => log(
                    index,
                    chain.usdc,
                    vec![
                        topics::transfer(),
                        chain.runner.to_word(),
                        CANISTER.to_word(),
                    ],
                    TokenAmount::from(*fee).to_be_bytes().to_vec(),
                ),
                Step::Burn => Log {
                    index,
                    ..burn.clone()
                },
            }
        })
        .collect();
    Receipt { logs, ..real() }
}

/// Each order of one transaction is judged alone (audit C10): each paid order proves, however
/// its neighbours' fees differ; an order that burned more than it paid proves nothing and takes
/// no one's place; an unpaid burn beside a paid order blocks nothing and is not counted.
#[test]
fn each_order_of_a_transaction_is_judged_alone() {
    use Step::{Burn, Fee, Order};
    let needed = NEEDED.try_into_u128().unwrap() as u64;
    let message = attested();
    let chain = base();
    let proved = |steps: &[Step]| proves(&run_of(steps), &message, &chain, CANISTER, NEEDED);
    // two paid orders, one paying far more than the other: two arrivals
    assert_eq!(
        proved(&[Order, Fee(needed), Burn, Order, Fee(needed * 50), Burn]),
        Ok(2)
    );
    // an unpaid order's burn before a paid order's: the paid one proves, one arrival is paid for
    assert_eq!(proved(&[Order, Burn, Order, Fee(needed), Burn]), Ok(1));
    assert_eq!(proved(&[Order, Fee(needed), Burn, Order, Burn]), Ok(1));
    // one fee, two burns in one order: nothing is proven
    assert_eq!(
        proved(&[Order, Fee(needed), Burn, Burn]),
        Err(FinishError::Source(
            "paid the canister less than the arrival's least network fee"
        ))
    );
    // that order takes no slot from a neighbour that paid
    assert_eq!(
        proved(&[Order, Fee(needed), Burn, Burn, Order, Fee(needed), Burn]),
        Ok(1)
    );
    // an order paying for both of its burns
    assert_eq!(proved(&[Order, Fee(needed * 2), Burn, Burn]), Ok(2));
    // no order paid at all
    assert_eq!(
        proved(&[Order, Burn, Order, Burn]),
        Err(FinishError::Source("paid the canister no fee step"))
    );
}
