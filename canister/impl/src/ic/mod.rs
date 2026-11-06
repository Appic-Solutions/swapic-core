//! The other canisters a flow calls: ICRC ledgers, an ICPSwap pool, and the ck minter.
//! The types are the real interfaces' (ICRC-1 and ICRC-2, ICPSwap's `SwapPool`, the ckETH
//! minter's `withdraw_erc20` and `withdrawal_status`), written out for just the calls made,
//! so the local mocks implement the same calls and a real canister answers them.
//!
//! A call that the system rejects is `Unknown`: it may or may not have run, which is what
//! the flow's begun markers are for. A reply the callee gave is definite.

#[cfg(test)]
mod tests;

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

/// Who a ledger block names as paying, paid or spending: an ICRC account, or an account
/// identifier (the ICP ledger's own blocks name no principal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Party {
    Account(Account),
    Identifier([u8; 32]),
}

/// A transfer as a ledger's block logs it, the fields the canister reads: an
/// `icrc2_transfer_from` carries its spender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerTransfer {
    pub from: Party,
    pub to: Party,
    pub amount: Nat,
    pub memo: Option<Vec<u8>>,
    pub created_at_time: Option<u64>,
    pub spender: Option<Party>,
}

/// What a block reads as in one read's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// The transfer it logs.
    Transfer(Box<LedgerTransfer>),
    /// A block of another kind (a mint, a burn, an approval): no transfer.
    Other,
    /// Not in the shape this read knows: the next read is tried.
    Unreadable,
}

/// The block reads a ledger may serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRead {
    /// ICRC-3's `icrc3_get_blocks`.
    Icrc3,
    /// The ICRC ledgers' `get_transactions`.
    Transactions,
    /// The ICP ledger's own `query_blocks`.
    IcpBlocks,
}

/// Whether `ledger` is the ICP ledger: the one the routes name.
pub fn is_icp_ledger(ledger: Principal, configured: Option<Principal>) -> bool {
    configured == Some(ledger)
}

/// A read's call: its answer; or, when the callee answered with a refusal, a trap or a reply
/// that does not decode (`CanisterReject`, `CanisterError`), why, so the next read is tried.
/// Any other rejection (no wasm, a reinstall, a busy subnet) is asked again later; the
/// replica's text is never read.
async fn call_read<
    A: candid::utils::ArgumentEncoder,
    R: for<'de> candid::utils::ArgumentDecoder<'de>,
>(
    canister: Principal,
    method: &str,
    args: A,
) -> Result<Result<R, String>, CallError> {
    use ic_cdk::api::call::RejectionCode;
    match ic_cdk::call(canister, method, args).await {
        Ok(answer) => Ok(Ok(answer)),
        Err((code @ (RejectionCode::CanisterReject | RejectionCode::CanisterError), message)) => {
            Ok(Err(format!("{method}: {code:?}: {message}")))
        }
        Err((code, message)) => Err(CallError::Unknown(format!("{method}: {code:?}: {message}"))),
    }
}

#[derive(CandidType, Deserialize)]
struct Standard {
    name: String,
}

/// How one read of a block ended: answered in its shape (the transfer, or none: no such
/// block, or a block of another kind), or fell through with why.
pub enum Outcome {
    Answered(Option<Box<LedgerTransfer>>),
    FellThrough(String),
}

impl Outcome {
    fn of(parsed: Parsed, read: &str) -> Self {
        match parsed {
            Parsed::Transfer(transfer) => Outcome::Answered(Some(transfer)),
            Parsed::Other => Outcome::Answered(None),
            Parsed::Unreadable => Outcome::FellThrough(format!("{read}: the block does not parse")),
        }
    }
}

/// What reading one ledger's block asks of it, so the choice of reads runs the same against
/// a fake.
#[allow(async_fn_in_trait)]
pub trait BlockReads {
    /// The names of the standards the ledger declares through `method`; none when it does not
    /// answer them (any failure, a passing one included).
    async fn standards(&self, method: &str) -> Option<Vec<String>>;
    /// One read of the block.
    async fn read(&self, read: BlockRead) -> Result<Outcome, CallError>;
}

/// Whether the ledger lists ICRC-3 among its standards: `icrc1_supported_standards`, and
/// `icrc10_supported_standards` only when the first does not list it.
async fn lists_icrc3(reads: &impl BlockReads) -> bool {
    for method in ["icrc1_supported_standards", "icrc10_supported_standards"] {
        if reads
            .standards(method)
            .await
            .is_some_and(|names| names.iter().any(|name| name == "ICRC-3"))
        {
            return true;
        }
    }
    false
}

/// A ledger's block read through the reads chosen by what the ledger is: the ICP ledger's own `query_blocks` first for the ICP ledger, no standards
/// asked; ICRC-3 when the ledger lists it among its standards; then `get_transactions`. The
/// first that answers in its own shape decides (the transfer, or none: no such block, or a
/// block of another kind). None answering so (a stopped ledger, no wasm, a trap, a shape
/// not read) is a read to try later naming the ledger, never a refusal.
pub async fn read_block(
    reads: &impl BlockReads,
    ledger: Principal,
    block: u64,
    is_icp: bool,
) -> Result<Option<LedgerTransfer>, CallError> {
    let mut why = Vec::new();
    for read in [
        BlockRead::IcpBlocks,
        BlockRead::Icrc3,
        BlockRead::Transactions,
    ] {
        // the standards are asked only once the ICP ledger's own read fell through
        let wanted = match read {
            BlockRead::IcpBlocks => is_icp,
            BlockRead::Icrc3 => lists_icrc3(reads).await,
            BlockRead::Transactions => true,
        };
        if !wanted {
            continue;
        }
        match reads.read(read).await? {
            Outcome::Answered(found) => return Ok(found.map(|found| *found)),
            Outcome::FellThrough(reason) => why.push(reason),
        }
    }
    Err(CallError::Unknown(format!(
        "ledger {ledger} answered none of its block reads in their own shape ({}): its block \
         {block} is not read now, try later",
        why.join("; ")
    )))
}

/// A ledger's block reads through its own calls.
struct LedgerCalls {
    ledger: Principal,
    block: u64,
}

impl BlockReads for LedgerCalls {
    async fn standards(&self, method: &str) -> Option<Vec<String>> {
        match call_read::<_, (Vec<Standard>,)>(self.ledger, method, ()).await {
            Ok(Ok((standards,))) => Some(
                standards
                    .into_iter()
                    .map(|standard| standard.name)
                    .collect(),
            ),
            Ok(Err(_)) | Err(_) => None,
        }
    }

    async fn read(&self, read: BlockRead) -> Result<Outcome, CallError> {
        match read {
            BlockRead::Icrc3 => icrc3_transfer_at(self.ledger, self.block).await,
            BlockRead::Transactions => transactions_transfer_at(self.ledger, self.block).await,
            BlockRead::IcpBlocks => icp_transfer_at(self.ledger, self.block).await,
        }
    }
}

/// The transfer in `ledger`'s block `block`, read on the ledger itself ([`read_block`]).
pub async fn ledger_transfer_at(
    ledger: Principal,
    block: u64,
    is_icp: bool,
) -> Result<Option<LedgerTransfer>, CallError> {
    read_block(&LedgerCalls { ledger, block }, ledger, block, is_icp).await
}

/// A transfer as `get_transactions` answers it.
#[derive(CandidType, Deserialize)]
struct IcrcTransfer {
    from: Account,
    to: Account,
    amount: Nat,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
    spender: Option<Account>,
}

#[derive(CandidType, Deserialize)]
struct LedgerTransaction {
    kind: String,
    transfer: Option<IcrcTransfer>,
}

/// What a `get_transactions` transaction reads as, by its kind.
fn transactions_transfer(transaction: LedgerTransaction) -> Parsed {
    match (transaction.kind.as_str(), transaction.transfer) {
        ("transfer", Some(transfer)) => Parsed::Transfer(Box::new(LedgerTransfer {
            from: Party::Account(transfer.from),
            to: Party::Account(transfer.to),
            amount: transfer.amount,
            memo: transfer.memo,
            created_at_time: transfer.created_at_time,
            spender: transfer.spender.map(Party::Account),
        })),
        ("transfer", None) => Parsed::Unreadable,
        _ => Parsed::Other,
    }
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

/// [`ledger_transfer_at`] through `get_transactions`.
async fn transactions_transfer_at(ledger: Principal, block: u64) -> Result<Outcome, CallError> {
    const READ: &str = "get_transactions";
    let request = || GetTransactionsRequest {
        start: Nat::from(block),
        length: Nat::from(1_u8),
    };
    let answer: GetTransactionsResponse = match call_read(ledger, READ, (request(),)).await? {
        Ok((answer,)) => answer,
        Err(why) => return Ok(Outcome::FellThrough(why)),
    };
    let at = Nat::from(block);
    let first = |transactions: Vec<LedgerTransaction>| match transactions.into_iter().next() {
        Some(transaction) => Outcome::of(transactions_transfer(transaction), READ),
        None => Outcome::Answered(None),
    };
    if answer.first_index == at && !answer.transactions.is_empty() {
        return Ok(first(answer.transactions));
    }
    let Some(range) = answer
        .archived_transactions
        .into_iter()
        .find(|range| range.start <= at && at < range.start.clone() + range.length.clone())
    else {
        return Ok(Outcome::Answered(None));
    };
    match call_read::<_, (TransactionRange,)>(
        range.callback.0.principal,
        &range.callback.0.method,
        (request(),),
    )
    .await?
    {
        Ok((archived,)) => Ok(first(archived.transactions)),
        Err(why) => Ok(Outcome::FellThrough(why)),
    }
}

/// An ICRC-3 value: what an ICRC-3 ledger's blocks are made of.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Icrc3Value {
    Blob(Vec<u8>),
    Text(String),
    Nat(Nat),
    Int(candid::Int),
    Array(Vec<Icrc3Value>),
    Map(Vec<(String, Icrc3Value)>),
}

impl Icrc3Value {
    fn get(&self, key: &str) -> Option<&Icrc3Value> {
        match self {
            Icrc3Value::Map(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Icrc3Value::Text(text) => Some(text),
            _ => None,
        }
    }

    fn nat(&self) -> Option<Nat> {
        match self {
            Icrc3Value::Nat(nat) => Some(nat.clone()),
            Icrc3Value::Int(int) => int.0.to_biguint().map(Nat),
            _ => None,
        }
    }

    fn blob(&self) -> Option<&[u8]> {
        match self {
            Icrc3Value::Blob(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// An ICRC-3 account: its owner and, if any, its subaccount.
    fn account(&self) -> Option<Account> {
        let Icrc3Value::Array(parts) = self else {
            return None;
        };
        let owner = Principal::try_from_slice(parts.first()?.blob()?).ok()?;
        let subaccount = match parts.get(1) {
            Some(part) => Some(part.blob()?.to_vec()),
            None => None,
        };
        Some(Account { owner, subaccount })
    }
}

/// What an ICRC-3 block reads as: a `1xfer` or `2xfer` (with its spender), or a block from
/// before block types whose `tx.op` is `xfer`, as its transfer; any other block type or op as
/// another block; a block in no shape the canister reads (no `tx`, a transfer missing a
/// field) as unreadable.
pub fn icrc3_transfer(block: &Icrc3Value) -> Parsed {
    let Some(tx) = block.get("tx") else {
        return Parsed::Unreadable;
    };
    let is_transfer = match (
        block.get("btype").map(Icrc3Value::text),
        tx.get("op").map(Icrc3Value::text),
    ) {
        (Some(Some(btype)), _) => btype == "1xfer" || btype == "2xfer",
        (None, Some(Some(op))) => op == "xfer",
        _ => return Parsed::Unreadable,
    };
    if !is_transfer {
        return Parsed::Other;
    }
    let read = || -> Option<LedgerTransfer> {
        let created_at_time = match tx.get("ts") {
            Some(ts) => Some(u64::try_from(ts.nat()?.0).ok()?),
            None => None,
        };
        let memo = match tx.get("memo") {
            Some(memo) => Some(memo.blob()?.to_vec()),
            None => None,
        };
        let spender = match tx.get("spender") {
            Some(spender) => Some(Party::Account(spender.account()?)),
            None => None,
        };
        Some(LedgerTransfer {
            from: Party::Account(tx.get("from")?.account()?),
            to: Party::Account(tx.get("to")?.account()?),
            amount: tx.get("amt")?.nat()?,
            memo,
            created_at_time,
            spender,
        })
    };
    read().map_or(Parsed::Unreadable, |transfer| {
        Parsed::Transfer(Box::new(transfer))
    })
}

#[derive(CandidType, Deserialize, Clone, Debug)]
struct Icrc3GetBlocksArgs {
    start: Nat,
    length: Nat,
}

#[derive(CandidType, Deserialize)]
struct Icrc3BlockWithId {
    id: Nat,
    block: Icrc3Value,
}

candid::define_function!(Icrc3ArchiveFn : (Vec<Icrc3GetBlocksArgs>) -> (Icrc3GetBlocksResult) query);

#[derive(CandidType, Deserialize)]
struct Icrc3ArchivedBlocks {
    args: Vec<Icrc3GetBlocksArgs>,
    callback: Icrc3ArchiveFn,
}

#[derive(CandidType, Deserialize)]
struct Icrc3GetBlocksResult {
    blocks: Vec<Icrc3BlockWithId>,
    archived_blocks: Vec<Icrc3ArchivedBlocks>,
}

/// [`ledger_transfer_at`] through ICRC-3.
async fn icrc3_transfer_at(ledger: Principal, block: u64) -> Result<Outcome, CallError> {
    const READ: &str = "icrc3_get_blocks";
    let at = Nat::from(block);
    let request = || {
        vec![Icrc3GetBlocksArgs {
            start: at.clone(),
            length: Nat::from(1_u8),
        }]
    };
    let answer: Icrc3GetBlocksResult = match call_read(ledger, READ, (request(),)).await? {
        Ok((answer,)) => answer,
        Err(why) => return Ok(Outcome::FellThrough(why)),
    };
    let found =
        |blocks: Vec<Icrc3BlockWithId>| match blocks.into_iter().find(|with_id| with_id.id == at) {
            Some(with_id) => Outcome::of(icrc3_transfer(&with_id.block), READ),
            None => Outcome::Answered(None),
        };
    if answer.blocks.iter().any(|with_id| with_id.id == at) {
        return Ok(found(answer.blocks));
    }
    let Some(archived) = answer.archived_blocks.into_iter().find(|archived| {
        archived
            .args
            .iter()
            .any(|args| args.start <= at && at < args.start.clone() + args.length.clone())
    }) else {
        return Ok(Outcome::Answered(None));
    };
    match call_read::<_, (Icrc3GetBlocksResult,)>(
        archived.callback.0.principal,
        &archived.callback.0.method,
        (request(),),
    )
    .await?
    {
        Ok((range,)) => Ok(found(range.blocks)),
        Err(why) => Ok(Outcome::FellThrough(why)),
    }
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug)]
struct IcpGetBlocksArgs {
    start: u64,
    length: u64,
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub e8s: u64,
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeStamp {
    pub timestamp_nanos: u64,
}

/// An ICP ledger block's operation, the transfer's fields the canister reads; the others
/// are read as nothing.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum IcpOperation {
    Transfer {
        from: Vec<u8>,
        to: Vec<u8>,
        amount: Tokens,
        fee: Tokens,
        spender: Option<Vec<u8>>,
    },
    Mint(candid::Reserved),
    Burn(candid::Reserved),
    Approve(candid::Reserved),
}

#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct IcpTransaction {
    pub memo: u64,
    pub icrc1_memo: Option<Vec<u8>>,
    pub operation: Option<IcpOperation>,
    pub created_at_time: Option<TimeStamp>,
}

/// An ICP ledger block (`query_blocks`), its transaction.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct IcpBlock {
    pub transaction: IcpTransaction,
}

/// What an ICP ledger block reads as: a transfer, its parties account identifiers; a mint,
/// a burn or an approval as another block; no operation, or an identifier that is not 32
/// bytes, as unreadable.
pub fn icp_transfer(block: IcpBlock) -> Parsed {
    let identifier = |bytes: Vec<u8>| <[u8; 32]>::try_from(bytes).ok().map(Party::Identifier);
    let (from, to, amount, spender) = match block.transaction.operation {
        Some(IcpOperation::Transfer {
            from,
            to,
            amount,
            spender,
            ..
        }) => (from, to, amount, spender),
        Some(_) => return Parsed::Other,
        None => return Parsed::Unreadable,
    };
    let read = || -> Option<LedgerTransfer> {
        let spender = match spender {
            Some(bytes) => Some(identifier(bytes)?),
            None => None,
        };
        Some(LedgerTransfer {
            from: identifier(from)?,
            to: identifier(to)?,
            amount: Nat::from(amount.e8s),
            memo: block.transaction.icrc1_memo,
            created_at_time: block
                .transaction
                .created_at_time
                .map(|time| time.timestamp_nanos),
            spender,
        })
    };
    read().map_or(Parsed::Unreadable, |transfer| {
        Parsed::Transfer(Box::new(transfer))
    })
}

#[derive(CandidType, Deserialize)]
struct IcpBlockRange {
    blocks: Vec<IcpBlock>,
}

#[derive(CandidType, Deserialize)]
enum IcpArchiveResult {
    Ok(IcpBlockRange),
    Err(candid::Reserved),
}

candid::define_function!(IcpArchiveFn : (IcpGetBlocksArgs) -> (IcpArchiveResult) query);

#[derive(CandidType, Deserialize)]
struct IcpArchivedRange {
    start: u64,
    length: u64,
    callback: IcpArchiveFn,
}

#[derive(CandidType, Deserialize)]
struct IcpQueryBlocksResponse {
    first_block_index: u64,
    blocks: Vec<IcpBlock>,
    archived_blocks: Vec<IcpArchivedRange>,
}

/// [`ledger_transfer_at`] through the ICP ledger's `query_blocks`.
async fn icp_transfer_at(ledger: Principal, block: u64) -> Result<Outcome, CallError> {
    const READ: &str = "query_blocks";
    let request = IcpGetBlocksArgs {
        start: block,
        length: 1,
    };
    let answer: IcpQueryBlocksResponse = match call_read(ledger, READ, (request,)).await? {
        Ok((answer,)) => answer,
        Err(why) => return Ok(Outcome::FellThrough(why)),
    };
    let first = |blocks: Vec<IcpBlock>| match blocks.into_iter().next() {
        Some(found) => Outcome::of(icp_transfer(found), READ),
        None => Outcome::Answered(None),
    };
    if answer.first_block_index == block && !answer.blocks.is_empty() {
        return Ok(first(answer.blocks));
    }
    let Some(range) = answer
        .archived_blocks
        .into_iter()
        .find(|range| range.start <= block && block < range.start.saturating_add(range.length))
    else {
        return Ok(Outcome::Answered(None));
    };
    match call_read::<_, (IcpArchiveResult,)>(
        range.callback.0.principal,
        &range.callback.0.method,
        (request,),
    )
    .await?
    {
        Ok((IcpArchiveResult::Ok(range),)) => Ok(first(range.blocks)),
        Ok((IcpArchiveResult::Err(_),)) => Ok(Outcome::FellThrough(format!(
            "{READ}: the archive refused the range"
        ))),
        Err(why) => Ok(Outcome::FellThrough(why)),
    }
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
/// deduplicated: a retry with the same pair is the same approval, which the ledger
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

/// One token the minter supports, as a `TokenNotSupported` refusal lists them.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CkErc20Token {
    pub ckerc20_token_symbol: String,
    pub erc20_contract_address: String,
    pub ledger_canister_id: Principal,
}

/// Why a burn of the minter's failed, its candid's `LedgerError`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
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

/// The minter's refusals of a `withdraw_erc20`, its candid's `WithdrawErc20Error`. Each is a
/// reply, and in each no ckUSDC was burned: the minter burns ckETH first, then ckUSDC, and
/// answers `CkErc20LedgerError` when the second burn failed, the ckETH reimbursed.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
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

impl WithdrawErc20Error {
    /// Whether asking again may pass: a minter or ledger out for now, or ckETH short of the
    /// gas of the moment. A token, a recipient or a ckUSDC burn refused stays refused.
    pub fn may_pass(&self) -> bool {
        match self {
            Self::TemporarilyUnavailable(_) | Self::CkEthLedgerError { .. } => true,
            Self::CkErc20LedgerError { error, .. } => {
                matches!(error, LedgerError::TemporarilyUnavailable(_))
            }
            Self::TokenNotSupported { .. } | Self::RecipientAddressBlocked { .. } => false,
        }
    }
}

impl std::fmt::Display for WithdrawErc20Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TokenNotSupported { .. } => write!(f, "the minter does not support the token"),
            Self::RecipientAddressBlocked { address } => {
                write!(f, "the minter blocks the recipient {address}")
            }
            Self::CkEthLedgerError { error } => {
                write!(f, "the minter's ckETH burn failed: {error:?}")
            }
            Self::CkErc20LedgerError {
                cketh_block_index,
                error,
            } => write!(
                f,
                "the minter's ckUSDC burn failed after its ckETH burn at {cketh_block_index}: {error:?}"
            ),
            Self::TemporarilyUnavailable(why) => {
                write!(f, "the minter is unavailable for now: {why}")
            }
        }
    }
}

/// How a `withdraw_erc20` that made no withdrawal ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WithdrawFailure {
    /// The minter answered: no ckUSDC was burned.
    Refused(WithdrawErc20Error),
    /// No answer to go on: the system rejected the call, its reply did not decode, or the
    /// minter trapped. The minter's guards trap before any burn (one call per caller, 100
    /// pending, 100 at once), but nothing tells such a trap from one after a burn, so this
    /// never says nothing was burned.
    Unknown(String),
}

/// `withdraw_erc20` to `recipient`. The withdrawal id is the ckETH burn's block index, as
/// the minter numbers withdrawals.
// the minter's own candid sets the size of its refusals
#[allow(clippy::result_large_err)]
pub async fn withdraw_erc20(
    minter: Principal,
    ckerc20_ledger: Principal,
    amount: Nat,
    recipient: String,
) -> Result<RetrieveErc20Request, WithdrawFailure> {
    let arg = WithdrawErc20Arg {
        amount,
        ckerc20_ledger_id: ckerc20_ledger,
        recipient,
        from_cketh_subaccount: None,
        from_ckerc20_subaccount: None,
    };
    let (answer,): (Result<RetrieveErc20Request, WithdrawErc20Error>,) =
        call(minter, "withdraw_erc20", (arg,))
            .await
            .map_err(|error| WithdrawFailure::Unknown(error.to_string()))?;
    answer.map_err(WithdrawFailure::Refused)
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
