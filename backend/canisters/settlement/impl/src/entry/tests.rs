use super::*;
use crate::deposits::{Wanted, WantedAmount};
use crate::state::transitions::tests::quote;
use crate::storage::{on_fresh_memory, sanctions};
use std::collections::BTreeMap;
use types::abi::Permit;
use types::config::ChainTable;
use types::evm::EvmAddressError;
use types::quote::{QuoteAddressError, QuoteAddressField};
use types::rail::RailTokenError;
use types::{BlockNumber, ChainId, Config, GasMode, Rail, TokenAmount};

const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC_ARBITRUM: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

/// The fixture quote with the rail's tokens on both sides, which is what a claim on an
/// EVM chain needs.
fn evm_quote(nonce: u64) -> Quote {
    Quote {
        src_token: USDC.parse().unwrap(),
        dst_token: USDC_ARBITRUM.parse().unwrap(),
        ..quote(nonce)
    }
}

/// The rails' tokens: the USDC of Base and of Arbitrum.
fn rail_config() -> Config {
    Config {
        usdc_addresses: ChainTable(BTreeMap::from([
            (ChainId::BASE, USDC.parse().unwrap()),
            (ChainId::ARBITRUM, USDC_ARBITRUM.parse().unwrap()),
        ])),
        ..Config::default()
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

/// The deposit the claim wants is the quote's: its token, and exactly its amount. A
/// deposit of another token cannot ride the quote's rail, and one of another amount is
/// neither the swap the user was quoted nor one the canister can price, so neither is the
/// deposit, whatever else the vault holds under the hash, and the funds stay where they
/// are for an operator.
#[test]
fn the_claim_wants_the_quotes_token_and_exactly_its_amount() {
    let quote = evm_quote(1);
    let token: EvmAddress = USDC.parse().unwrap();
    let wanted = wanted(&quote, token);
    assert_eq!(
        wanted,
        Wanted {
            token,
            amount: WantedAmount::Exactly(TokenAmount::from(100_u8)),
        }
    );
    assert!(wanted.admits(&deposit(USDC, 100)));
    assert!(!wanted.admits(&deposit(USER, 100)), "another token");
    for wrong in [99, 101] {
        assert!(!wanted.admits(&deposit(USDC, wrong)), "another amount");
    }
}

/// The tokens of a quote claimed on an EVM chain have to be the rail's, compared as
/// addresses: a worthless token on either side is refused by the field before any outcall,
/// as is text that is no address, and the source token the read then looks for is the
/// rail's own.
#[test]
fn a_source_token_that_is_not_an_address_is_refused_by_name() {
    let config = rail_config();
    let quote = quote(1);
    assert_eq!(
        rail_source_token(&config, &quote),
        Err(ClaimError::RailToken(RailTokenError::QuoteAddress(
            QuoteAddressError::NotAnAddress {
                field: QuoteAddressField::SrcToken,
                reason: EvmAddressError::NoPrefix,
            }
        )))
    );
    assert_eq!(
        rail_source_token(&config, &evm_quote(1)),
        Ok(USDC.parse().unwrap())
    );
}

/// The rails carry the configured USDC and nothing else: a quote naming any other token,
/// on either side, is refused before an outcall is bought, so the burn can never spend the
/// vault's USDC against a deposit of something else.
#[test]
fn a_quote_naming_a_worthless_token_is_refused_before_any_outcall() {
    let config = rail_config();
    let worthless: EvmAddress = USER.parse().unwrap();
    let wrong_source = Quote {
        src_token: USER.parse().unwrap(),
        ..evm_quote(1)
    };
    assert_eq!(
        rail_source_token(&config, &wrong_source),
        Err(ClaimError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::SrcToken,
            quoted: worthless,
            rail_token: USDC.parse().unwrap(),
            rail: Rail::CctpV2Fast,
        }))
    );
    let wrong_destination = Quote {
        dst_token: USER.parse().unwrap(),
        ..evm_quote(1)
    };
    assert_eq!(
        rail_source_token(&config, &wrong_destination),
        Err(ClaimError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::DstToken,
            quoted: worthless,
            rail_token: USDC_ARBITRUM.parse().unwrap(),
            rail: Rail::CctpV2Fast,
        }))
    );
    let no_table = Config::default();
    assert_eq!(
        rail_source_token(&no_table, &evm_quote(1)),
        Err(ClaimError::RailToken(RailTokenError::NoRailToken {
            chain_id: ChainId::BASE,
            rail: Rail::CctpV2Fast,
        }))
    );
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

/// The Eco rail is off until its route is designed: a quote naming it is refused at the
/// claim, before an outcall is bought, and the refusal says which knob turns it on. Every
/// other rail is unaffected, and with the knob on an Eco quote is admitted like any other.
#[test]
fn an_eco_quote_is_refused_while_the_rail_is_off() {
    let off = rail_config();
    let eco = Quote {
        rail: Rail::Eco,
        ..evm_quote(1)
    };
    assert_eq!(
        ensure_rail_is_enabled(&off, &eco),
        Err(ClaimError::RailUnavailable { rail: Rail::Eco })
    );
    assert_eq!(ensure_rail_is_enabled(&off, &evm_quote(1)), Ok(()));
    let on = Config {
        eco_enabled: types::config::EcoEnabled::ON,
        ..rail_config()
    };
    assert_eq!(ensure_rail_is_enabled(&on, &eco), Ok(()));
}

/// A swap that cannot be refunded is a swap that can freeze with the user's funds in the
/// vault, and this canister does not record the payer, so the refund address is the only
/// way back. Both doors refuse a quote without one, before anything is read or signed.
#[test]
fn a_quote_naming_no_refund_address_is_refused_at_both_doors() {
    let no_refund = Quote {
        refund_address: None,
        ..evm_quote(1)
    };
    assert_eq!(
        ensure_refundable(&no_refund),
        Err(ClaimError::QuoteAddress(QuoteAddressError::Absent {
            field: QuoteAddressField::RefundAddress,
        }))
    );
    let with_refund = Quote {
        refund_address: Some(USER.parse().unwrap()),
        ..evm_quote(1)
    };
    assert_eq!(ensure_refundable(&with_refund), Ok(()));
    // and it has to be an address a refund can be paid to
    let not_an_address = Quote {
        refund_address: Some("0xrefund".parse().unwrap()),
        ..evm_quote(1)
    };
    assert_eq!(
        ensure_refundable(&not_an_address),
        Err(ClaimError::QuoteAddress(QuoteAddressError::NotAnAddress {
            field: QuoteAddressField::RefundAddress,
            reason: EvmAddressError::WrongLength { len: 6 },
        }))
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
