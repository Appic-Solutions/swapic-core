use super::*;

fn ckusdc() -> Principal {
    Principal::from_text(CKUSDC_LEDGER).unwrap()
}

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

/// The interface's three vectors (section 2), with the owner and details hashes between: V1
/// the P11d swap vector (its ICP fees committed), V2 and V3 unchanged.
#[test]
fn the_ref_matches_the_interface_vectors() {
    let anon = Principal::anonymous();
    let v1 = DepositKind::Swap {
        recipient: IcpAccount {
            owner: anon,
            subaccount: None,
        },
        token_out: ckusdc(),
        min_out: 1_000_000,
    };
    assert_eq!(
        owner_hash(&v1.owner()).to_string(),
        "b546ee59c4dfe2b5e18af740a73302a8695435a79d422464a3fba288447f2cb7"
    );
    assert_eq!(
        hex::encode(details_hash(&v1, 10_000, 10_000)),
        "54d75ee3c718aa72b8136e37977f44e240b0b155260bcbc0fba1879bd50a786d"
    );
    assert_eq!(
        pool_ref(&v1, 10_000, 10_000, [0x11; 32]).to_string(),
        "2ca558b613087d4829efcfc433da96c2803012b2840e9775d83f94e880a03a35"
    );

    let payer: EvmAddress = "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap();
    let v2 = DepositKind::Stake {
        owner: PoolAccount::Evm(payer),
    };
    assert_eq!(
        owner_hash(&v2.owner()).to_string(),
        "ef01b19f23d59a2f61bf121d5ff293a5fd0cc722574b74df274851146afc2b56"
    );
    assert_eq!(
        hex::encode(details_hash(&v2, 500, 0)),
        "ccd4ae6ef6fa5c628d5b3e95d10589f3cbb4e21e9f997aeda1c7403b267bac1a"
    );
    assert_eq!(
        pool_ref(&v2, 500, 0, [0x22; 32]).to_string(),
        "27af0178f431306b588718a3e754c7fd1024ab2c720620145bec989680400b56"
    );

    let mut sub = [0u8; 32];
    sub[31] = 1;
    let v3 = DepositKind::Stake {
        owner: PoolAccount::Icp(IcpAccount {
            owner: anon,
            subaccount: Some(sub),
        }),
    };
    assert_eq!(
        owner_hash(&v3.owner()).to_string(),
        "f60423d4f356b3ebd5c1562ddb311d0df4354b84244921964c4feb2116e24871"
    );
    assert_eq!(
        hex::encode(details_hash(&v3, 0, 0)),
        "290decd9548b62a8d60345a988386fc84ba6bc95484008f6362f93160ef3e563"
    );
    assert_eq!(
        pool_ref(&v3, 0, 0, [0x33; 32]).to_string(),
        "66c5a89c63e57dbf6d1180f4af6090d5e7d98508d054e62b8f63d8a79c032b9d"
    );

    // a fee other than the committed one gives another ref
    assert_ne!(
        pool_ref(&v1, 10_001, 10_000, [0x11; 32]),
        pool_ref(&v1, 10_000, 10_000, [0x11; 32])
    );
    // and so do other ICP fees
    assert_ne!(
        pool_ref(&v1, 10_000, 10_001, [0x11; 32]),
        pool_ref(&v1, 10_000, 10_000, [0x11; 32])
    );
    assert_eq!(
        hex::encode(tag_swap()),
        "da6b3c5c7b1921500b1ea4ba6bac1a554f8ea97edee80ee1307e297ff5723ecf"
    );
    assert_eq!(
        hex::encode(tag_stake()),
        "c9373eb3cec9d16a3aef91826c6d8b79442aff43c17a3f5f831f91c250d6621e"
    );
}

/// The zero subaccount and no subaccount are one ICRC-1 account, so one key.
#[test]
fn the_default_subaccount_is_one_account() {
    let owner = Principal::anonymous();
    let none = PoolAccount::Icp(IcpAccount {
        owner,
        subaccount: None,
    });
    let zero = PoolAccount::Icp(IcpAccount {
        owner,
        subaccount: Some([0; 32]),
    });
    assert_eq!(none.key(), zero.key());
}

/// A pool where the stakers' assets and the shares outstanding are kept by hand, minting and
/// burning only through the share math.
struct Book {
    assets: TokenAmount,
    shares: Shares,
}

impl Book {
    fn stake(&mut self, amount: TokenAmount) -> Shares {
        let minted = Shares::mint_for(amount, self.assets, self.shares).unwrap();
        self.assets = self.assets.checked_add(amount).unwrap();
        self.shares = self.shares.checked_add(minted).unwrap();
        minted
    }

    /// Withdraws everything `owned` is worth, answering what was paid.
    fn withdraw_all(&mut self, owned: Shares) -> TokenAmount {
        let value = Shares::value_of(owned, self.assets, self.shares);
        if value == TokenAmount::ZERO {
            self.shares = self.shares.checked_sub(owned).unwrap();
            return value;
        }
        let burned = Shares::burn_for(value, self.assets, self.shares).unwrap();
        assert!(
            burned <= owned,
            "the burn of everything one owns fits in it"
        );
        self.assets = self.assets.checked_sub(value).unwrap();
        self.shares = self.shares.checked_sub(burned).unwrap();
        value
    }
}

/// Review Focus 3: three stakers, uneven amounts, a premium earned between them. No one
/// takes out more than their shares are worth, nobody is paid more than the pool holds,
/// and the last one out leaves at most one micro USDC per operation behind.
#[test]
fn shares_never_pay_out_more_than_they_took_in() {
    let mut book = Book {
        assets: TokenAmount::ZERO,
        shares: Shares::ZERO,
    };
    let a = book.stake(TokenAmount::from(100_000_001_u64));
    let b = book.stake(TokenAmount::from(33_333_333_u64));
    // a premium earned while a and b are in
    book.assets = book
        .assets
        .checked_add(TokenAmount::from(77_777_u64))
        .unwrap();
    let c = book.stake(TokenAmount::from(7_000_007_u64));
    let paid_c = book.withdraw_all(c);
    assert!(
        paid_c <= TokenAmount::from(7_000_007_u64),
        "c came in after the premium and takes none of it: {paid_c}"
    );
    let paid_a = book.withdraw_all(a);
    let paid_b = book.withdraw_all(b);
    let total_in = 100_000_001_u64 + 33_333_333 + 77_777 + 7_000_007;
    let total_out = [paid_a, paid_b, paid_c]
        .into_iter()
        .map(|paid| paid.try_into_u128().unwrap())
        .sum::<u128>();
    assert!(total_out <= u128::from(total_in));
    // six operations, at most one micro USDC each stays behind
    assert!(
        u128::from(total_in) - total_out <= 6,
        "{total_in} in, {total_out} out"
    );
    // what the ceiling left unburned stays with its owner, worth what is left at most
    assert!(Shares::value_of(book.shares, book.assets, book.shares) <= book.assets);
    assert!(
        paid_a > TokenAmount::from(100_000_001_u64),
        "a earned part of the premium"
    );
}

#[test]
fn burn_rounds_up_mint_rounds_down() {
    // 3 micro USDC of assets backing 10 shares: a share is worth 0.3 micro USDC
    let assets = TokenAmount::from(3_u8);
    let shares = Shares::new(10);
    // 1 micro USDC buys 10 / 3 = 3.33 shares: 3
    assert_eq!(
        Shares::mint_for(TokenAmount::ONE, assets, shares),
        Some(Shares::new(3))
    );
    // 1 micro USDC out burns 10 / 3 = 3.33 shares: 4
    assert_eq!(
        Shares::burn_for(TokenAmount::ONE, assets, shares),
        Some(Shares::new(4))
    );
    // 4 shares are worth 1.2: 1
    assert_eq!(
        Shares::value_of(Shares::new(4), assets, shares),
        TokenAmount::ONE
    );
    // the first mint is 10^12 units per micro USDC
    assert_eq!(
        Shares::mint_for(usdc(1), TokenAmount::ZERO, Shares::ZERO),
        Some(Shares::new(1_000_000 * FIRST_MINT_SCALE))
    );
    // nothing minted, nothing burned, more than the pool holds, a pool of no assets
    assert_eq!(
        Shares::mint_for(TokenAmount::ONE, usdc(1_000_000), Shares::new(1)),
        None
    );
    assert_eq!(Shares::burn_for(TokenAmount::ZERO, assets, shares), None);
    assert_eq!(
        Shares::burn_for(TokenAmount::from(4_u8), assets, shares),
        None
    );
    assert_eq!(
        Shares::mint_for(TokenAmount::ONE, TokenAmount::ZERO, shares),
        None
    );
    assert_eq!(
        Shares::value_of(shares, assets, Shares::ZERO),
        TokenAmount::ZERO
    );
}

fn settings() -> PoolSettings {
    PoolSettings {
        ledger_fees: vec![LedgerFee {
            ledger: ckusdc(),
            fee: TokenAmount::from(10_000_u32),
        }],
        ..PoolSettings::default()
    }
}

#[test]
fn settings_are_refused_when_inconsistent() {
    assert_eq!(settings().validate(), Ok(()));
    assert_eq!(settings().fees_complete(), Ok(()));
    let token = Principal::from_slice(&[7; 10]);
    let pool = Principal::from_slice(&[8; 10]);
    let cases = [
        (
            PoolSettings {
                tvl_cap: TokenAmount::ZERO,
                ..settings()
            },
            PoolSettingsError::Zero("tvl_cap"),
        ),
        (
            PoolSettings {
                per_swap_cap: usdc(20_000),
                ..settings()
            },
            PoolSettingsError::SwapCapAboveTvlCap,
        ),
        (
            PoolSettings {
                premium_bps: 1_001,
                ..settings()
            },
            PoolSettingsError::PremiumTooHigh(1_001),
        ),
        (
            PoolSettings {
                max_held_bps: 10_001,
                ..settings()
            },
            PoolSettingsError::HeldAboveAll(10_001),
        ),
        (
            PoolSettings {
                max_open_per_payer: 0,
                ..settings()
            },
            PoolSettingsError::Zero("max_open_per_payer"),
        ),
        (
            PoolSettings {
                icp_tokens: vec![IcpToken {
                    ledger: ckusdc(),
                    icpswap_pool: pool,
                }],
                ..settings()
            },
            PoolSettingsError::CkusdcPool,
        ),
        (
            PoolSettings {
                icp_tokens: vec![
                    IcpToken {
                        ledger: token,
                        icpswap_pool: pool,
                    },
                    IcpToken {
                        ledger: token,
                        icpswap_pool: pool,
                    },
                ],
                ..settings()
            },
            PoolSettingsError::Duplicate(token),
        ),
        (
            PoolSettings {
                icp_tokens: vec![IcpToken {
                    ledger: token,
                    icpswap_pool: ckusdc(),
                }],
                ..settings()
            },
            PoolSettingsError::BadPool(token),
        ),
    ];
    for (settings, error) in cases {
        assert_eq!(settings.validate(), Err(error));
    }
    // ckUSDC may be listed with itself as its pool, and a listed token needs a fee read
    let listed = PoolSettings {
        icp_tokens: vec![
            IcpToken {
                ledger: ckusdc(),
                icpswap_pool: ckusdc(),
            },
            IcpToken {
                ledger: token,
                icpswap_pool: pool,
            },
        ],
        ..settings()
    };
    assert_eq!(listed.validate(), Ok(()));
    assert_eq!(
        listed.fees_complete(),
        Err(PoolSettingsError::MissingFee(token))
    );
    assert_eq!(listed.ledgers(), vec![ckusdc(), token]);
    assert!(listed.is_listed(token) && listed.is_listed(ckusdc()));
    assert!(!listed.is_listed(pool));
    assert_eq!(listed.pool_of(token), Some(pool));
    assert_eq!(listed.pool_of(ckusdc()), None);
}

#[test]
fn a_credit_key_orders_by_chain_then_hash_then_log() {
    let key = |chain: u64, tx: u8, log: u64| CreditKey {
        chain: ChainId::new(chain),
        tx: TxHash::new([tx; 32]),
        log_index: log,
    };
    let keys = [
        key(1, 9, 9),
        key(8453, 1, 300),
        key(8453, 1, 301),
        key(8453, 2, 0),
    ];
    for pair in keys.windows(2) {
        assert!(pair[0].to_bytes() < pair[1].to_bytes());
        assert!(pair[0] < pair[1]);
    }
    for k in keys {
        assert_eq!(CreditKey::from_bytes(k.to_bytes()), k);
    }
}

/// The shapes this build's steps had before it gained their optional fields, written as the
/// earlier build wrote them, field for field and index for index.
mod earlier {
    use super::*;

    #[derive(Encode)]
    pub struct Attempt {
        #[n(0)]
        pub created_at: Timestamp,
        #[cbor(n(1), with = "minicbor::bytes")]
        pub memo: [u8; 32],
        #[n(2)]
        pub path: PayoutPath,
    }

    #[derive(Encode)]
    pub enum Step {
        #[n(5)]
        PayoutStarted {
            #[n(0)]
            r#ref: PoolRef,
            #[n(1)]
            attempt: Attempt,
        },
        #[n(7)]
        FellBack {
            #[n(0)]
            r#ref: PoolRef,
            #[n(1)]
            why: String,
        },
        #[n(13)]
        SwapBegun {
            #[n(0)]
            r#ref: PoolRef,
        },
        #[n(14)]
        Swapped {
            #[n(0)]
            r#ref: PoolRef,
            #[n(1)]
            out: TokenAmount,
        },
        #[n(20)]
        ScanCursor {
            #[n(0)]
            block: BlockNumber,
        },
    }
}

/// Review (state decode compatibility): a step written before its optional fields existed
/// still decodes, every new field absent, and one written now with them absent is the same
/// bytes as before.
#[test]
fn a_step_written_before_its_new_fields_still_decodes() {
    let r#ref = PoolRef::new([0x21; 32]);
    let path = PayoutPath::Icpswap {
        pool: Principal::from_slice(&[0x22; 10]),
    };
    let cases = [
        (
            earlier::Step::PayoutStarted {
                r#ref,
                attempt: earlier::Attempt {
                    created_at: Timestamp::from_nanos(23),
                    memo: [0x24; 32],
                    path,
                },
            },
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: Timestamp::from_nanos(23),
                    memo: [0x24; 32],
                    path,
                    amount: None,
                    token: None,
                },
            },
        ),
        (
            earlier::Step::FellBack {
                r#ref,
                why: "below the minimum twice".to_string(),
            },
            PoolStep::FellBack {
                r#ref,
                why: "below the minimum twice".to_string(),
                amount: None,
                token: None,
            },
        ),
        (
            earlier::Step::SwapBegun { r#ref },
            PoolStep::SwapBegun {
                r#ref,
                before: None,
                leg: None,
                amount_in: None,
            },
        ),
        (
            earlier::Step::Swapped {
                r#ref,
                out: TokenAmount::from(25_u8),
            },
            PoolStep::Swapped {
                r#ref,
                out: TokenAmount::from(25_u8),
                send: None,
                leg: None,
            },
        ),
        (
            earlier::Step::ScanCursor {
                block: BlockNumber::new(26),
            },
            PoolStep::ScanCursor {
                block: BlockNumber::new(26),
                log: None,
            },
        ),
    ];
    for (written, now) in cases {
        let bytes = minicbor::to_vec(&written).unwrap();
        assert_eq!(minicbor::decode::<PoolStep>(&bytes).unwrap(), now);
        assert_eq!(minicbor::to_vec(&now).unwrap(), bytes);
    }
}
