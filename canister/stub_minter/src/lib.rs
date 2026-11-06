//! The ck minter as the pool's integration tests meet it: `withdraw_erc20` and
//! `withdrawal_status` in the real candid, burning on the suite's real ledgers (this canister
//! is their minting account, so a burn is the ledger's own, with no fee). Each ask takes the
//! next line of a script: burn, burn and lose the answer, trap in a guard, or refuse.

use candid::{CandidType, Deserialize, Nat, Principal};
use ic_cdk::{init, post_upgrade, query, update};
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub owner: Principal,
    pub subaccount: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct WithdrawErc20Arg {
    pub amount: Nat,
    pub ckerc20_ledger_id: Principal,
    pub recipient: String,
    pub from_cketh_subaccount: Option<Vec<u8>>,
    pub from_ckerc20_subaccount: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct RetrieveErc20Request {
    pub cketh_block_index: Nat,
    pub ckerc20_block_index: Nat,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct EthTransaction {
    pub transaction_hash: String,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum TxFinalizedStatus {
    Success {
        transaction_hash: String,
        effective_transaction_fee: Option<Nat>,
    },
    Reimbursed {
        transaction_hash: String,
        reimbursed_amount: Nat,
        reimbursed_in_block: Nat,
    },
    PendingReimbursement(EthTransaction),
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum WithdrawalStatus {
    Pending,
    TxCreated,
    TxSent(EthTransaction),
    TxFinalized(TxFinalizedStatus),
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct WithdrawalDetail {
    pub withdrawal_id: u64,
    pub recipient_address: String,
    pub from: Principal,
    pub from_subaccount: Option<Vec<u8>>,
    pub token_symbol: String,
    pub withdrawal_amount: Nat,
    pub max_transaction_fee: Option<Nat>,
    pub status: WithdrawalStatus,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum WithdrawalSearchParameter {
    ByRecipient(String),
    BySenderAccount(Account),
    ByWithdrawalId(u64),
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct CkErc20Token {
    pub ckerc20_token_symbol: String,
    pub erc20_contract_address: String,
    pub ledger_canister_id: Principal,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum LedgerError {
    InsufficientFunds {
        balance: Nat,
        failed_burn_amount: Nat,
        token_symbol: String,
        ledger_id: Principal,
    },
    InsufficientAllowance {
        allowance: Nat,
        failed_burn_amount: Nat,
        token_symbol: String,
        ledger_id: Principal,
    },
    AmountTooLow {
        minimum_burn_amount: Nat,
        failed_burn_amount: Nat,
        token_symbol: String,
        ledger_id: Principal,
    },
    TemporarilyUnavailable(String),
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum WithdrawErc20Error {
    TokenNotSupported {
        supported_tokens: Vec<CkErc20Token>,
    },
    RecipientAddressBlocked {
        address: String,
    },
    CkEthLedgerError {
        error: LedgerError,
    },
    CkErc20LedgerError {
        cketh_block_index: Nat,
        error: LedgerError,
    },
    TemporarilyUnavailable(String),
}

/// What the next ask does.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Burns ckETH, then ckUSDC, lists it and answers.
    Burn,
    /// Burns and lists it, and the answer never comes.
    BurnLost,
    /// Burns and traps with nothing listed.
    BurnUnlisted,
    /// A guard's trap, nothing burned: what a second call or a full queue meets.
    Trap,
    RefuseUnavailable,
    RefuseBlocked,
    RefuseCkEth,
}

#[derive(CandidType, Deserialize, Clone, Debug, Default)]
struct State {
    ckusdc: Option<Principal>,
    cketh: Option<Principal>,
    price: Nat,
    asks: VecDeque<Ask>,
    asked: u64,
    withdrawals: Vec<WithdrawalDetail>,
    burned: Nat,
    gas: Nat,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct InitArg {
    pub ckusdc: Principal,
    pub cketh: Principal,
    pub price: Nat,
}

#[init]
fn init(arg: InitArg) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.ckusdc = Some(arg.ckusdc);
        state.cketh = Some(arg.cketh);
        state.price = arg.price;
    });
}

#[post_upgrade]
fn post_upgrade() {}

#[derive(CandidType)]
struct TransferFromArgs {
    spender_subaccount: Option<Vec<u8>>,
    from: Account,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Debug)]
enum TransferFromError {
    BadFee { expected_fee: Nat },
    BadBurn { min_burn_amount: Nat },
    InsufficientFunds { balance: Nat },
    InsufficientAllowance { allowance: Nat },
    TooOld,
    CreatedInFuture { ledger_time: u64 },
    Duplicate { duplicate_of: Nat },
    TemporarilyUnavailable,
    GenericError { error_code: Nat, message: String },
}

#[derive(CandidType)]
struct TransferArg {
    from_subaccount: Option<Vec<u8>>,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Debug)]
enum TransferError {
    BadFee { expected_fee: Nat },
    BadBurn { min_burn_amount: Nat },
    InsufficientFunds { balance: Nat },
    TooOld,
    CreatedInFuture { ledger_time: u64 },
    TemporarilyUnavailable,
    Duplicate { duplicate_of: Nat },
    GenericError { error_code: Nat, message: String },
}

fn me() -> Account {
    Account {
        owner: ic_cdk::api::id(),
        subaccount: None,
    }
}

/// The ledger's own burn: a transfer from `from` to the minting account, on its allowance.
async fn burn(ledger: Principal, from: Account, amount: Nat) -> Result<Nat, TransferFromError> {
    let arg = TransferFromArgs {
        spender_subaccount: None,
        from,
        to: me(),
        amount,
        fee: None,
        memo: None,
        created_at_time: None,
    };
    let (answer,): (Result<Nat, TransferFromError>,) =
        ic_cdk::call(ledger, "icrc2_transfer_from", (arg,))
            .await
            .expect("the ledger answers");
    answer
}

fn ledger_error(error: TransferFromError, ledger: Principal, amount: &Nat) -> LedgerError {
    match error {
        TransferFromError::InsufficientFunds { balance } => LedgerError::InsufficientFunds {
            balance,
            failed_burn_amount: amount.clone(),
            token_symbol: "stub".to_string(),
            ledger_id: ledger,
        },
        TransferFromError::InsufficientAllowance { allowance } => {
            LedgerError::InsufficientAllowance {
                allowance,
                failed_burn_amount: amount.clone(),
                token_symbol: "stub".to_string(),
                ledger_id: ledger,
            }
        }
        other => LedgerError::TemporarilyUnavailable(format!("{other:?}")),
    }
}

fn state<T>(read: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|state| read(&mut state.borrow_mut()))
}

// the minter's own candid sets the size of its refusals
#[allow(clippy::result_large_err)]
#[update]
async fn withdraw_erc20(arg: WithdrawErc20Arg) -> Result<RetrieveErc20Request, WithdrawErc20Error> {
    let ask = state(|state| {
        state.asked += 1;
        state.asks.pop_front().unwrap_or(Ask::Burn)
    });
    match ask {
        Ask::Trap => {
            // this script's line is kept: only the call traps
            let _: Result<(), _> = ic_cdk::call(ic_cdk::api::id(), "noop", ()).await;
            ic_cdk::trap("a guard: one call per caller")
        }
        Ask::RefuseUnavailable => {
            return Err(WithdrawErc20Error::TemporarilyUnavailable(
                "no gas price yet".to_string(),
            ))
        }
        Ask::RefuseBlocked => {
            return Err(WithdrawErc20Error::RecipientAddressBlocked {
                address: arg.recipient,
            })
        }
        Ask::RefuseCkEth => {
            return Err(WithdrawErc20Error::CkEthLedgerError {
                error: LedgerError::TemporarilyUnavailable("short".to_string()),
            })
        }
        Ask::Burn | Ask::BurnLost | Ask::BurnUnlisted => {}
    }
    let caller = ic_cdk::api::caller();
    let (cketh, price) = state(|state| (state.cketh.expect("set"), state.price.clone()));
    let from_eth = Account {
        owner: caller,
        subaccount: arg.from_cketh_subaccount.clone(),
    };
    let cketh_block = burn(cketh, from_eth, price.clone())
        .await
        .map_err(|error| WithdrawErc20Error::CkEthLedgerError {
            error: ledger_error(error, cketh, &price),
        })?;
    let from = Account {
        owner: caller,
        subaccount: arg.from_ckerc20_subaccount.clone(),
    };
    let ckerc20_block = burn(arg.ckerc20_ledger_id, from, arg.amount.clone())
        .await
        .map_err(|error| WithdrawErc20Error::CkErc20LedgerError {
            cketh_block_index: cketh_block.clone(),
            error: ledger_error(error, arg.ckerc20_ledger_id, &arg.amount),
        })?;
    state(|state| {
        state.burned += arg.amount.clone();
        state.gas += price.clone();
    });
    let id = u64::try_from(cketh_block.0.clone()).expect("a block index");
    if ask != Ask::BurnUnlisted {
        state(|state| {
            state.withdrawals.push(WithdrawalDetail {
                withdrawal_id: id,
                recipient_address: arg.recipient.clone(),
                from: caller,
                from_subaccount: arg.from_ckerc20_subaccount.clone(),
                token_symbol: "ckUSDC".to_string(),
                withdrawal_amount: arg.amount.clone(),
                max_transaction_fee: Some(price.clone()),
                status: WithdrawalStatus::Pending,
            })
        });
    }
    if ask != Ask::Burn {
        // what is written is kept: the answer alone is lost
        let _: Result<(), _> = ic_cdk::call(ic_cdk::api::id(), "noop", ()).await;
        ic_cdk::trap("the answer is lost after the burn");
    }
    Ok(RetrieveErc20Request {
        cketh_block_index: cketh_block,
        ckerc20_block_index: ckerc20_block,
    })
}

#[update]
fn noop() {}

#[query]
fn withdrawal_status(parameter: WithdrawalSearchParameter) -> Vec<WithdrawalDetail> {
    state(|state| {
        state
            .withdrawals
            .iter()
            .filter(|detail| match &parameter {
                WithdrawalSearchParameter::ByRecipient(to) => detail.recipient_address == *to,
                WithdrawalSearchParameter::ByWithdrawalId(id) => detail.withdrawal_id == *id,
                WithdrawalSearchParameter::BySenderAccount(account) => {
                    detail.from == account.owner && detail.from_subaccount == account.subaccount
                }
            })
            .cloned()
            .collect()
    })
}

/// The next asks' answers.
#[update]
fn script(asks: Vec<Ask>) {
    state(|state| state.asks = asks.into());
}

/// How many times it was asked.
#[query]
fn asked() -> u64 {
    state(|state| state.asked)
}

/// All it burned of ckUSDC, and of ckETH for gas.
#[query]
fn burned() -> (Nat, Nat) {
    state(|state| (state.burned.clone(), state.gas.clone()))
}

/// The minter's price of gas from now on.
#[update]
fn set_price(price: Nat) {
    state(|state| state.price = price);
}

/// The withdrawal `id` mined: `TxSent`.
#[update]
fn sent(id: u64) {
    set_status(id, WithdrawalStatus::TxSent(hash()));
}

/// The withdrawal `id` finalized: a success.
#[update]
fn finalized(id: u64) {
    set_status(
        id,
        WithdrawalStatus::TxFinalized(TxFinalizedStatus::Success {
            transaction_hash: hash().transaction_hash,
            effective_transaction_fee: None,
        }),
    );
}

/// The withdrawal `id` reverted on Ethereum: its ckUSDC minted back to the burn subaccount,
/// the ckETH spent.
#[update]
async fn reimburse(id: u64) {
    let (ledger, detail) = state(|state| {
        let detail = state
            .withdrawals
            .iter()
            .find(|detail| detail.withdrawal_id == id)
            .cloned()
            .expect("a withdrawal");
        (state.ckusdc.expect("set"), detail)
    });
    let arg = TransferArg {
        from_subaccount: None,
        to: Account {
            owner: detail.from,
            subaccount: detail.from_subaccount.clone(),
        },
        amount: detail.withdrawal_amount.clone(),
        fee: None,
        memo: None,
        created_at_time: None,
    };
    let (answer,): (Result<Nat, TransferError>,) = ic_cdk::call(ledger, "icrc1_transfer", (arg,))
        .await
        .expect("the ledger answers");
    let block = answer.expect("a mint");
    set_status(
        id,
        WithdrawalStatus::TxFinalized(TxFinalizedStatus::Reimbursed {
            transaction_hash: hash().transaction_hash,
            reimbursed_amount: detail.withdrawal_amount,
            reimbursed_in_block: block,
        }),
    );
}

fn hash() -> EthTransaction {
    EthTransaction {
        transaction_hash: format!("0x{}", "ab".repeat(32)),
    }
}

fn set_status(id: u64, status: WithdrawalStatus) {
    state(|state| {
        for detail in &mut state.withdrawals {
            if detail.withdrawal_id == id {
                detail.status = status.clone();
            }
        }
    });
}
