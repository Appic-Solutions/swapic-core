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

/// The shard of a ref. The quoter, the canister and the app all compute it this way.
pub const fn shard_of(ref_bytes: &[u8; 32]) -> usize {
    (ref_bytes[31] & 3) as usize
}

#[cfg(test)]
mod tests;
