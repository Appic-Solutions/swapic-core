//! The pool (spec 2026-10-19 sections 2, 3.1, 3.3 and 4; the fixed interface section 2): its
//! two pockets (USDC at the canister's Base address, ckUSDC on ICP), the stakers' shares,
//! the deposits registered into it, and the one log step that moves any of them.
//!
//! Pure: no canister calls. [`ledger`] holds the fold's rules (`admits` refuses, `apply`
//! never fails) and the registration's policy, which reads only the fold.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a new
//! field is optional.

#[cfg(test)]
mod tests;

pub mod ledger;
pub mod payout;
pub mod routes;
pub mod withdrawal;

pub use routes::{IcpRoute, IcpRouteVia, IcpRoutes, RoutePath, RoutesError};

pub use payout::{
    evm_payout_id, evm_payout_ref, EvmPayout, EvmWithdrawal, IcpSwapTerms, NonceUse, PayoutKind,
    PayoutLines, PayoutSettings, PayoutSettingsError, PayoutStatus, PayoutTerms, PinnedOrder, Undo,
};

pub use ledger::{
    check_ref, check_terms, least_premium, PoolEffects, PoolLedger, PoolLookup, PoolStepError,
    RegisterRefusal, ICPSWAP_PATH_FEES, MAX_PAYOUT_ATTEMPTS,
};

use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::hash::{OwnerKey, PoolOpId, PoolRef, TxHash};
use crate::numeric::{BlockNumber, Nonce, Timestamp, TokenAmount};
use alloy_primitives::keccak256;
use candid::Principal;
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
use std::time::Duration;
use thiserror::Error;

/// A principal as its raw bytes, at most 29 of them.
pub mod principal_cbor {
    use candid::Principal;
    use minicbor::decode::{Decoder, Error as DecodeError};
    use minicbor::encode::{Encoder, Error as EncodeError, Write};

    pub fn encode<C, W: Write>(
        principal: &Principal,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        e.bytes(principal.as_slice())?.ok()
    }

    pub fn decode<C>(d: &mut Decoder<'_>, _ctx: &mut C) -> Result<Principal, DecodeError> {
        Principal::try_from_slice(d.bytes()?)
            .map_err(|_| DecodeError::message("a principal is at most 29 bytes"))
    }
}

/// One ICRC-1 account on ICP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct IcpAccount {
    #[cbor(n(0), with = "principal_cbor")]
    pub owner: Principal,
    /// None is the default subaccount, which ICRC-1 reads as 32 zero bytes.
    #[cbor(n(1), with = "minicbor::bytes")]
    pub subaccount: Option<[u8; 32]>,
}

impl IcpAccount {
    /// The 32 bytes ICRC-1 reads the subaccount as.
    pub fn subaccount_bytes(&self) -> [u8; 32] {
        self.subaccount.unwrap_or([0; 32])
    }
}

/// Whoever owns shares: an EVM address (a stake from EVM) or an ICP account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum PoolAccount {
    #[n(0)]
    Evm(#[n(0)] EvmAddress),
    #[n(1)]
    Icp(#[n(0)] IcpAccount),
}

impl PoolAccount {
    /// The account's key in the shares map and in the open-deposit counts.
    pub fn key(&self) -> OwnerKey {
        owner_hash(self)
    }
}

/// What a registered deposit does when it arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum DepositKind {
    /// Pay `recipient` on ICP: ckUSDC, or `token_out` through its ICPSwap pool, at least
    /// `min_out` of it.
    #[n(0)]
    Swap {
        #[n(0)]
        recipient: IcpAccount,
        #[cbor(n(1), with = "principal_cbor")]
        token_out: Principal,
        #[n(2)]
        min_out: u128,
    },
    /// Mint shares to `owner`.
    #[n(1)]
    Stake {
        #[n(0)]
        owner: PoolAccount,
    },
}

impl DepositKind {
    /// Who the deposit is for: a swap's recipient, a stake's owner.
    pub fn owner(&self) -> PoolAccount {
        match self {
            DepositKind::Swap { recipient, .. } => PoolAccount::Icp(*recipient),
            DepositKind::Stake { owner } => *owner,
        }
    }

    pub fn is_swap(&self) -> bool {
        matches!(self, DepositKind::Swap { .. })
    }
}

/// How the USDC reaches the canister's Base address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum PoolRail {
    /// A CCTP Deliver into Base, its mint recipient the canister.
    #[n(0)]
    Cctp,
    /// An Eco intent whose route delivers to the canister.
    #[n(1)]
    Eco,
    /// An order on Base whose payout is the canister.
    #[n(2)]
    SameChain,
}

/// `keccak256("swapic.pool.v1.swap")`.
pub fn tag_swap() -> [u8; 32] {
    keccak256(b"swapic.pool.v1.swap").0
}

/// `keccak256("swapic.pool.v1.stake")`.
pub fn tag_stake() -> [u8; 32] {
    keccak256(b"swapic.pool.v1.stake").0
}

/// A u128 as a 32-byte big-endian word.
fn word(value: u128) -> [u8; 32] {
    let mut out = [0; 32];
    out[16..].copy_from_slice(&value.to_be_bytes());
    out
}

/// The interface's `owner`: `keccak256(0x02 ‖ u8(len) ‖ principal ‖ subaccount32)` for an
/// ICP account, `keccak256(0x01 ‖ address20)` for an EVM one.
pub fn owner_hash(account: &PoolAccount) -> OwnerKey {
    let mut preimage = Vec::with_capacity(64);
    match account {
        PoolAccount::Icp(icp) => {
            let bytes = icp.owner.as_slice();
            preimage.push(0x02);
            preimage.push(u8::try_from(bytes.len()).expect("BUG: a principal is 29 bytes at most"));
            preimage.extend_from_slice(bytes);
            preimage.extend_from_slice(&icp.subaccount_bytes());
        }
        PoolAccount::Evm(address) => {
            preimage.push(0x01);
            preimage.extend_from_slice(address.as_bytes());
        }
    }
    OwnerKey::new(keccak256(&preimage).0)
}

/// The interface's `details`: a swap's token out, its minimum and its premium; a stake's
/// rebalancing share. `fee` is the premium for a swap and the rebalancing share for a stake,
/// so the ref commits the fee.
pub fn details_hash(kind: &DepositKind, fee: u128) -> [u8; 32] {
    match kind {
        DepositKind::Swap {
            token_out, min_out, ..
        } => {
            let bytes = token_out.as_slice();
            let mut preimage = Vec::with_capacity(1 + bytes.len() + 64);
            preimage.push(u8::try_from(bytes.len()).expect("BUG: a principal is 29 bytes at most"));
            preimage.extend_from_slice(bytes);
            preimage.extend_from_slice(&word(*min_out));
            preimage.extend_from_slice(&word(fee));
            keccak256(&preimage).0
        }
        DepositKind::Stake { .. } => keccak256(word(fee)).0,
    }
}

/// The interface's `ref = keccak256(tag ‖ owner ‖ details ‖ salt32)`.
pub fn pool_ref(kind: &DepositKind, fee: u128, salt: [u8; 32]) -> PoolRef {
    let tag = match kind {
        DepositKind::Swap { .. } => tag_swap(),
        DepositKind::Stake { .. } => tag_stake(),
    };
    let mut preimage = Vec::with_capacity(128);
    preimage.extend_from_slice(&tag);
    preimage.extend_from_slice(owner_hash(&kind.owner()).as_ref());
    preimage.extend_from_slice(&details_hash(kind, fee));
    preimage.extend_from_slice(&salt);
    PoolRef::new(keccak256(&preimage).0)
}

/// Share units minted per micro USDC at the first mint: 18 decimals of shares.
pub const FIRST_MINT_SCALE: u128 = 1_000_000_000_000;

/// The pool's shares, in 18-decimal units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct Shares(#[n(0)] u128);

impl Shares {
    pub const ZERO: Self = Self(0);

    pub const fn new(units: u128) -> Self {
        Self(units)
    }

    pub const fn get(self) -> u128 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    /// The shares a stake of `amount` mints into a pool of `assets` and `shares`: at the
    /// first mint `amount * 10^12`, after it `floor(amount * S / A)`, rounded in the pool's
    /// favor. None on an overflow, on assets of zero while shares exist, and on a mint of
    /// nothing.
    pub fn mint_for(amount: TokenAmount, assets: TokenAmount, shares: Shares) -> Option<Shares> {
        let minted = if shares.0 == 0 {
            amount.checked_mul(FIRST_MINT_SCALE)?
        } else {
            if assets == TokenAmount::ZERO {
                return None;
            }
            amount
                .checked_mul(shares.0)?
                .checked_div_floor(assets.into_inner())?
        };
        let minted = minted.try_into_u128()?;
        (minted > 0).then_some(Shares(minted))
    }

    /// The shares a withdrawal of `usdc` burns: `ceil(usdc * S / A)`, rounded in the pool's
    /// favor. None when `usdc` is above the assets, when there are no assets, and on a burn
    /// of nothing (no usdc, or no shares).
    pub fn burn_for(usdc: TokenAmount, assets: TokenAmount, shares: Shares) -> Option<Shares> {
        if assets == TokenAmount::ZERO || usdc > assets {
            return None;
        }
        let burned = usdc
            .checked_mul(shares.0)?
            .checked_div_ceil(assets.into_inner())?
            .try_into_u128()?;
        (burned > 0).then_some(Shares(burned))
    }

    /// What `owned` shares are worth: `floor(owned * A / S)`, nothing while no shares exist.
    pub fn value_of(owned: Shares, assets: TokenAmount, shares: Shares) -> TokenAmount {
        if shares.0 == 0 {
            return TokenAmount::ZERO;
        }
        assets
            .checked_mul(owned.0)
            .and_then(|scaled| scaled.checked_div_floor(shares.0))
            .unwrap_or(TokenAmount::ZERO)
    }
}

impl std::fmt::Display for Shares {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One pocket's lines, in micro USDC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Pocket {
    /// Held and promised to nothing.
    #[n(0)]
    pub free: TokenAmount,
    /// Held and promised to a swap or a withdrawal in progress.
    #[n(1)]
    pub reserved: TokenAmount,
    /// Left this pocket and not yet arrived in the other (a rebalance, P11c).
    #[n(2)]
    pub in_flight: TokenAmount,
}

impl Pocket {
    pub fn total(&self) -> TokenAmount {
        [self.reserved, self.in_flight]
            .into_iter()
            .try_fold(self.free, TokenAmount::checked_add)
            .unwrap_or(TokenAmount::MAX)
    }
}

/// The pool's two pockets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Pockets {
    /// USDC at the canister's address on the pool chain (Base).
    #[n(0)]
    pub base: Pocket,
    /// ckUSDC in the canister's own account on ICP.
    #[n(1)]
    pub icp: Pocket,
}

impl Pockets {
    /// All six lines.
    pub fn assets(&self) -> TokenAmount {
        self.base
            .total()
            .checked_add(self.icp.total())
            .unwrap_or(TokenAmount::MAX)
    }
}

/// An ICP token a swap may pay out, and the ICPSwap pool it is bought through from ckUSDC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct IcpToken {
    #[cbor(n(0), with = "principal_cbor")]
    pub ledger: Principal,
    #[cbor(n(1), with = "principal_cbor")]
    pub icpswap_pool: Principal,
}

/// An ICRC ledger's transfer fee, as read when the settings were set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct LedgerFee {
    #[cbor(n(0), with = "principal_cbor")]
    pub ledger: Principal,
    #[n(1)]
    pub fee: TokenAmount,
}

/// The highest premium the settings take: 10%.
pub const MAX_PREMIUM_BPS: u16 = 1_000;

/// The pool's knobs (interface section 3's `PoolSettings`), and the ledger fees read when
/// they were set.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PoolSettings {
    /// The most the pool holds, micro USDC.
    #[n(0)]
    pub tvl_cap: TokenAmount,
    /// The most one swap may move, micro USDC.
    #[n(1)]
    pub per_swap_cap: TokenAmount,
    /// The least premium of a fast swap, of its `min_usdc`.
    #[n(2)]
    pub premium_bps: u16,
    #[cbor(n(3), with = "principal_cbor")]
    pub ckusdc_ledger: Principal,
    /// The tokens a swap may pay out besides ckUSDC. A ckUSDC entry names the ckUSDC ledger
    /// itself as its pool.
    #[n(4)]
    pub icp_tokens: Vec<IcpToken>,
    /// How long past `expires_at` an arrival is still credited.
    #[n(5)]
    pub deposit_grace: Duration,
    /// Open registrations per `src_payer`, and per owner.
    #[n(6)]
    pub max_open_per_payer: u32,
    /// The most of the ICP pocket held for open swaps at once.
    #[n(7)]
    pub max_held_bps: u16,
    /// How often the canister reads Base itself while registrations wait.
    #[n(8)]
    pub arrival_backup: Duration,
    #[n(9)]
    pub open_for_swaps: bool,
    /// The ckUSDC ledger's fee and every listed token's, read by `set_pool_settings`.
    #[n(10)]
    pub ledger_fees: Vec<LedgerFee>,
    /// The routes every ICP path reads from P11b on (`set_icp_routes`): while none are set,
    /// `icp_tokens` is the direct list it reads.
    #[n(11)]
    pub routes: Option<IcpRoutes>,
}

/// The mainnet ckUSDC ledger.
pub const CKUSDC_LEDGER: &str = "xevnm-gaaaa-aaaar-qafnq-cai";

impl Default for PoolSettings {
    /// The launch values (spec Q3 and the interface's defaults), the pool closed for swaps.
    fn default() -> Self {
        Self {
            tvl_cap: TokenAmount::new(10_000_000_000),
            per_swap_cap: TokenAmount::new(1_000_000_000),
            premium_bps: 10,
            ckusdc_ledger: Principal::from_text(CKUSDC_LEDGER).expect("BUG: a valid principal"),
            icp_tokens: Vec::new(),
            deposit_grace: Duration::from_secs(120),
            max_open_per_payer: 3,
            max_held_bps: 5_000,
            arrival_backup: Duration::from_secs(300),
            open_for_swaps: false,
            ledger_fees: Vec::new(),
            routes: None,
        }
    }
}

/// Why settings are not ones the pool runs on.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PoolSettingsError {
    #[error("{0} must be above zero")]
    Zero(&'static str),
    #[error("the per-swap cap is above the TVL cap")]
    SwapCapAboveTvlCap,
    #[error("a premium of {0} bps is above the ceiling of {MAX_PREMIUM_BPS}")]
    PremiumTooHigh(u16),
    #[error("max_held_bps of {0} is above 10000")]
    HeldAboveAll(u16),
    #[error("the ckUSDC entry must name the ckUSDC ledger itself as its pool")]
    CkusdcPool,
    #[error("{0} is listed twice")]
    Duplicate(Principal),
    #[error("{0} names no pool of its own")]
    BadPool(Principal),
    #[error("no fee was read for {0}")]
    MissingFee(Principal),
    #[error(transparent)]
    Routes(#[from] RoutesError),
}

impl PoolSettings {
    /// The knobs hold together: caps above zero and the swap cap within the TVL cap, the
    /// premium at most 10%, the held share at most all of the pocket, the ckUSDC entry (if
    /// any) with its own ledger as its pool, every other token with a pool that is neither
    /// its own ledger nor ckUSDC's, no ledger twice.
    pub fn validate(&self) -> Result<(), PoolSettingsError> {
        if self.tvl_cap == TokenAmount::ZERO {
            return Err(PoolSettingsError::Zero("tvl_cap"));
        }
        if self.per_swap_cap == TokenAmount::ZERO {
            return Err(PoolSettingsError::Zero("per_swap_cap"));
        }
        if self.per_swap_cap > self.tvl_cap {
            return Err(PoolSettingsError::SwapCapAboveTvlCap);
        }
        if self.premium_bps > MAX_PREMIUM_BPS {
            return Err(PoolSettingsError::PremiumTooHigh(self.premium_bps));
        }
        if self.max_held_bps > 10_000 {
            return Err(PoolSettingsError::HeldAboveAll(self.max_held_bps));
        }
        if self.max_held_bps == 0 {
            return Err(PoolSettingsError::Zero("max_held_bps"));
        }
        if self.max_open_per_payer == 0 {
            return Err(PoolSettingsError::Zero("max_open_per_payer"));
        }
        if self.arrival_backup.is_zero() {
            return Err(PoolSettingsError::Zero("arrival_backup_s"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for token in &self.icp_tokens {
            if !seen.insert(token.ledger) {
                return Err(PoolSettingsError::Duplicate(token.ledger));
            }
            if token.ledger == self.ckusdc_ledger {
                if token.icpswap_pool != self.ckusdc_ledger {
                    return Err(PoolSettingsError::CkusdcPool);
                }
            } else if token.icpswap_pool == token.ledger || token.icpswap_pool == self.ckusdc_ledger
            {
                return Err(PoolSettingsError::BadPool(token.ledger));
            }
        }
        if let Some(routes) = &self.routes {
            routes.validate(self.ckusdc_ledger)?;
        }
        Ok(())
    }

    /// A fee was read for ckUSDC and for every listed token.
    pub fn fees_complete(&self) -> Result<(), PoolSettingsError> {
        for ledger in self.ledgers() {
            if self.fee_of(ledger).is_none() {
                return Err(PoolSettingsError::MissingFee(ledger));
            }
        }
        Ok(())
    }

    /// ckUSDC, then every listed token that is not ckUSDC.
    pub fn ledgers(&self) -> Vec<Principal> {
        let mut out = vec![self.ckusdc_ledger];
        out.extend(
            self.icp_tokens
                .iter()
                .map(|token| token.ledger)
                .filter(|ledger| *ledger != self.ckusdc_ledger),
        );
        out
    }

    /// The fee read for `ledger`: by the settings, or with the routes.
    pub fn fee_of(&self, ledger: Principal) -> Option<TokenAmount> {
        self.ledger_fees
            .iter()
            .find(|fee| fee.ledger == ledger)
            .map(|fee| fee.fee)
            .or_else(|| self.routes.as_ref()?.fee_of(ledger))
    }

    /// The ICPSwap pool `token_out` is bought through directly, None for ckUSDC, for a token
    /// routed through ICP, and for a token not listed.
    pub fn pool_of(&self, token_out: Principal) -> Option<Principal> {
        match self.path_of(token_out)? {
            RoutePath::Direct { pool } => Some(pool),
            RoutePath::Ckusdc | RoutePath::ViaIcp { .. } => None,
        }
    }

    /// The path `token` takes to or from ckUSDC: the routes once set, the direct list before;
    /// None for a token not listed.
    pub fn path_of(&self, token: Principal) -> Option<RoutePath> {
        if token == self.ckusdc_ledger {
            return Some(RoutePath::Ckusdc);
        }
        match &self.routes {
            Some(routes) => routes.path_of(token, self.ckusdc_ledger),
            None => self
                .icp_tokens
                .iter()
                .find(|listed| listed.ledger == token)
                .map(|listed| RoutePath::Direct {
                    pool: listed.icpswap_pool,
                }),
        }
    }

    /// Whether a swap may pay out `token_out`: ckUSDC always, another token when listed.
    pub fn is_listed(&self, token_out: Principal) -> bool {
        self.path_of(token_out).is_some()
    }
}

/// A deposit's terms as the quoter registers them, before anything arrived.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Registration {
    #[n(0)]
    pub kind: DepositKind,
    /// The micro USDC that must reach the canister's Base address.
    #[n(1)]
    pub min_usdc: TokenAmount,
    /// A swap's premium; zero for a stake.
    #[n(2)]
    pub premium: TokenAmount,
    /// A stake's rebalancing share; zero for a swap.
    #[n(3)]
    pub rebalance_share: TokenAmount,
    #[n(4)]
    pub src_chain: ChainId,
    /// Who pays the order: screened, and where an orphan goes back to.
    #[n(5)]
    pub src_payer: EvmAddress,
    #[n(6)]
    pub rail: PoolRail,
    /// After this, plus the grace, an arrival for it is unattributed.
    #[n(7)]
    pub expires_at: Timestamp,
    #[cbor(n(8), with = "minicbor::bytes")]
    pub salt: [u8; 32],
}

impl Registration {
    /// The fee the ref commits: the premium for a swap, the rebalancing share for a stake.
    pub fn fee(&self) -> TokenAmount {
        if self.kind.is_swap() {
            self.premium
        } else {
            self.rebalance_share
        }
    }

    /// The ref these terms commit to, None for a fee no ref holds.
    pub fn pool_ref(&self) -> Option<PoolRef> {
        Some(pool_ref(&self.kind, self.fee().try_into_u128()?, self.salt))
    }

    /// What a swap's payout will need of the ICP pocket at the least, `min_usdc - premium`:
    /// what the registration checks is free (it holds none of it). A stake needs nothing on
    /// ICP.
    pub fn reserve(&self) -> TokenAmount {
        if self.kind.is_swap() {
            self.min_usdc
                .checked_sub(self.premium)
                .unwrap_or(TokenAmount::ZERO)
        } else {
            TokenAmount::ZERO
        }
    }

    /// The keys the open-deposit limit counts against: the payer, and the owner when it is
    /// another account.
    pub fn parties(&self) -> Vec<OwnerKey> {
        let payer = PoolAccount::Evm(self.src_payer).key();
        let owner = self.kind.owner().key();
        if payer == owner {
            vec![payer]
        } else {
            vec![payer, owner]
        }
    }

    /// The last instant an arrival for it is still credited.
    pub fn credit_until(&self, grace: Duration) -> Timestamp {
        self.expires_at
            .checked_add(grace)
            .unwrap_or(Timestamp::from_nanos(u64::MAX))
    }
}

/// Where a deposit stands.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum DepositStatus {
    #[n(0)]
    Registered,
    #[n(1)]
    Arrived,
    #[n(2)]
    Paying,
    #[n(3)]
    Paid,
    #[n(4)]
    Unattributed,
    #[n(5)]
    Expired,
    #[n(6)]
    PayoutFailed(#[n(0)] String),
}

/// One USDC transfer to the canister, as a chain logged it: the key it is credited under,
/// at most once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct CreditKey {
    #[n(0)]
    pub chain: ChainId,
    #[n(1)]
    pub tx: TxHash,
    #[n(2)]
    pub log_index: u64,
}

impl CreditKey {
    const BYTES: usize = 8 + 32 + 8;
}

/// The chain id, the hash and the log index, big endian, so a stable set orders by them.
impl Storable for CreditKey {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut out = Vec::with_capacity(Self::BYTES);
        out.extend_from_slice(&self.chain.get().to_be_bytes());
        out.extend_from_slice(self.tx.as_ref());
        out.extend_from_slice(&self.log_index.to_be_bytes());
        Cow::Owned(out)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        assert_eq!(
            bytes.len(),
            Self::BYTES,
            "BUG: a stored credit key is 48 bytes"
        );
        let chain = u64::from_be_bytes(bytes[..8].try_into().expect("BUG: eight bytes"));
        let tx: [u8; 32] = bytes[8..40].try_into().expect("BUG: thirty-two bytes");
        let log_index = u64::from_be_bytes(bytes[40..].try_into().expect("BUG: eight bytes"));
        Self {
            chain: ChainId::new(chain),
            tx: TxHash::new(tx),
            log_index,
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: Self::BYTES as u32,
        is_fixed_size: true,
    };
}

/// The transfer a deposit was credited for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Arrival {
    #[n(0)]
    pub credit: CreditKey,
    #[n(1)]
    pub amount: TokenAmount,
    #[n(2)]
    pub block: BlockNumber,
}

/// How a payout was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Paid {
    /// The token paid.
    #[cbor(n(0), with = "principal_cbor")]
    pub token: Principal,
    /// What the recipient got of it.
    #[n(1)]
    pub amount: TokenAmount,
    /// The ledger block of the transfer.
    #[n(2)]
    pub block: u64,
    /// The ckUSDC that left the ICP pocket for it, every ledger fee included.
    #[n(3)]
    pub spent: TokenAmount,
}

/// How a payout pays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PayoutPath {
    /// A ckUSDC transfer.
    #[n(0)]
    Ckusdc,
    /// ckUSDC swapped on `pool` into the token out, then the token's transfer.
    #[n(1)]
    Icpswap {
        #[cbor(n(0), with = "principal_cbor")]
        pool: Principal,
    },
    /// ckUSDC swapped into ICP on `first` (the ICP/ckUSDC pool), the ICP into the token out
    /// on `second`, then the token's transfer; a second leg that cannot pay swaps the ICP
    /// back to ckUSDC on `first`, and all of it is paid as ckUSDC.
    #[n(2)]
    IcpswapVia {
        #[cbor(n(0), with = "principal_cbor")]
        first: Principal,
        #[cbor(n(1), with = "principal_cbor")]
        second: Principal,
        #[cbor(n(2), with = "principal_cbor")]
        icp: Principal,
    },
}

impl PayoutPath {
    /// Whether it swaps on ICPSwap.
    pub fn swaps(&self) -> bool {
        !matches!(self, PayoutPath::Ckusdc)
    }
}

/// Which leg of a path a swap step names: the first (none, for a step from before legs were
/// named), the second, or the way back.
pub const LEG_SECOND: u8 = 1;
pub const LEG_BACK: u8 = 2;

/// A principal stored as its raw bytes, for a field that may be absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct PrincipalBytes(#[cbor(n(0), with = "principal_cbor")] pub Principal);

/// A payout's deduplication pair, its path, and the exact transfer it makes, recorded before
/// any call: every ICRC transfer of the payout carries this `created_at_time` and memo, and
/// this amount of this token, so a retried transfer whose first answer was lost is the same
/// transfer to the ledger, whatever the settings say by then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PayoutAttempt {
    #[n(0)]
    pub created_at: Timestamp,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub memo: [u8; 32],
    #[n(2)]
    pub path: PayoutPath,
    /// The ckUSDC path's transfer amount, or the ICPSwap path's amount in. None on an
    /// attempt recorded before it was, which the driver works out as it did then.
    #[n(3)]
    pub amount: Option<TokenAmount>,
    /// The token the path's first call moves (ckUSDC on both paths).
    #[n(4)]
    pub token: Option<PrincipalBytes>,
}

/// Where an ICPSwap leg stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct SwapLeg {
    /// The pool's quote read below the minimum: once is a retry, twice falls back.
    #[n(0)]
    pub declined: u8,
    /// `depositFromAndSwap` was called. Its outcome is known only by its answer, so a begun
    /// swap with no output is never called again (a human looks).
    #[n(1)]
    pub begun: bool,
    /// What the swap answered it paid into the canister's account.
    #[n(2)]
    pub out: Option<TokenAmount>,
    /// The output's transfer to the recipient, fixed when the output was recorded.
    #[n(3)]
    pub send: Option<TokenAmount>,
    /// The approvals given the pool, each a ckUSDC fee the payout spent. None on a leg from
    /// before they were counted, which made one.
    #[n(4)]
    pub approvals: Option<u32>,
    /// A swap that did not happen, undone: what it cost the canister's ckUSDC account in
    /// the end (its deposit, the pull's fee included, less what was withdrawn back).
    #[n(5)]
    pub undone: Option<TokenAmount>,
    /// What the pool held of the canister's just before the swap: what is there above it
    /// is this leg's.
    #[n(6)]
    pub before: Option<PoolHeld>,
    /// The ckUSDC withdrawn back from the pool when the swap was undone (for a leg after the
    /// first, its token in).
    #[n(7)]
    pub withdrawn: Option<TokenAmount>,
    /// What the leg deposited, its pull's fee aside, recorded when it began.
    #[n(8)]
    pub amount_in: Option<TokenAmount>,
}

/// What an ICPSwap pool holds of the canister's, unused: ckUSDC and the leg's token. For a
/// leg after the first, `ckusdc` holds what it sells and `token` what it buys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct PoolHeld {
    #[n(0)]
    pub ckusdc: TokenAmount,
    #[n(1)]
    pub token: TokenAmount,
}

/// A payout in progress: its dedup pair and path, its ICPSwap leg, and its failures.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct PayoutProgress {
    #[n(0)]
    pub attempt: Option<PayoutAttempt>,
    #[n(1)]
    pub swap: SwapLeg,
    /// Failed drives so far; at [`MAX_PAYOUT_ATTEMPTS`] the payout stops for a human.
    #[n(2)]
    pub failures: u32,
    /// Why the ICPSwap leg fell back to ckUSDC.
    #[n(3)]
    pub fell_back: Option<String>,
    /// The last failure, once the payout stopped for a human.
    #[n(4)]
    pub failed: Option<String>,
    /// A path through ICP: its second leg (ICP into the token out).
    #[n(5)]
    pub second: Option<SwapLeg>,
    /// A path through ICP whose second leg could not pay: the ICP swapped back to ckUSDC.
    #[n(6)]
    pub back: Option<SwapLeg>,
}

impl PayoutProgress {
    /// The leg a swap step names.
    pub fn leg(&self, leg: Option<u8>) -> SwapLeg {
        match leg {
            Some(LEG_SECOND) => self.second.unwrap_or_default(),
            Some(LEG_BACK) => self.back.unwrap_or_default(),
            _ => self.swap,
        }
    }

    /// The leg a swap step names, to change.
    pub fn leg_mut(&mut self, leg: Option<u8>) -> &mut SwapLeg {
        match leg {
            Some(LEG_SECOND) => self.second.get_or_insert_with(SwapLeg::default),
            Some(LEG_BACK) => self.back.get_or_insert_with(SwapLeg::default),
            _ => &mut self.swap,
        }
    }
}

/// A deposit as the fold holds it: its terms and how far it got.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Registered {
    #[n(0)]
    pub terms: Registration,
    #[n(1)]
    pub status: DepositStatus,
    #[n(2)]
    pub arrived: Option<Arrival>,
    #[n(3)]
    pub paid: Option<Paid>,
    #[n(4)]
    pub minted: Option<Shares>,
    #[n(5)]
    pub payout: PayoutProgress,
    /// What the deposit's payout holds once it arrived: its whole payout and every ledger fee
    /// its path can cost, queued until the ICP pocket's free ckUSDC covers it, then held there
    /// from its `PayoutStarted` until it is paid. None before it arrived: a registration holds
    /// nothing (Jeff, 2026-10-21).
    #[n(6)]
    pub held: Option<TokenAmount>,
}

impl Registered {
    pub fn new(terms: Registration) -> Self {
        Self {
            terms,
            status: DepositStatus::Registered,
            arrived: None,
            paid: None,
            minted: None,
            payout: PayoutProgress::default(),
            held: None,
        }
    }

    /// What the deposit's payout takes of the ICP pocket: nothing before it arrived, its
    /// whole payout and fees after.
    pub fn held(&self) -> TokenAmount {
        self.held.unwrap_or(TokenAmount::ZERO)
    }

    /// What the deposit owes its owner once it arrived: a swap's recipient everything less
    /// the premium; a stake's owner the whole arrival, which its mint is priced without.
    pub fn owed(&self) -> TokenAmount {
        let Some(arrival) = self.arrived else {
            return TokenAmount::ZERO;
        };
        if self.terms.kind.is_swap() {
            arrival
                .amount
                .checked_sub(self.terms.premium)
                .unwrap_or(TokenAmount::ZERO)
        } else {
            arrival.amount
        }
    }
}

crate::storable_as_cbor!(Registered);

/// A share holder's line: the account and its shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct ShareLine {
    #[n(0)]
    pub account: PoolAccount,
    #[n(1)]
    pub shares: Shares,
}

crate::storable_as_cbor!(ShareLine);

/// An operation started on ICP and not finished: a stake whose pull is out, or a withdrawal
/// whose payout is. Held by value in the ledger's map, a handful at a time, so the
/// withdrawal's size is no cost worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PendingOp {
    #[n(0)]
    Stake {
        #[n(0)]
        owner: PoolAccount,
        /// The caller's account the ckUSDC is pulled from.
        #[n(1)]
        from: IcpAccount,
        #[n(2)]
        amount: TokenAmount,
        /// The pull's dedup pair: `created_at_time` this, memo the op id.
        #[n(3)]
        created_at: Timestamp,
    },
    #[n(1)]
    Withdraw {
        #[n(0)]
        owner: IcpAccount,
        #[n(1)]
        burned: Shares,
        /// The ckUSDC the shares were worth, reserved on ICP: the payout's whole budget,
        /// ledger fees included.
        #[n(2)]
        usdc: TokenAmount,
        #[n(3)]
        to: IcpAccount,
        #[cbor(n(4), with = "principal_cbor")]
        token_out: Principal,
        #[n(5)]
        min_out: u128,
        #[n(6)]
        payout: PayoutProgress,
    },
}

/// The arrived swaps whose payouts wait for the ICP pocket's free ckUSDC, first in first out,
/// each with what it holds once it starts. Stored as the list, and as nothing at all when it
/// is empty, so a ledger written before it existed reads back with it empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PayoutQueue(pub Vec<(PoolRef, TokenAmount)>);

impl PayoutQueue {
    /// What every queued payout will hold.
    pub fn total(&self) -> TokenAmount {
        self.0
            .iter()
            .try_fold(TokenAmount::ZERO, |sum, (_, held)| sum.checked_add(*held))
            .unwrap_or(TokenAmount::MAX)
    }

    pub fn head(&self) -> Option<(PoolRef, TokenAmount)> {
        self.0.first().copied()
    }
}

impl<C> Encode<C> for PayoutQueue {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'b, C> Decode<'b, C> for PayoutQueue {
    fn decode(d: &mut minicbor::Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the list absent in the middle reads
        // back, and it means the same as the list missing off the end
        if matches!(
            d.datatype()?,
            minicbor::data::Type::Null | minicbor::data::Type::Undefined
        ) {
            d.skip()?;
            return Ok(Self::default());
        }
        Vec::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::default())
    }
}

/// A pool arrival (or an unattributed transfer) from a block no balance check has read at
/// yet: in the lines, and not yet in a balance read below its block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct UnseenArrival {
    #[n(0)]
    pub chain: ChainId,
    #[n(1)]
    pub block: BlockNumber,
    #[n(2)]
    pub amount: TokenAmount,
}

/// The ICP pocket's last balance check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct IcpCheck {
    #[n(0)]
    pub observed: TokenAmount,
    #[n(1)]
    pub expected: TokenAmount,
    #[n(2)]
    pub shortfall: bool,
}

/// Why the ICP pocket's check read no balance: a fixed kind, never a callee's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum IcpReadFailure {
    /// The pool has no settings, so no ledger to read.
    #[n(0)]
    NoSettings,
    /// The ledger call failed or was refused.
    #[n(1)]
    CallFailed,
    /// The answer is no balance the pool's lines hold (wider than 128 bits).
    #[n(2)]
    BadAnswer,
}

/// One step of the pool, as the log records it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PoolStep {
    /// A controller set the pool's knobs, with the ledger fees read for them.
    #[n(0)]
    SettingsSet {
        #[n(0)]
        settings: PoolSettings,
    },
    /// The quoter registered a deposit at the user's commit.
    #[n(1)]
    Registered {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        deposit: Registration,
    },
    /// A registration ran out with nothing arrived: its hold is released.
    #[n(2)]
    Expired {
        #[n(0)]
        r#ref: PoolRef,
    },
    /// The canister read a transfer to its Base address proving this deposit.
    #[n(3)]
    Arrived {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        credit: CreditKey,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: BlockNumber,
    },
    /// A transfer to the canister that proves no deposit it can credit: kept for the
    /// orphan refund (P11b) with its payer when known.
    #[n(4)]
    Unattributed {
        #[n(0)]
        credit: CreditKey,
        #[n(1)]
        amount: TokenAmount,
        #[n(2)]
        src_payer: Option<EvmAddress>,
        #[n(3)]
        why: String,
        #[n(4)]
        block: BlockNumber,
        /// The registration it named, if any: a late or short arrival closes it.
        #[n(5)]
        r#ref: Option<PoolRef>,
    },
    /// A swap's payout began: its dedup pair and path, before any call.
    #[n(5)]
    PayoutStarted {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        attempt: PayoutAttempt,
    },
    /// A swap's recipient was paid.
    #[n(6)]
    Paid {
        #[n(0)]
        r#ref: PoolRef,
        #[cbor(n(1), with = "principal_cbor")]
        token: Principal,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: u64,
        /// The ckUSDC that left the ICP pocket, every ledger fee included.
        #[n(4)]
        spent: TokenAmount,
    },
    /// The ICPSwap leg could not reach the minimum: all of it is paid as ckUSDC instead.
    /// `r#ref` names a deposit or a withdrawal.
    #[n(7)]
    FellBack {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
        /// The ckUSDC transfer that pays all of it, fixed now for every retry.
        #[n(2)]
        amount: Option<TokenAmount>,
        #[n(3)]
        token: Option<PrincipalBytes>,
    },
    /// A stake's shares were minted.
    #[n(8)]
    Minted {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        shares: Shares,
    },
    /// An ICP stake's pull landed and its shares were minted.
    #[n(9)]
    StakedIcp {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        rebalance_share: TokenAmount,
        #[n(4)]
        shares: Shares,
        #[n(5)]
        block: u64,
    },
    /// An ICP withdrawal was paid.
    #[n(10)]
    WithdrawnIcp {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: IcpAccount,
        #[n(2)]
        burned: Shares,
        #[n(3)]
        usdc: TokenAmount,
        #[cbor(n(4), with = "principal_cbor")]
        token: Principal,
        #[n(5)]
        paid: TokenAmount,
        #[n(6)]
        block: u64,
        #[n(7)]
        spent: TokenAmount,
    },
    /// A payout's drive failed and will be tried again, until [`MAX_PAYOUT_ATTEMPTS`].
    /// `r#ref` names a deposit or a withdrawal.
    #[n(11)]
    AttemptFailed {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
    },
    /// The pool's quote was below the minimum: tried once more on the next drive.
    #[n(12)]
    SwapDeclined {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
        /// The leg it names; none for the first.
        #[n(2)]
        leg: Option<u8>,
    },
    /// `depositFromAndSwap` is about to be called.
    #[n(13)]
    SwapBegun {
        #[n(0)]
        r#ref: PoolRef,
        /// What the pool held of the canister's just before, read while this leg alone was
        /// at the pool: only what is there above it is this leg's. None on a leg from before
        /// it was read.
        #[n(1)]
        before: Option<PoolHeld>,
        /// The leg it names; none for the first.
        #[n(2)]
        leg: Option<u8>,
        /// What the leg deposits, its pull's fee aside. None on a leg from before it was
        /// recorded, whose payout's attempt holds it.
        #[n(3)]
        amount_in: Option<TokenAmount>,
    },
    /// `depositFromAndSwap` answered its output.
    #[n(14)]
    Swapped {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        out: TokenAmount,
        /// The output's transfer to the recipient, fixed now for every retry.
        #[n(2)]
        send: Option<TokenAmount>,
        /// The leg it names; none for the first.
        #[n(3)]
        leg: Option<u8>,
    },
    /// An ICP stake's pull is about to be asked: its TVL room is held and its dedup pair set.
    #[n(15)]
    StakeStarted {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        from: IcpAccount,
        #[n(3)]
        amount: TokenAmount,
        #[n(4)]
        created_at: Timestamp,
    },
    /// The ledger refused an ICP stake's pull: nothing moved, the room is released.
    #[n(16)]
    StakeFailed {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        why: String,
    },
    /// An ICP withdrawal burned its shares and reserved its ckUSDC, before any call.
    #[n(17)]
    WithdrawStarted {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: IcpAccount,
        #[n(2)]
        burned: Shares,
        #[n(3)]
        usdc: TokenAmount,
        #[n(4)]
        to: IcpAccount,
        #[cbor(n(5), with = "principal_cbor")]
        token_out: Principal,
        #[n(6)]
        min_out: u128,
        #[n(7)]
        attempt: PayoutAttempt,
    },
    /// The ICP pocket's balance check.
    #[n(18)]
    IcpChecked {
        #[n(0)]
        check: IcpCheck,
    },
    /// The ICP pocket's balance check read no balance, and why: it passes nothing, and the
    /// pool's guard sees the pocket's check go stale.
    #[n(19)]
    IcpCheckFailed {
        #[n(0)]
        failure: IcpReadFailure,
    },
    /// The canister's own read of Base is done through this block: all of it, or with `log`
    /// through that log of it (the read goes on in the same block).
    #[n(20)]
    ScanCursor {
        #[n(0)]
        block: BlockNumber,
        #[n(1)]
        log: Option<u64>,
    },
    /// The ICPSwap leg's pool was approved for its amount in: one more ckUSDC fee spent.
    #[n(21)]
    SwapApproved {
        #[n(0)]
        r#ref: PoolRef,
        /// The leg it names; none for the first.
        #[n(1)]
        leg: Option<u8>,
    },
    /// A swap that did not happen was undone: the ckUSDC it deposited, if any, withdrawn back
    /// from the pool. `cost` is what its deposit took from the canister's ckUSDC account in
    /// the end (zero when the deposit never happened). All of it is paid as ckUSDC next.
    #[n(22)]
    SwapUndone {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        cost: TokenAmount,
        /// The ckUSDC withdrawn from the pool: this leg's own, never more than it deposited.
        #[n(2)]
        withdrawn: TokenAmount,
        /// The leg it names; none for the first.
        #[n(3)]
        leg: Option<u8>,
    },
    /// A controller set the Base payouts' knobs.
    #[n(23)]
    PayoutSettingsSet {
        #[n(0)]
        settings: PayoutSettings,
    },
    /// A payout from the Base pocket was opened with what undoes it: an ICP swap before its
    /// pull, a withdrawal with its shares burned and its nonce spent.
    #[n(24)]
    PayoutOpened {
        #[n(0)]
        id: u64,
        /// Boxed: a log line's size is its largest step's.
        #[n(1)]
        kind: Box<PayoutKind>,
        #[n(2)]
        terms: PayoutTerms,
        #[n(3)]
        undo: Undo,
    },
    /// The payout's exact amount is known: what its order spends on Base. An ICP swap's
    /// ckUSDC (`received`) is held on ICP until it is paid, its network fee on the ICP fee
    /// line; a withdrawal's network fee is held on Base with it.
    #[n(25)]
    PayoutAmountKnown {
        #[n(0)]
        id: u64,
        #[n(1)]
        amount: TokenAmount,
        #[n(2)]
        received: Option<TokenAmount>,
    },
    /// The Base pocket cannot cover it behind the queue: it waits, first in first out.
    #[n(26)]
    PayoutQueued {
        #[n(0)]
        id: u64,
    },
    /// Its Base hold is reserved: it waits for an order.
    #[n(27)]
    PayoutWaiting {
        #[n(0)]
        id: u64,
    },
    /// The canister signed `run(order)` for it: the transaction's exact bytes, recorded
    /// before they are broadcast, spending the nonce `TxCreated` handed out for it.
    #[n(28)]
    PayoutSigned {
        #[n(0)]
        id: u64,
        #[n(1)]
        order: PinnedOrder,
        #[n(2)]
        chain_id: ChainId,
        #[n(3)]
        nonce: Nonce,
        #[n(4)]
        tx_hash: TxHash,
        #[cbor(n(5), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// Its transaction succeeded at depth: the USDC left the Base pocket.
    #[n(29)]
    PayoutPaid {
        #[n(0)]
        id: u64,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        block: BlockNumber,
        /// The Eco intent the order published, read off the receipt.
        #[cbor(n(3), with = "minicbor::bytes")]
        intent: Option<[u8; 32]>,
    },
    /// Its transaction reverted at depth: it waits again, its order pinned.
    #[n(30)]
    PayoutReverted {
        #[n(0)]
        id: u64,
    },
    /// Its undo began: an ICP refund is about to be asked, nothing is signed for it again.
    #[n(31)]
    PayoutUndoing {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// It was undone: an ICP swap refunded (`block`: the refund's ledger block), a
    /// withdrawal's shares minted back.
    #[n(32)]
    PayoutUndone {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
        #[n(2)]
        block: Option<u64>,
    },
    /// A controller set the ICP routes, with the ICP ledger and every route ledger's fee
    /// read for them.
    #[n(33)]
    IcpRoutesSet {
        #[n(0)]
        routes: IcpRoutes,
    },
    /// The canister signed `USDC.approve(Swapic, max)` on Base: the transaction's exact bytes,
    /// recorded before they are broadcast, spending the nonce `TxCreated` handed out for it.
    #[n(34)]
    ApproveSigned {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// An ICP swap's pull landed (`block`, its ledger block): its legs to ckUSDC run next,
    /// on the path and with the dedup pair `attempt` fixes (none for ckUSDC, which has none).
    #[n(35)]
    PayoutPulled {
        #[n(0)]
        id: u64,
        #[n(1)]
        block: u64,
        #[n(2)]
        attempt: Option<PayoutAttempt>,
    },
    /// A payout stopped for a human: what happened to its money is not known.
    #[n(36)]
    PayoutStopped {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// A waiting payout waited `order_wait` with no valid order: listed again, at the back.
    #[n(37)]
    PayoutRelisted {
        #[n(0)]
        id: u64,
    },
    /// The Eco intent a paid payout's order published went unfilled and Eco refunded its
    /// reward to its creator, the canister: the USDC is back at the canister's Base address
    /// (credited once, under its transfer's key), and the payout waits again for a new order,
    /// its first intent never to be filled.
    #[n(38)]
    PayoutRefunded {
        #[n(0)]
        id: u64,
        #[n(1)]
        credit: CreditKey,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: BlockNumber,
    },
}

impl PoolStep {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    pub fn name(&self) -> &'static str {
        match self {
            PoolStep::SettingsSet { .. } => "SettingsSet",
            PoolStep::Registered { .. } => "Registered",
            PoolStep::Expired { .. } => "Expired",
            PoolStep::Arrived { .. } => "Arrived",
            PoolStep::Unattributed { .. } => "Unattributed",
            PoolStep::PayoutStarted { .. } => "PayoutStarted",
            PoolStep::Paid { .. } => "Paid",
            PoolStep::FellBack { .. } => "FellBack",
            PoolStep::Minted { .. } => "Minted",
            PoolStep::StakedIcp { .. } => "StakedIcp",
            PoolStep::WithdrawnIcp { .. } => "WithdrawnIcp",
            PoolStep::AttemptFailed { .. } => "AttemptFailed",
            PoolStep::SwapDeclined { .. } => "SwapDeclined",
            PoolStep::SwapBegun { .. } => "SwapBegun",
            PoolStep::Swapped { .. } => "Swapped",
            PoolStep::StakeStarted { .. } => "StakeStarted",
            PoolStep::StakeFailed { .. } => "StakeFailed",
            PoolStep::WithdrawStarted { .. } => "WithdrawStarted",
            PoolStep::IcpChecked { .. } => "IcpChecked",
            PoolStep::IcpCheckFailed { .. } => "IcpCheckFailed",
            PoolStep::ScanCursor { .. } => "ScanCursor",
            PoolStep::SwapApproved { .. } => "SwapApproved",
            PoolStep::SwapUndone { .. } => "SwapUndone",
            PoolStep::PayoutSettingsSet { .. } => "PayoutSettingsSet",
            PoolStep::PayoutOpened { .. } => "PayoutOpened",
            PoolStep::PayoutAmountKnown { .. } => "PayoutAmountKnown",
            PoolStep::PayoutQueued { .. } => "PayoutQueued",
            PoolStep::PayoutWaiting { .. } => "PayoutWaiting",
            PoolStep::PayoutSigned { .. } => "PayoutSigned",
            PoolStep::PayoutPaid { .. } => "PayoutPaid",
            PoolStep::PayoutReverted { .. } => "PayoutReverted",
            PoolStep::PayoutUndoing { .. } => "PayoutUndoing",
            PoolStep::PayoutUndone { .. } => "PayoutUndone",
            PoolStep::IcpRoutesSet { .. } => "IcpRoutesSet",
            PoolStep::ApproveSigned { .. } => "ApproveSigned",
            PoolStep::PayoutPulled { .. } => "PayoutPulled",
            PoolStep::PayoutStopped { .. } => "PayoutStopped",
            PoolStep::PayoutRelisted { .. } => "PayoutRelisted",
            PoolStep::PayoutRefunded { .. } => "PayoutRefunded",
        }
    }

    /// Whether the step gives a payout something to do now: the engine drives at once.
    /// The transaction a step records the exact bytes of, if it records one: its chain, its
    /// nonce, its hash and its bytes.
    pub fn signed_tx(&self) -> Option<(ChainId, Nonce, TxHash, &[u8])> {
        match self {
            PoolStep::PayoutSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            }
            | PoolStep::ApproveSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Some((*chain_id, *nonce, *tx_hash, raw_tx)),
            _ => None,
        }
    }

    /// The purpose of the transaction a step records the bytes of: the nonce it spends must
    /// have been handed out for exactly this.
    pub fn signed_purpose(&self) -> Option<crate::events::TxPurpose> {
        match self {
            PoolStep::PayoutSigned { id, .. } => Some(crate::events::TxPurpose::PoolPayout(*id)),
            PoolStep::ApproveSigned { .. } => Some(crate::events::TxPurpose::PoolApprove),
            _ => None,
        }
    }

    pub fn moves_a_payout(&self) -> bool {
        matches!(
            self,
            PoolStep::Arrived { .. }
                | PoolStep::PayoutStarted { .. }
                | PoolStep::FellBack { .. }
                | PoolStep::Swapped { .. }
                | PoolStep::StakeStarted { .. }
                | PoolStep::WithdrawStarted { .. }
                // the Base payouts: an ICP swap's next step, a revert's retry, and USDC
                // freed on Base for the queue
                | PoolStep::PayoutPulled { .. }
                | PoolStep::PayoutUndoing { .. }
                | PoolStep::PayoutUndone { .. }
                | PoolStep::PayoutReverted { .. }
                | PoolStep::PayoutRefunded { .. }
                | PoolStep::Minted { .. }
        )
    }
}

/// The id of an ICP stake or withdrawal: `keccak256(tag ‖ owner ‖ the log index it starts
/// at)`, so no two operations share one.
pub fn op_id(withdraw: bool, owner: &PoolAccount, index: u64) -> PoolOpId {
    let tag: &[u8] = if withdraw {
        b"swapic.pool.v1.withdraw"
    } else {
        b"swapic.pool.v1.stake_icp"
    };
    let mut preimage = Vec::with_capacity(80);
    preimage.extend_from_slice(&keccak256(tag).0);
    preimage.extend_from_slice(owner.key().as_ref());
    preimage.extend_from_slice(&index.to_be_bytes());
    PoolOpId::new(keccak256(&preimage).0)
}

/// A payout's id in the steps that name either a deposit or a withdrawal.
pub fn payout_ref(id: PoolOpId) -> PoolRef {
    PoolRef::new(id.into_bytes())
}

crate::storable_as_cbor!(PoolLedger);
