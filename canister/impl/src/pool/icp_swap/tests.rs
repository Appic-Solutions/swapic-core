use super::*;
use crate::ic::Dedup;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, base_stake, ckusdc, funded_pool, payer, settings, usdc, FEE,
};
use crate::storage::pool::{ledger, record_at};
use crate::test_util::block_on;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::{IcpRoute, IcpRouteVia, IcpRoutes, LedgerFee, PayoutSettings};
use types::ChainId;

fn clock() -> Timestamp {
    at(10)
}

/// Payout `id`'s plan decided now, as the pass's `queue::next` hands it to `refit`.
fn decided(id: u64) -> Result<crate::pool::plan::Decided, crate::pool::plan::Fit> {
    crate::pool::plan::decide_payout(&pool_store::payout(id).unwrap(), clock().as_secs().get())
        .expect("a planned payout with its amount")
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
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
        usdc_decimals: None,
        across: None,
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
                pool_tokens: Vec::new(),
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
            ck: Default::default(),
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
    /// A token whose approval may stand: what the pool may still pull of it, or that the read
    /// fails. None: an approval is for the swap alone.
    standing: Cell<Option<Result<TokenAmount, ()>>>,
    /// The ledger refuses a standing approval.
    standing_refused: Cell<bool>,
    /// The round read beside the pull, for the leg that asks.
    ahead: Cell<Option<crate::pool::payout::Round>>,
    /// Whether reads beside a sale land before it answers, as the scaler answers them.
    fit_read_ahead: Cell<bool>,
    /// What they read: for which route and amount, and what the fit then finds.
    scaled_ahead: RefCell<Option<ScaledAhead>>,
}

/// A read beside a sale: its route, its amount, and the fit it finds.
type ScaledAhead = (Vec<u8>, TokenAmount, Vec<u8>);

/// The scaler as the fake answers it, and a payout order's checks on `config()`.
impl PlanReads for Fake {
    async fn scaled(&self, _route: &[u8], amount: TokenAmount) -> Result<Vec<u8>, String> {
        self.calls.borrow_mut().push(format!("scale {amount}"));
        self.scaled
            .borrow()
            .clone()
            .unwrap_or_else(|| Err("no scaler here".to_string()))
    }

    fn read_fit_ahead(&self, _id: u64, route: Option<Vec<u8>>, amount: TokenAmount) {
        self.calls.borrow_mut().push(format!("read ahead {amount}"));
        if self.fit_read_ahead.get() {
            if let (Some(route), Some(Ok(scaled))) = (route, self.scaled.borrow().clone()) {
                *self.scaled_ahead.borrow_mut() = Some((route, amount, scaled));
            }
        }
    }

    fn scaled_ahead(&self, _id: u64, route: &[u8], amount: TokenAmount) -> Option<Vec<u8>> {
        let ahead = self.scaled_ahead.borrow();
        let (read_route, read_amount, scaled) = ahead.as_ref()?;
        (read_route == route && *read_amount == amount).then(|| scaled.clone())
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

    fn round_read_ahead(&self, _spec: &LegSpec) -> Option<crate::pool::payout::Round> {
        self.ahead.take()
    }

    fn may_stand(&self, _token: Principal) -> bool {
        self.standing.get().is_some()
    }

    async fn allowance(
        &self,
        _ledger: Principal,
        _spender: Principal,
    ) -> Result<TokenAmount, CallError> {
        self.calls.borrow_mut().push("allowance".to_string());
        self.standing
            .get()
            .expect("read only for a token that may stand")
            .map_err(|()| CallError::Unknown("the ledger did not answer".to_string()))
    }

    async fn approve_standing(
        &self,
        _ledger: Principal,
        _spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError> {
        self.calls
            .borrow_mut()
            .push(format!("approve standing {amount}"));
        if self.standing_refused.get() {
            return Err(CallError::Refused(
                "the ledger takes no such approval".into(),
            ));
        }
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
                    pool_tokens: Vec::new(),
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

/// `pool_with`, and `token()` routed through ICP.
fn routed_through_icp() {
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
                pool_tokens: Vec::new(),
            },
        },
        at(2),
    )
    .unwrap();
}

/// What a swap through ICP sold on its first leg, in the token.
const SOLD: u64 = 4_998_000;

/// A swap through ICP whose first leg ran: its ICP is at the canister, its second leg next.
fn past_the_first_leg() -> u64 {
    routed_through_icp();
    let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
    request.terms.premium = TokenAmount::from(8 * FEE);
    let id = open(&request);
    let r#ref = evm_payout_ref(id);
    record_at(
        PoolStep::PayoutPulled {
            id,
            block: 7,
            attempt: Some(PayoutAttempt {
                created_at: at(10),
                memo: r#ref.into_bytes(),
                path: PayoutPath::IcpswapVia {
                    first: token_pool(),
                    second: icp_pool(),
                    icp: icp_ledger(),
                },
                amount: Some(TokenAmount::from(5_000_000_u64)),
                token: Some(PrincipalBytes(token())),
            }),
        },
        at(10),
    )
    .unwrap();
    record_at(
        PoolStep::SwapBegun {
            r#ref,
            before: Some(types::pool::PoolHeld {
                ckusdc: TokenAmount::ZERO,
                token: TokenAmount::ZERO,
            }),
            leg: None,
            amount_in: Some(TokenAmount::from(SOLD)),
        },
        at(10),
    )
    .unwrap();
    record_at(
        PoolStep::Swapped {
            r#ref,
            out: TokenAmount::from(400_030_u64),
            send: None,
            leg: None,
        },
        at(10),
    )
    .unwrap();
    id
}

/// The second leg short, and the token's pool quoting `back` for the way back.
fn second_leg_short(back: u64) -> Fake {
    let fake = Fake::default();
    fake.quotes
        .borrow_mut()
        .insert(icp_pool(), TokenAmount::from(1_000_u64));
    fake.quotes
        .borrow_mut()
        .insert(token_pool(), TokenAmount::from(back));
    fake.outs
        .borrow_mut()
        .insert(token_pool(), TokenAmount::from(back));
    fake
}

fn swaps(fake: &Fake) -> Vec<String> {
    fake.calls
        .borrow()
        .iter()
        .filter(|call| call.starts_with("swap"))
        .cloned()
        .collect()
}

/// F2: someone pushed the token's pool while the second leg cannot pay. The way back quotes
/// below its least, so it never sells: once more on the next pass, then a human, who has it
/// quoted again once the pool is healthy. The door here is the pool's own: `swap_from_icp`
/// and the ck door share this path.
#[test]
fn a_way_back_with_the_pool_pushed_stops_and_does_not_sell() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = past_the_first_leg();
        let fake = second_leg_short(1_000);
        assert!(matches!(
            block_on(drive(&fake, id, clock, &open_pool)),
            Err(Stop::Later)
        ));
        assert_eq!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Swapping
        );
        assert_eq!(
            pool_store::payout(id)
                .unwrap()
                .legs
                .unwrap()
                .leg(Some(LEG_BACK))
                .declined,
            1
        );
        // the next pass quotes again and stops for a human
        assert!(matches!(
            block_on(drive(&fake, id, clock, &open_pool)),
            Err(Stop::Failed(_))
        ));
        assert!(matches!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Failed(_)
        ));
        assert!(swaps(&fake).is_empty(), "the way back sold into the push");

        // the pool healthy again: a controller has it quoted anew, and it goes back
        crate::pool::resolve::resolve(
            false,
            true,
            id,
            settlement_api::types::pool::PayoutResolution::Retry,
            at(11),
        )
        .unwrap();
        let healthy = second_leg_short(SOLD / 1_000 * 994);
        block_on(drive(&healthy, id, clock, &open_pool)).unwrap();
        assert_eq!(
            swaps(&healthy),
            vec![format!(
                "swap 400000 at least {}",
                way_back_least(TokenAmount::from(SOLD)).unwrap()
            )]
        );
        assert!(matches!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Undone(_)
        ));
    });
}

/// A way back's least leaves room for the two pool fees of the trip and a price move, so a
/// healthy way back never waits for a human.
#[test]
fn a_way_backs_least_is_five_percent_below_what_was_sold() {
    let least = way_back_least(TokenAmount::from(10_000_000_u64)).unwrap();
    assert_eq!(least, TokenAmount::from(9_500_000_u64));
    assert_eq!(way_back_least(TokenAmount::ZERO), None);
    // 0.3% each way
    let round_trip = 10_000_000_u64 * 997 / 1_000 * 997 / 1_000;
    assert!(TokenAmount::from(round_trip) > least);
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
        memo: Default::default(),
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
            ck: Default::default(),
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
                "read ahead 2037910".to_string(),
                "swap 4998000 at least 2051793".to_string(),
            ]
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.received, Some(TokenAmount::from(2_090_000_u64)));
        assert_eq!(payout.amount, Some(TokenAmount::from(2_037_910_u64)));
        assert_eq!(payout.status, PayoutStatus::Waiting);
    });
}

/// A planned sale of a token whose approval may stand, driven as far as it goes.
fn standing_sale(standing: Result<TokenAmount, ()>, quote: u64) -> (u64, Fake) {
    pool_with(usdc(500));
    let request = planned(
        token(),
        TokenAmount::from(5_000_000_u64),
        &plan_order(usdc(2), usdc(1)),
    );
    let id = open_planned(&request);
    let fake = Fake::default();
    fake.standing.set(Some(standing));
    fake.quote.set(Some(TokenAmount::from(quote)));
    fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
    block_on(drive(&fake, id, clock, &open_pool)).unwrap();
    (id, fake)
}

/// A standing approval that covers the swap: no approval, no line for one, no fee spent, and
/// the sale goes on. Its allowance is read in the quote's round, never after it.
#[test]
fn a_standing_approval_that_covers_the_swap_asks_for_no_other() {
    on_fresh_memory(|| {
        crate::storage::init();
        let (id, fake) = standing_sale(Ok(TokenAmount::from(4_999_000_u64)), 2_100_000);
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "quote 4998000".to_string(),
                "allowance".to_string(),
                "read ahead 2037910".to_string(),
                "swap 4998000 at least 2051793".to_string(),
            ]
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.received, Some(TokenAmount::from(2_090_000_u64)));
        assert_eq!(payout.status, PayoutStatus::Waiting);
        let leg = payout.legs.unwrap().swap;
        assert_eq!(
            leg.approvals, None,
            "no approval was given, none is counted"
        );
    });
}

/// An allowance one short of the swap, or one that could not be read, is approved for ten swaps
/// of this size, with its line: a swap never leans on an allowance it does not know covers it.
#[test]
fn a_standing_approval_that_falls_short_is_given_for_ten_swaps() {
    for standing in [Ok(TokenAmount::from(4_998_999_u64)), Err(())] {
        on_fresh_memory(move || {
            crate::storage::init();
            let (id, fake) = standing_sale(standing, 2_100_000);
            assert_eq!(
                *fake.calls.borrow(),
                vec![
                    "pull token 5000000".to_string(),
                    "quote 4998000".to_string(),
                    "allowance".to_string(),
                    "approve standing 49990000".to_string(),
                    "read ahead 2037910".to_string(),
                    "swap 4998000 at least 2051793".to_string(),
                ]
            );
            let leg = pool_store::payout(id).unwrap().legs.unwrap().swap;
            assert_eq!(leg.approvals, Some(1));
        });
    }
}

/// A price below the floor approves nothing and sells nothing, however the allowance stands: the
/// token goes back with no fee spent on an approval.
#[test]
fn a_quote_below_the_floor_approves_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        let (id, fake) = standing_sale(Ok(TokenAmount::ZERO), 1);
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.contains("approve")));
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("swap")));
        let payout = pool_store::payout(id).unwrap();
        assert!(
            matches!(payout.status, PayoutStatus::Undone(_)),
            "{:?}",
            payout.status
        );
    });
}

/// A sale the pool refuses after a standing approval: the refund is what it is after an approval
/// given for that swap, one fee short of what the canister holds: it can never pay out more than
/// the swap brought in.
#[test]
fn a_refused_sale_on_a_standing_approval_refunds_the_same() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let mut request = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        request.terms.premium = TokenAmount::from(4 * FEE);
        let id = open(&request);
        let fake = Fake::default();
        fake.standing.set(Some(Ok(TokenAmount::from(u64::MAX))));
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.contains("approve")));
        let transfers: Vec<_> = fake.transfers.borrow().values().copied().collect();
        assert_eq!(
            transfers,
            vec![(token(), account(6), TokenAmount::from(4_998_000_u64))]
        );
    });
}

/// A round read beside the pull: the leg asks the pool nothing more before it sells.
#[test]
fn a_round_read_beside_the_pull_goes_straight_to_the_sale() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let request = planned(
            token(),
            TokenAmount::from(5_000_000_u64),
            &plan_order(usdc(2), usdc(1)),
        );
        let id = open_planned(&request);
        let fake = Fake::default();
        fake.standing.set(Some(Ok(TokenAmount::ZERO)));
        fake.ahead.set(Some(crate::pool::payout::Round {
            quoted: TokenAmount::from(2_100_000_u64),
            before: types::pool::PoolHeld {
                ckusdc: TokenAmount::ZERO,
                token: TokenAmount::ZERO,
            },
            allowed: Some(TokenAmount::from(4_999_000_u64)),
        }));
        fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "read ahead 2037910".to_string(),
                "swap 4998000 at least 2051793".to_string(),
            ]
        );
    });
}

/// A planned sale whose round was read beside its pull, as `quoted` and `allowed`, with the pool
/// quoting `fresh` if it is asked again.
fn sale_read_ahead(quoted: u64, allowed: u64, fresh: u64) -> (u64, Fake) {
    pool_with(usdc(500));
    let request = planned(
        token(),
        TokenAmount::from(5_000_000_u64),
        &plan_order(usdc(2), usdc(1)),
    );
    let id = open_planned(&request);
    let fake = Fake::default();
    fake.standing.set(Some(Ok(TokenAmount::ZERO)));
    fake.ahead.set(Some(crate::pool::payout::Round {
        quoted: TokenAmount::from(quoted),
        before: types::pool::PoolHeld {
            ckusdc: TokenAmount::ZERO,
            token: TokenAmount::ZERO,
        },
        allowed: Some(TokenAmount::from(allowed)),
    }));
    fake.quote.set(Some(TokenAmount::from(fresh)));
    fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
    block_on(drive(&fake, id, clock, &open_pool)).unwrap();
    (id, fake)
}

/// A reading beside the pull with an allowance that falls short still approves.
#[test]
fn a_round_read_beside_the_pull_that_falls_short_still_approves() {
    on_fresh_memory(|| {
        crate::storage::init();
        let (_, fake) = sale_read_ahead(2_100_000, 4_998_999, 2_100_000);
        let calls = fake.calls.borrow();
        assert!(
            calls.iter().any(|call| call.contains("approve")),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.starts_with("swap")),
            "{calls:?}"
        );
    });
}

/// A low price read beside the pull never refunds by itself: a refund cannot be taken back, so
/// the pool is asked again. The price has recovered: the swap sells, and the user never paid for
/// a refund a price some seconds old would have forced.
#[test]
fn a_low_price_read_beside_the_pull_is_asked_again_before_it_refunds() {
    on_fresh_memory(|| {
        crate::storage::init();
        let (id, fake) = sale_read_ahead(1, u64::MAX, 2_100_000);
        let calls = fake.calls.borrow();
        assert!(calls.contains(&"quote 4998000".to_string()), "{calls:?}");
        assert!(
            calls.iter().any(|call| call.starts_with("swap")),
            "{calls:?}"
        );
        let payout = pool_store::payout(id).unwrap();
        assert_eq!(payout.status, PayoutStatus::Waiting);
    });
}

/// Asked again, the price is still low: then it refunds, with nothing approved or sold.
#[test]
fn a_low_price_still_low_when_asked_again_refunds() {
    on_fresh_memory(|| {
        crate::storage::init();
        let (id, fake) = sale_read_ahead(1, u64::MAX, 1);
        let calls = fake.calls.borrow();
        assert!(calls.contains(&"quote 4998000".to_string()), "{calls:?}");
        assert!(
            !calls.iter().any(|call| call.contains("approve")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("swap")),
            "{calls:?}"
        );
        assert!(matches!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Undone(_)
        ));
    });
}

/// A ledger that refuses a standing approval still sells: the approval falls back to this swap
/// alone, one line, one fee.
#[test]
fn a_ledger_that_refuses_a_standing_approval_is_approved_for_the_swap_alone() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let request = planned(
            token(),
            TokenAmount::from(5_000_000_u64),
            &plan_order(usdc(2), usdc(1)),
        );
        let id = open_planned(&request);
        let fake = Fake::default();
        fake.standing.set(Some(Ok(TokenAmount::ZERO)));
        fake.standing_refused.set(true);
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "pull token 5000000".to_string(),
                "quote 4998000".to_string(),
                "allowance".to_string(),
                "approve standing 49990000".to_string(),
                "approve 4999000".to_string(),
                "read ahead 2037910".to_string(),
                "swap 4998000 at least 2051793".to_string(),
            ]
        );
        let leg = pool_store::payout(id).unwrap().legs.unwrap().swap;
        assert_eq!(leg.approvals, Some(1));
    });
}

/// A token whose approval may not stand is approved for the swap alone, its allowance never read.
#[test]
fn a_token_that_may_not_stand_is_approved_for_the_swap_alone() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let request = planned(
            token(),
            TokenAmount::from(5_000_000_u64),
            &plan_order(usdc(2), usdc(1)),
        );
        let id = open_planned(&request);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(2_100_000_u64)));
        fake.out.set(Some(TokenAmount::from(2_100_000_u64)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        assert!(!fake.calls.borrow().contains(&"allowance".to_string()));
        assert!(fake.calls.borrow().contains(&"approve 4999000".to_string()));
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

/// A token sale as `a_token_plan_reads_the_scaler_after_its_last_leg` has it, its quote and its
/// output apart, with the reads beside it landing before it answers.
fn sale_with_reads_ahead(quote: u64, out: u64, pays: u64) -> Fake {
    pool_with(usdc(600));
    let id = open_planned(&planned(
        token(),
        TokenAmount::from(900_000_000_u64),
        &swap_plan(),
    ));
    let fake = Fake::default();
    fake.fit_read_ahead.set(true);
    fake.quote.set(Some(TokenAmount::from(quote)));
    fake.out.set(Some(TokenAmount::from(out)));
    *fake.scaled.borrow_mut() = Some(Ok(scaled_route(pays)));
    let fitted = block_on(drive(&fake, id, clock, &open_pool)).unwrap();
    assert_eq!(
        fitted.map(|fitted| fitted.order.steps[0].data.to_vec()),
        Some(scaled_route(pays)),
        "the order carries the rewrite for the payout"
    );
    fake
}

/// The sale gives what its quote said: the rewrite read beside it is the fit's, and after the
/// sale nothing is left to read.
#[test]
fn a_sale_that_gives_its_quote_finds_its_rewrite_read_ahead() {
    on_fresh_memory(|| {
        crate::storage::init();
        let out = PAYS_501_65 + FEE;
        let fake = sale_with_reads_ahead(out, out, 501_650_000);
        let calls = fake.calls.borrow();
        assert_eq!(calls[calls.len() - 2], "read ahead 501650000", "{calls:?}");
        assert!(calls.last().unwrap().starts_with("swap "), "{calls:?}");
        assert!(
            !calls.iter().any(|call| call.starts_with("scale")),
            "{calls:?}"
        );
    });
}

/// The sale gives other than its quote (someone traded in those seconds): what was read ahead is
/// for another amount and is not used, and the fit reads for the amount the sale really made.
#[test]
fn a_sale_that_gives_other_than_its_quote_reads_its_rewrite_again() {
    on_fresh_memory(|| {
        crate::storage::init();
        let out = PAYS_501_65 + FEE;
        let fake = sale_with_reads_ahead(out + 1_000_000, out, 501_650_000);
        let calls = fake.calls.borrow();
        assert!(
            calls.iter().any(|call| call.starts_with("read ahead ")),
            "{calls:?}"
        );
        assert!(
            !calls.contains(&"read ahead 501650000".to_string()),
            "{calls:?}"
        );
        assert_eq!(calls.last().unwrap(), "scale 501650000", "{calls:?}");
    });
}

/// Nothing is read beside a pull or a sale that never goes out: a refused pull and a quote below
/// the floor start no read, since an outcall is the canister's to pay for.
#[test]
fn no_read_ahead_starts_before_a_sale_goes_out() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(600));
        let id = open_planned(&planned(
            token(),
            TokenAmount::from(900_000_000_u64),
            &swap_plan(),
        ));
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(1_u32)));
        block_on(drive(&fake, id, clock, &open_pool)).unwrap();
        let calls = fake.calls.borrow();
        assert!(
            !calls.iter().any(|call| call.starts_with("read ahead")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("swap")),
            "{calls:?}"
        );
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
        let fitted = block_on(refit(&fake, id, decided(id), clock, &open_pool))
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
            assert_eq!(
                block_on(refit(&fake, id, decided(id), clock, &open_pool)).unwrap(),
                None
            );
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
        assert_eq!(
            block_on(refit(&fake, id, decided(id), clock, &open_pool)).unwrap(),
            None
        );
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
        let busy = crate::pool::payout::Busy::take(evm_payout_ref(id).into_bytes()).unwrap();
        let fitted = block_on(drive(&fake, id, clock, &open_pool))
            .unwrap()
            .expect("the plan fits");
        assert!(!block_on(crate::pool::plan::sign_fitted(
            &busy, id, fitted, clock
        )));
        assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(1));
        for tries in 2..=MAX_FIT_TRIES {
            let fitted = block_on(refit(&fake, id, decided(id), clock, &open_pool))
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
        assert!(crate::pool::queue::refittable(&payout, clock()).is_none());
        assert_eq!(
            block_on(refit(&fake, id, decided(id), clock, &open_pool)).unwrap(),
            None
        );
    });
}

/// The review (2026-10-24): the pool's guard is read before a plan is fitted. While the pool
/// is closed no scaler is read and no try is spent; once it opens, the plan fits.
#[test]
fn a_plan_is_not_fitted_while_the_pool_is_closed() {
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
        let reads = || {
            fake.calls
                .borrow()
                .iter()
                .filter(|call| call.starts_with("scale"))
                .count()
        };
        let before = reads();
        *fake.scaled.borrow_mut() = Some(Ok(scaled_route(501_650_000)));
        let closed = || Err("the pool is closed".to_string());
        for _ in 0..5 {
            assert_eq!(
                block_on(refit(&fake, id, decided(id), clock, &closed)).unwrap(),
                None
            );
        }
        assert_eq!(reads(), before);
        assert_eq!(pool_store::payout(id).unwrap().fit_tries, Some(1));
        assert!(block_on(refit(&fake, id, decided(id), clock, &open_pool))
            .unwrap()
            .is_some());
    });
}

/// The review (2026-10-24): a plan that could only revert is undone whatever the pool's
/// guard (an undo pays nothing on Base), as the deadline undoes a payout: it never waits for
/// the pool to open. Its refund on ICP waits for the guard, as every refund does.
#[test]
fn a_plan_that_could_only_revert_is_undone_while_the_pool_is_closed() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool_with(usdc(500));
        let order = plan_order(ckusdc_payout(), ckusdc_payout());
        let id = open_planned(&planned(ckusdc(), usdc(100), &order));
        let mut settings = ledger().settings.unwrap();
        settings.premium_bps = 20;
        record_at(PoolStep::SettingsSet { settings }, at(11)).unwrap();
        let closed = || Err("the pool is closed".to_string());
        let fake = Fake::default();
        assert!(matches!(
            block_on(drive(&fake, id, clock, &closed)),
            Err(Stop::Retry(why)) if why.contains("closed")
        ));
        let payout = pool_store::payout(id).unwrap();
        assert!(
            matches!(&payout.status, PayoutStatus::Undoing(why) if why.contains("first step")),
            "{:?}",
            payout.status
        );
        // open, its refund goes out
        assert_eq!(block_on(drive(&fake, id, clock, &open_pool)).unwrap(), None);
        assert!(matches!(
            pool_store::payout(id).unwrap().status,
            PayoutStatus::Undone(_)
        ));
    });
}

/// `check`'s reading of a plan, which a consent message shares: bytes that are no order are
/// refused, and the delivery is the payout's chain, recipient, token and least out.
#[test]
fn a_plan_is_decoded_and_its_delivery_read_as_check_reads_them() {
    assert_eq!(
        decode_plan(&[0xab; 10]),
        Err(PoolError::NotAllowed(
            "the payout plan is not abi.encode of an Order".to_string()
        ))
    );
    let order = plan_order(
        TokenAmount::from(5_000_000_u64),
        TokenAmount::from(4_900_000_u64),
    );
    assert_eq!(
        decode_plan(&types::swapic::encode_order(&order)),
        Ok(order.clone())
    );
    let config = config();
    let payer = pay::payer_of(&config, canister(), None).unwrap();
    let (read, delivery) = plan_delivery(&order, &payer.chains).unwrap();
    assert_eq!(read.action, GasAction::RelayLocal);
    assert_eq!(
        delivery,
        Delivery {
            chain: ChainId::BASE,
            recipient: recipient(),
            token: base_usdc(),
            min_out: TokenAmount::from(4_900_000_u64),
        }
    );
    let mut nobody = order;
    nobody.payout.token = alloy_primitives::Address::ZERO;
    assert_eq!(
        plan_delivery(&nobody, &payer.chains).map(|(_, delivery)| delivery),
        Err(PoolError::NotAllowed(
            "the plan the order names no recipient the canister can read: no CCTP burn, no Eco \
             intent and no payout"
                .to_string()
        ))
    );
}
