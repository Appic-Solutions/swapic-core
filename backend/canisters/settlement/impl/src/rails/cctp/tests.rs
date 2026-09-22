use super::*;
use crate::rails::tests::{
    config, fixture_swap, leg, quote, MINE, USDC_ARBITRUM, USDC_BASE, VAULT_ARBITRUM, VAULT_BASE,
};
use crate::rails::{RailStep, WaitingFor};
use types::abi::{decode_cctp_deposit_for_burn, decode_cctp_receive_message, decode_vault_execute};
use types::cctp::{BurnBody, BurnMessage, BURN_BODY_VERSION, MESSAGE_VERSION};
use types::config::ChainTable;
use types::evm::EvmAddressError;
use types::quote::{QuoteAddressError, QuoteAddressField};
use types::rail::RailTokenError;
use types::{Attestation, BlockNumber, ChainId, Config, EvmAddress, Outcome, Timestamp, TxHash};

fn word(address: &str) -> [u8; 32] {
    address.parse::<EvmAddress>().unwrap().to_word()
}

/// The message Circle attests for the fixture swap's fast burn: every field the burn
/// determined as the burn emitted it, and the nonce, the fee and the expiration as the
/// attestation service filled them in.
pub(crate) fn attested_message() -> BurnMessage {
    let config = config();
    BurnMessage {
        version: MESSAGE_VERSION,
        source_domain: 6,
        destination_domain: 3,
        nonce: [0x9a; 32],
        sender: config.token_messenger.unwrap().to_word(),
        recipient: config.token_messenger.unwrap().to_word(),
        destination_caller: word(MINE),
        min_finality_threshold: FAST_FINALITY_THRESHOLD,
        finality_threshold_executed: FAST_FINALITY_THRESHOLD,
        body: BurnBody {
            version: BURN_BODY_VERSION,
            burn_token: word(USDC_BASE),
            mint_recipient: word(VAULT_ARBITRUM),
            amount: TokenAmount::from(25_000_000_u32),
            message_sender: word(VAULT_BASE),
            max_fee: TokenAmount::from(5_000_u32),
            fee_executed: TokenAmount::from(2_500_u32),
            expiration_block: BlockNumber::new(19_000_100),
            hook_data: vec![],
        },
    }
}

fn attestation_of(message: &BurnMessage) -> Attestation {
    Attestation::new(message.encode(), vec![0xbb; 65], Timestamp::from_nanos(1)).unwrap()
}

fn fast() -> Cctp {
    Cctp { fast: true }
}

fn standard() -> Cctp {
    Cctp { fast: false }
}

/// The fast path is attested at threshold 1000 and pays at most two basis points, rounded
/// up so a tiny burn still offers Circle its minimum; the standard path is attested at
/// 2000 and pays nothing. The amount the destination is promised is the burn less that
/// ceiling.
#[test]
fn fee_and_threshold_follow_the_path() {
    assert_eq!(fast().finality_threshold(), 1_000);
    assert_eq!(standard().finality_threshold(), 2_000);
    let usdc = |units: u128| TokenAmount::from(units);
    assert_eq!(
        fast().max_fee(usdc(25_000_000)),
        Ok(usdc(5_000)),
        "two bps of 25 USDC"
    );
    assert_eq!(
        fast().max_fee(usdc(1)),
        Ok(usdc(1)),
        "rounded up, never zero"
    );
    assert_eq!(fast().max_fee(usdc(4_999)), Ok(usdc(1)));
    assert_eq!(fast().max_fee(usdc(5_001)), Ok(usdc(2)));
    assert_eq!(standard().max_fee(usdc(25_000_000)), Ok(usdc(0)));
    assert_eq!(fast().least_minted(usdc(25_000_000)), Ok(usdc(24_995_000)));
    assert_eq!(
        standard().least_minted(usdc(25_000_000)),
        Ok(usdc(25_000_000))
    );
    assert_eq!(
        fast().max_fee(TokenAmount::MAX),
        Err(RailError::FeeOverflow {
            amount: TokenAmount::MAX
        })
    );
}

/// The burn goes through the source vault's `execute`: one call to the token messenger,
/// approved for exactly the amount of the source USDC, whose `depositForBurn` names the
/// destination's domain, mints to the destination vault padded to a word, may be
/// delivered only by this canister, and asks the path's threshold and fee; and the vault
/// checks its USDC fell by no more than the amount.
#[test]
fn the_burn_is_a_vault_execute_of_deposit_for_burn_to_the_destination_vault() {
    let quote = quote();
    let swap = fixture_swap(None, None);
    let config = config();
    let leg = leg(&quote, &swap, &config, None, None);
    let burn = fast().burn(&leg).expect("everything is configured");
    assert_eq!(burn.purpose, TxPurpose::Burn(leg.quote_hash));
    assert_eq!(burn.chain_id, ChainId::BASE);
    assert_eq!(burn.to, VAULT_BASE.parse::<EvmAddress>().unwrap());
    assert_eq!(burn.value, Wei::ZERO);
    assert_eq!(burn.gas_limit, BURN_GAS_LIMIT);
    let (swap_ref, calls, deltas) = decode_vault_execute(&burn.data).expect("an execute");
    assert_eq!(swap_ref, leg.quote_hash);
    assert_eq!(calls.len(), 1);
    let usdc_base: EvmAddress = USDC_BASE.parse().unwrap();
    assert_eq!(calls[0].target, config.token_messenger.unwrap());
    assert_eq!(calls[0].value, Wei::ZERO);
    assert_eq!(calls[0].approve_token, usdc_base);
    assert_eq!(calls[0].approve_amount, swap.amount_in);
    let inner = decode_cctp_deposit_for_burn(&calls[0].data).expect("a depositForBurn");
    assert_eq!(inner.amount, swap.amount_in);
    assert_eq!(
        inner.destination_domain, 3,
        "Arbitrum's CCTP domain, never its chain id"
    );
    assert_eq!(
        inner.mint_recipient,
        VAULT_ARBITRUM.parse::<EvmAddress>().unwrap().to_word(),
        "the destination vault, padded to a word"
    );
    assert_eq!(inner.burn_token, usdc_base);
    assert_eq!(
        inner.destination_caller,
        MINE.parse::<EvmAddress>().unwrap().to_word(),
        "only this canister delivers the mint"
    );
    assert_eq!(inner.max_fee, TokenAmount::from(5_000_u32));
    assert_eq!(inner.min_finality_threshold, 1_000);
    assert_eq!(
        deltas,
        vec![VaultDelta {
            token: usdc_base,
            min_change: -25_000_000,
        }]
    );

    let standard = standard().burn(&leg).unwrap();
    let (_, calls, _) = decode_vault_execute(&standard.data).unwrap();
    let inner = decode_cctp_deposit_for_burn(&calls[0].data).unwrap();
    assert_eq!(
        (inner.max_fee, inner.min_finality_threshold),
        (TokenAmount::ZERO, 2_000)
    );
}

/// A knob the deploy left unset refuses the burn by name, before anything is signed.
#[test]
fn a_missing_knob_refuses_the_burn_by_name() {
    let quote = quote();
    let swap = fixture_swap(None, None);
    let no_domain = Config {
        cctp_domains: ChainTable::default(),
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_domain, None, None))
            .map(drop),
        Err(RailError::NoDomain {
            chain_id: ChainId::ARBITRUM
        })
    );
    let no_usdc = Config {
        usdc_addresses: ChainTable::default(),
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_usdc, None, None))
            .map(drop),
        Err(RailError::NoUsdc {
            chain_id: ChainId::BASE
        })
    );
    let no_messenger = Config {
        token_messenger: None,
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_messenger, None, None))
            .map(drop),
        Err(RailError::NoTokenMessenger)
    );
    let no_transmitter = Config {
        message_transmitter: None,
        ..config()
    };
    let attestation = attestation_of(&attested_message());
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert_eq!(
        fast()
            .step(&leg(
                &quote,
                &burned,
                &no_transmitter,
                Some(&attestation),
                None
            ))
            .map(drop),
        Err(RailError::NoMessageTransmitter)
    );
}

/// The rail's whole table: the burn first, then a wait for the attestation, then the mint
/// carrying exactly what the watcher handed in, then the arrival with the least the
/// destination vault holds; and a burn is never reclaimed.
#[test]
fn the_steps_run_burn_attestation_mint_arrival_and_a_burn_is_final() {
    let quote = quote();
    let config = config();
    let fresh = fixture_swap(None, None);
    assert!(matches!(
        fast().step(&leg(&quote, &fresh, &config, None, None)),
        Ok(RailStep::Send(RailTx {
            purpose: TxPurpose::Burn(_),
            ..
        }))
    ));

    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert_eq!(
        fast().step(&leg(&quote, &burned, &config, None, None)),
        Ok(RailStep::Wait(WaitingFor::Attestation))
    );
    let message = attested_message();
    let attestation = attestation_of(&message);
    let mint = match fast().step(&leg(&quote, &burned, &config, Some(&attestation), None)) {
        Ok(RailStep::Send(tx)) => tx,
        other => panic!("the mint is sent once the attestation is in: {other:?}"),
    };
    assert_eq!(
        mint.purpose,
        TxPurpose::Mint(leg(&quote, &burned, &config, None, None).quote_hash)
    );
    assert_eq!(mint.chain_id, ChainId::ARBITRUM);
    assert_eq!(mint.to, config.message_transmitter.unwrap());
    assert_eq!(mint.gas_limit, MINT_GAS_LIMIT);
    assert_eq!(
        decode_cctp_receive_message(&mint.data),
        Some((message.encode(), vec![0xbb; 65]))
    );

    // the mint confirmed: what it delivered is read off its own receipt, never computed
    let mut minted = fixture_swap(Some(SwapLeg::Mint), Some(Outcome::Confirmed));
    minted.last_tx_hash = Some(TxHash::new([0x42; 32]));
    assert_eq!(
        fast().step(&leg(&quote, &minted, &config, None, None)),
        Ok(RailStep::ReadMint {
            chain_id: ChainId::ARBITRUM,
            tx_hash: TxHash::new([0x42; 32]),
        })
    );
    assert_eq!(
        standard().step(&leg(&quote, &minted, &config, None, None)),
        Ok(RailStep::ReadMint {
            chain_id: ChainId::ARBITRUM,
            tx_hash: TxHash::new([0x42; 32]),
        })
    );
    let mut no_hash = fixture_swap(Some(SwapLeg::Mint), Some(Outcome::Confirmed));
    no_hash.last_tx_hash = None;
    assert_eq!(
        fast().step(&leg(&quote, &no_hash, &config, None, None)),
        Err(RailError::NoMintHash),
        "a confirmed mint with no hash recorded is a fold no line produces"
    );

    assert!(matches!(
        fast().reclaim(&leg(&quote, &burned, &config, None, None)),
        Ok(RailStep::Stuck(_))
    ));
    let paid_out = fixture_swap(Some(SwapLeg::Payout), Some(Outcome::Confirmed));
    assert!(matches!(
        fast().step(&leg(&quote, &paid_out, &config, None, None)),
        Ok(RailStep::Stuck(_))
    ));

    // a quote paying out a token that is no address is refused before the burn
    let odd_token = types::Quote {
        dst_token: "USDC".parse().unwrap(),
        ..quote.clone()
    };
    assert_eq!(
        fast().step(&leg(&odd_token, &fresh, &config, None, None)),
        Err(RailError::RailToken(RailTokenError::QuoteAddress(
            QuoteAddressError::NotAnAddress {
                field: QuoteAddressField::DstToken,
                reason: EvmAddressError::NoPrefix,
            }
        )))
    );
    let _ = USDC_ARBITRUM;
}

/// The rail carries the configured USDC and nothing else: a swap naming any other token on
/// either side is refused before its first leg, by the field, so the vault's USDC is never
/// burned against a deposit of something else and no other token is ever paid out.
#[test]
fn a_swap_naming_another_token_is_refused_before_the_burn() {
    let config = config();
    let swap = fixture_swap(None, None);
    let worthless: EvmAddress = MINE.parse().unwrap();
    let wrong_source = types::Quote {
        src_token: MINE.parse().unwrap(),
        ..quote()
    };
    assert_eq!(
        fast()
            .step(&leg(&wrong_source, &swap, &config, None, None))
            .map(drop),
        Err(RailError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::SrcToken,
            quoted: worthless,
            rail_token: USDC_BASE.parse().unwrap(),
            rail: types::Rail::CctpV2Fast,
        }))
    );
    let wrong_destination = types::Quote {
        dst_token: MINE.parse().unwrap(),
        ..quote()
    };
    assert_eq!(
        standard()
            .step(&leg(&wrong_destination, &swap, &config, None, None))
            .map(drop),
        Err(RailError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::DstToken,
            quoted: worthless,
            rail_token: USDC_ARBITRUM.parse().unwrap(),
            // the rail the refusal names is the quote's, which is what the pin is for
            rail: types::Rail::CctpV2Fast,
        }))
    );
    // and a swap already burned is held to the same pin at every later step
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert!(matches!(
        fast().step(&leg(&wrong_source, &burned, &config, None, None)),
        Err(RailError::RailToken(_))
    ));
}

/// The message the watcher hands in is bound to the swap before it is minted: every field
/// the burn determined must be the swap's own, so a message of another burn (another
/// amount, another lane, another recipient, another caller) is refused by the field, and
/// only the fields the attestation service fills in (the nonce, the fee executed within
/// the ceiling, the expiration) are free. A message that is not a burn message at all is
/// refused by the way it is not.
#[test]
fn a_message_is_bound_to_its_swap_field_by_field() {
    let quote = quote();
    let config = config();
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let view = leg(&quote, &burned, &config, None, None);
    let good = attested_message();
    assert_eq!(fast().ensure_message_binds(&view, &good), Ok(()));

    let messenger = config.token_messenger.unwrap().to_word();
    let mine = word(MINE);
    let cases: Vec<(&str, BurnMessage, MessageMismatch)> = vec![
        (
            "source domain",
            BurnMessage {
                source_domain: 7,
                ..good.clone()
            },
            MessageMismatch::Domain {
                field: MessageField::SourceDomain,
                expected: 6,
                found: 7,
            },
        ),
        (
            "destination domain",
            BurnMessage {
                destination_domain: 6,
                ..good.clone()
            },
            MessageMismatch::Domain {
                field: MessageField::DestinationDomain,
                expected: 3,
                found: 6,
            },
        ),
        (
            "sender",
            BurnMessage {
                sender: mine,
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::Sender,
                expected: messenger,
                found: mine,
            },
        ),
        (
            "recipient",
            BurnMessage {
                recipient: mine,
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::Recipient,
                expected: messenger,
                found: mine,
            },
        ),
        (
            "destination caller",
            BurnMessage {
                destination_caller: [0; 32],
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::DestinationCaller,
                expected: mine,
                found: [0; 32],
            },
        ),
        (
            "finality threshold",
            BurnMessage {
                min_finality_threshold: STANDARD_FINALITY_THRESHOLD,
                ..good.clone()
            },
            MessageMismatch::Threshold {
                expected: FAST_FINALITY_THRESHOLD,
                found: STANDARD_FINALITY_THRESHOLD,
            },
        ),
        (
            "burn token",
            BurnMessage {
                body: BurnBody {
                    burn_token: mine,
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::BurnToken,
                expected: word(USDC_BASE),
                found: mine,
            },
        ),
        (
            "mint recipient",
            BurnMessage {
                body: BurnBody {
                    mint_recipient: mine,
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::MintRecipient,
                expected: word(VAULT_ARBITRUM),
                found: mine,
            },
        ),
        (
            "amount",
            BurnMessage {
                body: BurnBody {
                    amount: TokenAmount::from(10_000_000_u32),
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Amount {
                field: MessageField::Amount,
                expected: TokenAmount::from(25_000_000_u32),
                found: TokenAmount::from(10_000_000_u32),
            },
        ),
        (
            "message sender",
            BurnMessage {
                body: BurnBody {
                    message_sender: mine,
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Word {
                field: MessageField::MessageSender,
                expected: word(VAULT_BASE),
                found: mine,
            },
        ),
        (
            "max fee",
            BurnMessage {
                body: BurnBody {
                    max_fee: TokenAmount::from(6_000_u32),
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Amount {
                field: MessageField::MaxFee,
                expected: TokenAmount::from(5_000_u32),
                found: TokenAmount::from(6_000_u32),
            },
        ),
        (
            "fee above the ceiling",
            BurnMessage {
                body: BurnBody {
                    fee_executed: TokenAmount::from(5_001_u32),
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::FeeAboveMaxFee {
                fee: TokenAmount::from(5_001_u32),
                max_fee: TokenAmount::from(5_000_u32),
            },
        ),
        (
            "hook data",
            BurnMessage {
                body: BurnBody {
                    hook_data: vec![1, 2, 3],
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::HookData { len: 3 },
        ),
    ];
    for (case, message, mismatch) in cases {
        assert_eq!(
            fast().ensure_message_binds(&view, &message),
            Err(RailError::Message(mismatch)),
            "{case}"
        );
    }
    // the fields the attestation service fills in are free
    let filled = BurnMessage {
        nonce: [0x11; 32],
        finality_threshold_executed: 2_000,
        body: BurnBody {
            fee_executed: TokenAmount::from(5_000_u32),
            expiration_block: BlockNumber::new(1),
            ..good.body.clone()
        },
        ..good.clone()
    };
    assert_eq!(fast().ensure_message_binds(&view, &filled), Ok(()));
    // the standard rail asks the standard threshold and no fee
    let standard_message = BurnMessage {
        min_finality_threshold: STANDARD_FINALITY_THRESHOLD,
        body: BurnBody {
            max_fee: TokenAmount::ZERO,
            fee_executed: TokenAmount::ZERO,
            ..good.body.clone()
        },
        ..good.clone()
    };
    assert_eq!(
        standard().ensure_message_binds(&view, &standard_message),
        Ok(())
    );

    // the mint is refused on a message that does not bind, and on bytes that are no message
    let other_amount = BurnMessage {
        body: BurnBody {
            amount: TokenAmount::from(10_000_000_u32),
            ..good.body.clone()
        },
        ..good.clone()
    };
    assert!(matches!(
        fast().step(&leg(
            &quote,
            &burned,
            &config,
            Some(&attestation_of(&other_amount)),
            None
        )),
        Err(RailError::Message(MessageMismatch::Amount { .. }))
    ));
    let garbage =
        Attestation::new(vec![0xaa; 376], vec![0xbb; 65], Timestamp::from_nanos(1)).unwrap();
    assert!(matches!(
        fast().step(&leg(&quote, &burned, &config, Some(&garbage), None)),
        Err(RailError::UnreadableMessage(_))
    ));
}
