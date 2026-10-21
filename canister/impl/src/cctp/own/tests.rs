//! Review Focus 5: a canister-paid CCTP transfer is finished only when its source
//! transaction is the canister's own for that move or payout, and the message is the burn of
//! the order it pinned. The order is a REAL one, the staging canary's Ethereum to Base CCTP
//! fast order (2026-10-19, `pool/rebalance/samples/eth_cctp_to_base.hex`), its fee step
//! dropped and its payer the canister, as a move's own order is.

use super::*;
use alloy_sol_types::SolCall;
use types::swapic::depositForBurnWithHookCall;
use crate::state::MemoryStore;
use types::cctp::BurnBody;
use types::evm::EvmAddress;
use types::pool::payout::PinnedOrder;
use types::pool::{Move, MoveDirection, MoveStatus, PoolLedger, RebalanceLines};
use types::{BlockNumber, Timestamp, TokenAmount};

fn canister_principal() -> Principal {
    Principal::from_slice(&[0x5c; 10])
}

fn canister() -> EvmAddress {
    "0x36D34038F04990a58c068e08eD9754e2E3D9bA53".parse().unwrap()
}

/// The canary's order, the canister's: its fee step dropped, its payer the canister.
fn order() -> swapic::Order {
    let mut order = swapic::decode_order(
        &hex::decode(include_str!("../../pool/rebalance/samples/eth_cctp_to_base.hex").trim())
            .unwrap(),
    )
    .unwrap();
    order.steps.remove(0);
    order.payer = swapic::addr(canister());
    order
}

/// The attested message of `order`'s burn, as Circle would deliver it.
fn message_of(order: &swapic::Order) -> BurnMessage {
    let call = depositForBurnWithHookCall::abi_decode(&order.steps[0].data).unwrap();
    BurnMessage {
        version: 1,
        source_domain: 0,
        destination_domain: call.destinationDomain,
        nonce: [0x31; 32],
        sender: [0x32; 32],
        recipient: [0x33; 32],
        destination_caller: call.destinationCaller.0,
        min_finality_threshold: call.minFinalityThreshold,
        finality_threshold_executed: 1_000,
        body: BurnBody {
            version: 1,
            burn_token: call.burnToken.into_word().0,
            mint_recipient: call.mintRecipient.0,
            amount: TokenAmount::from_be_bytes(order.amountIn.to_be_bytes::<32>()),
            message_sender: [0x34; 32],
            max_fee: TokenAmount::from_be_bytes(call.maxFee.to_be_bytes::<32>()),
            fee_executed: TokenAmount::from(400_u64),
            expiration_block: BlockNumber::new(0),
            hook_data: call.hookData.to_vec(),
        },
    }
}

fn plan_of(message: &BurnMessage) -> Plan {
    let envelope = Envelope::decode(&message.body.hook_data).unwrap();
    Plan {
        action: FinishAction::Receive,
        gas_action: GasAction::RelayDeliver,
        to: EvmAddress::new([0x35; 20]),
        mint_recipient: envelope.recipient,
        envelope,
    }
}

const SOURCE: [u8; 32] = [0x41; 32];

/// A fold holding move 4, burned, its order pinned, its source transaction indexed.
fn state_with_move(order: &swapic::Order, indexed: bool) -> State<MemoryStore> {
    let mut store = MemoryStore::default();
    let mut item = Move::opened(
        4,
        MoveDirection::IcpToBase,
        TokenAmount::from_be_bytes(order.amountIn.to_be_bytes::<32>()),
        false,
        Timestamp::from_nanos(1),
        None,
        TokenAmount::ZERO,
    );
    item.status = MoveStatus::Burned;
    item.order = Some(PinnedOrder {
        order_id: [0x42; 32],
        order: swapic::encode_order(order),
        gas_limit: 300_000,
        deadline_s: 1_800_000_000,
    });
    store.put_pool_move(4, item);
    store.put_pool_ledger(PoolLedger {
        rebalance: Some(RebalanceLines {
            open: [4].into_iter().collect(),
            ..RebalanceLines::default()
        }),
        ..PoolLedger::default()
    });
    if indexed {
        store.put_pool_own_source(TxHash::new(SOURCE), OwnSource::Move(4));
    }
    State::new(store)
}

#[test]
fn a_moves_own_burn_is_finished_under_its_own_gas_action() {
    let order = order();
    let message = message_of(&order);
    let state = state_with_move(&order, true);
    assert_eq!(
        finish_action(
            &state,
            TxHash::new(SOURCE),
            &message,
            &plan_of(&message),
            canister_principal()
        ),
        Ok(GasAction::PoolMoveFinish)
    );
    // a release past the deadline keeps the release's own row
    let release = Plan {
        action: FinishAction::Release,
        gas_action: GasAction::Release,
        ..plan_of(&message)
    };
    assert_eq!(
        finish_action(
            &state,
            TxHash::new(SOURCE),
            &message,
            &release,
            canister_principal()
        ),
        Ok(GasAction::Release)
    );
}

/// A stranger's burn under a move's ref (any source transaction but the canister's own) is
/// never finished as the canister's, nor a ck deposit for the canister's own principal and
/// rebalance subaccount.
#[test]
fn a_strangers_burn_naming_a_move_is_refused() {
    let mut order = order();
    // the stranger's burn names the open move's ref
    let mut call = depositForBurnWithHookCall::abi_decode(&order.steps[0].data).unwrap();
    let mut envelope = Envelope::decode(&call.hookData).unwrap();
    envelope.reference = move_ref(4).into_bytes();
    call.hookData = envelope.encode().into();
    order.steps[0].data = call.abi_encode().into();
    let message = message_of(&order);
    let state = state_with_move(&order, false);
    assert!(matches!(
        finish_action(
            &state,
            TxHash::new([0x43; 32]),
            &message,
            &plan_of(&message),
            canister_principal()
        ),
        Err(FinishError::Shape(why)) if why.contains("canister's own transaction")
    ));
    // a ck deposit for the canister's own principal and rebalance subaccount, likewise
    let mut deposit = plan_of(&message);
    deposit.envelope.reference = [0x44; 32];
    deposit.envelope.action = Action::CkDeposit;
    deposit.envelope.principal = ck_word(&canister_principal());
    deposit.envelope.subaccount = rebalance_subaccount();
    assert!(finish_action(
        &state,
        TxHash::new([0x43; 32]),
        &message,
        &deposit,
        canister_principal()
    )
    .is_err());
    // any other transfer keeps its own action: the watcher proved its fee
    let mut other = plan_of(&message);
    other.envelope.reference = [0x45; 32];
    assert_eq!(
        finish_action(
            &state,
            TxHash::new([0x43; 32]),
            &message,
            &other,
            canister_principal()
        ),
        Ok(GasAction::RelayDeliver)
    );
}

/// From the canister's own source, a message that is not exactly the pinned order's burn
/// is refused: another recipient, more than the order spent.
#[test]
fn a_message_that_is_not_the_pinned_orders_burn_is_refused() {
    let order = order();
    let state = state_with_move(&order, true);
    let mut redirected = message_of(&order);
    redirected.body.mint_recipient = [0x46; 32];
    assert!(finish_action(
        &state,
        TxHash::new(SOURCE),
        &redirected,
        &plan_of(&redirected),
        canister_principal()
    )
    .is_err());
    let mut more = message_of(&order);
    more.body.amount = more.body.amount.checked_add(TokenAmount::ONE).unwrap();
    assert!(!burn_of(&swapic::encode_order(&order), &more));
    assert!(burn_of(&swapic::encode_order(&order), &message_of(&order)));
}

#[test]
fn a_payouts_own_burn_keeps_the_arrivals_gas_action() {
    use types::pool::payout::{EvmPayout, EvmWithdrawal, PayoutKind, PayoutTerms, Undo};
    use types::pool::{PoolAccount, Shares};
    let order = order();
    let message = message_of(&order);
    let mut store = MemoryStore::default();
    let staker = EvmAddress::new([0x47; 20]);
    let mut payout = EvmPayout::new(
        9,
        PayoutKind::Withdrawal(EvmWithdrawal {
            staker,
            usdc: TokenAmount::from(10_u64),
            max_shares: Shares::new(1),
            rebalance_share: TokenAmount::ZERO,
            network_fee: TokenAmount::ZERO,
            nonce: 1,
            burned: Shares::new(1),
        }),
        PayoutTerms {
            dst_chain: types::ChainId::BASE,
            recipient: staker,
            token_out: staker,
            min_out: TokenAmount::ONE,
            deadline: Timestamp::from_nanos(9),
        },
        Undo::Remint {
            owner: PoolAccount::Evm(staker),
            shares: Shares::new(1),
        },
        Timestamp::from_nanos(1),
    );
    payout.order = Some(PinnedOrder {
        order_id: [0x48; 32],
        order: swapic::encode_order(&order),
        gas_limit: 300_000,
        deadline_s: 1_800_000_000,
    });
    store.put_pool_payout(9, payout);
    store.put_pool_own_source(TxHash::new(SOURCE), OwnSource::Payout(9));
    let state = State::new(store);
    assert_eq!(
        finish_action(
            &state,
            TxHash::new(SOURCE),
            &message,
            &plan_of(&message),
            canister_principal()
        ),
        Ok(GasAction::RelayDeliver)
    );
}
