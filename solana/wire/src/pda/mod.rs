//! Derived addresses, and the whole set of Swapic's addresses from its three compiled keys.

use crate::consts::*;
use crate::Pubkey;
use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha256};
use tiny_keccak::{Hasher, Keccak};

/// True when the 32 bytes are a point on the curve: a wallet may hold them, a PDA never does.
pub fn is_on_curve(address: &Pubkey) -> bool {
    CompressedEdwardsY(*address).decompress().is_some()
}

pub fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    hasher.update(bytes);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

/// `None` when the seeds land on the curve, as `create_program_address`.
pub fn create_program_address(seeds: &[&[u8]], program: &Pubkey) -> Option<Pubkey> {
    hash_address(seeds, &[], program)
}

fn hash_address(seeds: &[&[u8]], bump: &[u8], program: &Pubkey) -> Option<Pubkey> {
    let mut hasher = Sha256::new();
    for seed in seeds {
        hasher.update(seed);
    }
    hasher.update(bump);
    hasher.update(program);
    hasher.update(b"ProgramDerivedAddress");
    let hash: [u8; 32] = hasher.finalize().into();
    (!is_on_curve(&hash)).then_some(hash)
}

/// The canonical PDA: the highest bump that lands off the curve.
pub fn find_program_address(seeds: &[&[u8]], program: &Pubkey) -> (Pubkey, u8) {
    for bump in (0..=255u8).rev() {
        if let Some(address) = hash_address(seeds, &[bump], program) {
            return (address, bump);
        }
    }
    unreachable!("a program address exists for every seed set in practice")
}

pub fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    find_program_address(&[owner, token_program, mint], &ATA_PROGRAM).0
}

/// CCTP's PDAs that depend on the message or the domain.
pub fn used_nonce(nonce: &[u8; 32]) -> Pubkey {
    find_program_address(&[SEED_USED_NONCE, nonce], &MT).0
}

fn decimal(domain: u32, buf: &mut [u8; 10]) -> &[u8] {
    let mut n = domain;
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return &buf[at..];
        }
    }
}

pub fn remote_token_messenger(domain: u32) -> Pubkey {
    let mut buf = [0u8; 10];
    let text = decimal(domain, &mut buf);
    find_program_address(&[SEED_REMOTE_TOKEN_MESSENGER, text], &TMM).0
}

/// `source_token` is the 32-byte word of the remote USDC (an EVM address left-padded).
pub fn token_pair(domain: u32, source_token: &[u8; 32]) -> Pubkey {
    let mut buf = [0u8; 10];
    let text = decimal(domain, &mut buf);
    find_program_address(&[SEED_TOKEN_PAIR, text, source_token], &TMM).0
}

/// Every address of a Swapic deployment, from the program id and the canister's two keys. The
/// program compiles the same values in through its build script; a test compares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapicAddresses {
    pub program: Pubkey,
    pub canister: Pubkey,
    pub admin: Pubkey,
    pub config: (Pubkey, u8),
    pub auth: [(Pubkey, u8); SHARDS],
    pub vault: [Pubkey; SHARDS],
    pub wsol: [Pubkey; SHARDS],
    pub denylist: [Pubkey; SHARDS],
    pub rent: (Pubkey, u8),
    pub fee_account: Pubkey,
}

impl SwapicAddresses {
    pub fn derive(program: Pubkey, canister: Pubkey, admin: Pubkey) -> Self {
        let auth: [(Pubkey, u8); SHARDS] =
            core::array::from_fn(|i| find_program_address(&[SEED_AUTH, &[i as u8]], &program));
        Self {
            program,
            canister,
            admin,
            config: find_program_address(&[SEED_CONFIG], &program),
            vault: core::array::from_fn(|i| ata(&auth[i].0, &USDC_MINT, &TOKEN_PROGRAM)),
            wsol: core::array::from_fn(|i| ata(&auth[i].0, &WSOL_MINT, &TOKEN_PROGRAM)),
            denylist: core::array::from_fn(|i| {
                find_program_address(&[SEED_DENYLIST, &auth[i].0], &TMM).0
            }),
            auth,
            rent: find_program_address(&[SEED_RENT], &program),
            fee_account: ata(&admin, &USDC_MINT, &TOKEN_PROGRAM),
        }
    }

    /// The canonical event account of a burn: one live burn per ref.
    pub fn event(&self, ref_bytes: &[u8; 32]) -> Pubkey {
        find_program_address(&[SEED_EVENT, ref_bytes], &self.program).0
    }

    /// The shard whose `auth` this is.
    pub fn auth_index(&self, address: &Pubkey) -> Option<usize> {
        self.auth.iter().position(|(a, _)| a == address)
    }

    /// The shard whose `vault` this is.
    pub fn vault_index(&self, address: &Pubkey) -> Option<usize> {
        self.vault.iter().position(|a| a == address)
    }

    /// The program's `is_program_address`: where `release` pays Swapic's fee account and not the
    /// hook's recipient.
    pub fn is_program_address(&self, address: &Pubkey) -> bool {
        *address == self.program
            || *address == self.config.0
            || *address == self.rent.0
            || self.auth_index(address).is_some()
            || self.vault_index(address).is_some()
    }

    /// True for every account a user's recipient may never be.
    pub fn is_swapic_account(&self, address: &Pubkey) -> bool {
        *address == self.program
            || *address == self.canister
            || *address == self.admin
            || *address == self.config.0
            || *address == self.rent.0
            || *address == self.fee_account
            || self.auth_index(address).is_some()
            || self.vault_index(address).is_some()
            || self.wsol.contains(address)
    }
}

#[cfg(test)]
mod tests;
