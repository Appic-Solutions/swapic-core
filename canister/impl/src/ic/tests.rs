use super::*;
use crate::test_util::block_on;
use candid::{Decode, Encode};

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
            .map(|(method, names)| (method, names.iter().map(|name| name.to_string()).collect()))
            .collect(),
        answering,
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

/// Replies of `withdraw_erc20` as the live minter's own candid encodes them: every line of
/// `samples/minter_withdraw_erc20_replies.txt` was written from `candid:service` of
/// `sv3dd-oaaaa-aaaar-qacoa-cai` (read 2026-11-06), not from these types.
fn minter_reply(name: &str) -> Vec<u8> {
    let line = include_str!("../../samples/minter_withdraw_erc20_replies.txt")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name} ")))
        .unwrap();
    hex::decode(line).unwrap()
}

#[allow(clippy::result_large_err)]
fn decoded(name: &str) -> Result<RetrieveErc20Request, WithdrawErc20Error> {
    let bytes = minter_reply(name);
    Decode!(&bytes, Result<RetrieveErc20Request, WithdrawErc20Error>).unwrap()
}

#[test]
fn the_minters_own_replies_read_as_their_typed_refusals() {
    let request = decoded("ok").unwrap();
    assert_eq!(request.cketh_block_index, Nat::from(7_u8));
    assert_eq!(request.ckerc20_block_index, Nat::from(8_u8));
    let refusal = |name: &str| decoded(name).unwrap_err();
    assert_eq!(
        refusal("temporarily"),
        WithdrawErc20Error::TemporarilyUnavailable(
            "Failed to retrieve current gas fee".to_string()
        )
    );
    assert!(matches!(
        refusal("blocked"),
        WithdrawErc20Error::RecipientAddressBlocked { .. }
    ));
    assert!(matches!(
        refusal("unsupported"),
        WithdrawErc20Error::TokenNotSupported { supported_tokens } if supported_tokens.len() == 1
    ));
    assert!(matches!(
        refusal("cketh"),
        WithdrawErc20Error::CkEthLedgerError {
            error: LedgerError::InsufficientAllowance { .. }
        }
    ));
    assert!(matches!(
        refusal("ckerc20_funds"),
        WithdrawErc20Error::CkErc20LedgerError {
            error: LedgerError::InsufficientFunds { .. },
            ..
        }
    ));
    // passing: the minter or a ledger out for now, ckETH short of the gas of the moment
    for name in ["temporarily", "cketh", "ckerc20_busy"] {
        assert!(refusal(name).may_pass(), "{name}");
    }
    for name in ["blocked", "unsupported", "ckerc20_funds", "ckerc20_low"] {
        assert!(!refusal(name).may_pass(), "{name}");
    }
}

/// A refusal this build does not know does not decode, and `ic_cdk` answers a reply that does
/// not decode as a rejection: `Unknown`, never a refusal, so it is never read as nothing burned.
#[test]
fn a_refusal_of_a_kind_not_known_is_no_refusal() {
    #[derive(CandidType)]
    enum Newer {
        BrandNew(String),
    }
    let bytes = Encode!(&Err::<RetrieveErc20Request, Newer>(Newer::BrandNew(
        "a new refusal".to_string()
    )))
    .unwrap();
    assert!(Decode!(&bytes, Result<RetrieveErc20Request, WithdrawErc20Error>).is_err());
}
