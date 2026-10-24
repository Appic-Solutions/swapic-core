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

/// What a block reads as in one read's shape (the review, 2026-10-24).
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

/// Whether `ledger` is the ICP ledger: the one the routes name (the review, 2026-10-24).
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

/// A ledger's block read through the reads chosen by what the ledger is (the review,
/// 2026-10-24): the ICP ledger's own `query_blocks` first for the ICP ledger, no standards
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

    fn principal(byte: u8) -> Principal {
        Principal::from_slice(&[byte; 10])
    }

    fn account_value(owner: Principal, subaccount: Option<[u8; 32]>) -> Icrc3Value {
        let mut parts = vec![Icrc3Value::Blob(owner.as_slice().to_vec())];
        if let Some(bytes) = subaccount {
            parts.push(Icrc3Value::Blob(bytes.to_vec()));
        }
        Icrc3Value::Array(parts)
    }

    fn map(entries: Vec<(&str, Icrc3Value)>) -> Icrc3Value {
        Icrc3Value::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// The review (2026-10-24): an ICRC-3 block reads as the transfer it logs, in the older
    /// shape (`tx.op` "xfer") and the newer (`btype` "1xfer" or "2xfer", a `transfer_from` with
    /// its spender); a block that moves nothing to anyone (a mint, an approval) is another
    /// block; one that does not parse in the ICRC-3 shape is unreadable, for the next read.
    #[test]
    fn an_icrc3_block_reads_as_its_transfer() {
        let tx = |op: Option<&str>, spender: bool| {
            let mut entries = vec![
                ("amt", Icrc3Value::Nat(Nat::from(98_990_000_u64))),
                ("from", account_value(principal(1), None)),
                ("to", account_value(principal(2), Some([7; 32]))),
                ("memo", Icrc3Value::Blob(vec![0xab; 32])),
                (
                    "ts",
                    Icrc3Value::Nat(Nat::from(1_800_000_000_000_000_005_u64)),
                ),
            ];
            if let Some(op) = op {
                entries.push(("op", Icrc3Value::Text(op.to_string())));
            }
            if spender {
                entries.push(("spender", account_value(principal(3), None)));
            }
            map(entries)
        };
        let expected = |spender: Option<Party>| {
            Parsed::Transfer(Box::new(LedgerTransfer {
                from: Party::Account(Account::of(principal(1), None)),
                to: Party::Account(Account::of(principal(2), Some([7; 32]))),
                amount: Nat::from(98_990_000_u64),
                memo: Some(vec![0xab; 32]),
                created_at_time: Some(1_800_000_000_000_000_005),
                spender,
            }))
        };
        let older = map(vec![
            ("phash", Icrc3Value::Blob(vec![0; 32])),
            ("ts", Icrc3Value::Nat(Nat::from(9_u8))),
            ("tx", tx(Some("xfer"), false)),
        ]);
        assert_eq!(icrc3_transfer(&older), expected(None));
        let newer = map(vec![
            ("btype", Icrc3Value::Text("2xfer".to_string())),
            ("ts", Icrc3Value::Nat(Nat::from(9_u8))),
            ("tx", tx(None, true)),
        ]);
        assert_eq!(
            icrc3_transfer(&newer),
            expected(Some(Party::Account(Account::of(principal(3), None))))
        );
        let mint = map(vec![("tx", tx(Some("mint"), false))]);
        assert_eq!(icrc3_transfer(&mint), Parsed::Other);
        let approval = map(vec![
            ("btype", Icrc3Value::Text("2approve".to_string())),
            ("tx", tx(None, true)),
        ]);
        assert_eq!(icrc3_transfer(&approval), Parsed::Other);
        // no shape the canister reads: unreadable, never "no transfer"
        assert_eq!(
            icrc3_transfer(&Icrc3Value::Nat(Nat::from(1_u8))),
            Parsed::Unreadable
        );
        assert_eq!(
            icrc3_transfer(&map(vec![("tx", tx(None, false))])),
            Parsed::Unreadable
        );
        let mut no_amount = tx(Some("xfer"), false);
        if let Icrc3Value::Map(entries) = &mut no_amount {
            entries.retain(|(key, _)| key != "amt");
        }
        assert_eq!(
            icrc3_transfer(&map(vec![("tx", no_amount)])),
            Parsed::Unreadable
        );
    }

    /// The review (2026-10-24): a `get_transactions` transaction reads by its kind: a transfer
    /// with its fields, another kind as another block, a transfer with none as unreadable.
    #[test]
    fn a_transactions_block_reads_by_its_kind() {
        let transfer = IcrcTransfer {
            from: Account::of(principal(1), None),
            to: Account::of(principal(2), None),
            amount: Nat::from(5_u8),
            memo: None,
            created_at_time: Some(7),
            spender: None,
        };
        let transaction = |kind: &str, transfer: Option<IcrcTransfer>| LedgerTransaction {
            kind: kind.to_string(),
            transfer,
        };
        assert!(matches!(
            transactions_transfer(transaction("transfer", Some(transfer))),
            Parsed::Transfer(_)
        ));
        assert_eq!(
            transactions_transfer(transaction("mint", None)),
            Parsed::Other
        );
        assert_eq!(
            transactions_transfer(transaction("transfer", None)),
            Parsed::Unreadable
        );
    }

    /// The review (2026-10-24): an ICP ledger block (`query_blocks`) reads as the transfer it
    /// logs, its parties the account identifiers the ICP ledger keeps, its memo the ICRC-1
    /// one; a mint is another block; an operation the canister cannot read is unreadable.
    #[test]
    fn an_icp_ledger_block_reads_as_its_transfer() {
        let block = |operation: Option<IcpOperation>| IcpBlock {
            transaction: IcpTransaction {
                memo: 0,
                icrc1_memo: Some(vec![0xab; 32]),
                operation,
                created_at_time: Some(TimeStamp {
                    timestamp_nanos: 1_800_000_000_000_000_005,
                }),
            },
        };
        let transfer = |from: Vec<u8>| {
            Some(IcpOperation::Transfer {
                from,
                to: vec![2; 32],
                amount: Tokens { e8s: 12_345 },
                fee: Tokens { e8s: 10_000 },
                spender: None,
            })
        };
        assert_eq!(
            icp_transfer(block(transfer(vec![1; 32]))),
            Parsed::Transfer(Box::new(LedgerTransfer {
                from: Party::Identifier([1; 32]),
                to: Party::Identifier([2; 32]),
                amount: Nat::from(12_345_u64),
                memo: Some(vec![0xab; 32]),
                created_at_time: Some(1_800_000_000_000_000_005),
                spender: None,
            }))
        );
        assert_eq!(
            icp_transfer(block(Some(IcpOperation::Mint(candid::Reserved)))),
            Parsed::Other
        );
        assert_eq!(
            icp_transfer(block(transfer(vec![1; 31]))),
            Parsed::Unreadable
        );
        assert_eq!(icp_transfer(block(None)), Parsed::Unreadable);
    }

    /// A ledger's block reads as a fake answers them, and the calls made.
    struct Reads {
        calls: std::cell::RefCell<Vec<String>>,
        /// What each standards method lists; absent: it does not answer.
        standards: Vec<(&'static str, Vec<String>)>,
        /// Which reads answer the transfer; any other falls through.
        answering: Vec<BlockRead>,
    }

    impl BlockReads for Reads {
        async fn standards(&self, method: &str) -> Option<Vec<String>> {
            self.calls.borrow_mut().push(method.to_string());
            self.standards
                .iter()
                .find(|(name, _)| *name == method)
                .map(|(_, listed)| listed.clone())
        }

        async fn read(&self, read: BlockRead) -> Result<Outcome, CallError> {
            self.calls.borrow_mut().push(format!("{read:?}"));
            Ok(if self.answering.contains(&read) {
                Outcome::Answered(None)
            } else {
                Outcome::FellThrough(format!("{read:?} fell through"))
            })
        }
    }

    fn reads(standards: Vec<(&'static str, &[&str])>, answering: Vec<BlockRead>) -> Reads {
        Reads {
            calls: std::cell::RefCell::default(),
            standards: standards
                .into_iter()
                .map(|(method, names)| {
                    (method, names.iter().map(|name| name.to_string()).collect())
                })
                .collect(),
            answering,
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(out) => out,
            std::task::Poll::Pending => panic!("the fake ledger never waits"),
        }
    }

    /// The review (2026-10-24): the read is chosen by what the ledger is. The ICP ledger (the
    /// one the routes name) reads `query_blocks` first, no standards asked; ICRC-3 is read when
    /// the ledger lists it (`icrc10_supported_standards` asked only if `icrc1_supported_standards`
    /// does not); a standards call that does not answer falls to `get_transactions`; each read
    /// not answering in its shape falls to the next, and none answering is a read to try later,
    /// naming the ledger, never a refusal.
    #[test]
    fn the_block_read_is_chosen_by_the_ledgers_own_standards() {
        let ledger = principal(9);
        let icp = reads(vec![], vec![BlockRead::IcpBlocks]);
        assert_eq!(block_on(read_block(&icp, ledger, 1, true)), Ok(None));
        assert_eq!(*icp.calls.borrow(), vec!["IcpBlocks".to_string()]);
        let icrc3 = reads(
            vec![("icrc1_supported_standards", &["ICRC-1", "ICRC-3"])],
            vec![BlockRead::Icrc3],
        );
        assert_eq!(block_on(read_block(&icrc3, ledger, 1, false)), Ok(None));
        assert_eq!(
            *icrc3.calls.borrow(),
            vec!["icrc1_supported_standards".to_string(), "Icrc3".to_string()]
        );
        let icrc10 = reads(
            vec![
                ("icrc1_supported_standards", &["ICRC-1"]),
                ("icrc10_supported_standards", &["ICRC-3"]),
            ],
            vec![BlockRead::Icrc3],
        );
        assert_eq!(block_on(read_block(&icrc10, ledger, 1, false)), Ok(None));
        assert_eq!(icrc10.calls.borrow().last(), Some(&"Icrc3".to_string()));
        let unlisted = reads(vec![], vec![BlockRead::Transactions]);
        assert_eq!(block_on(read_block(&unlisted, ledger, 1, false)), Ok(None));
        assert_eq!(
            *unlisted.calls.borrow(),
            vec![
                "icrc1_supported_standards".to_string(),
                "icrc10_supported_standards".to_string(),
                "Transactions".to_string()
            ]
        );
        let none = reads(vec![("icrc1_supported_standards", &["ICRC-3"])], vec![]);
        assert!(matches!(
            block_on(read_block(&none, ledger, 1, false)),
            Err(CallError::Unknown(why)) if why.contains(&ledger.to_text())
        ));
        assert!(is_icp_ledger(principal(4), Some(principal(4))));
        assert!(!is_icp_ledger(
            Principal::from_text("ryjl3-tyaaa-aaaaa-aaaba-cai").unwrap(),
            None
        ));
    }
}
