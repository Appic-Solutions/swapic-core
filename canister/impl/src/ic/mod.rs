//! The other canisters a flow calls: ICRC ledgers, an ICPSwap pool, and the ck minter.
//! The types are the real interfaces' (ICRC-1 and ICRC-2, ICPSwap's `SwapPool`, the ckETH
//! minter's `withdraw_erc20` and `withdrawal_status`), written out for just the calls made,
//! so the local mocks implement the same calls and a real canister answers them.
//!
//! A call that the system rejects is `Unknown`: it may or may not have run, which is what
//! the flow's begun markers are for. A reply the callee gave is definite.

use candid::{CandidType, Nat, Principal};
use serde::Deserialize;

/// How a call ended, when it did not answer what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The system rejected the call, or its reply did not decode: it may have run.
    Unknown(String),
    /// The callee answered a refusal: nothing moved.
    Refused(String),
    /// The ledger's deduplication window passed (`TooOld`): a retry can no longer tell
    /// whether the first call ran, so its outcome stays unknown for good.
    Expired(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Unknown(reason) => write!(f, "outcome unknown: {reason}"),
            CallError::Refused(reason) => write!(f, "refused: {reason}"),
            CallError::Expired(reason) => write!(f, "outcome unknown for good: {reason}"),
        }
    }
}

async fn call<
    A: candid::utils::ArgumentEncoder,
    R: for<'de> candid::utils::ArgumentDecoder<'de>,
>(
    canister: Principal,
    method: &str,
    args: A,
) -> Result<R, CallError> {
    ic_cdk::call(canister, method, args)
        .await
        .map_err(|(code, message)| CallError::Unknown(format!("{code:?}: {message}")))
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub owner: Principal,
    pub subaccount: Option<Vec<u8>>,
}

impl Account {
    pub fn of(owner: Principal, subaccount: Option<[u8; 32]>) -> Self {
        Self {
            owner,
            subaccount: subaccount.map(|bytes| bytes.to_vec()),
        }
    }
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct TransferArg {
    from_subaccount: Option<Vec<u8>>,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
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

#[derive(CandidType, Deserialize, Clone, Debug)]
struct TransferFromArgs {
    spender_subaccount: Option<Vec<u8>>,
    from: Account,
    to: Account,
    amount: Nat,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
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

#[derive(CandidType, Deserialize, Clone, Debug)]
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

#[derive(CandidType, Deserialize, Clone, Debug)]
enum ApproveError {
    BadFee { expected_fee: Nat },
    InsufficientFunds { balance: Nat },
    AllowanceChanged { current_allowance: Nat },
    Expired { ledger_time: u64 },
    TooOld,
    CreatedInFuture { ledger_time: u64 },
    Duplicate { duplicate_of: Nat },
    TemporarilyUnavailable,
    GenericError { error_code: Nat, message: String },
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct GetTransactionsRequest {
    start: Nat,
    length: Nat,
}

/// A transfer as an ICRC-1 ledger's `get_transactions` logs it, the fields the canister
/// reads: an `icrc2_transfer_from` carries its spender.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LedgerTransfer {
    pub from: Account,
    pub to: Account,
    pub amount: Nat,
    pub memo: Option<Vec<u8>>,
    pub created_at_time: Option<u64>,
    pub spender: Option<Account>,
}

#[derive(CandidType, Deserialize)]
struct LedgerTransaction {
    transfer: Option<LedgerTransfer>,
}

#[derive(CandidType, Deserialize)]
struct TransactionRange {
    transactions: Vec<LedgerTransaction>,
}

candid::define_function!(ArchiveFn : (GetTransactionsRequest) -> (TransactionRange) query);

#[derive(CandidType, Deserialize)]
struct ArchivedRange {
    start: Nat,
    length: Nat,
    callback: ArchiveFn,
}

#[derive(CandidType, Deserialize)]
struct GetTransactionsResponse {
    first_index: Nat,
    transactions: Vec<LedgerTransaction>,
    archived_transactions: Vec<ArchivedRange>,
}

/// The transfer in `ledger`'s block `block`, read with `get_transactions` on the ledger, or on
/// the archive that holds it: none for no such block, or a block that is no transfer.
pub async fn ledger_transfer_at(
    ledger: Principal,
    block: u64,
) -> Result<Option<LedgerTransfer>, CallError> {
    let request = || GetTransactionsRequest {
        start: Nat::from(block),
        length: Nat::from(1_u8),
    };
    let (answer,): (GetTransactionsResponse,) =
        call(ledger, "get_transactions", (request(),)).await?;
    let at = Nat::from(block);
    if answer.first_index == at && !answer.transactions.is_empty() {
        return Ok(answer
            .transactions
            .into_iter()
            .next()
            .and_then(|transaction| transaction.transfer));
    }
    let Some(range) = answer
        .archived_transactions
        .into_iter()
        .find(|range| range.start <= at && at < range.start.clone() + range.length.clone())
    else {
        return Ok(None);
    };
    let (archived,): (TransactionRange,) = call(
        range.callback.0.principal,
        &range.callback.0.method,
        (request(),),
    )
    .await?;
    Ok(archived
        .transactions
        .into_iter()
        .next()
        .and_then(|transaction| transaction.transfer))
}

/// The deduplication key of one money call: the quote hash as the memo, and the instant
/// its begun marker was sealed as `created_at_time`, so a retry is the same transfer.
#[derive(Clone, Copy, Debug)]
pub struct Dedup {
    pub memo: [u8; 32],
    pub created_at_nanos: u64,
}

fn nat_u64(value: &Nat) -> u64 {
    u64::try_from(value.0.clone()).unwrap_or(u64::MAX)
}

pub async fn icrc1_fee(ledger: Principal) -> Result<Nat, CallError> {
    let (fee,): (Nat,) = call(ledger, "icrc1_fee", ()).await?;
    Ok(fee)
}

pub async fn icrc1_balance_of(ledger: Principal, account: Account) -> Result<Nat, CallError> {
    let (balance,): (Nat,) = call(ledger, "icrc1_balance_of", (account,)).await?;
    Ok(balance)
}

/// An ICRC-1 transfer, deduplicated: a `Duplicate` answer is the earlier transfer's block.
pub async fn icrc1_transfer(
    ledger: Principal,
    from_subaccount: Option<[u8; 32]>,
    to: Account,
    amount: Nat,
    dedup: Dedup,
) -> Result<u64, CallError> {
    let arg = TransferArg {
        from_subaccount: from_subaccount.map(|bytes| bytes.to_vec()),
        to,
        amount,
        fee: None,
        memo: Some(dedup.memo.to_vec()),
        created_at_time: Some(dedup.created_at_nanos),
    };
    let (answer,): (Result<Nat, TransferError>,) = call(ledger, "icrc1_transfer", (arg,)).await?;
    transfer_outcome(answer)
}

/// What a transfer's answer means: its block, or the earlier identical transfer's block for
/// a `Duplicate` (the same `created_at_time` and memo: the transfer a lost answer hid), a
/// moment's unavailability as an outcome unknown, and any other error as a refusal.
fn transfer_outcome(answer: Result<Nat, TransferError>) -> Result<u64, CallError> {
    match answer {
        Ok(block) => Ok(nat_u64(&block)),
        Err(TransferError::Duplicate { duplicate_of }) => Ok(nat_u64(&duplicate_of)),
        Err(TransferError::TemporarilyUnavailable) => Err(CallError::Unknown(
            "the ledger is temporarily unavailable".to_string(),
        )),
        Err(TransferError::TooOld) => Err(CallError::Expired(
            "the transfer is too old for the ledger to deduplicate".to_string(),
        )),
        Err(error) => Err(CallError::Refused(format!("{error:?}"))),
    }
}

/// An ICRC-2 pull from `from`, spent from the allowance whose spender subaccount is
/// `spender_subaccount` (spec 4b.1: the quote hash, and never a standing allowance), or
/// the canister's main account's when there is none (an ICP staker's own approval).
pub async fn icrc2_transfer_from(
    ledger: Principal,
    spender_subaccount: Option<[u8; 32]>,
    from: Account,
    to: Account,
    amount: Nat,
    dedup: Dedup,
) -> Result<u64, CallError> {
    let arg = TransferFromArgs {
        spender_subaccount: spender_subaccount.map(|bytes| bytes.to_vec()),
        from,
        to,
        amount,
        fee: None,
        memo: Some(dedup.memo.to_vec()),
        created_at_time: Some(dedup.created_at_nanos),
    };
    let (answer,): (Result<Nat, TransferFromError>,) =
        call(ledger, "icrc2_transfer_from", (arg,)).await?;
    match answer {
        Ok(block) => Ok(nat_u64(&block)),
        Err(TransferFromError::Duplicate { duplicate_of }) => Ok(nat_u64(&duplicate_of)),
        Err(TransferFromError::TemporarilyUnavailable) => Err(CallError::Unknown(
            "the ledger is temporarily unavailable".to_string(),
        )),
        Err(TransferFromError::TooOld) => Err(CallError::Expired(
            "the pull is too old for the ledger to deduplicate".to_string(),
        )),
        Err(error) => Err(CallError::Refused(format!("{error:?}"))),
    }
}

/// An ICRC-2 approval of `spender` for `amount` from the canister's own account.
pub async fn icrc2_approve(
    ledger: Principal,
    spender: Principal,
    amount: Nat,
) -> Result<(), CallError> {
    let arg = ApproveArgs {
        from_subaccount: None,
        spender: Account::of(spender, None),
        amount,
        expected_allowance: None,
        expires_at: None,
        fee: None,
        memo: None,
        created_at_time: None,
    };
    let (answer,): (Result<Nat, ApproveError>,) = call(ledger, "icrc2_approve", (arg,)).await?;
    match answer {
        Ok(_) | Err(ApproveError::Duplicate { .. }) => Ok(()),
        Err(error) => Err(CallError::Refused(format!("{error:?}"))),
    }
}

/// An ICRC-2 approval of `spender` for `amount` from the canister's own account,
/// deduplicated (P11c): a retry with the same pair is the same approval, which the ledger
/// answers `Duplicate`, so its fee is spent once.
pub async fn icrc2_approve_once(
    ledger: Principal,
    spender: Principal,
    amount: Nat,
    dedup: Dedup,
) -> Result<(), CallError> {
    let arg = ApproveArgs {
        from_subaccount: None,
        spender: Account::of(spender, None),
        amount,
        expected_allowance: None,
        expires_at: None,
        fee: None,
        memo: Some(dedup.memo.to_vec()),
        created_at_time: Some(dedup.created_at_nanos),
    };
    let (answer,): (Result<Nat, ApproveError>,) = call(ledger, "icrc2_approve", (arg,)).await?;
    match answer {
        Ok(_) | Err(ApproveError::Duplicate { .. }) => Ok(()),
        Err(ApproveError::TemporarilyUnavailable) => Err(CallError::Unknown(
            "the ledger is temporarily unavailable".to_string(),
        )),
        Err(ApproveError::TooOld) => Err(CallError::Expired(
            "the approval is too old for the ledger to deduplicate".to_string(),
        )),
        Err(error) => Err(CallError::Refused(format!("{error:?}"))),
    }
}

// ICPSwap's SwapPool, the calls made.

#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum PoolError {
    CommonError,
    InternalError(String),
    UnsupportedToken(String),
    InsufficientFunds,
}

/// ICPSwap answers `variant { ok; err }`, in lower case, where candid's `Result` is
/// `Ok` and `Err`.
#[derive(CandidType, Deserialize, Clone, Debug)]
enum PoolResult<T> {
    #[serde(rename = "ok")]
    Ok(T),
    #[serde(rename = "err")]
    Err(PoolError),
}

impl<T> PoolResult<T> {
    fn into_result(self) -> Result<T, PoolError> {
        match self {
            PoolResult::Ok(value) => Ok(value),
            PoolResult::Err(error) => Err(error),
        }
    }
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct PoolToken {
    pub address: String,
    pub standard: String,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct PoolMetadata {
    pub fee: Nat,
    pub key: String,
    #[serde(rename = "sqrtPriceX96")]
    pub sqrt_price_x96: Nat,
    pub tick: candid::Int,
    pub liquidity: Nat,
    pub token0: PoolToken,
    pub token1: PoolToken,
    #[serde(rename = "maxLiquidityPerTick")]
    pub max_liquidity_per_tick: Nat,
    #[serde(rename = "nextPositionId")]
    pub next_position_id: Nat,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct SwapArgs {
    #[serde(rename = "amountIn")]
    amount_in: String,
    #[serde(rename = "zeroForOne")]
    zero_for_one: bool,
    #[serde(rename = "amountOutMinimum")]
    amount_out_minimum: String,
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct DepositAndSwapArgs {
    #[serde(rename = "tokenInFee")]
    token_in_fee: Nat,
    #[serde(rename = "amountIn")]
    amount_in: String,
    #[serde(rename = "zeroForOne")]
    zero_for_one: bool,
    #[serde(rename = "amountOutMinimum")]
    amount_out_minimum: String,
    #[serde(rename = "tokenOutFee")]
    token_out_fee: Nat,
}

pub async fn pool_metadata(pool: Principal) -> Result<PoolMetadata, CallError> {
    let (answer,): (PoolResult<PoolMetadata>,) = call(pool, "metadata", ()).await?;
    answer
        .into_result()
        .map_err(|error| CallError::Refused(format!("{error:?}")))
}

/// What the pool would pay for `amount_in`, without moving anything.
pub async fn pool_quote(
    pool: Principal,
    amount_in: &Nat,
    zero_for_one: bool,
) -> Result<Nat, CallError> {
    let arg = SwapArgs {
        amount_in: amount_in.0.to_string(),
        zero_for_one,
        amount_out_minimum: "0".to_string(),
    };
    let (answer,): (PoolResult<Nat>,) = call(pool, "quote", (arg,)).await?;
    answer
        .into_result()
        .map_err(|error| CallError::Refused(format!("{error:?}")))
}

/// ICPSwap's one-call swap: pulls `amount_in` from the canister's own account (an ICRC-2
/// allowance to the pool), swaps, and pays the output back to the canister's own account.
pub async fn deposit_from_and_swap(
    pool: Principal,
    amount_in: &Nat,
    zero_for_one: bool,
    amount_out_minimum: &Nat,
    token_in_fee: Nat,
    token_out_fee: Nat,
) -> Result<Nat, CallError> {
    let arg = DepositAndSwapArgs {
        token_in_fee,
        amount_in: amount_in.0.to_string(),
        zero_for_one,
        amount_out_minimum: amount_out_minimum.0.to_string(),
        token_out_fee,
    };
    let (answer,): (PoolResult<Nat>,) = call(pool, "depositFromAndSwap", (arg,)).await?;
    // the pool's own `err` answer: the swap did not happen (what was deposited, if anything,
    // is the canister's unused balance at the pool)
    answer
        .into_result()
        .map_err(|error| CallError::Refused(format!("the pool answered {error:?}")))
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct UnusedBalance {
    balance0: Nat,
    balance1: Nat,
}

/// The canister's unused balances at an ICPSwap pool (`getUserUnusedBalance`): what it
/// deposited and did not swap, and what a swap paid it and was not withdrawn, as token0 and
/// token1. ICPSwap's SwapPool interface, not exercised against a live pool in this build.
pub async fn pool_unused_balance(pool: Principal) -> Result<(Nat, Nat), CallError> {
    let (answer,): (PoolResult<UnusedBalance>,) =
        call(pool, "getUserUnusedBalance", (ic_cdk::api::id(),)).await?;
    answer
        .into_result()
        .map(|unused| (unused.balance0, unused.balance1))
        .map_err(|error| CallError::Refused(format!("{error:?}")))
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct PoolWithdrawArgs {
    token: String,
    fee: Nat,
    amount: Nat,
}

/// ICPSwap's `withdraw` of the canister's unused `amount` of `token` back to its own account,
/// the ledger's `fee` out of it: answers what was withdrawn. ICPSwap's SwapPool interface,
/// not exercised against a live pool in this build.
pub async fn pool_withdraw(
    pool: Principal,
    token: Principal,
    amount: Nat,
    fee: Nat,
) -> Result<Nat, CallError> {
    let arg = PoolWithdrawArgs {
        token: token.to_text(),
        fee,
        amount,
    };
    let (answer,): (PoolResult<Nat>,) = call(pool, "withdraw", (arg,)).await?;
    answer
        .into_result()
        .map_err(|error| CallError::Refused(format!("{error:?}")))
}

// The ckETH minter, the calls made.

#[derive(CandidType, Deserialize, Clone, Debug)]
struct WithdrawErc20Arg {
    amount: Nat,
    ckerc20_ledger_id: Principal,
    recipient: String,
    from_cketh_subaccount: Option<Vec<u8>>,
    from_ckerc20_subaccount: Option<Vec<u8>>,
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

// the ck minter's own candid names
#[allow(clippy::enum_variant_names)]
#[derive(CandidType, Deserialize, Clone, Debug)]
enum WithdrawalSearchParameter {
    ByRecipient(String),
    BySenderAccount(Account),
    ByWithdrawalId(u64),
}

/// `withdraw_erc20` to `recipient`. The withdrawal id is the ckETH burn's block index, as
/// the minter numbers withdrawals. A refusal the minter answers moved nothing.
pub async fn withdraw_erc20(
    minter: Principal,
    ckerc20_ledger: Principal,
    amount: Nat,
    recipient: String,
) -> Result<RetrieveErc20Request, CallError> {
    let arg = WithdrawErc20Arg {
        amount,
        ckerc20_ledger_id: ckerc20_ledger,
        recipient,
        from_cketh_subaccount: None,
        from_ckerc20_subaccount: None,
    };
    // the error variant is read as text: the canister acts on its being an error, and the
    // ops read which one
    let (answer,): (Result<RetrieveErc20Request, candid::Reserved>,) =
        call(minter, "withdraw_erc20", (arg,)).await?;
    answer.map_err(|_| CallError::Refused("the minter refused the withdrawal".to_string()))
}

/// The minter's view of one withdrawal.
pub async fn withdrawal_status(
    minter: Principal,
    withdrawal_id: u64,
) -> Result<Vec<WithdrawalDetail>, CallError> {
    let (details,): (Vec<WithdrawalDetail>,) = call(
        minter,
        "withdrawal_status",
        (WithdrawalSearchParameter::ByWithdrawalId(withdrawal_id),),
    )
    .await?;
    Ok(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review Focus 2: a retried transfer the ledger has seen answers `Duplicate`, which is
    /// the earlier transfer's block: paid, once.
    #[test]
    fn a_duplicate_answer_is_the_earlier_transfer() {
        assert_eq!(transfer_outcome(Ok(Nat::from(7_u8))), Ok(7));
        assert_eq!(
            transfer_outcome(Err(TransferError::Duplicate {
                duplicate_of: Nat::from(7_u8)
            })),
            Ok(7)
        );
        assert!(matches!(
            transfer_outcome(Err(TransferError::TemporarilyUnavailable)),
            Err(CallError::Unknown(_))
        ));
        // too old to deduplicate: unknown for good, never a refusal (it may have run), and
        // never sent again under a new instant
        assert!(matches!(
            transfer_outcome(Err(TransferError::TooOld)),
            Err(CallError::Expired(_))
        ));
        assert!(matches!(
            transfer_outcome(Err(TransferError::InsufficientFunds {
                balance: Nat::from(0_u8)
            })),
            Err(CallError::Refused(_))
        ));
    }
}
