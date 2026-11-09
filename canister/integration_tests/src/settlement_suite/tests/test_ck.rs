//! The ck minter's slow route on the real ICRC ledgers, with a stub of the minter as their
//! minting account: `swap_from_icp_ck` from the door to the burn, a lost answer, a refusal, a
//! reimbursement, the hourly check through a hundred swaps, and a fee sweep after them.

use super::test_pool::{
    account, base_chain, install_ledger_of, staker, usdc, Account, World, BASE, FEE,
};
use crate::client::pocket::{query, update};
use crate::settlement_suite::init::{empty_canister, init_arg, install, upgrade};
use crate::wasms;
use candid::{encode_args, encode_one, CandidType, Nat, Principal};
use settlement_api::types::config::Config;
use settlement_api::types::flow::FlowCallError;
use settlement_api::types::flow::FlowSettings;
use settlement_api::types::init::InitArg;
use settlement_api::types::pool::{
    CkResolution, CkSettings, CkSwapTerms, PayoutResolution, PayoutSettings, PayoutStatus,
    PayoutView, PoolError, PoolSettings, SwapFromIcpOk,
};
use settlement_api::types::sweep::{IcpSweepAsset, SweepError, SweepIcpArgs, SweepIcpOk};
use std::time::Duration;
use types::pool::ck_subaccount;

const ETHEREUM: u64 = 1;
const ETH_USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const RECIPIENT: &str = "0x1111111111111111111111111111111111111111";
const NETWORK_FEE: u64 = 670_000;
/// The ckETH the stub burns for gas, under the allowance.
const PRICE: u64 = 200_000;
const ALLOWANCE: u64 = 300_000;
const STAKED: u64 = 1_000_000_000;

#[derive(CandidType, Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum Ask {
    Burn,
    BurnLost,
    BurnUnlisted,
    Trap,
    RefuseUnavailable,
    RefuseBlocked,
    RefuseCkEth,
}

#[derive(CandidType)]
struct StubInit {
    ckusdc: Principal,
    cketh: Principal,
    price: Nat,
}

struct CkWorld {
    world: World,
    stub: Principal,
    cketh: Principal,
}

fn user(n: u8) -> Principal {
    Principal::from_slice(&[0x60, n, 1, 1, 1, 1, 1, 1, 1, 1])
}

fn config() -> Config {
    Config {
        pool_chain: Some(BASE),
        evm_chains: std::collections::BTreeMap::from([
            (BASE, base_chain()),
            (
                ETHEREUM,
                settlement_api::types::config::EvmChainConfig {
                    usdc: ETH_USDC.to_string(),
                    ..base_chain()
                },
            ),
        ]),
        ..init_arg().config
    }
}

/// The canister on `wasm`, its pool holding 1,000 ckUSDC staked, the stub the minter of both
/// ledgers, ckETH enough for the float, and `users` funded.
fn ck_world_on(wasm: Option<Vec<u8>>, users: u8) -> CkWorld {
    let (pic, canister, admin) = empty_canister();
    let arg = InitArg {
        config: config(),
        ..init_arg()
    };
    match wasm {
        None => install(&pic, canister, admin, &arg).expect("installs"),
        Some(wasm) => pic.install_canister(canister, wasm, encode_one(&arg).unwrap(), Some(admin)),
    }
    let stub = pic.create_canister_with_settings(Some(admin), None);
    pic.add_cycles(stub, 100_000_000_000_000);
    let mut balances = vec![(canister, STAKED), (staker(), usdc(1_000))];
    balances.extend((0..users).map(|n| (user(n), usdc(200))));
    let ledger = install_ledger_of(&pic, admin, "ckUSDC", balances, stub);
    let cketh = install_ledger_of(&pic, admin, "ckETH", vec![(canister, 1_000_000_000)], stub);
    pic.install_canister(
        stub,
        wasms::stub_minter(),
        encode_one(StubInit {
            ckusdc: ledger,
            cketh,
            price: Nat::from(PRICE),
        })
        .unwrap(),
        Some(admin),
    );
    let world = World {
        pic,
        canister,
        admin,
        ledger,
    };
    let settings = PoolSettings {
        tvl_cap: Nat::from(usdc(10_000)),
        per_swap_cap: Nat::from(usdc(1_000)),
        premium_bps: 10,
        ckusdc_ledger: ledger,
        icp_tokens: vec![],
        arrival_backup_s: 300,
        // this route exists for when the pool takes no swaps
        pool_open_for_swaps: false,
        large_deposit_usdc: None,
        large_deposit_confirmations: None,
        least_move_fee_base_to_icp: None,
        least_move_fee_icp_to_base: None,
        pool_fee: None,
    };
    let set: Result<(), PoolError> = update(
        &world.pic,
        canister,
        admin,
        "set_pool_settings",
        encode_one(settings).unwrap(),
    );
    assert_eq!(set, Ok(()));
    world.settle();
    world.books_checked();
    world.plant_stake(world.admin, STAKED, 1_000_000_000_000_000_000_000);
    let flow: Result<(), FlowCallError> = update(
        &world.pic,
        canister,
        admin,
        "set_flow_settings",
        encode_one(FlowSettings {
            ck_minter: Some(stub),
            cketh_ledger: Some(cketh),
            cketh_fee_allowance: Nat::from(6_000_000_u64),
            relay_window_s: 3_600,
            ck_helper: None,
            eco_prover: None,
        })
        .unwrap(),
    );
    assert_eq!(flow, Ok(()));
    CkWorld { world, stub, cketh }
}

fn ck_world(users: u8) -> CkWorld {
    let ck = ck_world_on(None, users);
    ck.open_route(true);
    ck
}

impl CkWorld {
    fn open_route(&self, open: bool) {
        let set: Result<(), PoolError> = update(
            &self.world.pic,
            self.world.canister,
            self.world.admin,
            "set_payout_settings",
            encode_one(PayoutSettings {
                min_network_fee: Nat::from(50_000_u32),
                order_wait_s: 120,
                max_payout_gas: 2_000_000,
                cctp_payouts: false,
                ck: Some(CkSettings {
                    open,
                    min_network_fee: Nat::from(NETWORK_FEE),
                    ck_swap_fee_allowance: Nat::from(ALLOWANCE),
                }),
                across_in_flight_max: None,
            })
            .unwrap(),
        );
        assert_eq!(set, Ok(()));
    }

    fn script(&self, asks: Vec<Ask>) {
        let () = update(
            &self.world.pic,
            self.stub,
            self.world.admin,
            "script",
            encode_one(asks).unwrap(),
        );
    }

    fn stub_says(&self, method: &str, id: u64) {
        let () = update(
            &self.world.pic,
            self.stub,
            self.world.admin,
            method,
            encode_one(id).unwrap(),
        );
    }

    fn asked(&self) -> u64 {
        query(
            &self.world.pic,
            self.stub,
            self.world.admin,
            "asked",
            encode_one(()).unwrap(),
        )
    }

    fn burned(&self) -> (Nat, Nat) {
        let raw = self
            .world
            .pic
            .query_call(
                self.stub,
                self.world.admin,
                "burned",
                encode_one(()).unwrap(),
            )
            .unwrap();
        candid::decode_args(&raw).unwrap()
    }

    fn terms(&self, amount: u64) -> CkSwapTerms {
        let min_out = amount - NETWORK_FEE - 2 * FEE;
        CkSwapTerms {
            token_in: self.world.ledger,
            amount_in: Nat::from(amount),
            from_subaccount: None,
            refund_to: None,
            deadline_ns: self
                .world
                .now()
                .checked_add(Duration::from_secs(900))
                .unwrap()
                .as_nanos(),
            dst_chain: ETHEREUM,
            recipient: RECIPIENT.to_string(),
            token_out: ETH_USDC.to_string(),
            min_out: Nat::from(min_out),
            min_usdc: Nat::from(amount),
            network_fee: Nat::from(NETWORK_FEE),
        }
    }

    fn approve(&self, owner: Principal, amount: u64) {
        #[derive(CandidType)]
        struct ApproveArgs {
            from_subaccount: Option<Vec<u8>>,
            spender: Account,
            amount: Nat,
            expected_allowance: Option<Nat>,
            expires_at: Option<u64>,
            fee: Option<Nat>,
            memo: Option<Vec<u8>>,
            created_at_time: Option<u64>,
        }
        let args = ApproveArgs {
            from_subaccount: None,
            spender: account(self.world.canister),
            amount: Nat::from(amount),
            expected_allowance: None,
            expires_at: None,
            fee: None,
            memo: None,
            created_at_time: None,
        };
        self.world
            .pic
            .update_call(
                self.world.ledger,
                owner,
                "icrc2_approve",
                encode_one(args).unwrap(),
            )
            .expect("the ledger answers");
    }

    fn door(&self, owner: Principal, terms: CkSwapTerms) -> Result<SwapFromIcpOk, PoolError> {
        update(
            &self.world.pic,
            self.world.canister,
            owner,
            "swap_from_icp_ck",
            encode_one(terms).unwrap(),
        )
    }

    /// `owner` swaps 100 ckUSDC.
    fn swap(&self, owner: Principal) -> SwapFromIcpOk {
        let amount = usdc(100);
        self.approve(owner, amount + FEE);
        let ok = self
            .door(owner, self.terms(amount))
            .expect("the door opens");
        // the door answers once the money is in; its burn goes on in a message of its own
        assert_eq!(ok.status, PayoutStatus::Swapping);
        self.carried(ok)
    }

    /// `ok` once the message its door left behind has gone as far as it can.
    fn carried(&self, ok: SwapFromIcpOk) -> SwapFromIcpOk {
        for _ in 0..30 {
            self.world.pic.tick();
        }
        SwapFromIcpOk {
            id: ok.id,
            status: self.payout(ok.id).status,
        }
    }

    fn payout(&self, id: u64) -> PayoutView {
        let view: Option<PayoutView> = query(
            &self.world.pic,
            self.world.canister,
            self.world.admin,
            "payout",
            encode_one(id).unwrap(),
        );
        view.expect("a payout")
    }

    fn withdrawal_of(&self, id: u64) -> u64 {
        self.payout(id).ck_withdrawal_id.expect("a burn")
    }

    fn sub_balance(&self, id: u64) -> Nat {
        query(
            &self.world.pic,
            self.world.ledger,
            self.world.admin,
            "icrc1_balance_of",
            encode_one(Account {
                owner: self.world.canister,
                subaccount: Some(ck_subaccount(id).to_vec()),
            })
            .unwrap(),
        )
    }

    fn main_balance(&self) -> Nat {
        self.world.balance(self.world.canister)
    }

    /// Past the pay pass and the minter's pace, and the books read as the hourly check does.
    fn passes(&self, n: u32, step: Duration) {
        for _ in 0..n {
            self.world.pic.advance_time(step);
            self.world.settle();
            self.world.books_checked();
        }
    }

    fn fees_icp_equals_main_above_staked(&self, fees: u64) {
        assert_eq!(self.main_balance(), Nat::from(STAKED + fees));
    }
}

/// The whole way: pulled, funded, burned from the payout's own subaccount, the stub's real
/// burn taking the sum and no fee, the main account down by the hold alone, the network fee
/// staying, and the row closed at the minter's end.
#[test]
fn a_ckusdc_swap_is_burned_and_lands() {
    let ck = ck_world(1);
    let amount = usdc(100);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawn);
    let burn = amount - NETWORK_FEE - 2 * FEE;
    let (burned, gas) = ck.burned();
    assert_eq!(burned, Nat::from(burn));
    assert_eq!(gas, Nat::from(PRICE));
    // nothing left behind: the approval and the burn took exactly what the funding sent
    assert_eq!(ck.sub_balance(ok.id), Nat::from(0_u8));
    ck.fees_icp_equals_main_above_staked(NETWORK_FEE);
    assert_eq!(
        ck.world.balance(user(0)),
        Nat::from(usdc(200) - amount - 2 * FEE)
    );
    let withdrawal = ck.withdrawal_of(ok.id);
    assert_eq!(ck.payout(ok.id).ck_amount, Some(Nat::from(burn)));

    ck.stub_says("sent", withdrawal);
    ck.passes(1, Duration::from_secs(61));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawn);
    ck.stub_says("finalized", withdrawal);
    ck.passes(1, Duration::from_secs(61));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::Paid);
    assert!(ck.world.state().open);
}

/// Burn, lost answer: found on the list, one burn, and no refund, whatever comes after.
#[test]
fn a_lost_answer_is_adopted_and_never_burned_twice() {
    let ck = ck_world(1);
    ck.script(vec![Ask::BurnLost, Ask::RefuseCkEth]);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawing);
    let before = ck.world.balance(user(0));
    // past the deadline too
    ck.passes(2, Duration::from_secs(600));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawn);
    assert_eq!(ck.asked(), 1, "asked again after a burn");
    assert_eq!(ck.burned().0, Nat::from(usdc(100) - NETWORK_FEE - 2 * FEE));
    assert_eq!(ck.world.balance(user(0)), before);
}

/// A guard's trap: nothing burned, asked again at its pace, and it goes through.
#[test]
fn a_guards_trap_is_asked_again_and_burns_once() {
    let ck = ck_world(1);
    ck.script(vec![Ask::Trap, Ask::RefuseUnavailable, Ask::Burn]);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawing);
    ck.passes(8, Duration::from_secs(61));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawn);
    assert_eq!(ck.asked(), 3);
    assert_eq!(ck.burned().0, Nat::from(usdc(100) - NETWORK_FEE - 2 * FEE));
    ck.fees_icp_equals_main_above_staked(NETWORK_FEE);
}

/// A gas spike past the allowance: nothing burned, the float's allowance the cap, and past the
/// deadline all of it back, the network fee too.
#[test]
fn a_gas_price_past_the_allowance_burns_nothing_and_refunds_the_fee() {
    let ck = ck_world(1);
    let () = update(
        &ck.world.pic,
        ck.stub,
        ck.world.admin,
        "set_price",
        encode_one(Nat::from(ALLOWANCE + 1)).unwrap(),
    );
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawing);
    assert_eq!(ck.burned().0, Nat::from(0_u8));
    ck.passes(11, Duration::from_secs(120));
    assert!(matches!(ck.payout(ok.id).status, PayoutStatus::Undone(_)));
    // its approval and pull, then the funding, the approval, the way back and the refund:
    // the ledger's, nothing else
    assert_eq!(
        ck.world.balance(user(0)),
        Nat::from(usdc(200) - 2 * FEE - 4 * FEE)
    );
    ck.fees_icp_equals_main_above_staked(0);
}

/// A recipient the minter blocks: back at once, on proof, the fee with it.
#[test]
fn a_blocked_recipient_is_refunded_at_once() {
    let ck = ck_world(1);
    ck.script(vec![Ask::RefuseBlocked]);
    let ok = ck.swap(user(0));
    assert!(
        matches!(ok.status, PayoutStatus::Undone(_)),
        "{:?}",
        ok.status
    );
    assert_eq!(ck.burned().0, Nat::from(0_u8));
    assert_eq!(
        ck.world.balance(user(0)),
        Nat::from(usdc(200) - 2 * FEE - 4 * FEE)
    );
    assert_eq!(ck.sub_balance(ok.id), Nat::from(0_u8));
    ck.fees_icp_equals_main_above_staked(0);
}

/// Reverted on Ethereum: reimbursed to the burn subaccount, home to the main account, refunded
/// less the ledger's fees, and the network fee kept: the burn really spent gas.
#[test]
fn a_reimbursed_withdrawal_comes_home_and_keeps_the_fee() {
    let ck = ck_world(1);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawn);
    let withdrawal = ck.withdrawal_of(ok.id);
    ck.stub_says("reimburse", withdrawal);
    ck.passes(1, Duration::from_secs(61));
    assert!(matches!(ck.payout(ok.id).status, PayoutStatus::Undone(_)));
    let burn = usdc(100) - NETWORK_FEE - 2 * FEE;
    assert_eq!(
        ck.world.balance(user(0)),
        Nat::from(usdc(200) - usdc(100) - 2 * FEE + burn - 2 * FEE)
    );
    assert_eq!(ck.sub_balance(ok.id), Nat::from(0_u8));
    ck.fees_icp_equals_main_above_staked(NETWORK_FEE);
    assert!(ck.world.state().open);
}

/// Burned with nothing listed: a minter bug, so no way back: it stops, a controller's refund is
/// refused, and nobody is paid twice.
#[test]
fn no_refund_past_a_burn_the_minter_does_not_list() {
    let ck = ck_world(1);
    ck.script(vec![Ask::BurnUnlisted]);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawing);
    let before = ck.world.balance(user(0));
    ck.passes(2, Duration::from_secs(600));
    assert!(
        matches!(ck.payout(ok.id).status, PayoutStatus::Failed(_)),
        "{:?}",
        ck.payout(ok.id).status
    );
    let refund: Result<(), PoolError> = update(
        &ck.world.pic,
        ck.world.canister,
        ck.world.admin,
        "resolve_payout",
        encode_args((ok.id, PayoutResolution::Refund)).unwrap(),
    );
    assert!(refund.is_err(), "a refund past the burn pays from stakers");
    assert_eq!(ck.world.balance(user(0)), before);
    let stranger: Result<(), PoolError> = update(
        &ck.world.pic,
        ck.world.canister,
        user(0),
        "resolve_ck_withdrawal",
        encode_args((ok.id, CkResolution::Retry)).unwrap(),
    );
    assert!(stranger.is_err());
    let retried: Result<(), PoolError> = update(
        &ck.world.pic,
        ck.world.canister,
        ck.world.admin,
        "resolve_ck_withdrawal",
        encode_args((ok.id, CkResolution::Retry)).unwrap(),
    );
    assert_eq!(retried, Ok(()));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawing);
}

/// The route's switch and the door's rules.
#[test]
fn the_door_refuses_what_it_must() {
    let ck = ck_world_on(None, 2);
    ck.approve(user(0), usdc(100) + FEE);
    // off by default
    assert!(matches!(
        ck.door(user(0), ck.terms(usdc(100))),
        Err(PoolError::Closed(_))
    ));
    ck.open_route(true);
    let terms = ck.terms(usdc(100));
    let low_fee = CkSwapTerms {
        network_fee: Nat::from(NETWORK_FEE - 1),
        ..terms.clone()
    };
    assert!(ck.door(user(0), low_fee).is_err());
    let short_floor = CkSwapTerms {
        min_usdc: Nat::from(usdc(100) - 1),
        ..terms.clone()
    };
    assert!(ck.door(user(0), short_floor).is_err());
    assert!(ck.door(Principal::anonymous(), terms.clone()).is_err());
    // a refund naming the anonymous principal, which anyone can spend from (audit C4)
    let refund_to_anon = CkSwapTerms {
        refund_to: Some(settlement_api::types::pool::IcpAccount {
            owner: Principal::anonymous(),
            subaccount: None,
        }),
        ..terms.clone()
    };
    assert!(matches!(
        ck.door(user(0), refund_to_anon),
        Err(PoolError::NotAllowed(_))
    ));
    // a caller with no approval is refused by the ledger, with nothing written (audit C15)
    assert!(matches!(
        ck.door(user(1), terms.clone()),
        Err(PoolError::Ledger(_))
    ));
    let ok = ck.door(user(0), terms).unwrap();
    assert_eq!(ck.carried(ok).status, PayoutStatus::CkWithdrawn);
}

fn hourly_check_passes(ck: &CkWorld) {
    ck.world.pic.advance_time(Duration::from_secs(3_601));
    ck.world.settle();
    ck.world.books_checked();
    let state = ck.world.state();
    assert!(state.open, "{:?}", state.closed_reason);
}

/// A hundred swaps, the hourly check on through them: landed, reimbursed and blocked in turn.
/// The lines and the ledger agree to the unit at the end, the pool stays open, and a fee sweep
/// then takes the network fees and nothing of the stakers'.
#[test]
fn a_hundred_swaps_keep_the_books_and_a_fee_sweep_leaves_stakers_whole() {
    let ck = ck_world(100);
    let assets = ck.world.state().total_assets;
    let mut kept = 0;
    for n in 0..100_u8 {
        ck.script(vec![if n % 3 == 2 {
            Ask::RefuseBlocked
        } else {
            Ask::Burn
        }]);
        let ok = ck.swap(user(n));
        match ok.status {
            PayoutStatus::CkWithdrawn => {
                kept += NETWORK_FEE;
                let end = if n % 3 == 0 { "finalized" } else { "reimburse" };
                ck.stub_says(end, ck.withdrawal_of(ok.id));
                ck.passes(1, Duration::from_secs(61));
                assert!(matches!(
                    ck.payout(ok.id).status,
                    PayoutStatus::Paid | PayoutStatus::Undone(_)
                ));
            }
            PayoutStatus::Undone(_) => {}
            other => panic!("{other:?}"),
        }
        if n % 10 == 9 {
            hourly_check_passes(&ck);
        }
    }
    ck.fees_icp_equals_main_above_staked(kept);
    assert_eq!(ck.world.state().total_assets, assets);
    assert_eq!(ck.world.state().icp.reserved, Nat::from(0_u8));

    // the network fees, to the unit, less the sweep's own fee
    let swept: Result<SweepIcpOk, SweepError> = update(
        &ck.world.pic,
        ck.world.canister,
        ck.world.admin,
        "sweep_icp",
        encode_one(SweepIcpArgs {
            asset: IcpSweepAsset::CkUsdcFees,
            to: settlement_api::types::pool::IcpAccount {
                owner: user(200),
                subaccount: None,
            },
            amount: Nat::from(kept - FEE),
        })
        .unwrap(),
    );
    swept.expect("the fee line pays it");
    assert_eq!(ck.world.balance(user(200)), Nat::from(kept - FEE));
    ck.fees_icp_equals_main_above_staked(0);
    assert_eq!(ck.world.state().total_assets, assets);
    // and a sweep of a unit more than the line holds is refused
    let more: Result<SweepIcpOk, SweepError> = update(
        &ck.world.pic,
        ck.world.canister,
        ck.world.admin,
        "sweep_icp",
        encode_one(SweepIcpArgs {
            asset: IcpSweepAsset::CkUsdcFees,
            to: settlement_api::types::pool::IcpAccount {
                owner: user(200),
                subaccount: None,
            },
            amount: Nat::from(1_u8),
        })
        .unwrap(),
    );
    assert!(more.is_err());
    hourly_check_passes(&ck);
}

/// A kit run that names no ck route shuts the door, and a swap on its way still has its numbers.
#[test]
fn settings_that_name_no_ck_route_strand_no_swap_on_its_way() {
    let ck = ck_world(1);
    ck.script(vec![Ask::Trap]);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawing);
    let set: Result<(), PoolError> = update(
        &ck.world.pic,
        ck.world.canister,
        ck.world.admin,
        "set_payout_settings",
        encode_one(PayoutSettings {
            min_network_fee: Nat::from(50_000_u32),
            order_wait_s: 120,
            max_payout_gas: 2_000_000,
            cctp_payouts: false,
            ck: None,
            across_in_flight_max: None,
        })
        .unwrap(),
    );
    assert_eq!(set, Ok(()));
    assert!(matches!(
        ck.door(user(0), ck.terms(usdc(100))),
        Err(PoolError::Closed(_))
    ));
    ck.passes(3, Duration::from_secs(61));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawn);
}

/// Two swaps in a row reach the minter one at a time, each with its own ckETH approval.
#[test]
fn two_swaps_in_a_row_each_approve_their_own_gas() {
    let ck = ck_world(2);
    let first = ck.swap(user(0));
    let second = ck.swap(user(1));
    ck.passes(2, Duration::from_secs(61));
    for ok in [first, second] {
        assert_eq!(ck.payout(ok.id).status, PayoutStatus::CkWithdrawn);
    }
    // each withdrawal's gas, and each its own ckETH approval
    let float: Nat = query(
        &ck.world.pic,
        ck.cketh,
        ck.world.admin,
        "icrc1_balance_of",
        encode_one(account(ck.world.canister)).unwrap(),
    );
    assert_eq!(float, Nat::from(1_000_000_000 - 2 * PRICE - 2 * FEE));
}

/// From the deployed build, its stored state whole: the route is off, and it takes a swap once
/// a controller opens it, whose row survives this build's own upgrade.
#[test]
#[ignore = "needs PRIOR_WASM: scripts/build-canister.sh --integration-test settlement at the deployed commit"]
fn the_deployed_builds_state_takes_the_ck_route_after_this_upgrade() {
    let ck = ck_world_on(Some(wasms::prior_settlement()), 2);
    let events =
        crate::client::settlement::event_count(&ck.world.pic, ck.world.canister, ck.world.admin);
    upgrade(&ck.world.pic, ck.world.canister, ck.world.admin).unwrap();
    assert_eq!(
        crate::client::settlement::event_count(&ck.world.pic, ck.world.canister, ck.world.admin),
        events
    );
    ck.world.books_checked();
    ck.approve(user(0), usdc(100) + FEE);
    // off until a controller opens it
    assert!(matches!(
        ck.door(user(0), ck.terms(usdc(100))),
        Err(PoolError::Closed(_))
    ));
    ck.open_route(true);
    let ok = ck.swap(user(0));
    assert_eq!(ok.status, PayoutStatus::CkWithdrawn);
    let withdrawal = ck.withdrawal_of(ok.id);

    // a row mid way through its withdrawal survives the upgrade, and goes on
    ck.script(vec![Ask::BurnLost]);
    let open = ck.swap(user(1));
    assert_eq!(open.status, PayoutStatus::CkWithdrawing);
    upgrade(&ck.world.pic, ck.world.canister, ck.world.admin).unwrap();
    ck.stub_says("finalized", withdrawal);
    ck.passes(2, Duration::from_secs(61));
    assert_eq!(ck.payout(ok.id).status, PayoutStatus::Paid);
    assert_eq!(ck.payout(open.id).status, PayoutStatus::CkWithdrawn);
    assert_eq!(ck.asked(), 2);
}
