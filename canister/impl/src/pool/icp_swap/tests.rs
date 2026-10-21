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
        runner: EvmAddress::new([0x22; 20]),
        cctp_receiver: EvmAddress::new([0x23; 20]),
        usdc: EvmAddress::new([0x24; 20]),
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

fn request(token_in: Principal, amount_in: TokenAmount, min_usdc: TokenAmount) -> SwapRequest {
    let from = account(5);
    SwapRequest {
        caller: from.owner,
        terms: IcpSwapTerms {
            token_in,
            amount_in,
            from,
            min_usdc,
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
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

fn checked(request: &SwapRequest) -> Result<RoutePath, PoolError> {
    read_state(|state| {
        check(
            state,
            &config(),
            request,
            at(10),
            Some(EvmAddress::new([0x36; 20])),
            |_| false,
        )
    })
}

fn open(request: &SwapRequest) -> u64 {
    let id = ledger().lines().next_id;
    record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(request.terms)),
            terms: request.payout,
            undo: Undo::Refund {
                to: request.terms.refund_to,
            },
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

#[test]
fn each_refusal_comes_before_anything_is_recorded() {
    on_fresh_memory(|| {
        crate::storage::init();
        // no settings at all
        let good = request(ckusdc(), usdc(100), usdc(100));
        assert!(matches!(checked(&good), Err(PoolError::Closed(_))));
        pool_with(usdc(500));
        assert_eq!(checked(&good), Ok(RoutePath::Ckusdc));
        let with = |change: &dyn Fn(&mut SwapRequest)| {
            let mut request = good.clone();
            change(&mut request);
            checked(&request)
        };
        assert_eq!(
            with(&|r| r.terms.token_in = Principal::from_slice(&[0x99; 10])),
            Err(PoolError::TokenNotListed(Principal::from_slice(
                &[0x99; 10]
            )))
        );
        assert!(matches!(
            with(&|r| r.terms.min_usdc = usdc(99)),
            Err(PoolError::NotAllowed(_))
        ));
        // ckUSDC alone: one ckUSDC fee at the least, and 10 bps of 100 USDC is above it
        assert_eq!(
            with(&|r| r.terms.premium = TokenAmount::from(99_999_u32)),
            Err(PoolError::PremiumTooLow {
                least: candid::Nat::from(100_000_u32)
            })
        );
        assert!(matches!(
            with(&|r| r.terms.network_fee = TokenAmount::from(49_999_u32)),
            Err(PoolError::NotAllowed(_))
        ));
        assert!(matches!(
            with(&|r| {
                r.terms.amount_in = usdc(1_001);
                r.terms.min_usdc = usdc(1_001);
                r.terms.premium = usdc(2);
            }),
            Err(PoolError::AboveSwapCap { .. })
        ));
        assert_eq!(
            with(&|r| r.payout.deadline = at(10)
                .checked_add(MIN_DEADLINE)
                .unwrap()
                .checked_sub(Duration::from_nanos(1))
                .unwrap()),
            Err(PoolError::Expired)
        );
        assert!(matches!(
            with(&|r| r.payout.dst_chain = ChainId::new(10)),
            Err(PoolError::NotAllowed(_))
        ));
        // never the canister, nor one of Swapic's own contracts
        assert!(matches!(
            with(&|r| r.payout.recipient = EvmAddress::new([0x36; 20])),
            Err(PoolError::NotAllowed(_))
        ));
        assert!(matches!(
            with(&|r| r.payout.recipient = swapic()),
            Err(PoolError::NotAllowed(_))
        ));
        let sanctioned = read_state(|state| {
            check(state, &config(), &good, at(10), None, |address| {
                *address == Address::from(recipient())
            })
        });
        assert!(matches!(sanctioned, Err(PoolError::Sanctioned(_))));
        let caller = good.caller.to_text();
        let sanctioned = read_state(|state| {
            check(state, &config(), &good, at(10), None, |address| {
                address.as_str() == caller
            })
        });
        assert!(matches!(sanctioned, Err(PoolError::Sanctioned(_))));
        // a token's route: four ckUSDC fees a leg
        let direct = request(token(), TokenAmount::from(5_000_000_u64), usdc(2));
        assert_eq!(
            checked(&SwapRequest {
                terms: IcpSwapTerms {
                    premium: TokenAmount::from(4 * FEE - 1),
                    ..direct.terms
                },
                ..direct.clone()
            }),
            Err(PoolError::PremiumTooLow {
                least: candid::Nat::from(4 * FEE)
            })
        );
        assert!(matches!(
            checked(&SwapRequest {
                terms: IcpSwapTerms {
                    premium: TokenAmount::from(4 * FEE),
                    ..direct.terms
                },
                ..direct
            }),
            Ok(RoutePath::Direct { .. })
        ));
    });
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
        // two legs: eight ckUSDC fees at the least
        request.terms.premium = TokenAmount::from(8 * FEE - 1);
        assert!(matches!(
            checked(&request),
            Err(PoolError::PremiumTooLow { .. })
        ));
        request.terms.premium = TokenAmount::from(8 * FEE);
        assert!(matches!(checked(&request), Ok(RoutePath::ViaIcp { .. })));
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
