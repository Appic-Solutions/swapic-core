use super::*;

/// Every row is a real id spelled as the canister itself writes it (an ICP ledger's text, an
/// EVM address's EIP-55), so a lookup by text matches it.
#[test]
fn every_row_is_spelled_as_the_canister_writes_it() {
    for token in &ICP_TOKENS {
        assert_eq!(
            Principal::from_text(token.ledger).unwrap().to_text(),
            token.ledger
        );
    }
    for token in &EVM_TOKENS {
        let address: EvmAddress = token.address.parse().unwrap();
        assert_eq!(address.to_string(), token.address);
    }
}

#[test]
fn the_list_names_its_tokens_on_their_chain_and_nothing_else() {
    let ckbtc = Principal::from_text("mxzaz-hqaaa-aaaar-qaada-cai").unwrap();
    assert_eq!(
        icp_token(ckbtc),
        Some(TokenName::Known {
            symbol: "ckBTC",
            decimals: 8
        })
    );
    let ckxaut = Principal::from_text("nza5v-qaaaa-aaaar-qahzq-cai").unwrap();
    assert_eq!(
        icp_token(ckxaut),
        Some(TokenName::Known {
            symbol: "ckXAUT",
            decimals: 6
        })
    );
    let vchf = Principal::from_text("ly36x-wiaaa-aaaai-aqj7q-cai").unwrap();
    assert_eq!(icp_token(vchf), None);
    let weth: EvmAddress = "0x4200000000000000000000000000000000000006"
        .parse()
        .unwrap();
    assert_eq!(
        evm_token(ChainId::BASE, weth),
        Some(TokenName::Known {
            symbol: "WETH",
            decimals: 18
        })
    );
    // the same address on another chain is not the list's
    assert_eq!(evm_token(ChainId::ARBITRUM, weth), None);
    let cbbtc: EvmAddress = "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf"
        .parse()
        .unwrap();
    assert_eq!(
        evm_token(ChainId::BASE, cbbtc),
        Some(TokenName::Known {
            symbol: "cbBTC",
            decimals: 8
        })
    );
    let usdt0: EvmAddress = "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"
        .parse()
        .unwrap();
    assert_eq!(
        evm_token(ChainId::ARBITRUM, usdt0),
        Some(TokenName::Known {
            symbol: "USDT0",
            decimals: 6
        })
    );
}
