#![allow(dead_code)]
//! What the program's test files share: routes and arrivals with the fewest moving parts.

use sha2::{Digest, Sha256};
use swapic_solana_tests::wire::args::{ArrivalArgs, TAG_COMPLETE, TAG_RELEASE};
use swapic_solana_tests::wire::consts::*;
use swapic_solana_tests::wire::envelope::{SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_tests::wire::instructions::{self, ArrivalSource};
use swapic_solana_tests::wire::message::{AccountMeta, Instruction};
use swapic_solana_tests::wire::Pubkey;
use swapic_solana_tests::*;

pub fn hash(label: &str) -> [u8; 32] {
    Sha256::digest(label.as_bytes()).into()
}

/// A ref that lands on the shard asked for.
pub fn ref_on(shard: usize, label: &str) -> [u8; 32] {
    let mut r = hash(label);
    r[31] = (r[31] & !3) | shard as u8;
    r
}

/// The stand-in's fixed route accounts, then whatever the mode needs.
pub fn route_accounts(
    authority: &Pubkey,
    source: &Pubkey,
    destination: &Pubkey,
    wallet: &Pubkey,
    pool_in: &Pubkey,
    pool_out: &Pubkey,
    pool: &Pubkey,
    extra: &[AccountMeta],
) -> Vec<AccountMeta> {
    let mut accounts = vec![
        AccountMeta::readonly(*authority),
        AccountMeta::writable(*source),
        AccountMeta::writable(*destination),
        AccountMeta::readonly(USDC_MINT),
        AccountMeta::readonly(WSOL_MINT),
        AccountMeta::readonly(TOKEN_PROGRAM),
        AccountMeta::readonly(TOKEN_PROGRAM),
        AccountMeta::writable(*wallet),
        AccountMeta::readonly(JUPITER_EVENT_AUTHORITY),
        AccountMeta::readonly(JUPITER),
        AccountMeta::writable(*pool_in),
        AccountMeta::writable(*pool_out),
        AccountMeta::writable(*pool),
    ];
    accounts.extend_from_slice(extra);
    // the stand-in's own calls into the System program need it in reach
    accounts.push(AccountMeta::readonly(SYSTEM_PROGRAM));
    accounts
}

/// The arrival a test starts from: a swap of `AMOUNT` less `FEE` into the recipient's token account.
pub const AMOUNT: u64 = 1_000 * ONE_USDC;
pub const FEE: u64 = 130_000;
pub const MINTED: u64 = AMOUNT - FEE;
pub const OUT: u64 = 5_000 * ONE_USDC;
pub const MIN_OUT: u64 = 4_900 * ONE_USDC;

pub struct Arrival {
    pub shard: usize,
    pub nonce: [u8; 32],
    pub hook: SolanaEnvelope,
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
}

impl Arrival {
    pub fn new(
        env: &Env,
        label: &str,
        shard: usize,
        change: impl FnOnce(&mut SolanaEnvelope),
    ) -> Self {
        let mut hook = SolanaEnvelope {
            ref_bytes: ref_on(shard, label),
            action: SOLANA_ACTION_SWAP,
            deadline: (env.now() + 3600) as u32,
            recipient: pubkey_of(&env.recipient),
            mint_out: env.mint_out,
            min_out: MIN_OUT,
        };
        change(&mut hook);
        Self::with_hook(env, label, shard, shard, &hook.encode(), hook)
    }

    /// A message whose caller and mint recipient may name different shards and whose hook may be anything.
    pub fn with_hook(
        env: &Env,
        label: &str,
        caller: usize,
        vault: usize,
        hook_bytes: &[u8],
        hook: SolanaEnvelope,
    ) -> Self {
        let nonce = hash(&format!("nonce-{label}"));
        let (message, attestation) = env.craft(&MessageSpec {
            header_version: 1,
            destination: SOLANA_DOMAIN,
            nonce: &nonce,
            caller: &env.a.auth[caller].0,
            mint_recipient: &env.a.vault[vault],
            amount: AMOUNT,
            fee: FEE,
            hook: hook_bytes,
        });
        Self {
            shard: caller,
            nonce,
            hook,
            message,
            attestation,
        }
    }

    fn source<'a>(&'a self, env: &'a Env, token: &'a [u8; 32]) -> ArrivalSource<'a> {
        let _ = env;
        ArrivalSource {
            nonce: &self.nonce,
            source_domain: 6,
            source_token: token,
        }
    }

    pub fn complete_data(&self, jupiter: &[u8]) -> Vec<u8> {
        ArrivalArgs {
            tag: TAG_COMPLETE,
            message: &self.message,
            attestation: &self.attestation,
            jupiter,
        }
        .encode()
    }

    pub fn release_data(&self) -> Vec<u8> {
        ArrivalArgs {
            tag: TAG_RELEASE,
            message: &self.message,
            attestation: &self.attestation,
            jupiter: &[],
        }
        .encode()
    }

    /// `complete` with the stand-in's honest route into `output`, whatever the mode.
    pub fn complete(
        &self,
        env: &Env,
        submitter: &Pubkey,
        output: &Pubkey,
        mode: u8,
        quoted: u64,
        arg: u64,
        extra: &[AccountMeta],
    ) -> Instruction {
        let (_, token) = env.source();
        let mut route = route_accounts(
            &env.a.auth[self.shard].0,
            &env.a.vault[self.shard],
            output,
            &pubkey_of(&env.recipient),
            &env.pool_usdc,
            &env.pool_out,
            &env.pool,
            extra,
        );
        // a hostile route is free to ask for `auth` to be writable
        route[0].writable = matches!(
            mode,
            route::ASSIGN_AUTH | route::ALLOCATE_AUTH | route::FUND_AUTH
        );
        instructions::complete(
            &env.a,
            submitter,
            self.shard,
            output,
            self.source(env, &token),
            &route,
            self.complete_data(&route::data(MINTED, quoted, mode, arg)),
        )
    }

    /// `complete` for a native arrival: `wsol[i]` as the route's destination, the wallet as the output.
    pub fn complete_native(
        &self,
        env: &Env,
        submitter: &Pubkey,
        mode: u8,
        quoted: u64,
    ) -> Instruction {
        let (_, token) = env.source();
        let wallet = pubkey_of(&env.recipient);
        let route = route_accounts(
            &env.a.auth[self.shard].0,
            &env.a.vault[self.shard],
            &env.a.wsol[self.shard],
            &wallet,
            &env.pool_usdc,
            &env.pool_wsol,
            &env.pool,
            &[],
        );
        instructions::complete(
            &env.a,
            submitter,
            self.shard,
            &wallet,
            self.source(env, &token),
            &route,
            self.complete_data(&route::data(MINTED, quoted, mode, 0)),
        )
    }

    pub fn release(
        &self,
        env: &Env,
        submitter: &Pubkey,
        caller: usize,
        vault: usize,
        output: &Pubkey,
    ) -> Instruction {
        let (_, token) = env.source();
        instructions::release(
            &env.a,
            submitter,
            caller,
            vault,
            output,
            self.source(env, &token),
            self.release_data(),
        )
    }
}
