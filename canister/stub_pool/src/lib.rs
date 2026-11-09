//! An ICPSwap pool as the sale's integration tests meet it: the calls the canister makes, in
//! their real candid, moving the suite's real ledgers. `depositFromAndSwap` pulls on the
//! canister's own ICRC-2 allowance, as the pool does, so an allowance too small or past its end
//! fails here as it would there. What it quotes and pays, whether it refuses, and when a sale
//! ends are the test's to say. Never deployed.

use candid::{CandidType, Deserialize, Int, Nat, Principal};
use ic_cdk::{init, query, update};
use std::cell::RefCell;

#[derive(CandidType, Deserialize, Clone)]
pub struct Init {
    pub token0: Principal,
    pub token1: Principal,
}

#[derive(CandidType, Deserialize, Clone, Default)]
pub struct Sale {
    pub quote: Nat,
    pub out: Nat,
    /// The pool's own refusal of the swap, before its deposit.
    pub refuse: bool,
    /// A sale waits here once its deposit is in, until the test lets it go: what goes on beside
    /// it can be answered first, or after.
    pub hold: bool,
    /// The quote fails, as a pool that does not answer does.
    pub quote_fails: bool,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum PoolError {
    CommonError,
    InternalError(String),
    UnsupportedToken(String),
    InsufficientFunds,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum PoolResult<T> {
    #[serde(rename = "ok")]
    Ok(T),
    #[serde(rename = "err")]
    Err(PoolError),
}

#[derive(CandidType, Deserialize, Clone)]
pub struct PoolToken {
    pub address: String,
    pub standard: String,
}

#[derive(CandidType, Deserialize, Clone)]
pub struct PoolMetadata {
    pub fee: Nat,
    pub key: String,
    #[serde(rename = "sqrtPriceX96")]
    pub sqrt_price_x96: Nat,
    pub tick: Int,
    pub liquidity: Nat,
    pub token0: PoolToken,
    pub token1: PoolToken,
    #[serde(rename = "maxLiquidityPerTick")]
    pub max_liquidity_per_tick: Nat,
    #[serde(rename = "nextPositionId")]
    pub next_position_id: Nat,
}

#[derive(CandidType, Deserialize, Clone)]
pub struct SwapArgs {
    #[serde(rename = "amountIn")]
    pub amount_in: String,
    #[serde(rename = "zeroForOne")]
    pub zero_for_one: bool,
    #[serde(rename = "amountOutMinimum")]
    pub amount_out_minimum: String,
}

#[derive(CandidType, Deserialize, Clone)]
pub struct DepositAndSwapArgs {
    #[serde(rename = "tokenInFee")]
    pub token_in_fee: Nat,
    #[serde(rename = "amountIn")]
    pub amount_in: String,
    #[serde(rename = "zeroForOne")]
    pub zero_for_one: bool,
    #[serde(rename = "amountOutMinimum")]
    pub amount_out_minimum: String,
    #[serde(rename = "tokenOutFee")]
    pub token_out_fee: Nat,
}

#[derive(CandidType, Deserialize, Clone)]
pub struct UnusedBalance {
    pub balance0: Nat,
    pub balance1: Nat,
}

#[derive(CandidType, Deserialize, Clone)]
pub struct PoolWithdrawArgs {
    pub token: String,
    pub fee: Nat,
    pub amount: Nat,
}

#[derive(CandidType, Deserialize, Clone)]
struct Account {
    owner: Principal,
    subaccount: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Clone)]
struct TransferFromArgs {
    spender_subaccount: Option<Vec<u8>>,
    from: Account,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone)]
struct TransferArg {
    from_subaccount: Option<Vec<u8>>,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

thread_local! {
    static TOKENS: RefCell<Option<Init>> = const { RefCell::new(None) };
    static SALE: RefCell<Sale> = RefCell::new(Sale::default());
    static QUOTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn tokens() -> Init {
    TOKENS.with(|tokens| tokens.borrow().clone().expect("installed with its tokens"))
}

fn sale() -> Sale {
    SALE.with(|sale| sale.borrow().clone())
}

fn refused<T>(why: impl Into<String>) -> PoolResult<T> {
    PoolResult::Err(PoolError::InternalError(why.into()))
}

#[init]
fn install(init: Init) {
    TOKENS.with(|tokens| *tokens.borrow_mut() = Some(init));
}

#[update]
fn set_sale(next: Sale) {
    SALE.with(|sale| *sale.borrow_mut() = next);
}

#[update]
fn noop() {}

#[query]
fn metadata() -> PoolResult<PoolMetadata> {
    let tokens = tokens();
    let token = |ledger: Principal| PoolToken {
        address: ledger.to_text(),
        standard: "ICRC2".to_string(),
    };
    PoolResult::Ok(PoolMetadata {
        fee: Nat::from(3_000_u32),
        key: "stub".to_string(),
        sqrt_price_x96: Nat::from(1_u32),
        tick: Int::from(0),
        liquidity: Nat::from(1_u32),
        token0: token(tokens.token0),
        token1: token(tokens.token1),
        max_liquidity_per_tick: Nat::from(1_u32),
        next_position_id: Nat::from(1_u32),
    })
}

/// How many times the pool was asked for a quote.
#[query]
fn quotes_asked() -> u64 {
    QUOTES.with(|quotes| quotes.get())
}

#[update]
fn quote(_args: SwapArgs) -> PoolResult<Nat> {
    QUOTES.with(|quotes| quotes.set(quotes.get() + 1));
    let sale = sale();
    if sale.quote_fails {
        return refused("the quote fails");
    }
    PoolResult::Ok(sale.quote)
}

#[update(name = "getUserUnusedBalance")]
fn unused(_user: Principal) -> PoolResult<UnusedBalance> {
    PoolResult::Ok(UnusedBalance {
        balance0: Nat::from(0_u32),
        balance1: Nat::from(0_u32),
    })
}

#[update]
fn withdraw(_args: PoolWithdrawArgs) -> PoolResult<Nat> {
    refused("nothing unused here")
}

#[update(name = "depositFromAndSwap")]
async fn deposit_from_and_swap(args: DepositAndSwapArgs) -> PoolResult<Nat> {
    let sale = sale();
    if sale.refuse {
        return refused("the pool refuses this swap");
    }
    let caller = ic_cdk::api::caller();
    let tokens = tokens();
    let (token_in, token_out) = if args.zero_for_one {
        (tokens.token0, tokens.token1)
    } else {
        (tokens.token1, tokens.token0)
    };
    let amount: Nat = match args.amount_in.parse::<u128>() {
        Ok(amount) => Nat::from(amount),
        Err(_) => return refused("the amount is no number"),
    };
    let pull = TransferFromArgs {
        spender_subaccount: None,
        from: Account {
            owner: caller,
            subaccount: None,
        },
        to: Account {
            owner: ic_cdk::api::id(),
            subaccount: None,
        },
        amount,
        fee: Some(args.token_in_fee.clone()),
        memo: None,
        created_at_time: None,
    };
    let pulled: Result<(Result<Nat, candid::Reserved>,), _> =
        ic_cdk::call(token_in, "icrc2_transfer_from", (pull,)).await;
    match pulled {
        Ok((Ok(_),)) => {}
        _ => return refused("the deposit was not pulled"),
    }
    let mut spins = 0_u32;
    while SALE.with(|sale| sale.borrow().hold) && spins < 100_000 {
        let _: Result<(), _> = ic_cdk::call(ic_cdk::api::id(), "noop", ()).await;
        spins += 1;
    }
    let sent = TransferArg {
        from_subaccount: None,
        to: Account {
            owner: caller,
            subaccount: None,
        },
        amount: sale.out.clone() - args.token_out_fee.clone(),
        fee: Some(args.token_out_fee),
        memo: None,
        created_at_time: None,
    };
    let paid: Result<(Result<Nat, candid::Reserved>,), _> =
        ic_cdk::call(token_out, "icrc1_transfer", (sent,)).await;
    match paid {
        Ok((Ok(_),)) => PoolResult::Ok(sale.out),
        _ => refused("the output was not paid"),
    }
}
