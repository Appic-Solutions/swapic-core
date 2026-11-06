use super::*;
use crate::ic::{
    EthTransaction, LedgerError, TxFinalizedStatus, WithdrawalDetail, WithdrawalStatus,
};
use crate::pool::icp::pending_payouts;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, ckusdc, funded_pool, settings, usdc, FEE};
use crate::storage::pool::{ledger, payout, record_at};
use crate::test_util::block_on;
use settlement_api::types::flow::FlowSettings as FlowWire;
use settlement_api::types::pool::IcpAccount as WireAccount;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::payout::{CkSettings, PayoutSettings};
use types::pool::{LedgerFee, Pocket};

const NETWORK_FEE: u64 = 670_000;
const ALLOWANCE: u64 = 300_000_000_000_000;

fn minter() -> Principal {
    Principal::from_slice(&[0x90; 10])
}

fn cketh() -> Principal {
    Principal::from_slice(&[0x91; 10])
}

fn own() -> Principal {
    Principal::from_slice(&[0x92; 10])
}

fn eth_usdc() -> EvmAddress {
    EvmAddress::new([0xa0; 20])
}

fn recipient() -> EvmAddress {
    EvmAddress::new([0x11; 20])
}

fn config() -> types::Config {
    let chain = EvmChain {
        swapic: EvmAddress::new([0x21; 20]),
        runner: EvmAddress::new([0x22; 20]),
        cctp_receiver: EvmAddress::new([0x23; 20]),
        usdc: eth_usdc(),
        token_messenger: EvmAddress::new([0x25; 20]),
        message_transmitter: EvmAddress::new([0x26; 20]),
        gas_limit: PerAction::all(types::GasAmount::from(500_000_u64)),
        gas_budget: PerAction::all(types::Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::from(50_000_u64)),
        usdc_name: None,
        usdc_version: None,
        block_time: Duration::from_secs(12),
    };
    types::Config {
        evm_chains: ChainTable(BTreeMap::from([(ChainId::ETHEREUM, chain)])),
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    }
}

fn ck_settings(open: bool) -> CkSettings {
    CkSettings {
        open,
        min_network_fee: TokenAmount::from(NETWORK_FEE),
        ck_swap_fee_allowance: TokenAmount::from(ALLOWANCE),
    }
}

/// A pool with 1,000 ckUSDC on ICP, the route open, and the minter configured.
fn pool() {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::SettingsSet {
            settings: types::pool::PoolSettings {
                ledger_fees: vec![LedgerFee {
                    ledger: ckusdc(),
                    fee: TokenAmount::from(FEE),
                }],
                ..settings()
            },
        },
        at(1),
    )
    .unwrap();
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings {
                ck: Some(ck_settings(true)),
                ..PayoutSettings::default()
            },
        },
        at(1),
    )
    .unwrap();
    flow_settings::set(FlowWire {
        ck_minter: Some(minter()),
        cketh_ledger: Some(cketh()),
        cketh_fee_allowance: Nat::from(6_000_000_000_000_000_u64),
        ..FlowWire::default()
    })
    .unwrap();
}

const DEADLINE_S: u64 = 3_000;

fn seconds(s: u64) -> Timestamp {
    Timestamp::from_nanos(s * 1_000_000_000)
}

fn request(amount_in: TokenAmount, min_out: TokenAmount, min_usdc: TokenAmount) -> CkRequest {
    CkRequest {
        caller: account(5).owner,
        token_in: ckusdc(),
        amount_in,
        from: account(5),
        refund_to: account(6),
        terms: PayoutTerms {
            dst_chain: ChainId::ETHEREUM,
            recipient: recipient(),
            token_out: eth_usdc(),
            min_out,
            deadline: seconds(DEADLINE_S),
        },
        min_usdc,
        network_fee: TokenAmount::from(NETWORK_FEE),
    }
}

/// A 100 ckUSDC swap, its floor all of it.
fn hundred() -> CkRequest {
    let f = TokenAmount::from(2 * FEE);
    let least = usdc(100)
        .checked_sub(TokenAmount::from(NETWORK_FEE))
        .and_then(|left| left.checked_sub(f))
        .unwrap();
    request(usdc(100), least, usdc(100))
}

fn checked(request: &CkRequest, now: Timestamp) -> Result<IcpSwapTerms, PoolError> {
    read_state(|state| check(state, &config(), request, now, None, |_| false))
}

fn open(request: &CkRequest) -> u64 {
    let terms = checked(request, seconds(100)).unwrap();
    let id = ledger().lines().next_id;
    record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(terms)),
            terms: request.terms,
            undo: Undo::Refund { to: request.refund_to },
            plan: None,
            ck: Flag(true),
        },
        seconds(100),
    )
    .unwrap();
    id
}

/// How the stub answers `withdraw_erc20` once.
#[derive(Clone, Debug)]
enum Ask {
    /// Burns and answers.
    Burn,
    /// Burns, and the answer is lost on its way.
    BurnLost,
    /// Burns the subaccount, and never lists it: a minter bug.
    BurnUnlisted,
    /// A guard's trap: nothing burned, and it reads as an unknown outcome.
    Trap,
    Refuse(WithdrawErc20Error),
}

/// The ledgers and the minter as this route meets them: ckUSDC in the main account and the
/// burn subaccount, one ledger fee on every transfer and approval, one transfer per dedup pair.
#[derive(Default)]
struct Stub {
    main: Cell<u64>,
    sub: Cell<u64>,
    burned: Cell<u64>,
    cketh: Cell<u64>,
    refunded: Cell<u64>,
    ledger_down: Cell<bool>,
    minter_down: Cell<bool>,
    funding_refused: Cell<bool>,
    asks: RefCell<VecDeque<Ask>>,
    asked: Cell<u32>,
    list: RefCell<Vec<WithdrawalDetail>>,
    status: RefCell<Option<WithdrawalStatus>>,
    seen: RefCell<BTreeSet<([u8; 32], u64)>>,
    log: RefCell<Vec<String>>,
}

fn detail(id: u64, status: WithdrawalStatus) -> WithdrawalDetail {
    WithdrawalDetail {
        withdrawal_id: id,
        recipient_address: recipient().to_string(),
        from: own(),
        from_subaccount: None,
        token_symbol: "ckUSDC".to_string(),
        withdrawal_amount: Nat::from(0_u8),
        max_transaction_fee: None,
        status,
    }
}

impl Stub {
    fn with_float() -> Self {
        let stub = Self::default();
        stub.cketh.set(ALLOWANCE);
        stub
    }

    fn with(asks: Vec<Ask>) -> Self {
        let stub = Self::with_float();
        *stub.asks.borrow_mut() = asks.into();
        stub
    }

    fn fresh(&self, dedup: Dedup) -> bool {
        self.seen
            .borrow_mut()
            .insert((dedup.memo, dedup.created_at_nanos))
    }

    fn burn(&self, amount: u64) {
        assert!(self.sub.get() >= amount, "a burn past the subaccount");
        self.sub.set(self.sub.get() - amount);
        self.burned.set(self.burned.get() + amount);
    }
}

fn units(amount: TokenAmount) -> u64 {
    amount.try_into_u128().unwrap() as u64
}

impl Ledgers for Stub {
    async fn transfer(
        &self,
        _ledger: Principal,
        _to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        if self.ledger_down.get() {
            return Err(CallError::Unknown("the ledger is down".to_string()));
        }
        if self.fresh(dedup) {
            self.main.set(self.main.get() - units(amount) - FEE);
            self.refunded.set(self.refunded.get() + units(amount));
        }
        Ok(9)
    }

    async fn pull(
        &self,
        _ledger: Principal,
        _from: IcpAccount,
        amount: TokenAmount,
        _dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.main.set(self.main.get() + units(amount));
        Ok(7)
    }

    async fn approve(&self, _: Principal, _: Principal, _: TokenAmount) -> Result<(), CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }

    async fn direction(
        &self,
        _: Principal,
        _: Principal,
        _: Principal,
    ) -> Result<Option<bool>, CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }

    async fn quote(
        &self,
        _: Principal,
        _: TokenAmount,
        _: bool,
    ) -> Result<TokenAmount, CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }

    async fn swap(
        &self,
        _: Principal,
        _: TokenAmount,
        _: bool,
        _: TokenAmount,
        _: TokenAmount,
        _: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }

    async fn unused(&self, _: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }

    async fn withdraw(
        &self,
        _: Principal,
        _: Principal,
        _: TokenAmount,
        _: TokenAmount,
    ) -> Result<(), CallError> {
        unreachable!("a ckUSDC swap has no leg")
    }
}

impl PlanReads for Stub {
    async fn scaled(&self, _: &[u8], _: TokenAmount) -> Result<Vec<u8>, String> {
        unreachable!("a ck swap has no plan")
    }

    fn checked(
        &self,
        _: &EvmPayout,
        _: &types::swapic::Order,
        _: &[u8],
        _: u64,
        _: Timestamp,
    ) -> Result<types::pool::payout::PinnedOrder, PoolError> {
        unreachable!("a ck swap has no plan")
    }
}

impl CkCalls for Stub {
    fn own_principal(&self) -> Principal {
        own()
    }

    async fn ck_balance(
        &self,
        ledger: Principal,
        account: Account,
    ) -> Result<TokenAmount, CallError> {
        if self.ledger_down.get() {
            return Err(CallError::Unknown("the ledger is down".to_string()));
        }
        Ok(TokenAmount::from(if ledger == cketh() {
            self.cketh.get()
        } else if account.subaccount.is_some() {
            self.sub.get()
        } else {
            self.main.get()
        }))
    }

    async fn ck_transfer(
        &self,
        _ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        _to: Account,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        if self.ledger_down.get() {
            return Err(CallError::Unknown("the ledger is down".to_string()));
        }
        if self.funding_refused.get() && from_subaccount.is_none() {
            return Err(CallError::Refused("InsufficientFunds".to_string()));
        }
        let amount = units(amount);
        if self.fresh(dedup) {
            match from_subaccount {
                // the main account funds the subaccount
                None => {
                    self.log.borrow_mut().push(format!("fund {amount}"));
                    self.main.set(self.main.get() - amount - FEE);
                    self.sub.set(self.sub.get() + amount);
                }
                // the subaccount brings it back
                Some(_) => {
                    self.log.borrow_mut().push(format!("back {amount}"));
                    self.sub.set(self.sub.get() - amount - FEE);
                    self.main.set(self.main.get() + amount);
                }
            }
        }
        Ok(5)
    }

    async fn ck_approve(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        spender: Principal,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<(), CallError> {
        assert_eq!(spender, minter());
        if ledger == cketh() {
            assert_eq!(amount, TokenAmount::from(ALLOWANCE));
            assert_eq!(from_subaccount, None);
            assert!(
                self.seen
                    .borrow_mut()
                    .insert((dedup.memo, dedup.created_at_nanos.wrapping_add(1))),
                "a ckETH approval is its own for each ask"
            );
            return Ok(());
        }
        assert!(from_subaccount.is_some());
        let key = (dedup.memo, dedup.created_at_nanos.wrapping_add(2));
        if self.seen.borrow_mut().insert(key) {
            self.sub.set(self.sub.get() - FEE);
        }
        Ok(())
    }

    async fn ck_withdraw(
        &self,
        _minter: Principal,
        _ledger: Principal,
        amount: TokenAmount,
        recipient: String,
        _from_subaccount: [u8; 32],
    ) -> Result<ic::RetrieveErc20Request, WithdrawFailure> {
        assert_eq!(recipient, self::recipient().to_string());
        self.asked.set(self.asked.get() + 1);
        let burnt = |listed: bool| {
            self.burn(units(amount));
            if listed {
                self.list
                    .borrow_mut()
                    .push(detail(33, WithdrawalStatus::Pending));
            }
            *self.status.borrow_mut() = Some(WithdrawalStatus::Pending);
        };
        let ask = self.asks.borrow_mut().pop_front().unwrap_or(Ask::Burn);
        match ask {
            Ask::Burn => {
                burnt(true);
                Ok(ic::RetrieveErc20Request {
                    cketh_block_index: Nat::from(33_u8),
                    ckerc20_block_index: Nat::from(44_u8),
                })
            }
            Ask::BurnLost => {
                burnt(true);
                Err(WithdrawFailure::Unknown("the reply was lost".to_string()))
            }
            Ask::BurnUnlisted => {
                burnt(false);
                Err(WithdrawFailure::Unknown("a trap after the burn".to_string()))
            }
            Ask::Trap => Err(WithdrawFailure::Unknown("a guard trapped".to_string())),
            Ask::Refuse(refusal) => Err(WithdrawFailure::Refused(refusal)),
        }
    }

    async fn ck_listed(
        &self,
        _minter: Principal,
        account: Account,
    ) -> Result<Vec<WithdrawalDetail>, CallError> {
        if self.minter_down.get() {
            return Err(CallError::Unknown("the minter is down".to_string()));
        }
        assert!(account.subaccount.is_some(), "only the payout's own account");
        Ok(self.list.borrow().clone())
    }

    async fn ck_status(
        &self,
        _minter: Principal,
        id: u64,
    ) -> Result<Vec<WithdrawalDetail>, CallError> {
        self.log.borrow_mut().push("read".to_string());
        Ok(self
            .status
            .borrow()
            .clone()
            .map(|status| vec![detail(id, status)])
            .unwrap_or_default())
    }
}

fn open_pool() -> Result<(), String> {
    Ok(())
}

fn closed_pool() -> Result<(), String> {
    Err("closed".to_string())
}

fn go(stub: &Stub, id: u64, s: u64) -> Result<(), Stop> {
    block_on(drive(stub, id, || seconds(s), &open_pool))
}

fn status(id: u64) -> PayoutStatus {
    payout(id).unwrap().status
}

fn ledger_error() -> LedgerError {
    LedgerError::InsufficientFunds {
        balance: Nat::from(0_u8),
        failed_burn_amount: Nat::from(0_u8),
        token_symbol: "ckUSDC".to_string(),
        ledger_id: ckusdc(),
    }
}

/// The pool's lines against the main account: what the canister must hold beside the money
/// that came in before the lines moved: never below them, and all of the stakers' whole.
fn assert_books(stub: &Stub, assets: TokenAmount) {
    let pool = ledger();
    assert_eq!(pool.total_assets(), assets, "the stakers' assets moved");
    let lines = read_state(crate::pool::icp::icp_lines);
    assert!(
        TokenAmount::from(stub.main.get()) >= lines,
        "main {} below the lines {lines}",
        stub.main.get()
    );
}

/// Burn of the whole sum: the main account falls by exactly the hold, the network fee stays
/// on its line and in the account, the minter closes the row, and no Base line ever moved.
#[test]
fn a_ckusdc_swap_burns_its_hold_less_both_ledger_fees_and_lands() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let base = ledger().pockets.base;
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        let burn = 100_000_000 - NETWORK_FEE - 2 * FEE;
        assert_eq!(stub.burned.get(), burn);
        assert_eq!(stub.sub.get(), 0);
        assert_eq!(stub.main.get(), units(usdc(1_000)) + NETWORK_FEE);
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
        let ck = payout(id).unwrap().ck.unwrap();
        assert_eq!(ck.withdrawal_id, Some(33));
        assert_eq!(ck.amount, Some(TokenAmount::from(burn)));
        let pool = ledger();
        assert_eq!(pool.pockets.icp.reserved, TokenAmount::ZERO);
        assert_eq!(pool.owed, TokenAmount::ZERO);
        assert_eq!(pool.lines().fees_icp, TokenAmount::from(NETWORK_FEE));
        assert_books(&stub, assets);

        // mined is not closed, and the row waits for the minter's end
        *stub.status.borrow_mut() = Some(WithdrawalStatus::TxSent(EthTransaction {
            transaction_hash: format!("0x{}", "ab".repeat(32)),
        }));
        go(&stub, id, 120).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);

        *stub.status.borrow_mut() = Some(WithdrawalStatus::TxFinalized(
            TxFinalizedStatus::Success {
                transaction_hash: format!("0x{}", "ab".repeat(32)),
                effective_transaction_fee: None,
            },
        ));
        go(&stub, id, 130).unwrap();
        assert_eq!(status(id), PayoutStatus::Paid);
        assert!(ledger().lines().open.is_empty());
        assert_eq!(ledger().pockets.base, base);
        assert_eq!(ledger().lines().fees_icp, TokenAmount::from(NETWORK_FEE));
        assert_books(&stub, assets);
    });
}

/// A lost answer is never asked again: the minter's list finds the burn.
#[test]
fn a_lost_answer_is_found_on_the_minters_list_and_never_asked_again() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::BurnLost]);
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        assert!(matches!(go(&stub, id, 110), Err(Stop::Retry(_))));
        assert_eq!(status(id), PayoutStatus::CkWithdrawing);
        go(&stub, id, 111).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
        assert_eq!(stub.asked.get(), 1);
        assert_eq!(payout(id).unwrap().ck.unwrap().withdrawal_id, Some(33));
    });
}

/// U1: burn, the answer lost, and whatever the deadline or a later refusal says, no refund.
#[test]
fn a_burn_with_a_lost_answer_is_never_refunded_past_the_deadline() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![
            Ask::BurnLost,
            Ask::Refuse(WithdrawErc20Error::CkEthLedgerError {
                error: ledger_error(),
            }),
        ]);
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let id = open(&hundred());
        assert!(go(&stub, id, 110).is_err());
        go(&stub, id, DEADLINE_S + 1).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
        assert_eq!(stub.asked.get(), 1);
        assert_eq!(stub.refunded.get(), 0);
        assert_books(&stub, assets);
    });
}

/// Every guard failure is a trap: asked again only once the list says nothing, at its pace.
#[test]
fn a_trap_burned_nothing_only_once_the_list_says_so_and_is_asked_again() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::Trap, Ask::Burn]);
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        assert!(matches!(go(&stub, id, 110), Err(Stop::Retry(_))));
        // not yet at its pace
        go(&stub, id, 111).unwrap();
        assert_eq!(stub.asked.get(), 1);
        go(&stub, id, 110 + ASK_EVERY.as_secs()).unwrap();
        assert_eq!(stub.asked.get(), 2);
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
        // the same funding and approval, whatever the tries
        let funded = stub
            .log
            .borrow()
            .iter()
            .filter(|line| line.starts_with("fund"))
            .count();
        assert_eq!(funded, 1);
        assert_eq!(stub.burned.get(), 100_000_000 - NETWORK_FEE - 2 * FEE);
    });
}

/// Nothing burned past the deadline: all of it back, the network fee too, only the ledger
/// fees of the trip kept, and the books whole.
#[test]
fn nothing_burned_past_the_deadline_refunds_with_the_network_fee() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::Trap]);
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let id = open(&hundred());
        assert!(go(&stub, id, 110).is_err());
        go(&stub, id, DEADLINE_S + 1).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        // funding, approval, the way back and the refund
        assert_eq!(stub.refunded.get(), 100_000_000 - 4 * FEE);
        assert_eq!(stub.main.get(), units(usdc(1_000)));
        assert_eq!(stub.sub.get(), 0);
        let pool = ledger();
        assert_eq!(pool.lines().fees_icp, TokenAmount::ZERO);
        assert_eq!(pool.pockets.icp.reserved, TokenAmount::ZERO);
        assert_eq!(pool.owed, TokenAmount::ZERO);
        assert_books(&stub, assets);
    });
}

/// A refusal that cannot pass goes back at once, on the same proof.
#[test]
fn a_blocked_recipient_goes_back_at_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::Refuse(
            WithdrawErc20Error::RecipientAddressBlocked {
                address: recipient().to_string(),
            },
        )]);
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        assert_eq!(stub.refunded.get(), 100_000_000 - 4 * FEE);
        assert_eq!(ledger().lines().fees_icp, TokenAmount::ZERO);
        assert_eq!(stub.asked.get(), 1);
    });
}

/// The minter's refusals, each as the design's table says.
#[test]
fn the_minters_refusals_each_take_their_way() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let down = || WithdrawErc20Error::TemporarilyUnavailable("for now".to_string());
        let gas = || WithdrawErc20Error::CkEthLedgerError {
            error: ledger_error(),
        };
        let burn_down = || WithdrawErc20Error::CkErc20LedgerError {
            cketh_block_index: Nat::from(1_u8),
            error: LedgerError::TemporarilyUnavailable("for now".to_string()),
        };
        let stub = Stub::with(vec![
            Ask::Refuse(down()),
            Ask::Refuse(gas()),
            Ask::Refuse(burn_down()),
            Ask::Burn,
        ]);
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        let mut at_s = 110;
        for tries in 1..=3 {
            go(&stub, id, at_s).unwrap();
            assert_eq!(status(id), PayoutStatus::CkWithdrawing);
            assert_eq!(stub.asked.get(), tries);
            at_s += ASK_EVERY.as_secs();
        }
        go(&stub, id, at_s).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
    });
}

/// `CkErc20LedgerError` that cannot pass: its ckETH came back less a penalty, nothing burned.
#[test]
fn a_ckusdc_burn_that_cannot_pass_goes_back() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::Refuse(WithdrawErc20Error::CkErc20LedgerError {
            cketh_block_index: Nat::from(1_u8),
            error: ledger_error(),
        })]);
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        assert_eq!(stub.refunded.get(), 100_000_000 - 4 * FEE);
    });
}

/// Burned and reimbursed: the ckUSDC comes home, the refund is it less the ledger fees of the
/// way back, and the network fee stays, as the gas was spent. Stakers and the fee line whole.
#[test]
fn a_reimbursed_withdrawal_refunds_and_keeps_the_network_fee() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        let burn = stub.burned.get();

        // the minter's word without the money yet: nothing moves
        *stub.status.borrow_mut() = Some(WithdrawalStatus::TxFinalized(
            TxFinalizedStatus::Reimbursed {
                transaction_hash: format!("0x{}", "cd".repeat(32)),
                reimbursed_amount: Nat::from(burn),
                reimbursed_in_block: Nat::from(77_u8),
            },
        ));
        go(&stub, id, 120).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);

        stub.sub.set(burn);
        go(&stub, id, 130).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        assert_eq!(stub.refunded.get(), burn - 2 * FEE);
        assert_eq!(stub.main.get(), units(usdc(1_000)) + NETWORK_FEE);
        assert_eq!(stub.sub.get(), 0);
        assert_eq!(ledger().lines().fees_icp, TokenAmount::from(NETWORK_FEE));
        assert_books(&stub, assets);
    });
}

/// A pending reimbursement is no reimbursement: the row waits, however long.
#[test]
fn a_quarantined_reimbursement_waits_and_a_controller_can_end_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        let burn = stub.burned.get();
        *stub.status.borrow_mut() = Some(WithdrawalStatus::TxFinalized(
            TxFinalizedStatus::PendingReimbursement(EthTransaction {
                transaction_hash: format!("0x{}", "cd".repeat(32)),
            }),
        ));
        go(&stub, id, 120).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);

        let resolve = |resolution: CkResolution, is_controller: bool| {
            block_on(super::resolve(
                &stub,
                false,
                is_controller,
                id,
                resolution,
                seconds(200),
            ))
        };
        assert!(resolve(CkResolution::Reimbursed, false).is_err());
        // nothing in the subaccount: no way back on anyone's word
        assert!(resolve(CkResolution::Reimbursed, true).is_err());
        assert!(resolve(CkResolution::Retry, true).is_err(), "it is not stopped");
        stub.sub.set(burn);
        resolve(CkResolution::Reimbursed, true).unwrap();
        go(&stub, id, 210).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        assert_eq!(stub.refunded.get(), burn - 2 * FEE);
    });
}

/// A controller finds the USDC paid: the row closes, moving nothing.
#[test]
fn a_controller_closes_a_withdrawal_found_delivered() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        block_on(super::resolve(
            &stub,
            false,
            true,
            id,
            CkResolution::Delivered {
                tx_hash: vec![0xab; 32],
            },
            seconds(200),
        ))
        .unwrap();
        assert_eq!(status(id), PayoutStatus::Paid);
        assert_books(&stub, assets);
    });
}

/// The subaccount short and nothing listed: it may be burned, so it stops, and neither the
/// fold nor a controller's refund reaches stakers' money.
#[test]
fn a_short_subaccount_with_nothing_listed_stops_and_cannot_be_refunded() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with(vec![Ask::BurnUnlisted]);
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let id = open(&hundred());
        assert!(go(&stub, id, 110).is_err());
        assert!(go(&stub, id, DEADLINE_S + 1).is_err());
        assert!(matches!(status(id), PayoutStatus::Failed(_)));
        assert_eq!(stub.refunded.get(), 0);
        let refused = crate::pool::resolve::resolve(
            false,
            true,
            id,
            settlement_api::types::pool::PayoutResolution::Refund,
            seconds(DEADLINE_S + 10),
        );
        assert!(refused.is_err(), "a refund past the burn pays from stakers");
        assert_books(&stub, assets);

        // the human found it listed after all: read again, it is adopted, never refunded
        stub.list
            .borrow_mut()
            .push(detail(33, WithdrawalStatus::Pending));
        block_on(super::resolve(
            &stub,
            false,
            true,
            id,
            CkResolution::Retry,
            seconds(DEADLINE_S + 20),
        ))
        .unwrap();
        go(&stub, id, DEADLINE_S + 30).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
        assert!(ledger().lines().open.contains(&id));
    });
}

/// A funding the ledger refused: nothing left the main account, so the refund is today's,
/// all of it less one fee, the network fee too.
#[test]
fn a_funding_that_never_landed_refunds_all_less_one_fee() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        stub.funding_refused.set(true);
        let assets = ledger().total_assets();
        let id = open(&hundred());
        go(&stub, id, 110).unwrap();
        assert!(matches!(status(id), PayoutStatus::Undone(_)));
        assert_eq!(stub.asked.get(), 0);
        assert_eq!(stub.refunded.get(), 100_000_000 - FEE);
        assert_eq!(stub.main.get(), units(usdc(1_000)));
        assert_eq!(ledger().lines().fees_icp, TokenAmount::ZERO);
        assert_books(&stub, assets);
    });
}

/// The minter that cannot be read: nothing is asked, nothing undone, and a controller gets it.
#[test]
fn a_minter_that_cannot_be_read_waits_then_stops_for_a_controller() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        go(&stub, id, 110).ok();
        // a second payout, the minter gone
        stub.minter_down.set(true);
        let second = open(&hundred());
        assert!(go(&stub, second, 120).is_err());
        assert_eq!(status(second), PayoutStatus::CkWithdrawing);
        assert_eq!(stub.asked.get(), 1);
        assert!(go(&stub, second, DEADLINE_S + 1).is_err());
        assert_eq!(status(second), PayoutStatus::CkWithdrawing);
        assert!(go(&stub, second, 120 + STUCK_AFTER.as_secs()).is_err());
        assert!(matches!(status(second), PayoutStatus::Failed(_)));
    });
}

/// One withdrawal at the minter at a time, moves included: the second waits, nothing asked.
#[test]
fn a_second_withdrawal_waits_while_one_is_at_the_minter() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        let held = AtMinter::take().unwrap();
        assert!(AtMinter::take().is_none());
        assert!(matches!(go(&stub, id, 110), Err(Stop::Retry(_))));
        assert_eq!(status(id), PayoutStatus::CkWithdrawing);
        assert_eq!(stub.asked.get(), 0);
        drop(held);
        go(&stub, id, 111).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
    });
}

/// A closed pool starts nothing and asks nothing; its refund waits too, and the deadline
/// still finds it before any burn.
#[test]
fn a_closed_pool_starts_no_withdrawal() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        let driven = block_on(drive(&stub, id, || seconds(110), &closed_pool));
        assert!(driven.is_err());
        assert_eq!(status(id), PayoutStatus::Known);
        assert_eq!(stub.asked.get(), 0);
        assert_eq!(stub.sub.get(), 0);
    });
}

/// The float short at its start: it waits as a known amount, lines up, at no cost, for its
/// deadline to refund it whole.
#[test]
fn a_float_too_short_waits_before_any_fee_is_spent() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::default();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        assert!(matches!(go(&stub, id, 110), Err(Stop::Retry(_))));
        assert_eq!(status(id), PayoutStatus::Known);
        assert_eq!(stub.sub.get(), 0);
    });
}

/// Turning the route off stops new swaps at the door, and leaves the ones on their way alone.
#[test]
fn the_switch_stops_the_door_and_never_a_swap_on_its_way() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let id = open(&hundred());
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings {
                    ck: Some(ck_settings(false)),
                    ..PayoutSettings::default()
                },
            },
            seconds(105),
        )
        .unwrap();
        assert!(matches!(
            checked(&hundred(), seconds(106)),
            Err(PoolError::Closed(_))
        ));
        go(&stub, id, 110).unwrap();
        assert_eq!(status(id), PayoutStatus::CkWithdrawn);
    });
}

/// The door's rules, each before anything is recorded.
#[test]
fn each_refusal_comes_before_anything_is_recorded() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let events = crate::storage::events::event_count();
        let good = hundred();
        assert!(checked(&good, seconds(100)).is_ok());
        let refused = |change: fn(&mut CkRequest)| {
            let mut request = good.clone();
            change(&mut request);
            checked(&request, seconds(100)).unwrap_err()
        };
        assert!(matches!(
            refused(|r| r.token_in = Principal::from_slice(&[9; 10])),
            PoolError::TokenNotListed(_)
        ));
        assert!(matches!(
            refused(|r| r.terms.deadline = seconds(100 + 299)),
            PoolError::Expired
        ));
        for change in [
            (|r: &mut CkRequest| r.amount_in = TokenAmount::ZERO) as fn(&mut CkRequest),
            |r| r.terms.deadline = seconds(100 + 3_601),
            |r| r.terms.dst_chain = ChainId::BASE,
            |r| r.terms.token_out = EvmAddress::new([0xb0; 20]),
            |r| r.terms.min_out = TokenAmount::from(FEE),
            // below the floor, and F3: min_usdc short of min_out, the fee and both ledger fees
            |r| r.network_fee = TokenAmount::from(NETWORK_FEE - 1),
            |r| {
                r.min_usdc = r
                    .min_usdc
                    .checked_sub(TokenAmount::from(1_u8))
                    .unwrap()
            },
            |r| r.min_usdc = r.amount_in.checked_add(TokenAmount::from(1_u8)).unwrap(),
        ] {
            assert!(matches!(refused(change), PoolError::NotAllowed(_)));
        }
        assert_eq!(crate::storage::events::event_count(), events);
        // the pool taking no swaps and an empty Base pocket are why this route exists
        assert_eq!(ledger().pockets.base, Pocket::default());
    });
}

/// `sanctioned` screens the caller, the refund and the recipient.
#[test]
fn no_sanctioned_party_gets_in() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let answer = read_state(|state| {
            check(state, &config(), &hundred(), seconds(100), None, |address| {
                *address == Address::from(recipient())
            })
        });
        assert!(matches!(answer, Err(PoolError::Sanctioned(_))));
    });
}

/// `request_of` reads a caller's own account and names nothing else.
#[test]
fn the_wire_terms_read_as_the_callers() {
    let args = CkSwapTerms {
        token_in: ckusdc(),
        amount_in: Nat::from(10_u8),
        from_subaccount: Some(vec![7; 32]),
        refund_to: None,
        deadline_ns: 5,
        dst_chain: 1,
        recipient: recipient().to_string(),
        token_out: eth_usdc().to_string(),
        min_out: Nat::from(1_u8),
        min_usdc: Nat::from(2_u8),
        network_fee: Nat::from(3_u8),
    };
    let request = request_of(account(5).owner, args.clone()).unwrap();
    assert_eq!(request.refund_to, request.from);
    assert_eq!(request.from.subaccount, Some([7; 32]));
    let other = CkSwapTerms {
        refund_to: Some(WireAccount {
            owner: account(6).owner,
            subaccount: None,
        }),
        ..args
    };
    assert_eq!(
        request_of(account(5).owner, other).unwrap().refund_to,
        account(6)
    );
}

/// F6: a hundred swaps, some reimbursed and some refunded, leave the main account above the
/// lines by nothing more than fees, and the check never latches.
#[test]
fn a_hundred_swaps_never_drift_the_books() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let stub = Stub::with_float();
        stub.main.set(units(usdc(1_000)));
        let assets = ledger().total_assets();
        let mut at_s = 100;
        for round in 0..100_u64 {
            let mut request = hundred();
            request.terms.deadline = seconds(at_s + 1_000);
            at_s += 2;
            let id = ledger().lines().next_id;
            let terms = read_state(|state| {
                check(state, &config(), &request, seconds(at_s), None, |_| false)
            })
            .unwrap();
            record_at(
                PoolStep::PayoutOpened {
                    id,
                    kind: Box::new(PayoutKind::IcpSwap(terms)),
                    terms: request.terms,
                    undo: Undo::Refund { to: request.refund_to },
                    plan: None,
                    ck: Flag(true),
                },
                seconds(at_s),
            )
            .unwrap();
            stub.list.borrow_mut().clear();
            *stub.asks.borrow_mut() = vec![match round % 3 {
                0 => Ask::Burn,
                1 => Ask::BurnLost,
                _ => Ask::Refuse(WithdrawErc20Error::RecipientAddressBlocked {
                    address: recipient().to_string(),
                }),
            }]
            .into();
            let _ = go(&stub, id, at_s);
            let _ = go(&stub, id, at_s + 1);
            assert_books(&stub, assets);
            if round % 3 == 2 {
                assert!(matches!(status(id), PayoutStatus::Undone(_)));
                continue;
            }
            let burn = stub.burned.get();
            *stub.status.borrow_mut() = Some(if round % 3 == 0 {
                WithdrawalStatus::TxFinalized(TxFinalizedStatus::Success {
                    transaction_hash: format!("0x{}", "ab".repeat(32)),
                    effective_transaction_fee: None,
                })
            } else {
                stub.sub.set(stub.sub.get() + units(TokenAmount::from(burn)));
                stub.burned.set(0);
                WithdrawalStatus::TxFinalized(TxFinalizedStatus::Reimbursed {
                    transaction_hash: format!("0x{}", "cd".repeat(32)),
                    reimbursed_amount: Nat::from(burn),
                    reimbursed_in_block: Nat::from(77_u8),
                })
            });
            go(&stub, id, at_s + 2).unwrap();
            stub.burned.set(0);
            assert_books(&stub, assets);
            assert!(ledger().lines().open.is_empty());
        }
        let pool = ledger();
        // every landed and reimbursed swap kept its fee; each blocked one gave it back
        let kept = 67 * NETWORK_FEE;
        assert_eq!(pool.lines().fees_icp, TokenAmount::from(kept));
        assert_eq!(pool.pockets.icp.reserved, TokenAmount::ZERO);
        assert_eq!(pool.owed, TokenAmount::ZERO);
        let surplus = read_state(pending_payouts);
        assert_eq!(surplus, TokenAmount::ZERO);
        // the account holds the lines and the fee line, to the unit
        assert_eq!(stub.main.get(), units(usdc(1_000)) + kept);
    });
}
