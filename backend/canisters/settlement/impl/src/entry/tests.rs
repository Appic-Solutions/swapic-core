use super::*;
use crate::state::transitions::tests::quote;
use crate::storage::{on_fresh_memory, sanctions};
use types::abi::Permit;
use types::{BlockNumber, GasMode};

const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

/// The fixture quote with an EVM token, which is what a claim on an EVM chain needs.
fn evm_quote(nonce: u64) -> Quote {
    Quote {
        src_token: USDC.parse().unwrap(),
        ..quote(nonce)
    }
}

fn deposit(token: &str, amount: u128) -> VerifiedDeposit {
    VerifiedDeposit {
        token: token.parse().unwrap(),
        from: USER.parse().unwrap(),
        amount: TokenAmount::from(amount),
        tx_ref: TxHash::new([0x77; 32]),
        block: BlockNumber::new(19_000_000),
    }
}

/// A quote is claimable through its expiry and the permit window after it, the same
/// window the pending store evicts on: a deposit made in the quote's last second and
/// claimed a few blocks later is still the user's swap, and past the window nobody can pay
/// the quote so nothing is claimed for it.
#[test]
fn a_quote_is_claimable_through_its_expiry_and_the_permit_window() {
    let quote = evm_quote(1);
    let window = Duration::from_secs(120);
    let expires_at = quote.expires_at.get();
    assert_eq!(
        claim_deadline(&quote, window),
        Some(UnixSeconds::new(expires_at + 120))
    );
    for now in [expires_at - 1, expires_at, expires_at + 120] {
        assert_eq!(
            ensure_claimable(&quote, UnixSeconds::new(now), window),
            Ok(()),
            "claimable at {now}"
        );
    }
    assert_eq!(
        ensure_claimable(&quote, UnixSeconds::new(expires_at + 121), window),
        Err(ClaimError::QuoteExpired {
            expires_at: quote.expires_at,
            claim_until: UnixSeconds::new(expires_at + 120),
            now: UnixSeconds::new(expires_at + 121),
        })
    );
    // a window past the end of time holds every claim
    let forever = Quote {
        expires_at: UnixSeconds::new(u64::MAX - 1),
        ..evm_quote(1)
    };
    assert_eq!(claim_deadline(&forever, window), None);
    assert_eq!(
        ensure_claimable(&forever, UnixSeconds::new(u64::MAX), window),
        Ok(())
    );
}

/// Both addresses a quote pays to are checked before an outcall is spent, and the refusal
/// names which.
#[test]
fn a_sanctioned_destination_or_refund_address_names_itself() {
    on_fresh_memory(|| {
        crate::storage::init();
        let quote = Quote {
            refund_address: Some("0xrefund".parse().unwrap()),
            ..evm_quote(1)
        };
        assert_eq!(sanctioned_party(&quote), None);
        sanctions::apply(&["0xrefund".parse().unwrap()], &[]).unwrap();
        assert_eq!(sanctioned_party(&quote), Some("refund_address"));
        sanctions::apply(std::slice::from_ref(&quote.dst_address), &[]).unwrap();
        assert_eq!(
            sanctioned_party(&quote),
            Some("dst_address"),
            "the destination is named first"
        );
        let no_refund = Quote {
            refund_address: None,
            dst_address: "0xclean".parse().unwrap(),
            ..evm_quote(1)
        };
        assert_eq!(sanctioned_party(&no_refund), None);
    });
}

/// The deposit the vault holds must be the quote's: its token, and exactly its amount. A
/// deposit of another token cannot ride the quote's rail, and one of another amount is
/// neither the swap the user was quoted nor one the canister can price, so both are refused
/// and the funds stay where they are for an operator.
#[test]
fn a_deposit_must_be_the_quotes_token_and_exactly_its_amount() {
    let quote = evm_quote(1);
    let token: EvmAddress = USDC.parse().unwrap();
    assert_eq!(
        ensure_deposit_matches(&quote, token, &deposit(USDC, 100)),
        Ok(())
    );
    assert_eq!(
        ensure_deposit_matches(&quote, token, &deposit(USER, 100)),
        Err(ClaimError::TokenMismatch {
            quoted: token,
            deposited: USER.parse().unwrap()
        })
    );
    for wrong in [99, 101] {
        assert_eq!(
            ensure_deposit_matches(&quote, token, &deposit(USDC, wrong)),
            Err(ClaimError::AmountMismatch {
                quoted: TokenAmount::from(100_u8),
                deposited: TokenAmount::from(wrong)
            })
        );
    }
}

/// The source token of a quote claimed on an EVM chain has to be an address, and the
/// refusal is made before any outcall.
#[test]
fn a_source_token_that_is_not_an_address_is_refused_by_name() {
    let quote = quote(1);
    assert_eq!(
        source_token(&quote),
        Err(ClaimError::SourceTokenNotAnAddress {
            token: quote.src_token.clone(),
            reason: EvmAddressError::NoPrefix,
        })
    );
    assert_eq!(source_token(&evm_quote(1)), Ok(USDC.parse().unwrap()));
}

/// The line the claim writes carries the chain's truth: the token as the vault logged it,
/// the amount the vault measured, and the transaction the deposit is in.
#[test]
fn the_funds_received_line_carries_what_the_vault_logged() {
    let quote = evm_quote(1);
    let line = funds_received(&quote, &deposit(&USDC.to_ascii_lowercase(), 100));
    assert_eq!(
        line,
        EventType::FundsReceived {
            quote_hash: quote.hash().unwrap(),
            quote_bytes: quote.canonical_bytes().unwrap(),
            chain_id: quote.src_chain,
            token: USDC.parse().unwrap(),
            amount: TokenAmount::from(100_u8),
            tx_ref: format!("0x{}", hex::encode([0x77; 32])),
        }
    );
}

/// The pull needs the quote's own permit: the vault pulls the quote's token and amount, so
/// a permit for anything else would either fail on the chain or take the wrong funds.
#[test]
fn a_permit_must_be_for_the_quotes_token_and_amount() {
    let quote = evm_quote(1);
    let token: EvmAddress = USDC.parse().unwrap();
    let permit = |token: &str, amount: u128| PullPermit {
        token: token.parse().unwrap(),
        owner: USER.parse().unwrap(),
        amount: TokenAmount::from(amount),
        deadline: UnixSeconds::new(1_800_000_000),
        signature: Permit {
            v: 27,
            r: [1; 32],
            s: [2; 32],
        },
    };
    assert_eq!(
        ensure_permit_matches(&quote, token, &permit(USDC, 100)),
        Ok(())
    );
    assert_eq!(
        ensure_permit_matches(&quote, token, &permit(USER, 100)),
        Err(PullError::PermitMismatch { field: "token" })
    );
    assert_eq!(
        ensure_permit_matches(&quote, token, &permit(USDC, 101)),
        Err(PullError::PermitMismatch { field: "amount" })
    );
    let legacy = Quote {
        gas_mode: GasMode::Legacy,
        ..evm_quote(1)
    };
    assert_eq!(ensure_gasless(&legacy), Err(PullError::NotGasless));
    assert_eq!(ensure_gasless(&quote), Ok(()));
}
