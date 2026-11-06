//! The tokens a consent message names by symbol beyond what the canister counts itself (the
//! pool's ckUSDC, each configured chain's USDC, a named chain's native coin): a built-in list
//! in code, no stored state. Every id and decimals is copied from the
//! services' curated lists at swapic-backend 8973626 (`config/tokens/*.json`, cited per row),
//! every address in its EIP-55 spelling. Any other token is named by its full id.

#[cfg(test)]
mod tests;

use super::text::TokenName;
use candid::Principal;
use types::{ChainId, EvmAddress};

/// An ICP token the list names, by its ledger's text.
pub struct IcpToken {
    pub ledger: &'static str,
    pub symbol: &'static str,
    pub decimals: u8,
}

/// An EVM token the list names, by its contract's EIP-55 text on one chain.
pub struct EvmToken {
    pub chain: ChainId,
    pub address: &'static str,
    pub symbol: &'static str,
    pub decimals: u8,
}

/// `config/tokens/icp.json:4-8`, and ckXAUT, read off its ledger's own `icrc1_symbol` and
/// `icrc1_decimals` on 2026-11-06.
pub const ICP_TOKENS: [IcpToken; 6] = [
    IcpToken {
        ledger: "ryjl3-tyaaa-aaaaa-aaaba-cai",
        symbol: "ICP",
        decimals: 8,
    },
    IcpToken {
        ledger: "xevnm-gaaaa-aaaar-qafnq-cai",
        symbol: "ckUSDC",
        decimals: 6,
    },
    IcpToken {
        ledger: "cngnf-vqaaa-aaaar-qag4q-cai",
        symbol: "ckUSDT",
        decimals: 6,
    },
    IcpToken {
        ledger: "mxzaz-hqaaa-aaaar-qaada-cai",
        symbol: "ckBTC",
        decimals: 8,
    },
    IcpToken {
        ledger: "ss2fx-dyaaa-aaaar-qacoq-cai",
        symbol: "ckETH",
        decimals: 18,
    },
    IcpToken {
        ledger: "nza5v-qaaaa-aaaar-qahzq-cai",
        symbol: "ckXAUT",
        decimals: 6,
    },
];

const fn evm(chain: u64, address: &'static str, symbol: &'static str, decimals: u8) -> EvmToken {
    EvmToken {
        chain: ChainId::new(chain),
        address,
        symbol,
        decimals,
    }
}

/// USDC, USDT (USDT0 where the list names it so), WETH and WBTC on the four chains the
/// canister launched on; Base's list has no WBTC, and its cbBTC stands in.
pub const EVM_TOKENS: [EvmToken; 16] = [
    // config/tokens/ethereum.json:5-9
    evm(1, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6),
    evm(1, "0xdAC17F958D2ee523a2206206994597C13D831ec7", "USDT", 6),
    evm(1, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "WETH", 18),
    evm(1, "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", "WBTC", 8),
    // config/tokens/polygon.json:6-11
    evm(137, "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", "USDC", 6),
    evm(
        137,
        "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
        "USDT0",
        6,
    ),
    evm(
        137,
        "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619",
        "WETH",
        18,
    ),
    evm(137, "0x1BFD67037B42Cf73acF2047067bd4F2C47D9BfD6", "WBTC", 8),
    // config/tokens/base.json:5-10
    evm(
        8453,
        "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        "USDC",
        6,
    ),
    evm(
        8453,
        "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2",
        "USDT",
        6,
    ),
    evm(
        8453,
        "0x4200000000000000000000000000000000000006",
        "WETH",
        18,
    ),
    evm(
        8453,
        "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf",
        "cbBTC",
        8,
    ),
    // config/tokens/arbitrum.json:5-10
    evm(
        42161,
        "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        "USDC",
        6,
    ),
    evm(
        42161,
        "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
        "USDT0",
        6,
    ),
    evm(
        42161,
        "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
        "WETH",
        18,
    ),
    evm(
        42161,
        "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f",
        "WBTC",
        8,
    ),
];

/// An ICP ledger's token, if the list names it.
pub fn icp_token(ledger: Principal) -> Option<TokenName> {
    let text = ledger.to_text();
    ICP_TOKENS
        .iter()
        .find(|token| token.ledger == text)
        .map(|token| TokenName::Known {
            symbol: token.symbol,
            decimals: token.decimals,
        })
}

/// An EVM token on `chain`, if the list names it there.
pub fn evm_token(chain: ChainId, token: EvmAddress) -> Option<TokenName> {
    let text = token.to_string();
    EVM_TOKENS
        .iter()
        .find(|row| row.chain == chain && row.address == text)
        .map(|row| TokenName::Known {
            symbol: row.symbol,
            decimals: row.decimals,
        })
}
