//! Addresses every Swapic Solana component shares: public programs and accounts that exist
//! whatever Swapic deploys. Swapic's own addresses come from `pda::SwapicAddresses`.

use crate::Pubkey;

/// Base58 text to 32 bytes at compile time; a bad constant stops the build.
pub const fn b58(text: &str) -> Pubkey {
    const fn digit(c: u8) -> u32 {
        match c {
            b'1'..=b'9' => (c - b'1') as u32,
            b'A'..=b'H' => (c - b'A') as u32 + 9,
            b'J'..=b'N' => (c - b'J') as u32 + 17,
            b'P'..=b'Z' => (c - b'P') as u32 + 22,
            b'a'..=b'k' => (c - b'a') as u32 + 33,
            b'm'..=b'z' => (c - b'm') as u32 + 44,
            _ => panic!("not a base58 character"),
        }
    }
    let input = text.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < input.len() {
        let mut carry = digit(input[i]);
        let mut j = 32;
        while j > 0 {
            j -= 1;
            let value = out[j] as u32 * 58 + carry;
            out[j] = value as u8;
            carry = value >> 8;
        }
        if carry != 0 {
            panic!("base58 text longer than 32 bytes");
        }
        i += 1;
    }
    out
}

pub const SYSTEM_PROGRAM: Pubkey = [0; 32];
pub const TOKEN_PROGRAM: Pubkey = b58("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM: Pubkey = b58("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const ATA_PROGRAM: Pubkey = b58("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const COMPUTE_BUDGET_PROGRAM: Pubkey = b58("ComputeBudget111111111111111111111111111111");
pub const UPGRADEABLE_LOADER: Pubkey = b58("BPFLoaderUpgradeab1e11111111111111111111111");
pub const RENT_SYSVAR: Pubkey = b58("SysvarRent111111111111111111111111111111111");
pub const CLOCK_SYSVAR: Pubkey = b58("SysvarC1ock11111111111111111111111111111111");

pub const USDC_MINT: Pubkey = b58("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const WSOL_MINT: Pubkey = b58("So11111111111111111111111111111111111111112");

pub const JUPITER: Pubkey = b58("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
pub const JUPITER_EVENT_AUTHORITY: Pubkey = b58("D8cy77BBepLMngZx6ZukaTff5hCt1HrWyKk3Hnd9oitf");
/// JupiterZ, the market-maker route of `/order`. The program never calls it; the app's guard knows it.
pub const JUPITER_Z: Pubkey = b58("61DFfeTKM7trxYcPQCM78bJ794ddZprZpAwAnLiwTpYH");
/// The fee payer of Jupiter's automatic gasless sponsorship.
pub const JUPITER_SPONSOR: Pubkey = b58("gasTzr94Pmp4Gf8vknQnqxeYxdgwFjbgdJa4msYRpnB");

/// TokenMessengerMinterV2, the receiver of a CCTP burn message.
pub const TMM: Pubkey = b58("CCTPV2vPZJS2u2BBsUoscuikbYjnpFmbFsvVuJdgUMQe");
/// MessageTransmitterV2.
pub const MT: Pubkey = b58("CCTPV2Sm4AdWt5296sk4P66VBZ7bEhcARwFaaS9YPbeC");
pub const MESSAGE_TRANSMITTER: Pubkey = b58("W1k5ijkaSTo5iA5zChNpfzcy796fLhkBxfmJuR8W8HU");
pub const TOKEN_MESSENGER: Pubkey = b58("AawthJCGRmggpfv9MMWV6Jmo9cue4gL9wUZgRBShg58W");
pub const TOKEN_MINTER: Pubkey = b58("E1bQJ8eMMn3zmeSewW3HQ8zmJr7KR75JonbwAtWx2bux");
pub const LOCAL_TOKEN: Pubkey = b58("CRBBbuLCyrkQy4dCTHxqstSmDQv4ajBeUVb9qUdMVaP1");
pub const SENDER_AUTHORITY: Pubkey = b58("45hzrGLQ2EGo1Ln7QpXjDwb589GDQ9H2aEXXw6ds6BFE");
pub const TMM_EVENT_AUTHORITY: Pubkey = b58("6TCCnJ9R1m1RXFzyoH7GYH2J6NJDtZaUvfipPuLWxHNd");
pub const MT_EVENT_AUTHORITY: Pubkey = b58("2PcXTomVAbX5Es1NUZUkxwuCm8tvV4NmRk3fmQWFCWoV");
pub const MT_AUTHORITY_TMM: Pubkey = b58("DsAdX23SVpTPYhKP2ua1mx8gTPqLyzx7a43cyxYjS2up");
pub const FEE_RECIPIENT_TOKEN_ACCOUNT: Pubkey = b58("6zNSMmZGMhNyqZMHkx2L63DLuqh5qoqBhaQJPJD7Fvt3");
pub const CUSTODY: Pubkey = b58("6xTBTqJMBr5m7BKqVxmW2x11DfqUwtD3TJsqpxELx72L");

/// The CCTP domain of Solana.
pub const SOLANA_DOMAIN: u32 = 5;
/// Plasma: no Forwarder there, so no USDC-only route into Solana from it.
pub const PLASMA_DOMAIN: u32 = 33;

/// The size of CCTP's `MessageSent` event account for a Swapic burn: its rent is what the user
/// (or the float) pays on a burn out of Solana.
pub const EVENT_ACCOUNT_LEN: u64 = 748;
/// The length of a CCTP message that carries a Solana envelope.
pub const ARRIVAL_MESSAGE_LEN: usize = 486;

pub const SEED_CONFIG: &[u8] = b"config";
pub const SEED_AUTH: &[u8] = b"auth";
pub const SEED_RENT: &[u8] = b"rent";
pub const SEED_EVENT: &[u8] = b"event";
pub const SEED_DENYLIST: &[u8] = b"denylist_account";
pub const SEED_USED_NONCE: &[u8] = b"used_nonce";
pub const SEED_REMOTE_TOKEN_MESSENGER: &[u8] = b"remote_token_messenger";
pub const SEED_TOKEN_PAIR: &[u8] = b"token_pair";

/// How many `auth` and `vault` pairs the program has.
pub const SHARDS: usize = 4;

pub const ADDRESS_LOOKUP_TABLE_PROGRAM: Pubkey = b58("AddressLookupTab1e1111111111111111111111111");

const fn hex20(text: &str) -> [u8; 20] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("not hex"),
        }
    }
    let input = text.as_bytes();
    let mut out = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        out[i] = nibble(input[i * 2]) << 4 | nibble(input[i * 2 + 1]);
        i += 1;
    }
    out
}

/// One EVM chain Swapic reaches over CCTP: its domain and its USDC (the kit's chain table).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SwapicDomain {
    pub domain: u32,
    pub chain_id: u64,
    pub usdc: [u8; 20],
}

/// The ten EVM chains live on 2026-11-08. A new chain is one more row and a new lookup table.
pub const SWAPIC_DOMAINS: [SwapicDomain; 10] = [
    SwapicDomain { domain: 0, chain_id: 1, usdc: hex20("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48") },
    SwapicDomain { domain: 2, chain_id: 10, usdc: hex20("0b2c639c533813f4aa9d7837caf62653d097ff85") },
    SwapicDomain { domain: 3, chain_id: 42161, usdc: hex20("af88d065e77c8cc2239327c5edb3a432268e5831") },
    SwapicDomain { domain: 6, chain_id: 8453, usdc: hex20("833589fcd6edb6e08f4c7c32d4f71b54bda02913") },
    SwapicDomain { domain: 7, chain_id: 137, usdc: hex20("3c499c542cef5e3811e1192ce70d8cc03d5c3359") },
    SwapicDomain { domain: 10, chain_id: 130, usdc: hex20("078d782b760474a361dda0af3839290b0ef57ad6") },
    SwapicDomain { domain: 14, chain_id: 480, usdc: hex20("79a02482a880bce3f13e09da970dc34db4cd24d1") },
    SwapicDomain { domain: 15, chain_id: 143, usdc: hex20("754704bc059f8c67012fed69bc8a327a5aafb603") },
    SwapicDomain { domain: 19, chain_id: 999, usdc: hex20("b88339cb7199b77e23db6e890353e22632ba630f") },
    SwapicDomain { domain: 33, chain_id: 9745, usdc: hex20("2d661c89d812261039af9764eceaaee884f5f67f") },
];

/// The shard of a ref. The quoter, the canister and the app all compute it this way.
pub const fn shard_of(ref_bytes: &[u8; 32]) -> usize {
    (ref_bytes[31] & 3) as usize
}

#[cfg(test)]
mod tests;
