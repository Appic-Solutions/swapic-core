//! The watcher's nudge on a delivering Across payout: one read, at once; filled is paid, and
//! anything else notes only the read's time.

use super::*;
use crate::agreed::Header;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{across_cap, at, sign_payout_with, waiting_withdrawal};
use crate::storage::pool::{ledger, payout, record_at};
use crate::test_util::block_on;
use std::cell::RefCell;
use types::eco::Fill;
use types::pool::AcrossOut;
use types::{BlockNumber, TokenAmount};

/// Its deposit's fill deadline, unix seconds.
const DEADLINE: u64 = 1_500;

/// A chain read that answers one Across fill and counts its reads.
struct FillReads {
    across: Result<AcrossFill, FillRead>,
    reads: RefCell<Vec<(ChainId, u64)>>,
}

impl FillReads {
    fn answering(across: Result<AcrossFill, FillRead>) -> Self {
        Self {
            across,
            reads: RefCell::new(Vec::new()),
        }
    }
}

impl ChainReads for FillReads {
    async fn ran(&self, _order_id: [u8; 32], _after: BlockNumber) -> Result<bool, String> {
        Err("not read here".to_string())
    }

    async fn fill_after(
        &self,
        _chain: ChainId,
        _intent: [u8; 32],
        _after_s: u64,
    ) -> Result<(Fill, BlockNumber), FillRead> {
        Err(FillRead::Unread("not read here".to_string()))
    }

    async fn across_fill_after(
        &self,
        chain: ChainId,
        _across: &AcrossOut,
        after_s: u64,
    ) -> Result<(AcrossFill, Header), FillRead> {
        self.reads.borrow_mut().push((chain, after_s));
        // an agreed block long before the deadline: the fill landed in seconds
        self.across.clone().map(|fill| {
            (
                fill,
                Header {
                    number: BlockNumber::new(77),
                    timestamp: DEADLINE - 800,
                },
            )
        })
    }

    async fn fill_logged(
        &self,
        _chain: ChainId,
        _across: &AcrossOut,
        _header: Header,
    ) -> Result<Option<bool>, String> {
        Err("not read here".to_string())
    }
}

/// A withdrawal of 100 USDC whose order deposited on Across to BNB Chain, delivering.
fn delivering() -> u64 {
    use alloy_primitives::{Address, U256};
    let id = waiting_withdrawal(
        "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap(),
    );
    across_cap();
    let mut data = types::swapic::across::SWAP_AND_BRIDGE.to_vec();
    data.extend_from_slice(&[0x6a; 32]);
    let order = types::swapic::encode_order(&types::swapic::Order {
        r#ref: [0x6a; 32].into(),
        payer: Address::repeat_byte(0x8f),
        tokenIn: Address::repeat_byte(0x83),
        amountIn: U256::from(1u8),
        deadline: U256::from(900u64),
        steps: vec![types::swapic::Step {
            target: Address::repeat_byte(0x97),
            data: data.into(),
            token: Address::repeat_byte(0x83),
            min: U256::from(1u8),
            writes: Vec::new(),
        }],
        payout: types::swapic::Payout {
            token: Address::ZERO,
            recipient: Address::ZERO,
            minOut: U256::ZERO,
        },
        memo: Vec::new().into(),
    });
    sign_payout_with(id, 0, 9, order);
    let amount = payout(id).unwrap().amount.unwrap();
    record_at(
        PoolStep::PayoutPaid {
            id,
            tx_hash: types::TxHash::new([9; 32]),
            block: BlockNumber::new(30),
            intent: None,
            across: Some(Box::new(AcrossOut {
                destination: ChainId::BSC,
                relay: types::swapic::across::RelayData {
                    depositor: [0x8f; 32],
                    recipient: [0x0f; 32],
                    exclusive_relayer: [0; 32],
                    input_token: [0x83; 32],
                    output_token: [0x8a; 32],
                    input_amount: amount,
                    output_amount: amount,
                    origin_chain_id: ChainId::BASE,
                    deposit_id: TokenAmount::from(7_u8),
                    fill_deadline: DEADLINE as u32,
                    exclusivity_deadline: 0,
                    message: Vec::new(),
                },
                deposit_time: 300,
            })),
        },
        at(10),
    )
    .unwrap();
    assert_eq!(payout(id).unwrap().status, PayoutStatus::Delivering);
    id
}

fn secs(s: u64) -> Timestamp {
    Timestamp::from_secs(s).unwrap()
}

/// The gate: a payout with no Across deposit is refused; one no longer delivering is not read;
/// one read early within one fill wait is not read again until the wait passed.
#[test]
fn a_nudge_reads_a_delivering_across_payout_at_most_once_a_wait() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = delivering();
        let mut payout = payout(id).unwrap();
        assert_eq!(
            gate(&payout, 700, 76),
            Ok(Gate::Read {
                chain: ChainId::BSC,
                after_s: DEADLINE
            })
        );
        payout.fill_early_at = Some(secs(700));
        assert_eq!(gate(&payout, 775, 76), Ok(Gate::TooSoon { again_at_s: 776 }));
        assert!(matches!(gate(&payout, 776, 76), Ok(Gate::Read { .. })));
        payout.status = PayoutStatus::Paid;
        assert_eq!(gate(&payout, 900, 76), Ok(Gate::NotDelivering));
        payout.across = None;
        assert!(matches!(gate(&payout, 900, 76), Err(PoolError::NotAllowed(_))));
    });
}

/// Jeff's first live test: the fill landed in seconds and the payout showed in progress until
/// its deadline. Nudged, a fill read filled long before the deadline is paid at once, exactly
/// as the queue pays it; a second nudge reads nothing.
#[test]
fn a_nudge_finding_the_fill_before_the_deadline_pays_the_payout() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = delivering();
        let reads = FillReads::answering(Ok(AcrossFill::Filled));
        let answer = block_on(check(&reads, id, || secs(700)));
        assert_eq!(answer, Ok(AcrossFillCheck::Paid));
        assert_eq!(*reads.reads.borrow(), vec![(ChainId::BSC, DEADLINE)]);
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Paid);
        assert_eq!(ledger().pockets.base.in_flight, TokenAmount::ZERO);
        assert!(!ledger().lines().across_owed.contains(&id));
        assert_eq!(
            block_on(check(&reads, id, || secs(701))),
            Ok(AcrossFillCheck::NotDelivering)
        );
        assert_eq!(reads.reads.borrow().len(), 1);
    });
}

/// Nudged while the deposit is open (or no agreed block is past the read), nothing changes but
/// the read's time: it stays delivering, its USDC in flight; a second nudge within one wait
/// reads nothing; an outcall that failed notes nothing and answers ask again.
#[test]
fn a_nudge_finding_no_fill_changes_nothing_but_the_reads_time() {
    for read in [Ok(AcrossFill::Open), Err(FillRead::TooEarly)] {
        on_fresh_memory(move || {
            crate::storage::init();
            let id = delivering();
            let before = ledger();
            let wait = crate::agreed::fill_wait_on(ChainId::BSC).as_secs();
            let reads = FillReads::answering(read);
            let answer = block_on(check(&reads, id, || secs(700)));
            assert_eq!(
                answer,
                Ok(AcrossFillCheck::NotYet {
                    again_at_s: 700 + wait
                })
            );
            let after = payout(id).unwrap();
            assert_eq!(after.status, PayoutStatus::Delivering);
            assert_eq!(after.fill_early_at, Some(secs(700)));
            assert_eq!(ledger(), before);
            // within one wait: not read again
            assert_eq!(
                block_on(check(&reads, id, || secs(700 + wait - 1))),
                Ok(AcrossFillCheck::TooSoon {
                    again_at_s: 700 + wait
                })
            );
            assert_eq!(reads.reads.borrow().len(), 1);
        });
    }
    on_fresh_memory(|| {
        crate::storage::init();
        let id = delivering();
        let reads = FillReads::answering(Err(FillRead::Unread("a node failed".to_string())));
        assert!(matches!(
            block_on(check(&reads, id, || secs(700))),
            Err(PoolError::Unavailable(_))
        ));
        assert_eq!(payout(id).unwrap().fill_early_at, None);
    });
    // no such payout
    on_fresh_memory(|| {
        crate::storage::init();
        let reads = FillReads::answering(Ok(AcrossFill::Filled));
        assert_eq!(
            block_on(check(&reads, 99, || secs(700))),
            Err(PoolError::Unknown)
        );
    });
}
