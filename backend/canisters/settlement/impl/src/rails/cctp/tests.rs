use super::*;
use crate::rails::tests::{
    at, config, fixture_swap, quote, MINE, USDC_ARBITRUM, USDC_BASE, VAULT_ARBITRUM, VAULT_BASE,
};
use crate::rails::{RailStep, ReclaimStep, WaitingFor};
use types::abi::{
    decode_cctp_deposit_for_burn_with_hook, decode_cctp_receive_message, decode_vault_execute,
    CctpMint, VaultDelta, VaultExecution,
};
use types::cctp::{BurnBody, BurnMessage, BURN_BODY_VERSION, MESSAGE_VERSION};
use types::config::ChainTable;
use types::evm::EvmAddressError;
use types::quote::{QuoteAddressError, QuoteAddressField};
use types::rail::RailTokenError;
use types::{
    Attestation, BlockNumber, ChainId, Config, EvmAddress, Outcome, Swap, Timestamp, TxHash,
};

fn word(address: &str) -> [u8; 32] {
    address.parse::<EvmAddress>().unwrap().to_word()
}

/// The message Circle attests for the fixture swap's fast burn: every field the burn
/// determined as the burn emitted it, the swap's quote hash in the hook included, and the
/// nonce, the fee and the expiration as the attestation service filled them in.
///
/// Rewritten for fix wave 4 (N1): the burn now writes its swap's quote hash as hook data,
/// where it wrote none before.
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
            hook_data: quote().hash().unwrap().into_bytes().to_vec(),
        },
    }
}

/// The message Circle attests for the burn this rail itself builds at `at`: every field
/// the burn determined, read back out of the burn's own calldata (so the message is the
/// one this burn emits, whatever the burn writes), and the nonce, the fee and the
/// expiration as the attestation service fills them in, the nonce numbered `nonce`.
fn attested_of_burn(rail: Cctp, at: &Position, nonce: u8) -> BurnMessage {
    let tx = rail.burn(at).expect("the burn builds");
    let calls = decode_vault_execute(&tx.data).expect("an execute").calls;
    let burn = decode_cctp_deposit_for_burn_with_hook(&calls[0].data).expect("a burn");
    let messenger = calls[0].target.to_word();
    BurnMessage {
        version: MESSAGE_VERSION,
        source_domain: at
            .config
            .cctp_domains
            .get(at.quote.src_chain)
            .unwrap()
            .get(),
        destination_domain: burn.destination_domain,
        nonce: [nonce; 32],
        sender: messenger,
        recipient: messenger,
        destination_caller: burn.destination_caller,
        min_finality_threshold: burn.min_finality_threshold,
        finality_threshold_executed: burn.min_finality_threshold,
        body: BurnBody {
            version: BURN_BODY_VERSION,
            burn_token: burn.burn_token.to_word(),
            mint_recipient: burn.mint_recipient,
            amount: burn.amount,
            message_sender: tx.to.to_word(),
            max_fee: burn.max_fee,
            fee_executed: TokenAmount::from(2_500_u32),
            expiration_block: BlockNumber::new(19_000_100),
            hook_data: burn.hook_data,
        },
    }
}

fn attestation_of(message: &BurnMessage) -> Attestation {
    Attestation::new(message.encode(), vec![0xbb; 65], Timestamp::from_nanos(1)).unwrap()
}

fn fast() -> Cctp {
    Cctp::Fast
}

fn standard() -> Cctp {
    Cctp::Standard
}

/// The fast path is attested at threshold 1000 and pays at most two basis points, rounded
/// up so a tiny burn still offers Circle a unit; the standard path is attested at 2000 and
/// pays nothing of its own. With no minimum listed for the chain, that ceiling is what a
/// burn offers.
///
/// Rewritten for fix wave 5 (N12): `least_minted` is gone (nothing called it since the
/// mint's own receipt became what `PaidInStable` records), and the offer a burn makes is
/// asserted in its place.
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
    let none = config();
    assert_eq!(
        fast().offered_fee(&none, ChainId::BASE, usdc(25_000_000)),
        Ok(usdc(5_000))
    );
    assert_eq!(
        standard().offered_fee(&none, ChainId::BASE, usdc(25_000_000)),
        Ok(usdc(0))
    );
    assert_eq!(
        fast().max_fee(TokenAmount::MAX),
        Err(RailError::FeeOverflow {
            amount: TokenAmount::MAX
        })
    );
}

/// The burn goes through the source vault's `execute`: one call to the token messenger,
/// approved for exactly the amount of the source USDC, whose `depositForBurnWithHook`
/// names the destination's domain, mints to the destination vault padded to a word, may be
/// delivered only by this canister, asks the path's threshold and fee, and carries the
/// swap's quote hash as its hook; and the vault checks its USDC fell by no more than the
/// amount. The hook was added in fix wave 4 (N1), when the burn became the hooked call.
#[test]
fn the_burn_is_a_vault_execute_of_deposit_for_burn_to_the_destination_vault() {
    let quote = quote();
    let swap = fixture_swap(None, None);
    let config = config();
    let at = at(&quote, &swap, &config, None, None);
    let burn = fast().burn(&at).expect("everything is configured");
    assert_eq!(burn.purpose, TxPurpose::Burn(at.quote_hash));
    assert_eq!(burn.chain_id, ChainId::BASE);
    assert_eq!(burn.to, VAULT_BASE.parse::<EvmAddress>().unwrap());
    assert_eq!(burn.value, Wei::ZERO);
    assert_eq!(burn.gas_limit, BURN_GAS_LIMIT);
    let VaultExecution {
        swap_ref,
        calls,
        deltas,
    } = decode_vault_execute(&burn.data).expect("an execute");
    assert_eq!(swap_ref, at.quote_hash);
    assert_eq!(calls.len(), 1);
    let usdc_base: EvmAddress = USDC_BASE.parse().unwrap();
    assert_eq!(calls[0].target, config.token_messenger.unwrap());
    assert_eq!(calls[0].value, Wei::ZERO);
    assert_eq!(calls[0].approve_token, usdc_base);
    assert_eq!(calls[0].approve_amount, swap.amount_in);
    let inner = decode_cctp_deposit_for_burn_with_hook(&calls[0].data).expect("a hooked burn");
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
        inner.hook_data,
        at.quote_hash.into_bytes().to_vec(),
        "the swap's quote hash, which its message carries"
    );
    assert_eq!(
        deltas,
        vec![VaultDelta {
            token: usdc_base,
            min_change: -25_000_000,
        }]
    );

    let standard = standard().burn(&at).unwrap();
    let calls = decode_vault_execute(&standard.data).unwrap().calls;
    let inner = decode_cctp_deposit_for_burn_with_hook(&calls[0].data).unwrap();
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
            .burn(&at(&quote, &swap, &no_domain, None, None))
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
            .burn(&at(&quote, &swap, &no_usdc, None, None))
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
            .burn(&at(&quote, &swap, &no_messenger, None, None))
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
            .step(&at(
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
/// carrying exactly what the watcher handed in, then the read of what that mint delivered.
#[test]
fn the_steps_run_burn_attestation_mint_and_the_read_of_what_it_delivered() {
    let quote = quote();
    let config = config();
    let fresh = fixture_swap(None, None);
    assert!(matches!(
        fast().step(&at(&quote, &fresh, &config, None, None)),
        Ok(RailStep::Send(RailTx {
            purpose: TxPurpose::Burn(_),
            ..
        }))
    ));

    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert_eq!(
        fast().step(&at(&quote, &burned, &config, None, None)),
        Ok(RailStep::Wait(WaitingFor::Attestation))
    );
    let message = attested_message();
    let attestation = attestation_of(&message);
    let mint = match fast().step(&at(&quote, &burned, &config, Some(&attestation), None)) {
        Ok(RailStep::Send(tx)) => tx,
        other => panic!("the mint is sent once the attestation is in: {other:?}"),
    };
    assert_eq!(
        mint.purpose,
        TxPurpose::Mint(at(&quote, &burned, &config, None, None).quote_hash)
    );
    assert_eq!(mint.chain_id, ChainId::ARBITRUM);
    assert_eq!(mint.to, config.message_transmitter.unwrap());
    assert_eq!(mint.gas_limit, MINT_GAS_LIMIT);
    assert_eq!(
        decode_cctp_receive_message(&mint.data),
        Some(CctpMint {
            message: message.encode(),
            attestation: vec![0xbb; 65],
        })
    );

    // the mint confirmed: what it delivered is read off its own receipt, never computed
    let mut minted = fixture_swap(Some(SwapLeg::Mint), Some(Outcome::Confirmed));
    minted.last_tx_hash = Some(TxHash::new([0x42; 32]));
    assert_eq!(
        fast().step(&at(&quote, &minted, &config, None, None)),
        Ok(RailStep::ReadMint {
            chain_id: ChainId::ARBITRUM,
            tx_hash: TxHash::new([0x42; 32]),
        })
    );
    assert_eq!(
        standard().step(&at(&quote, &minted, &config, None, None)),
        Ok(RailStep::ReadMint {
            chain_id: ChainId::ARBITRUM,
            tx_hash: TxHash::new([0x42; 32]),
        })
    );
    let mut no_hash = fixture_swap(Some(SwapLeg::Mint), Some(Outcome::Confirmed));
    no_hash.last_tx_hash = None;
    assert_eq!(
        fast().step(&at(&quote, &no_hash, &config, None, None)),
        Err(RailError::NoMintHash),
        "a confirmed mint with no hash recorded is a fold no line produces"
    );

    let _ = USDC_ARBITRUM;
}

/// A burn is final on the source side, and a swap already paid out has no leg left: both
/// stop the rail rather than sending anything.
#[test]
fn a_burn_is_never_reclaimed_and_a_paid_out_swap_has_no_step() {
    let quote = quote();
    let config = config();
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert!(matches!(
        fast().reclaim(&at(&quote, &burned, &config, None, None)),
        Ok(ReclaimStep::Stuck(_))
    ));
    let paid_out = fixture_swap(Some(SwapLeg::Payout), Some(Outcome::Confirmed));
    assert!(matches!(
        fast().step(&at(&quote, &paid_out, &config, None, None)),
        Ok(RailStep::Stuck(_))
    ));
}

/// A quote whose destination token is no address at all is refused before the burn, by the
/// field: the rail pins both tokens, and text that is no address is not the rail's token
/// however it is spelled.
#[test]
fn a_quote_whose_token_is_no_address_is_refused_by_the_field() {
    let config = config();
    let fresh = fixture_swap(None, None);
    let odd_token = types::Quote {
        dst_token: "USDC".parse().unwrap(),
        ..quote()
    };
    assert_eq!(
        fast().step(&at(&odd_token, &fresh, &config, None, None)),
        Err(RailError::RailToken(RailTokenError::QuoteAddress(
            QuoteAddressError::NotAnAddress {
                field: QuoteAddressField::DstToken,
                reason: EvmAddressError::NoPrefix,
            }
        )))
    );
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
            .step(&at(&wrong_source, &swap, &config, None, None))
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
            .step(&at(&wrong_destination, &swap, &config, None, None))
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
        fast().step(&at(&wrong_source, &burned, &config, None, None)),
        Err(RailError::RailToken(_))
    ));
}

/// The message the watcher hands in is bound to the swap before it is minted: every field
/// the burn determined must be the swap's own, so a message of another burn (another
/// amount, another lane, another recipient, another caller, another swap's hook) is
/// refused by the field, and only the fields the attestation service fills in (the nonce,
/// the fee executed within the ceiling, the expiration) are free. A message that is not a
/// burn message at all is refused by the way it is not.
///
/// Rewritten for fix wave 4 (N1): the hook is no longer required empty but required to be
/// the swap's quote hash, so a hook of any other length (none at all included) is refused
/// by its length and a hook naming another swap by the swap it names.
///
/// Rewritten for fix wave 5 (N12): the fee a message must carry is the one the fold
/// recorded off the swap's own burn, so the standard case binds under a swap whose burn
/// offered no fee.
#[test]
fn a_message_is_bound_to_its_swap_field_by_field() {
    let quote = quote();
    let config = config();
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let view = at(&quote, &burned, &config, None, None);
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
        (
            "no hook data",
            BurnMessage {
                body: BurnBody {
                    hook_data: vec![],
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::HookData { len: 0 },
        ),
        (
            "another swap's hook",
            BurnMessage {
                body: BurnBody {
                    hook_data: vec![0x77; 32],
                    ..good.body.clone()
                },
                ..good.clone()
            },
            MessageMismatch::Swap {
                expected: view.quote_hash,
                found: types::QuoteHash::new([0x77; 32]),
            },
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
    // the standard rail asks the standard threshold, and a burn from a chain with no
    // minimum offered no fee (fix wave 5, N12: the offer is the one the fold recorded)
    let standard_burned = types::Swap {
        burn_max_fee: Some(TokenAmount::ZERO),
        ..burned.clone()
    };
    let standard_view = at(&quote, &standard_burned, &config, None, None);
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
        standard().ensure_message_binds(&standard_view, &standard_message),
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
        fast().step(&at(
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
        fast().step(&at(&quote, &burned, &config, Some(&garbage), None)),
        Err(RailError::UnreadableMessage(_))
    ));
}

/// Two swaps whose burns are identical in every parameter (one lane, one amount, one
/// destination vault, one caller, one threshold, one fee ceiling) still emit two messages:
/// each burn writes its own swap's quote hash as its hook data, and the attested message
/// of one is refused under the other by that hook, at the door's check and again at the
/// mint. So a message handed to the wrong swap is never minted there, and the swap whose
/// burn emitted it keeps an unspent message to mint. Each swap's own message binds.
#[test]
fn a_message_of_an_identical_burn_is_refused_under_the_other_swap() {
    let config = config();
    let first = quote();
    let second = types::Quote {
        nonce: 2,
        ..quote()
    };
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let first_at = at(&first, &burned, &config, None, None);
    let second_at = at(&second, &burned, &config, None, None);
    assert_ne!(first_at.quote_hash, second_at.quote_hash);
    let first_message = attested_of_burn(fast(), &first_at, 0x9a);
    let second_message = attested_of_burn(fast(), &second_at, 0x9b);

    assert_eq!(
        fast().ensure_message_binds(&first_at, &first_message),
        Ok(())
    );
    assert_eq!(
        fast().ensure_message_binds(&second_at, &second_message),
        Ok(())
    );
    assert_eq!(
        fast().ensure_message_binds(&second_at, &first_message),
        Err(RailError::Message(MessageMismatch::Swap {
            expected: second_at.quote_hash,
            found: first_at.quote_hash,
        })),
        "the first swap's message under the second swap"
    );
    assert_eq!(
        fast()
            .step(&at(
                &second,
                &burned,
                &config,
                Some(&attestation_of(&first_message)),
                None
            ))
            .map(drop),
        Err(RailError::Message(MessageMismatch::Swap {
            expected: second_at.quote_hash,
            found: first_at.quote_hash,
        })),
        "and the mint refuses it as the door does"
    );
}

/// The most the burn at `at` offers Circle, read back out of its own calldata.
fn offered_by_burn(rail: Cctp, at: &Position) -> TokenAmount {
    let tx = rail.burn(at).expect("the burn builds");
    let calls = decode_vault_execute(&tx.data).expect("an execute").calls;
    decode_cctp_deposit_for_burn_with_hook(&calls[0].data)
        .expect("a burn")
        .max_fee
}

/// The fixture config with the source chain's messenger charging `min_fee` of Circle's
/// thousandths of a basis point.
fn with_min_fee(min_fee: u32) -> Config {
    Config {
        cctp_min_fees: ChainTable(std::collections::BTreeMap::from([(
            ChainId::BASE,
            types::CctpMinFee::new(min_fee),
        )])),
        ..config()
    }
}

/// Circle's `TokenMessengerV2` reverts a burn whose `maxFee` is below the minimum fee it
/// holds that amount to, the Standard path included (N12). So a Standard burn from a chain
/// that charges a minimum offers exactly it, where it offered nothing, and a chain the
/// config lists no minimum for is one that charges none.
#[test]
fn a_standard_burn_offers_the_chains_minimum_fee() {
    let quote = quote();
    let fresh = fixture_swap(None, None);
    let one_bps = with_min_fee(1_000);
    assert_eq!(
        offered_by_burn(standard(), &at(&quote, &fresh, &one_bps, None, None)),
        TokenAmount::from(2_500_u32),
        "one basis point of 25 USDC"
    );
    let none = config();
    assert_eq!(
        offered_by_burn(standard(), &at(&quote, &fresh, &none, None, None)),
        TokenAmount::ZERO,
        "no minimum listed, none charged"
    );
}

/// A Fast burn offers its two basis point ceiling, or the chain's minimum where that is
/// more: a minimum of five basis points is what the burn offers, and one of one basis
/// point leaves the ceiling, which is already above it.
#[test]
fn a_fast_burn_offers_the_minimum_where_its_ceiling_is_below_it() {
    let quote = quote();
    let fresh = fixture_swap(None, None);
    let five_bps = with_min_fee(5_000);
    assert_eq!(
        offered_by_burn(fast(), &at(&quote, &fresh, &five_bps, None, None)),
        TokenAmount::from(12_500_u32),
        "the minimum, above the 5,000 ceiling"
    );
    let one_bps = with_min_fee(1_000);
    assert_eq!(
        offered_by_burn(fast(), &at(&quote, &fresh, &one_bps, None, None)),
        TokenAmount::from(5_000_u32),
        "the ceiling, above the 2,500 minimum"
    );
}

/// The message a burn emits carries the fee that burn offered, and the binding reads that
/// fee off the fold, where the burn's calldata recorded it (rule A3): a minimum the
/// operator lowered or raised after the burn, with Circle's, cannot unbind the message
/// the burn emitted and leave its USDC burned and never minted. A message carrying another
/// fee is refused by the field, and a swap whose burn the fold holds no fee for is a fold
/// no line produces, refused by name.
#[test]
fn a_message_binds_to_the_fee_its_own_burn_offered() {
    let quote = quote();
    // burned when the chain charged five basis points, and the table is empty now
    let burned = Swap {
        burn_max_fee: Some(TokenAmount::from(12_500_u32)),
        ..fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed))
    };
    let now = config();
    let view = at(&quote, &burned, &now, None, None);
    let offered = BurnMessage {
        body: BurnBody {
            max_fee: TokenAmount::from(12_500_u32),
            ..attested_message().body
        },
        ..attested_message()
    };
    assert_eq!(fast().ensure_message_binds(&view, &offered), Ok(()));
    assert_eq!(
        fast().ensure_message_binds(&view, &attested_message()),
        Err(RailError::Message(MessageMismatch::Amount {
            field: MessageField::MaxFee,
            expected: TokenAmount::from(12_500_u32),
            found: TokenAmount::from(5_000_u32),
        })),
        "the ceiling the config would give now is not the fee the burn offered"
    );
    let unrecorded = Swap {
        burn_max_fee: None,
        ..burned
    };
    assert_eq!(
        fast().ensure_message_binds(&at(&quote, &unrecorded, &now, None, None), &offered),
        Err(RailError::NoBurnFeeRecorded)
    );
}

/// The worst a burn can deliver is its amount less the fee it offers Circle, and the
/// payout exit refuses anything that, less the platform's fee, falls below the least the
/// user was quoted: it freezes the swap with the funds on the destination side (review 5,
/// M4). So a burn whose worst case is below `min_out` is never sent: the rail answers a
/// refund from the source vault, by name, before anything is signed.
///
/// A Standard swap the quoter priced for no fee, with a slack of 1,000 units, on a chain
/// whose messenger now charges a minimum of one basis point (2,500 units of 25 USDC), is
/// refunded at the source; with no minimum listed it burns. The platform's fee counts the
/// same way, and a fee that is the whole amount leaves nothing to pay out at all.
#[test]
fn a_burn_that_would_deliver_below_the_quotes_minimum_is_refunded_at_the_source() {
    use crate::rails::SourceRefund;
    use types::Quote;
    let priced_for_no_fee = Quote {
        rail: Rail::CctpV2Standard,
        min_out: TokenAmount::from(24_999_000_u32),
        ..quote()
    };
    let fresh = fixture_swap(None, None);
    let one_bps = with_min_fee(1_000);
    assert_eq!(
        standard().step(&at(&priced_for_no_fee, &fresh, &one_bps, None, None)),
        Ok(RailStep::Refund(SourceRefund::BelowMinOut {
            max_fee: TokenAmount::from(2_500_u32),
            worst_payout: TokenAmount::from(24_997_500_u32),
            min_out: TokenAmount::from(24_999_000_u32),
        }))
    );
    assert!(
        matches!(
            standard().step(&at(&priced_for_no_fee, &fresh, &config(), None, None)),
            Ok(RailStep::Send(RailTx {
                purpose: TxPurpose::Burn(_),
                ..
            }))
        ),
        "no minimum listed, nothing charged, and the burn goes"
    );
    // exactly the minimum out at worst is enough
    let exact = Quote {
        min_out: TokenAmount::from(24_997_500_u32),
        ..priced_for_no_fee.clone()
    };
    assert!(matches!(
        standard().step(&at(&exact, &fresh, &one_bps, None, None)),
        Ok(RailStep::Send(_))
    ));
    // the platform's fee on what arrives counts: ten basis points of 25 USDC
    let with_platform_fee = Config {
        platform_fee: types::BasisPoints::new(10),
        ..config()
    };
    assert_eq!(
        standard().step(&at(
            &priced_for_no_fee,
            &fresh,
            &with_platform_fee,
            None,
            None
        )),
        Ok(RailStep::Refund(SourceRefund::BelowMinOut {
            max_fee: TokenAmount::ZERO,
            worst_payout: TokenAmount::from(24_975_000_u32),
            min_out: TokenAmount::from(24_999_000_u32),
        }))
    );
    // the fast path's ceiling on one unit is the unit itself
    let one_unit = Quote {
        amount_in: TokenAmount::ONE,
        min_out: TokenAmount::ZERO,
        ..quote()
    };
    let one_unit_swap = Swap {
        amount_in: TokenAmount::ONE,
        ..fresh.clone()
    };
    assert_eq!(
        fast().step(&at(&one_unit, &one_unit_swap, &config(), None, None)),
        Ok(RailStep::Refund(SourceRefund::FeeTakesAll {
            amount: TokenAmount::ONE,
            max_fee: TokenAmount::ONE,
        }))
    );
    // the reason the refund line carries names the numbers
    assert_eq!(
        SourceRefund::BelowMinOut {
            max_fee: TokenAmount::from(2_500_u32),
            worst_payout: TokenAmount::from(24_997_500_u32),
            min_out: TokenAmount::from(24_999_000_u32),
        }
        .to_string(),
        "a burn charged the 2500 it offers would pay the user out 24997500, below the \
         24999000 they were quoted"
    );
}
