//! The Solana line's domain types: its config, the lines the log holds, and the fold's entries.
//! Every byte is the wire crate's business; here an address is 32 bytes and a signature 64.

#[cfg(test)]
mod tests;

use crate::numeric::TokenAmount;
use candid::Principal;
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;

pub type SolanaAddress = [u8; 32];
pub type SolanaSignature = [u8; 64];
pub type Lamports = u64;
/// A burn's ref, or an arrival's id.
pub type SolanaRef = [u8; 32];

/// The two threshold keys (design decision 6). A bug in the gasless co-signer can never sign an
/// upgrade or a fee sweep, because it never holds the admin key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SolanaKeyRole {
    /// Fee payer, arrivals, keeper.
    #[n(0)]
    Payer,
    /// Upgrade authority, `set_config`, owner of the fee account.
    #[n(1)]
    Admin,
}

/// What the canister's Solana work costs it, priced by the kit (append-only, separate from the EVM
/// `GasAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SolanaAction {
    #[n(0)]
    GaslessBurn,
    /// An arrival's `complete`, one new account included.
    #[n(1)]
    CompleteNewAccount,
    /// An arrival's `release`, one new account included.
    #[n(2)]
    ReleaseNewAccount,
    /// A relay of a stuck Forwarder message.
    #[n(3)]
    ForwardRelay,
    #[n(4)]
    Reclaim,
}

/// The least network fee of each action, in micro USDC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct SolanaFees {
    #[n(0)]
    pub gasless_burn: TokenAmount,
    #[n(1)]
    pub complete_new_account: TokenAmount,
    #[n(2)]
    pub release_new_account: TokenAmount,
    #[n(3)]
    pub forward_relay: TokenAmount,
    #[n(4)]
    pub reclaim: TokenAmount,
}

impl SolanaFees {
    pub fn get(&self, action: SolanaAction) -> TokenAmount {
        match action {
            SolanaAction::GaslessBurn => self.gasless_burn,
            SolanaAction::CompleteNewAccount => self.complete_new_account,
            SolanaAction::ReleaseNewAccount => self.release_new_account,
            SolanaAction::ForwardRelay => self.forward_relay,
            SolanaAction::Reclaim => self.reclaim,
        }
    }

    /// What a swap arrival always pays: the dearer of its two ends, with one new account, so closing
    /// accounts between the quote and the arrival cannot make the canister pay (attack L1).
    pub fn arrival(&self) -> TokenAmount {
        self.complete_new_account.max(self.release_new_account)
    }
}

/// The frozen Swapic lookup table, a constant of the config: the v0 form of an arrival compiles
/// against it when the v1 send failed its proof (design decision 8).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct SolanaTable {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub address: SolanaAddress,
    #[n(1)]
    pub entries: Vec<SolanaAddress>,
}

/// The Solana line's config. Absent: the canister signs nothing on Solana.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct SolanaConfig {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub program_id: SolanaAddress,
    /// The SOL RPC canister.
    #[cbor(n(1), with = "crate::pool::principal_cbor")]
    pub sol_rpc: Principal,
    /// The threshold Ed25519 key's name, `key_1` in production, fixed once derived.
    #[n(2)]
    pub key_name: String,
    #[n(3)]
    pub min_network_fee: SolanaFees,
    #[n(4)]
    pub max_cu_limit: u32,
    /// Micro-lamports per compute unit.
    #[n(5)]
    pub max_cu_price: u64,
    #[n(6)]
    pub max_priority_lamports: Lamports,
    #[n(7)]
    pub loaded_data_limit: u32,
    #[n(8)]
    pub rent_float_low: Lamports,
    #[n(9)]
    pub rent_float_target: Lamports,
    #[n(10)]
    pub max_complete_attempts: u32,
    #[n(11)]
    pub max_release_attempts: u32,
    #[n(12)]
    pub complete_margin_s: u64,
    #[n(13)]
    pub cosign_per_signer_per_hour: u32,
    #[n(14)]
    pub cosign_global_per_hour: u32,
    /// A smaller budget that landed failures are charged to (attack M1).
    #[n(15)]
    pub cosign_failures_per_hour: u32,
    /// The least `min_usdc` a gasless burn may name, in micro USDC.
    #[n(16)]
    pub gasless_min_usdc: u64,
    /// Added to the program's `min_deadline_s` for a gasless deadline (attack M1).
    #[n(17)]
    pub deadline_margin_s: u64,
    /// The program's `min_deadline_s`, as `set_solana_program_config` last set it.
    #[n(18)]
    pub program_min_deadline_s: u64,
    /// The program's `min_burn`, in micro USDC.
    #[n(19)]
    pub program_min_burn: u64,
    /// The most a burn's max fee may be, in deci-bps.
    #[n(20)]
    pub max_fee_deci_bps: u16,
    /// The most a Forwarder burn's `maxFee` may be above the fast fee, in micro USDC.
    #[n(21)]
    pub max_forward_fee: u64,
    #[n(22)]
    pub upgrade_delay_s: u64,
    #[n(23)]
    pub enabled_gasless: bool,
    #[n(24)]
    pub enabled_arrivals: bool,
    /// Some: arrivals compile as v0 against this table, 1-leg routes only (design decision 8).
    #[n(25)]
    pub arrival_table: Option<SolanaTable>,
    /// The program's `event_keypair_allowed`, as `set_solana_program_config` last set it.
    #[n(26)]
    pub program_event_keypair_allowed: bool,
    /// The program's `paused`, as `set_solana_program_config` last set it: a paused program refuses
    /// what is co-signed or completed, so neither is signed.
    #[n(27)]
    pub program_paused: bool,
}

impl SolanaConfig {
    /// What a stored Solana config must satisfy.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.key_name.is_empty() {
            return Err("key_name is empty");
        }
        if self.program_id == [0; 32] {
            return Err("program_id is empty");
        }
        if self.rent_float_low > self.rent_float_target {
            return Err("rent_float_low is above rent_float_target");
        }
        if self.max_cu_limit == 0 || self.max_cu_limit > 1_400_000 {
            return Err("max_cu_limit is outside 1 to 1,400,000");
        }
        if self.max_complete_attempts == 0 || self.max_release_attempts == 0 {
            return Err("an attempt cap is zero");
        }
        if self.loaded_data_limit == 0 {
            return Err("loaded_data_limit is zero");
        }
        Ok(())
    }
}

/// Which form a signed transaction took.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SolanaTxVersion {
    #[n(0)]
    V0,
    #[n(1)]
    V1,
}

/// Why a Solana transaction exists.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum SolanaPurpose {
    /// A gasless user burn, co-signed as its fee payer.
    #[n(0)]
    Cosign {
        #[cbor(n(0), with = "minicbor::bytes")]
        r#ref: SolanaRef,
        #[cbor(n(1), with = "minicbor::bytes")]
        signer: SolanaAddress,
        #[cbor(n(2), with = "minicbor::bytes")]
        message_hash: [u8; 32],
    },
    #[n(1)]
    Complete {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    #[n(2)]
    Release {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    #[n(3)]
    MisroutedRelease {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    #[n(4)]
    Reclaim {
        #[n(0)]
        refs: Vec<SolanaRef>,
    },
    #[n(5)]
    TopUp,
    #[n(6)]
    ForwardRelay {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    #[n(7)]
    Config,
    #[n(8)]
    Upgrade {
        #[n(0)]
        proposal: u64,
    },
    #[n(9)]
    Sweep,
    /// The proof that a v1 transaction this canister signs can land (rollout step 4).
    #[n(10)]
    Proof,
}

impl SolanaPurpose {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cosign { .. } => "Cosign",
            Self::Complete { .. } => "Complete",
            Self::Release { .. } => "Release",
            Self::MisroutedRelease { .. } => "MisroutedRelease",
            Self::Reclaim { .. } => "Reclaim",
            Self::TopUp => "TopUp",
            Self::ForwardRelay { .. } => "ForwardRelay",
            Self::Config => "Config",
            Self::Upgrade { .. } => "Upgrade",
            Self::Sweep => "Sweep",
            Self::Proof => "Proof",
        }
    }
}

/// A program upgrade, published and waiting out its delay (design 4.13).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct SolanaProposal {
    #[n(0)]
    pub id: u64,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub buffer: SolanaAddress,
    #[cbor(n(2), with = "minicbor::bytes")]
    pub build_hash: [u8; 32],
    #[n(3)]
    pub source_commit: String,
    #[n(4)]
    pub proposed_at_s: u64,
    #[n(5)]
    pub not_before_s: u64,
    #[n(6)]
    pub status: SolanaProposalStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SolanaProposalStatus {
    #[n(0)]
    Waiting,
    #[n(1)]
    Cancelled,
    #[n(2)]
    Executed,
}

/// One line of the log about Solana.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum SolanaStep {
    #[n(0)]
    KeysDerived {
        #[cbor(n(0), with = "minicbor::bytes")]
        payer: SolanaAddress,
        #[cbor(n(1), with = "minicbor::bytes")]
        admin: SolanaAddress,
    },
    /// A transaction the canister signed: what the watcher broadcasts from `signed_txs`.
    #[n(1)]
    Signed {
        #[n(0)]
        purpose: SolanaPurpose,
        #[n(1)]
        version: SolanaTxVersion,
        #[cbor(n(2), with = "minicbor::bytes")]
        signature: SolanaSignature,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw: Vec<u8>,
        #[n(4)]
        last_valid_block_height: u64,
    },
    /// A co-signed transaction that landed and failed, read on chain by the canister.
    #[n(2)]
    CosignFailed {
        #[cbor(n(0), with = "minicbor::bytes")]
        signer: SolanaAddress,
        #[cbor(n(1), with = "minicbor::bytes")]
        signature: SolanaSignature,
    },
    #[n(3)]
    UpgradeProposed {
        #[n(0)]
        proposal: SolanaProposal,
    },
    #[n(4)]
    UpgradeCancelled {
        #[n(0)]
        id: u64,
    },
    #[n(5)]
    UpgradeExecuted {
        #[n(0)]
        id: u64,
    },
    /// The one finish of a burn out of Solana: one per `(domain 5, ref)`.
    #[n(6)]
    RefFinished {
        #[cbor(n(0), with = "minicbor::bytes")]
        r#ref: SolanaRef,
        #[cbor(n(1), with = "minicbor::bytes")]
        arrival: SolanaRef,
        /// When the burn happened, seconds: the keeper reclaims 5 days after it.
        #[n(2)]
        burned_at_s: u64,
    },
    /// An arrival into Solana whose `used_nonce` exists: nothing more is ever signed for it.
    #[n(7)]
    ArrivalFinished {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    /// A message `complete` refuses, paid to Swapic's fee account: the order and the EVM payer to
    /// refund by hand (design 5.2.7).
    #[n(8)]
    Misrouted {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
        #[cbor(n(1), with = "minicbor::bytes")]
        order_ref: [u8; 32],
        #[cbor(n(2), with = "minicbor::bytes")]
        payer: [u8; 20],
    },
    /// An arrival into Solana a source transaction stood for: one fee pays one arrival.
    #[n(9)]
    ArrivalOfSource {
        #[cbor(n(0), with = "minicbor::bytes")]
        source_tx: [u8; 32],
        #[cbor(n(1), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
}

impl SolanaStep {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::KeysDerived { .. } => "KeysDerived",
            Self::Signed { .. } => "Signed",
            Self::CosignFailed { .. } => "CosignFailed",
            Self::UpgradeProposed { .. } => "UpgradeProposed",
            Self::UpgradeCancelled { .. } => "UpgradeCancelled",
            Self::UpgradeExecuted { .. } => "UpgradeExecuted",
            Self::RefFinished { .. } => "RefFinished",
            Self::ArrivalFinished { .. } => "ArrivalFinished",
            Self::Misrouted { .. } => "Misrouted",
            Self::ArrivalOfSource { .. } => "ArrivalOfSource",
        }
    }

    /// The signed transaction this line holds: its signature and its wire bytes.
    pub fn signed(&self) -> Option<(&SolanaSignature, &[u8])> {
        match self {
            Self::Signed { signature, raw, .. } => Some((signature, raw)),
            _ => None,
        }
    }

    /// This line with every transaction byte emptied (audit C5).
    pub fn clear_raw(&mut self) {
        if let Self::Signed { raw, .. } = self {
            raw.clear();
        }
    }
}

/// A key of the Solana fold: a tag, then what it is about.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SolanaKey(pub Vec<u8>);

impl SolanaKey {
    fn of(tag: u8, payload: &[u8]) -> Self {
        let mut key = Vec::with_capacity(1 + payload.len());
        key.push(tag);
        key.extend_from_slice(payload);
        Self(key)
    }
    /// How many `complete`s were signed for an arrival.
    pub fn complete_attempts(arrival: &SolanaRef) -> Self {
        Self::of(1, arrival)
    }
    pub fn release_attempts(arrival: &SolanaRef) -> Self {
        Self::of(2, arrival)
    }
    pub fn forward_attempts(arrival: &SolanaRef) -> Self {
        Self::of(3, arrival)
    }
    pub fn arrival_done(arrival: &SolanaRef) -> Self {
        Self::of(4, arrival)
    }
    pub fn finished_ref(r#ref: &SolanaRef) -> Self {
        Self::of(5, r#ref)
    }
    pub fn cosign_by_ref(r#ref: &SolanaRef) -> Self {
        Self::of(6, r#ref)
    }
    pub fn cosign_by_signature(signature: &SolanaSignature) -> Self {
        Self::of(7, signature)
    }
    pub fn signer(signer: &SolanaAddress) -> Self {
        Self::of(8, signer)
    }
    pub fn global() -> Self {
        Self::of(9, &[])
    }
    pub fn burn(r#ref: &SolanaRef) -> Self {
        Self::of(10, r#ref)
    }
    pub fn proposal(id: u64) -> Self {
        Self::of(11, &id.to_be_bytes())
    }
    pub fn proposal_count() -> Self {
        Self::of(12, &[])
    }
    pub fn last_top_up() -> Self {
        Self::of(13, &[])
    }
    /// That a source transaction stood for this arrival.
    pub fn source_arrival(source_tx: &[u8; 32], arrival: &SolanaRef) -> Self {
        Self::of(14, &[&source_tx[..], &arrival[..]].concat())
    }
    /// How many arrivals a source transaction stood for.
    pub fn source_arrivals(source_tx: &[u8; 32]) -> Self {
        Self::of(15, source_tx)
    }
}

impl Storable for SolanaKey {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(&self.0)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Self(bytes.into_owned())
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 65,
        is_fixed_size: false,
    };
}

/// A value of the Solana fold. Only `Solana` lines write them.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum SolanaEntry {
    #[n(0)]
    Count(#[n(0)] u64),
    #[n(1)]
    Done,
    /// A co-signature: the line that holds it, by ref and by signature.
    #[n(2)]
    Cosigned {
        #[cbor(n(0), with = "minicbor::bytes")]
        message_hash: [u8; 32],
        #[n(1)]
        line: u64,
        #[n(2)]
        at_s: u64,
        #[cbor(n(3), with = "minicbor::bytes")]
        signer: SolanaAddress,
    },
    /// What a signer did: its co-signs this hour, and how its landed ones went.
    #[n(3)]
    Signer {
        #[n(0)]
        blocked: bool,
        #[n(1)]
        succeeded: bool,
        #[n(2)]
        hour: u64,
        #[n(3)]
        count: u32,
    },
    /// All signers this hour.
    #[n(4)]
    Global {
        #[n(0)]
        hour: u64,
        #[n(1)]
        count: u32,
        #[n(2)]
        failures: u32,
    },
    /// The arrival a burn's ref finished as.
    #[n(5)]
    Finished {
        #[cbor(n(0), with = "minicbor::bytes")]
        arrival: SolanaRef,
    },
    /// A burn out of Solana the keeper may reclaim.
    #[n(6)]
    Burn {
        #[n(0)]
        at_s: u64,
        #[n(1)]
        reclaim_tries: u32,
    },
    #[n(7)]
    Proposal(#[n(0)] SolanaProposal),
    #[n(8)]
    Time(#[n(0)] u64),
}

crate::storable_as_cbor!(SolanaEntry);
