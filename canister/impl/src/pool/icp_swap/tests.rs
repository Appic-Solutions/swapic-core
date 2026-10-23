use super::*;
use crate::ic::Dedup;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, base_stake, ckusdc, funded_pool, payer, settings, usdc, FEE,
};
use crate::storage::pool::{ledger, record_at};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::{IcpRoute, IcpRouteVia, IcpRoutes, LedgerFee, PayoutSettings};
use types::ChainId;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake ledger never waits"),
    }
}

fn clock() -> Timestamp {
    at(10)
}

fn open_pool() -> Result<(), String> {
    Ok(())
}

fn token() -> Principal {
    Principal::from_slice(&[0x70; 10])
}

fn token_pool() -> Principal {
    Principal::from_slice(&[0x71; 10])
}

const TOKEN_FEE: u64 = 1_000;

fn recipient() -> EvmAddress {
    "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap()
}

fn swapic() -> EvmAddress {
    "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"
        .parse()
        .unwrap()
}

fn config() -> types::Config {
    let chain = EvmChain {
        swapic: swapic(),
        // the deployed Runner, the real KyberSwap route's receiver
        runner: "0x7E345acaB79102ac9588C60B9358Ec4d79438eEe"
            .parse()
            .unwrap(),
        cctp_receiver: EvmAddress::new([0x23; 20]),
        usdc: base_usdc(),
        token_messenger: EvmAddress::new([0x25; 20]),
        message_transmitter: EvmAddress::new([0x26; 20]),
        gas_limit: PerAction::all(types::GasAmount::from(500_000_u64)),
        gas_budget: PerAction::all(types::Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::from(50_000_u64)),
    };
    types::Config {
        evm_chains: ChainTable(BTreeMap::from([
            (ChainId::BASE, chain),
            (ChainId::ARBITRUM, chain),
        ])),
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    }
}

/// A pool open for swaps with 1,000 ckUSDC on ICP, `base` USDC on Base, the payout settings,
/// and `token()` routed directly through `token_pool()`.
fn pool_with(base: TokenAmount) {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::SettingsSet {
            settings: types::pool::PoolSettings {
                ledger_fees: vec![
                    LedgerFee {
                        ledger: ckusdc(),
                        fee: TokenAmount::from(FEE),
                    },
                    LedgerFee {
                        ledger: token(),
                        fee: TokenAmount::from(TOKEN_FEE),
                    },
                ],
                ..settings()
            },
        },
        at(1),
    )
    .unwrap();
    record_at(
        PoolStep::IcpRoutesSet {
            routes: IcpRoutes {
                icp_ckusdc_pool: None,
                icp_ledger: None,
                routes: vec![IcpRoute {
                    ledger: token(),
                    via: IcpRouteVia::Direct(token_pool()),
                }],
                ledger_fees: vec![LedgerFee {
                    ledger: token(),
                    fee: TokenAmount::from(TOKEN_FEE),
                }],
            },
        },
        at(1),
    )
    .unwrap();
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        },
        at(1),
    )
    .unwrap();
    if base > TokenAmount::ZERO {
        base_stake(payer(), base, 0x61, 1);
    }
}

/// A swap opened before the plan flow, as its log would hold it: its user's own premium and
/// least ckUSDC, recorded straight into the fold (no door opens one any more).
#[derive(Clone, Copy)]
struct Legacy {
    terms: IcpSwapTerms,
    payout: PayoutTerms,
}

fn request(token_in: Principal, amount_in: TokenAmount, min_usdc: TokenAmount) -> Legacy {
    let from = account(5);
    Legacy {
        terms: IcpSwapTerms {
            token_in,
            amount_in,
            from,
            min_usdc,
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
            slippage_bps: None,
            planned: None,
        },
        payout: PayoutTerms {
            dst_chain: ChainId::ARBITRUM,
            recipient: recipient(),
            token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
                .parse()
                .unwrap(),
            min_out: TokenAmount::from(1_u8),
            deadline: at(10).checked_add(MIN_DEADLINE).unwrap(),
        },
    }
}

fn open(request: &Legacy) -> u64 {
    let id = ledger().lines().next_id;
    record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(request.terms)),
            terms: request.payout,
            undo: Undo::Refund {
                to: request.terms.refund_to,
            },
            plan: None,
        },
        at(10),
    )
    .unwrap();
    id
}

/// A transfer by its dedup pair (memo, `created_at_time`): its ledger, where and how much.
type Transfers = BTreeMap<([u8; 32], u64), (Principal, IcpAccount, TokenAmount)>;

/// A ledger and a pool: a pull's answer, one quote and one swap answer, and every transfer
/// once per dedup pair.
#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    pull_refused: Cell<bool>,
    quote: Cell<Option<TokenAmount>>,
    out: Cell<Option<TokenAmount>>,
    /// A pool's own quote and swap answer, over the ones above.
    quotes: RefCell<BTreeMap<Principal, TokenAmount>>,
    outs: RefCell<BTreeMap<Principal, TokenAmount>>,
    transfers: RefCell<Transfers>,
    /// What the scaler answers: none, it is not read.
    scaled: RefCell<Option<Result<Vec<u8>, String>>>,
}

/// The scaler as the fake answers it, and a payout order's checks on `config()`.
impl PlanReads for Fake {
    async fn scaled(&self, _route: &[u8], amount: TokenAmount) -> Result<Vec<u8>, String> {
        self.calls.borrow_mut().push(format!("scale {amount}"));
        self.scaled
            .borrow()
            .clone()
            .unwrap_or_else(|| Err("no scaler here".to_string()))
    }

    fn checked(
        &self,
        payout: &EvmPayout,
        order: &types::swapic::Order,
        order_bytes: &[u8],
        gas_limit: u64,
        now: Timestamp,
    ) -> Result<PinnedOrder, PoolError> {
        let config = config();
        let payer = pay::payer_of(&config, canister(), None)?;
        pay::check_order(
            payout,
            order,
            order_bytes,
            gas_limit,
            &payer,
            &PayoutSettings::default(),
            now.as_secs().get(),
            |_| false,
            AmountIn::Exact,
        )
    }
}

impl Ledgers for Fake {
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.calls.borrow_mut().push(format!("transfer {amount}"));
        self.transfers
            .borrow_mut()
            .insert((dedup.memo, dedup.created_at_nanos), (ledger, to, amount));
        Ok(300)
    }

    async fn pull(
        &self,
        ledger: Principal,
        _from: IcpAccount,
        amount: TokenAmount,
        _dedup: Dedup,
    ) -> Result<u64, CallError> {
        let name = if ledger == ckusdc() {
            "ckUSDC"
        } else {
            "token"
        };
        self.calls
            .borrow_mut()
            .push(format!("pull {name} {amount}"));
        if self.pull_refused.get() {
            return Err(CallError::Refused("InsufficientAllowance".into()));
        }
        Ok(7)
    }

    async fn approve(
        &self,
        _ledger: Principal,
        _spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError> {
        self.calls.borrow_mut().push(format!("approve {amount}"));
        Ok(())
    }

    async fn direction(
        &self,
        _pool: Principal,
        _token_in: Principal,
        _token_out: Principal,
    ) -> Result<Option<bool>, CallError> {
        Ok(Some(false))
    }

    async fn quote(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        _zero_for_one: bool,
    ) -> Result<TokenAmount, CallError> {
        self.calls.borrow_mut().push(format!("quote {amount_in}"));
        if let Some(quoted) = self.quotes.borrow().get(&pool) {
            return Ok(*quoted);
        }
        Ok(self.quote.get().unwrap_or(TokenAmount::ZERO))
    }

    async fn swap(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        _zero_for_one: bool,
        min_out: TokenAmount,
        _fee_in: TokenAmount,
        _fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        self.calls
            .borrow_mut()
            .push(format!("swap {amount_in} at least {min_out}"));
        if let Some(out) = self.outs.borrow().get(&pool) {
            return Ok(*out);
        }
        self.out
            .get()
            .ok_or_else(|| CallError::Refused("slippage".into()))
    }

    async fn unused(&self, _pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        Ok((TokenAmount::ZERO, TokenAmount::ZERO))
    }

    async fn withdraw(
        &self,
        _pool: Principal,
        _token: Principal,
        _amount: TokenAmount,
        _fee: TokenAmount,
    ) -> Result<(), CallError> {
        Ok(())
    }
}

/// ckUSDC in: pulled with the payout's dedup pair, the amount exactly the ckUSDC less the
/// premium and the network fee, its ckUSDC held on ICP, waiting with its USDC reserved.
#[test]
fn a_ckusdc_swap_is_pulled_and_waits_with_its_exact_amount() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let request = request(ckusdc(), usdc(100), usdc(100));
        let id = open(&request);
        let fake = Fake::default();
        let icp_before = ledger().pockets.icp;
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![format!("pull ckUSDC {}", usdc(100))]
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Waiting);
        let amount = usdc(100)
            .checked_sub(usdc(1))
            .and_then(|left| left.checked_sub(TokenAmount::from(50_000_u32)))
            .unwrap();
        assert_eq!(payout.amount, Some(amount));
        assert_eq!(payout.received, Some(usdc(100)));
        assert_eq!(payout.icp_block, Some(7));
        let pool = ledger();
        assert_eq!(pool.pockets.base.reserved, amount);
        assert_eq!(
            pool.pockets.icp.reserved,
            icp_before
                .reserved
                .checked_add(usdc(100))
                .and_then(|sum| sum.checked_sub(TokenAmount::from(50_000_u32)))
                .unwrap()
        );
        assert_eq!(pool.lines().fees_icp, TokenAmount::from(50_000_u32));
    });
}

/// The Base pocket short: the payout is queued with its exact amount, nothing reserved.
#[test]
fn a_swap_the_base_pocket_cannot_cover_is_queued() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(50));
        let id = open(&request(ckusdc(), usdc(100), usdc(100)));
        block_on(drive(&Fake::default(), id, clock, &open_pool)).unwrap();
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Queued);
        assert_eq!(ledger().pockets.base.reserved, TokenAmount::ZERO);
        assert_eq!(ledger().lines().queue_len, 1);
    });
}

/// A pull the ledger refuses: nothing moved, the payout undone, nothing more called.
#[test]
fn a_refused_pull_undoes_the_swap() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let id = open(&request(ckusdc(), usdc(100), usdc(100)));
        let fake = Fake::default();
        fake.pull_refused.set(true);
        assert!(matches!(
            block_on(drive(&fake, id, clock, &open_pool)),
            Err(Stop::Failed(_))
        ));
        let payout = pool_store::payout(id).unwrap();
        assert!(matches!(payout.status, PayoutStatus::Undone(_)));
        assert_eq!(payout.icp_block, None);
        assert!(ledger().lines().open.is_empty());
        assert_eq!(fake.calls.borrow().len(), 1);
    });
}

/// A token through its ckUSDC pool: the leg sells what was pulled less its approval's and
/// pull's fees, at least `min_usdc` and the withdrawal's ckUSDC fee; the ckUSDC held is the
/// output less that fee.
#[test]
fn a_token_reaches_ckusdc_through_its_pool() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        request.terms.premium = TokenAmount::from(4 * FEE);
        let id = open(&request);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        fake.out.set(Some(TokenAmount::from(2_050_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "quote 4998000".to_string(),
                "approve 4999000".to_string(),
                "swap 4998000 at least 2010000".to_string(),
            ]
        );
        let payout = pool_store::payout(id).unwrap();
        let received = TokenAmount::from(2_040_000_u64);
        assert_eq!(payout.received, Some(received));
        assert_eq!(
            payout.amount,
            received
                .checked_sub(TokenAmount::from(4 * FEE + 50_000))
                .map(Some)
                .unwrap()
        );
        assert_eq!(payout.status, PayoutStatus::Waiting);
    });
}

/// Below `min_usdc`: the token goes back to `refund_to`, all of it less its fee, and the
/// payout is undone with the refund's block.
#[test]
fn a_token_short_of_min_usdc_is_refunded_in_full() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        request.terms.premium = TokenAmount::from(4 * FEE);
        let id = open(&request);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_009_999_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        let transfers: Vec<_> = fake.transfers.borrow().values().copied().collect();
        assert_eq!(
            transfers,
            vec![(token(), account(6), TokenAmount::from(4_999_000_u64))]
        );
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("swap")));
        let payout = pool_store::payout(id).unwrap();
        assert!(matches!(payout.status, PayoutStatus::Undone(_)));
        assert_eq!(payout.icp_block, Some(300));
        assert!(ledger().lines().open.is_empty());
        assert_eq!(ledger().owed, TokenAmount::ZERO);
    });
}

/// A swap the pool refused before its deposit: the token goes back, all of it less the
/// approval's fee and the transfer's.
#[test]
fn a_refused_leg_refunds_the_token_less_what_it_spent() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        request.terms.premium = TokenAmount::from(4 * FEE);
        let id = open(&request);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        let transfers: Vec<_> = fake.transfers.borrow().values().copied().collect();
        assert_eq!(
            transfers,
            vec![(token(), account(6), TokenAmount::from(4_998_000_u64))]
        );
        assert!(matches!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Undone(_)
        ));
    });
}

fn icp_ledger() -> Principal {
    Principal::from_text("ryjl3-tyaaa-aaaaa-aaaba-cai").unwrap()
}

fn icp_pool() -> Principal {
    Principal::from_slice(&[0x81; 10])
}

/// A token routed through ICP comes in on two legs: the token into ICP on its ICP pool, then
/// ICP into ckUSDC on the ICP/ckUSDC pool, the first leg's minimum what the second needs to
/// give `min_usdc` and the withdrawal's fee.
#[test]
fn a_token_through_icp_reaches_ckusdc_on_two_legs() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        record_at(
            PoolStep::IcpRoutesSet {
                routes: IcpRoutes {
                    icp_ckusdc_pool: Some(PrincipalBytes(icp_pool())),
                    icp_ledger: Some(PrincipalBytes(icp_ledger())),
                    routes: vec![
                        IcpRoute {
                            ledger: icp_ledger(),
                            via: IcpRouteVia::Direct(icp_pool()),
                        },
                        IcpRoute {
                            ledger: token(),
                            via: IcpRouteVia::Icp(token_pool()),
                        },
                    ],
                    ledger_fees: vec![
                        LedgerFee {
                            ledger: token(),
                            fee: TokenAmount::from(TOKEN_FEE),
                        },
                        LedgerFee {
                            ledger: icp_ledger(),
                            fee: TokenAmount::from(10_u32),
                        },
                    ],
                },
            },
            at(2),
        )
        .unwrap();
        let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        request.terms.premium = TokenAmount::from(8 * FEE);
        let id = open(&request);
        let fake = Fake::default();
        fake.quotes
            .borrow_mut()
            .insert(token_pool(), TokenAmount::from(400_030_u64));
        fake.quotes
            .borrow_mut()
            .insert(icp_pool(), TokenAmount::from(4_020_000_u64));
        fake.outs
            .borrow_mut()
            .insert(token_pool(), TokenAmount::from(400_030_u64));
        fake.outs
            .borrow_mut()
            .insert(icp_pool(), TokenAmount::from(4_010_000_u64));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        // the second leg sells 400,000 (three ICP fees less) and must give 2,010,000 of the
        // 4,020,000 quoted: half, 200,000, and the three fees for the first leg's minimum
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "quote 4998000".to_string(),
                "quote 400000".to_string(),
                "quote 4998000".to_string(),
                "approve 4999000".to_string(),
                "swap 4998000 at least 200030".to_string(),
                "quote 400000".to_string(),
                "approve 400010".to_string(),
                "swap 400000 at least 2010000".to_string(),
            ]
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.received, Some(TokenAmount::from(4_000_000_u64)));
        assert_eq!(payout.status, PayoutStatus::Waiting);
        let legs = payout.legs.unwrap();
        assert!(legs.swap.out.is_some());
        assert!(legs.second.unwrap().out.is_some());
    });
}

fn canister() -> EvmAddress {
    EvmAddress::new([0x36; 20])
}

/// The pool chain's USDC in `config()`: Base's.
fn base_usdc() -> EvmAddress {
    "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap()
}

/// A payout plan for `planned`: USDC on Base paid out to `recipient()`, at least `min_out`,
/// no step, the order's deadline 600 s.
fn plan_order(planned: TokenAmount, min_out: TokenAmount) -> types::swapic::Order {
    use alloy_primitives::{FixedBytes, U256};
    use types::swapic::{addr, Order, Payout};
    Order {
        r#ref: FixedBytes([0x61; 32]),
        payer: addr(canister()),
        tokenIn: addr(base_usdc()),
        amountIn: U256::from_be_bytes(planned.to_be_bytes()),
        deadline: U256::from(600_u64),
        steps: vec![],
        payout: Payout {
            token: addr(base_usdc()),
            recipient: addr(recipient()),
            minOut: U256::from_be_bytes(min_out.to_be_bytes()),
        },
    }
}

/// A planned swap of `amount_in` of `token_in` with `order` as its plan, 100 bps of
/// slippage, its deadline 15 minutes away.
fn planned(
    token_in: Principal,
    amount_in: TokenAmount,
    order: &types::swapic::Order,
) -> SwapRequest {
    SwapRequest {
        caller: account(5).owner,
        token_in,
        amount_in,
        from: account(5),
        refund_to: account(6),
        slippage_bps: 100,
        deadline: at(10).checked_add(Duration::from_secs(900)).unwrap(),
        plan: types::swapic::encode_order(order),
        gas_limit: 400_000,
    }
}

fn opened_with(
    request: &SwapRequest,
    canister: Option<EvmAddress>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<Opening, PoolError> {
    read_state(|state| {
        check(
            state,
            &config(),
            request,
            at(10),
            canister,
            None,
            sanctioned,
        )
    })
}

fn opened(request: &SwapRequest) -> Result<Opening, PoolError> {
    opened_with(request, Some(canister()), |_| false)
}

/// Opens `request` as `swap_from_icp` does: its checks, then its line.
fn open_planned(request: &SwapRequest) -> u64 {
    let opening = opened(request).unwrap();
    let id = ledger().lines().next_id;
    record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(opening.terms)),
            terms: opening.payout,
            undo: Undo::Refund {
                to: opening.terms.refund_to,
            },
            plan: Some(Box::new(opening.plan)),
        },
        at(10),
    )
    .unwrap();
    id
}

/// What 100 ckUSDC pays at the launch settings: less 10 bps and 5 cents.
fn ckusdc_payout() -> TokenAmount {
    TokenAmount::from(99_850_000_u64)
}

/// Every rule `swap_from_icp` checks, before anything is recorded: the plan read, the
/// terms its delivery makes, the window, the first minimum, the Base pocket.
#[test]
fn each_refusal_comes_before_anything_is_recorded() {
    on_fresh_memory(|| {
        crate::storage::init();
        let order = plan_order(ckusdc_payout(), usdc(1));
        let good = planned(ckusdc(), usdc(100), &order);
        // no settings at all
        assert!(matches!(opened(&good), Err(PoolError::Closed(_))));
        pool_with(usdc(500));
        assert_eq!(opened(&good).unwrap().path, RoutePath::Ckusdc);
        let with = |change: &dyn Fn(&mut SwapRequest)| {
            let mut request = good.clone();
            change(&mut request);
            opened(&request)
        };
        let plan = |order: types::swapic::Order| types::swapic::encode_order(&order);
        let refused =
            |answer: Result<Opening, PoolError>| matches!(answer, Err(PoolError::NotAllowed(_)));
        assert_eq!(
            with(&|r| r.token_in = Principal::from_slice(&[0x99; 10])),
            Err(PoolError::TokenNotListed(Principal::from_slice(
                &[0x99; 10]
            )))
        );
        assert!(refused(with(&|r| r.amount_in = TokenAmount::ZERO)));
        assert!(refused(with(&|r| r.slippage_bps = 501)));
        assert_eq!(
            with(&|r| r.deadline = at(10)
                .checked_add(MIN_DEADLINE)
                .unwrap()
                .checked_sub(Duration::from_nanos(1))
                .unwrap()),
            Err(PoolError::Expired)
        );
        assert!(refused(with(&|r| r.deadline = at(10)
            .checked_add(MAX_DEADLINE)
            .unwrap()
            .checked_add(Duration::from_nanos(1))
            .unwrap())));
        assert!(refused(with(&|r| r.plan = vec![0x0a; 96])));
        // its delivery: never one of Swapic's own contracts, nor the canister
        let to = |recipient: EvmAddress| {
            let mut order = order.clone();
            order.payout.recipient = types::swapic::addr(recipient);
            plan(order)
        };
        assert!(refused(with(&|r| r.plan = to(swapic()))));
        assert!(refused(with(&|r| r.plan = to(canister()))));
        // the payer is the canister; the order's deadline within the payout's
        let mut stranger = order.clone();
        stranger.payer = types::swapic::addr(recipient());
        assert!(refused(with(&|r| r.plan = plan(stranger.clone()))));
        let mut late = order.clone();
        late.deadline = alloy_primitives::U256::from(901_u64);
        assert!(refused(with(&|r| r.plan = plan(late.clone()))));
        // a ckUSDC swap's exact payout more than 5% from the plan, either way
        for amount in [94_000_000_u64, 106_000_000] {
            let far = plan_order(TokenAmount::from(amount), usdc(1));
            assert!(refused(with(&|r| r.plan = plan(far.clone()))), "{amount}");
        }
        // a first minimum above what the swap pays
        let greedy = plan_order(ckusdc_payout(), usdc(100));
        assert!(refused(with(&|r| r.plan = plan(greedy.clone()))));
        // no canister address yet
        assert!(matches!(
            opened_with(&good, None, |_| false),
            Err(PoolError::Unavailable(_))
        ));
        // sanctioned: the recipient, the caller
        assert!(matches!(
            opened_with(&good, Some(canister()), |address| *address
                == Address::from(recipient())),
            Err(PoolError::Sanctioned(_))
        ));
        let caller = good.caller.to_text();
        assert!(matches!(
            opened_with(&good, Some(canister()), |address| address.as_str()
                == caller),
            Err(PoolError::Sanctioned(_))
        ));
        // nothing was recorded
        assert_eq!(ledger().lines().next_id, 0);
        assert!(ledger().lines().open.is_empty());
    });
}

/// The Base pocket must pay the plan and 5% (and a ckUSDC swap's own payout) behind an empty
/// queue, else nothing is opened.
#[test]
fn a_swap_the_base_pocket_cannot_pay_is_refused_at_open() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(104));
        // 99.85 and 5% is 104.84...: refused
        let request = planned(ckusdc(), usdc(100), &plan_order(ckusdc_payout(), usdc(1)));
        assert!(matches!(
            opened(&request),
            Err(PoolError::NotAllowed(why)) if why.contains("Base pocket")
        ));
    });
}

/// The payout's terms are the plan's own delivery, the deadline the user's; the swap's least
/// ckUSDC, premium and fee are the canister's.
#[test]
fn the_payout_terms_are_read_from_the_plan() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let order = plan_order(ckusdc_payout(), usdc(1));
        let request = planned(ckusdc(), usdc(100), &order);
        let opening = opened(&request).unwrap();
        assert_eq!(
            opening.payout,
            PayoutTerms {
                dst_chain: ChainId::BASE,
                recipient: recipient(),
                token_out: base_usdc(),
                min_out: usdc(1),
                deadline: request.deadline,
            }
        );
        assert_eq!(opening.terms.min_usdc, usdc(100));
        assert_eq!(opening.terms.premium, TokenAmount::ZERO);
        assert_eq!(opening.terms.network_fee, TokenAmount::from(50_000_u32));
        assert_eq!(opening.terms.planned, Some(ckusdc_payout()));
        assert_eq!(opening.terms.slippage_bps, Some(100));
        assert_eq!(opening.plan.order, types::swapic::encode_order(&order));
        assert_eq!(opening.plan.gas_limit, 400_000);
        assert_eq!(opening.plan.deadline_s, 600);
    });
}

/// A plan paying the canister a fee step is refused: the canister pays itself nothing.
#[test]
fn a_plan_with_a_fee_step_is_refused() {
    use alloy_sol_types::SolCall;
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let mut order = plan_order(ckusdc_payout(), usdc(1));
        order.steps.push(types::swapic::Step {
            target: types::swapic::addr(base_usdc()),
            data: types::swapic::transferCall {
                to: types::swapic::addr(canister()),
                amount: alloy_primitives::U256::from(1_000_u64),
            }
            .abi_encode()
            .into(),
            token: alloy_primitives::Address::ZERO,
            min: alloy_primitives::U256::ZERO,
            writes: vec![],
        });
        assert!(matches!(
            opened(&planned(ckusdc(), usdc(100), &order)),
            Err(PoolError::NotAllowed(why)) if why.contains("fee step")
        ));
    });
}

/// M-01: no per swap cap on the canister any more; only what the Base pocket can pay.
#[test]
fn a_swap_above_the_old_per_swap_cap_opens() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(2_000));
        let cap = ledger().settings.unwrap().per_swap_cap;
        assert!(usdc(1_500) > cap);
        let payout = TokenAmount::from(1_498_450_000_u64);
        let request = planned(ckusdc(), usdc(1_500), &plan_order(payout, usdc(1)));
        assert!(opened(&request).is_ok());
    });
}

/// A planned ckUSDC swap: pulled, its premium the settings' share of the 100 it made, its
/// payout waiting with exactly that amount.
#[test]
fn a_planned_ckusdc_swap_waits_with_the_premium_from_its_amount() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let id = open_planned(&planned(
            ckusdc(),
            usdc(100),
            &plan_order(ckusdc_payout(), usdc(1)),
        ));
        let fake = Fake::default();
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Waiting);
        assert_eq!(payout.amount, Some(ckusdc_payout()));
        assert_eq!(payout.received, Some(usdc(100)));
        assert!(payout.plan.is_some());
    });
}

/// A planned swap never queues: one the Base pocket cannot pay any more by the time its
/// amount is known (another payout took it meanwhile) is undone at once, all its ckUSDC back.
#[test]
fn a_planned_swap_the_base_pocket_cannot_pay_any_more_is_undone_at_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let id = open_planned(&planned(
            ckusdc(),
            usdc(100),
            &plan_order(ckusdc_payout(), usdc(1)),
        ));
        // an older swap takes the Base pocket meanwhile
        let mut other = request(ckusdc(), usdc(450), usdc(450));
        other.terms.premium = usdc(1);
        let other = open(&other);
        block_on(drive(&Fake::default(), other, clock, &open_pool)).unwrap();
        assert_eq!(
            pool_store::payout(other).unwrap().status,
            PayoutStatus::Waiting
        );
        let fake = Fake::default();
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Undone(BASE_SHORT.to_string()));
        let transfers: Vec<_> = fake.transfers.borrow().values().copied().collect();
        assert_eq!(
            transfers,
            vec![(
                ckusdc(),
                account(6),
                usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap()
            )]
        );
        assert_eq!(ledger().lines().queue_len, 0);
    });
}

/// A planned token swap: its sale's floor is the ckUSDC the plan needs less half the
/// slippage, and its premium the settings' share of what the sale made.
#[test]
fn a_planned_token_sale_has_half_the_slippage_as_its_floor() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let request = planned(
            token(),
            TokenAmount::from(5_000_000_u64),
            &plan_order(usdc(2), usdc(1)),
        );
        let opening = opened(&request).unwrap();
        // 2.05 / 0.999 is 2,052,053; less 50 bps, rounded up
        assert_eq!(opening.terms.min_usdc, TokenAmount::from(2_041_793_u64));
        let id = open_planned(&request);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "quote 4998000".to_string(),
                "approve 4999000".to_string(),
                "swap 4998000 at least 2051793".to_string(),
            ]
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.received, Some(TokenAmount::from(2_090_000_u64)));
        assert_eq!(payout.amount, Some(TokenAmount::from(2_037_910_u64)));
        assert_eq!(payout.status, PayoutStatus::Waiting);
    });
}

/// The real KyberSwap route on Base for 500 USDC and the scaler's own answers for it
/// (`pool/plan/samples/scaler_base.json`).
fn scaler_sample() -> serde_json::Value {
    serde_json::from_str(include_str!("../plan/samples/scaler_base.json")).unwrap()
}

fn sample_hex(value: &serde_json::Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

/// The scaler's rewritten route for `amount`, as its answer carries it.
fn scaled_route(amount: u64) -> Vec<u8> {
    let sample = scaler_sample();
    let found = sample["answers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|answer| {
            answer["new_amount"]
                .as_str()
                .and_then(|text| text.parse().ok())
                == Some(amount)
        })
        .unwrap()
        .clone();
    crate::pool::plan::scaled_of(&sample_hex(&found["result"])).unwrap()
}

/// A payout plan for 500 USDC through the real KyberSwap route to WETH on Base.
fn swap_plan() -> types::swapic::Order {
    use alloy_primitives::U256;
    let mut order = plan_order(usdc(500), usdc(1));
    order.steps = vec![types::swapic::Step {
        target: types::swapic::addr(types::swapic::KYBER_ROUTER),
        data: sample_hex(&scaler_sample()["route_calldata"]).into(),
        token: types::swapic::addr(base_usdc()),
        min: U256::ZERO,
        writes: vec![],
    }];
    order.payout.token = "0x4200000000000000000000000000000000000006"
        .parse()
        .unwrap();
    order.payout.minOut = U256::from(200_000_000_000_000_000_u64);
    order
}

/// 502.202203 ckUSDC pays exactly 501.65 USDC: less 10 bps (502,203) and 5 cents.
const PAYS_501_65: u64 = 502_202_203;

/// ckUSDC in, a plan with no swap: its plan fits the exact amount and is handed to be signed
/// in the same drive, its order spending exactly the payout, with no read at all.
#[test]
fn a_ckusdc_plan_that_fits_is_handed_to_sign_at_its_exact_amount() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let order = plan_order(ckusdc_payout(), usdc(1));
        let id = open_planned(&planned(ckusdc(), usdc(100), &order));
        let fake = Fake::default();
        let fitted = block_on(drive(&fake, id, clock, &open_pool))
            .unwrap()
            .expect("the plan fits");
        assert_eq!(
            fitted.order.amountIn,
            alloy_primitives::U256::from(99_850_000_u64)
        );
        assert_eq!(
            fitted.pinned.order,
            types::swapic::encode_order(&fitted.order)
        );
        assert_eq!(
            *fake.calls.borrow(),
            vec!["pull ckUSDC 100000000".to_string()]
        );
        // the payout waits, its USDC reserved, for the caller's signature
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Waiting);
        assert_eq!(ledger().pockets.base.reserved, ckusdc_payout());
    });
}

/// ckUSDC in, a plan through KyberSwap on Base: the scaler is read once the pull answered
/// (never beside it), for the exact payout, and its real answer is the signed step.
#[test]
fn a_kyber_plan_is_rewritten_by_the_scaler_after_the_pull() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(600));
        let id = open_planned(&planned(
            ckusdc(),
            TokenAmount::from(PAYS_501_65),
            &swap_plan(),
        ));
        let fake = Fake::default();
        *fake.scaled.borrow_mut() = Some(Ok(scaled_route(501_650_000)));
        let fitted = block_on(drive(&fake, id, clock, &open_pool))
            .unwrap()
            .expect("the plan fits");
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                format!("pull ckUSDC {PAYS_501_65}"),
                "scale 501650000".to_string()
            ]
        );
        assert_eq!(
            fitted.order.amountIn,
            alloy_primitives::U256::from(501_650_000_u64)
        );
        assert_eq!(
            fitted.order.steps[0].data.to_vec(),
            scaled_route(501_650_000)
        );
    });
}

/// A token in: the scaler is read after the last leg's answer, which is when the amount is
/// first known.
#[test]
fn a_token_plan_reads_the_scaler_after_its_last_leg() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(600));
        let id = open_planned(&planned(
            token(),
            TokenAmount::from(900_000_000_u64),
            &swap_plan(),
        ));
        let fake = Fake::default();
        // the leg's output less the ckUSDC fee pays exactly 501.65
        let out = TokenAmount::from(PAYS_501_65 + FEE);
        fake.quote.set(Some(out));
        fake.out.set(Some(out));
        *fake.scaled.borrow_mut() = Some(Ok(scaled_route(501_650_000)));
        let fitted = block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert!(fitted.is_some());
        let calls = fake.calls.borrow();
        assert!(calls[calls.len() - 2].starts_with("swap "), "{calls:?}");
        assert_eq!(calls.last().unwrap(), "scale 501650000");
    });
}

/// A sale far from its plan, with the Base pocket covering it: the payout waits for the
/// watcher's order, as before the plan flow, and nothing is read.
#[test]
fn a_sale_far_from_its_plan_waits_without_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let id = open_planned(&planned(
            token(),
            TokenAmount::from(5_000_000_u64),
            &plan_order(usdc(2), usdc(1)),
        ));
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_500_000_u64)));
        fake.out.set(Some(TokenAmount::from(2_500_000_u64)));
        assert_eq!(block_on(drive(&fake, id, clock, &open_pool)).unwrap(), None);
        assert_eq!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Waiting
        );
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("scale")));
    });
}

/// A plan whose order deadline is under a minute away when the amount is known: it waits.
#[test]
fn a_plan_whose_order_deadline_is_too_close_waits_without_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let order = plan_order(ckusdc_payout(), usdc(1));
        let id = open_planned(&planned(ckusdc(), usdc(100), &order));
        let late = || Timestamp::from_secs(541).unwrap();
        assert_eq!(
            block_on(drive(&Fake::default(), id, late, &open_pool)).unwrap(),
            None
        );
        assert_eq!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Waiting
        );
    });
}

/// The scaler refusing the amount, a read that failed, or a rewritten route that is another
/// swap: the payout waits for the watcher's order.
#[test]
fn a_refused_failed_or_tampered_scaler_answer_waits_without_the_plan() {
    use alloy_sol_types::SolCall;
    let mut tampered = types::swapic::swapCall::abi_decode(&scaled_route(501_650_000)).unwrap();
    tampered.execution.desc.dstReceiver = alloy_primitives::Address::repeat_byte(0xbd);
    for answer in [
        Err("the scaler refused the amount".to_string()),
        Err("the scaler's read: Unreachable".to_string()),
        Ok(tampered.abi_encode()),
    ] {
        on_fresh_memory(move || {
            crate::storage::init();
            pool_with(usdc(600));
            let id = open_planned(&planned(
                ckusdc(),
                TokenAmount::from(PAYS_501_65),
                &swap_plan(),
            ));
            let fake = Fake::default();
            *fake.scaled.borrow_mut() = Some(answer);
            assert_eq!(block_on(drive(&fake, id, clock, &open_pool)).unwrap(), None);
            assert_eq!(
                pool_store::payout(id).unwrap().status,
                PayoutStatus::Waiting
            );
        });
    }
}

/// A plan whose first step asks more than the payout once it is known (the premium rose
/// between the opening and the sale) could only revert: the payout is undone at once, all its
/// ckUSDC back.
#[test]
fn a_plan_whose_first_minimum_is_above_the_payout_undoes_at_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        // the plan asks exactly what 100 ckUSDC pays at 10 bps
        let order = plan_order(ckusdc_payout(), ckusdc_payout());
        let id = open_planned(&planned(ckusdc(), usdc(100), &order));
        let mut settings = ledger().settings.unwrap();
        settings.premium_bps = 20;
        record_at(PoolStep::SettingsSet { settings }, at(11)).unwrap();
        let fake = Fake::default();
        assert_eq!(block_on(drive(&fake, id, clock, &open_pool)).unwrap(), None);
        let payout = pool_store::payout(id).unwrap();
        assert!(matches!(payout.status, PayoutStatus::Undone(why) if why.contains("first step")));
        let transfers: Vec<_> = fake.transfers.borrow().values().copied().collect();
        assert_eq!(
            transfers,
            vec![(
                ckusdc(),
                account(6),
                usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap()
            )]
        );
    });
}

/// The review (2026-10-23): a scaler read that failed once does not give up the fast path.
/// The try is counted in the payout, the plan is tried again, and fits; after the last try,
/// no read is made any more.
#[test]
fn a_plan_whose_scaler_read_failed_is_tried_again_and_fits() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(600));
        let id = open_planned(&planned(
            ckusdc(),
            TokenAmount::from(PAYS_501_65),
            &swap_plan(),
        ));
        let fake = Fake::default();
        *fake.scaled.borrow_mut() = Some(Err("the scaler's read: Unreachable".to_string()));
        assert_eq!(block_on(drive(&fake, id, clock, &open_pool)).unwrap(), None);
        assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(1));
        // tried again: the read answers now
        *fake.scaled.borrow_mut() = Some(Ok(scaled_route(501_650_000)));
        let fitted = block_on(refit(&fake, id, clock, &open_pool))
            .unwrap()
            .expect("the plan fits");
        assert_eq!(
            fitted.order.amountIn,
            alloy_primitives::U256::from(501_650_000_u64)
        );
        assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(1));
        // two more failed tries: the last; then nothing is read any more
        *fake.scaled.borrow_mut() = Some(Err("the scaler's read: Unreachable".to_string()));
        for _ in 0..2 {
            assert_eq!(block_on(refit(&fake, id, clock, &open_pool)).unwrap(), None);
        }
        assert_eq!(
            pool_store::payout(id).unwrap().fit_tries,
            Some(types::pool::payout::MAX_FIT_TRIES)
        );
        let reads = fake
            .calls
            .borrow()
            .iter()
            .filter(|call| call.starts_with("scale"))
            .count();
        assert_eq!(block_on(refit(&fake, id, clock, &open_pool)).unwrap(), None);
        assert_eq!(
            fake.calls
                .borrow()
                .iter()
                .filter(|call| call.starts_with("scale"))
                .count(),
            reads
        );
        assert_eq!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Waiting
        );
    });
}

/// The review (2026-10-24): a plan that fits but whose signature is refused (a halt, the gas
/// rule, a stale reading) counts as a try, so the pass refits it at most `MAX_FIT_TRIES`
/// times, each with its scaler read, then leaves it to the watcher's relist.
#[test]
fn a_fitted_plan_whose_signature_is_refused_counts_a_try() {
    use types::pool::payout::MAX_FIT_TRIES;
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let order = plan_order(ckusdc_payout(), usdc(1));
        let id = open_planned(&planned(ckusdc(), usdc(100), &order));
        let fake = Fake::default();
        crate::storage::halt::set_halted(true);
        let busy = Busy::take(evm_payout_ref(id).into_bytes()).unwrap();
        let fitted = block_on(drive(&fake, id, clock, &open_pool))
            .unwrap()
            .expect("the plan fits");
        assert!(!block_on(crate::pool::plan::sign_fitted(
            &busy, id, fitted, clock
        )));
        assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(1));
        for tries in 2..=MAX_FIT_TRIES {
            let fitted = block_on(refit(&fake, id, clock, &open_pool))
                .unwrap()
                .expect("the plan fits");
            assert!(!block_on(crate::pool::plan::sign_fitted(
                &busy, id, fitted, clock
            )));
            assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(tries));
        }
        // its tries spent: never refitted again, the watcher's order it waits for
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Waiting);
        assert!(!crate::pool::queue::refittable(&payout, clock()));
        assert_eq!(block_on(refit(&fake, id, clock, &open_pool)).unwrap(), None);
    });
}
