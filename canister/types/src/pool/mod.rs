//! The pool (spec sections 2, 3.1, 3.3 and 4; the fixed interface section 2): its
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

pub mod callers;
pub mod ledger;
pub mod memo;
pub mod payout;
pub mod plan;
pub mod rebalance;
pub mod routes;
pub mod step;
pub mod withdrawal;

pub use routes::{IcpRoute, IcpRouteVia, IcpRoutes, PoolTokens, RoutePath, RoutesError};
pub use step::PoolStep;

pub use payout::{
    evm_payout_id, evm_payout_ref, orphan_key, EvmPayout, EvmWithdrawal, IcpSwapTerms, NonceUse,
    Orphan, OrphanStatus, PayoutKind, PayoutLines, PayoutResolution, PayoutSettings,
    PayoutSettingsError, PayoutStatus, PayoutTerms, PinnedOrder, Reclaim, Undo,
};

pub use rebalance::{
    ck_word, move_ref, rebalance_subaccount, Landing, Move, MoveDirection, MoveRoute, MoveStatus,
    OwnSource, Proposal, RebalanceLines, RebalanceSettings, RebalanceSettingsError, Sweep,
};

pub use ledger::{
    check_arrival, icp_fees_floor, premium_share, PoolEffects, PoolLedger, PoolLookup,
    PoolStepError, RegisterRefusal, MAX_PAYOUT_ATTEMPTS,
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

    /// The account identifier the ICP ledger's own blocks name this account by: the CRC-32
    /// (big endian) of the SHA-224 of `\x0Aaccount-id`, the owner and the 32 subaccount
    /// bytes, then that hash.
    pub fn identifier(&self) -> [u8; 32] {
        use sha2::{Digest, Sha224};
        let mut hasher = Sha224::new();
        hasher.update(b"\x0Aaccount-id");
        hasher.update(self.owner.as_slice());
        hasher.update(self.subaccount_bytes());
        let hash = hasher.finalize();
        let mut identifier = [0; 32];
        identifier[..4].copy_from_slice(&crc32(&hash).to_be_bytes());
        identifier[4..].copy_from_slice(&hash);
        identifier
    }
}

/// How long a ledger deduplicates a transfer: a pull asked again after it can no longer be
/// told from a new one, so the drive stops asking (a little before, for the clocks' skew).
pub const DEDUP_WINDOW: Duration = Duration::from_secs(23 * 3_600);

/// What of the ledger's window a retry must leave its next drive.
pub const RETRY_MARGIN: Duration = Duration::from_secs(3_600);

/// Whether a ledger still deduplicates a transfer whose dedup instant is `attempt_at` after a
/// retry at `now`, its next drive [`RETRY_MARGIN`] away at most: a transfer that landed is
/// then answered `Duplicate`.
pub fn deduplicated(attempt_at: Timestamp, now: Timestamp) -> bool {
    now.saturating_duration_since(attempt_at)
        .saturating_add(RETRY_MARGIN)
        < DEDUP_WINDOW
}

/// CRC-32 (IEEE 802.3), the check the ICP ledger puts before an account identifier's hash.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let low = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & low);
        }
    }
    !crc
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
    /// Left this pocket and not yet arrived in the other (a rebalance).
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
    /// The most one swap may move, micro USDC: the QUOTER's cap, which the canister only
    /// stores and shows (`pool_state`). Since the final review round the canister
    /// enforces it nowhere: an ICP swap to EVM opens while the Base pocket can pay it, an EVM to
    /// ICP deposit is credited whatever its size. Kept, as its wire field and the old
    /// `AboveSwapCap` refusal are: append-only.
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
    /// How often the canister reads Base itself, its backup of the reports.
    #[n(8)]
    pub arrival_backup: Duration,
    #[n(9)]
    pub open_for_swaps: bool,
    /// The ckUSDC ledger's fee and every listed token's, read by `set_pool_settings`.
    #[n(10)]
    pub ledger_fees: Vec<LedgerFee>,
    /// The routes every ICP path reads (`set_icp_routes`): while none are set,
    /// `icp_tokens` is the direct list it reads.
    #[n(11)]
    pub routes: Option<IcpRoutes>,
    /// A deposit whose one transfer to the canister is above this, micro USDC, is credited
    /// only once [`Self::large_deposit_confirmations`] deep; None:
    /// [`DEFAULT_LARGE_DEPOSIT_USDC`].
    #[n(12)]
    pub large_deposit_usdc: Option<TokenAmount>,
    /// The pool chain's blocks a large deposit waits before it is credited; None:
    /// [`DEFAULT_LARGE_DEPOSIT_CONFIRMATIONS`].
    #[n(13)]
    pub large_deposit_confirmations: Option<u64>,
    /// The least move fee a stake or a withdrawal carries, per direction of the move it
    /// pays for; zero until a controller sets it.
    #[n(14)]
    pub least_move_fee: MoveFees,
}

/// One amount per direction of a rebalancing move, micro USDC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct MoveFees {
    /// What one Base to ICP move costs: an EVM stake's and an ICP withdrawal's fee.
    #[n(0)]
    pub base_to_icp: TokenAmount,
    /// What one ICP to Base move costs: an ICP stake's and an EVM withdrawal's fee.
    #[n(1)]
    pub icp_to_base: TokenAmount,
}

impl MoveFees {
    /// Whether `move_fee` covers the least for the move in `direction`; the refusal names
    /// the least.
    pub fn admits(&self, direction: MoveDirection, move_fee: TokenAmount) -> Result<(), String> {
        let least = match direction {
            MoveDirection::BaseToIcp => self.base_to_icp,
            MoveDirection::IcpToBase => self.icp_to_base,
        };
        if move_fee < least {
            return Err(format!(
                "the move fee must be at least {least}, not {move_fee}"
            ));
        }
        Ok(())
    }
}

/// A deposit above this (1,000 USDC) waits more blocks before it is credited.
pub const DEFAULT_LARGE_DEPOSIT_USDC: TokenAmount = TokenAmount::new(1_000_000_000);

/// The pool chain's blocks a large deposit waits: six Base blocks, about twelve seconds.
pub const DEFAULT_LARGE_DEPOSIT_CONFIRMATIONS: u64 = 6;

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
            arrival_backup: Duration::from_secs(3_600),
            open_for_swaps: false,
            ledger_fees: Vec::new(),
            routes: None,
            large_deposit_usdc: None,
            large_deposit_confirmations: None,
            least_move_fee: MoveFees::default(),
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
        if self.large_deposit_confirmations == Some(0) {
            return Err(PoolSettingsError::Zero("large_deposit_confirmations"));
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

    /// Above what one transfer is a large deposit, and how many blocks a large deposit
    /// waits: the settings', or the defaults.
    pub fn large_deposit(&self) -> (TokenAmount, crate::BlockDepth) {
        (
            self.large_deposit_usdc
                .unwrap_or(DEFAULT_LARGE_DEPOSIT_USDC),
            crate::BlockDepth::new(
                self.large_deposit_confirmations
                    .unwrap_or(DEFAULT_LARGE_DEPOSIT_CONFIRMATIONS),
            ),
        )
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

/// A deposit's terms as its arrival registers them: what its memo committed, and the rail,
/// source chain and payer its proof showed.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Registration {
    #[n(0)]
    pub kind: DepositKind,
    /// The micro USDC that arrived for this deposit, what it is credited and judged on.
    #[n(1)]
    pub min_usdc: TokenAmount,
    /// A swap's premium; zero for a stake.
    #[n(2)]
    pub premium: TokenAmount,
    /// A stake's move fee, Swapic's; zero for a swap.
    #[n(3)]
    pub move_fee: TokenAmount,
    #[n(4)]
    pub src_chain: ChainId,
    /// Who pays the order: screened, and where an orphan goes back to.
    #[n(5)]
    pub src_payer: EvmAddress,
    #[n(6)]
    pub rail: PoolRail,
    /// When the arrival registered it.
    #[n(7)]
    pub expires_at: Timestamp,
    #[cbor(n(8), with = "minicbor::bytes")]
    pub salt: [u8; 32],
    /// A swap's ICP fees: the user's own line for its payout path's ledger fees. None for a
    /// stake.
    #[n(9)]
    pub icp_fees: Option<TokenAmount>,
    /// The memo's `min_usdc`: the least an arrival under this ref must bring, committed by
    /// the ref.
    #[n(10)]
    pub least_usdc: TokenAmount,
}

impl Registration {
    /// The terms the registration was made of.
    pub fn terms(&self) -> DepositTerms {
        DepositTerms {
            kind: self.kind,
            min_usdc: self.least_usdc,
            premium: self.premium,
            icp_fees: self.icp_fees(),
            move_fee: self.move_fee,
            salt: self.salt,
        }
    }

    /// The ref these terms commit to: the keccak of their memo. None for terms with no memo.
    pub fn pool_ref(&self) -> Option<PoolRef> {
        self.terms().pool_ref()
    }

    /// A swap's ICP fees, none for a stake.
    pub fn icp_fees(&self) -> TokenAmount {
        self.icp_fees.unwrap_or(TokenAmount::ZERO)
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
}

/// A pool deposit's terms: what its order's memo carries ([`memo`]), and so the preimage of
/// its ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepositTerms {
    pub kind: DepositKind,
    /// The least micro USDC an arrival must bring to be credited under these terms: a copy
    /// of the memo paid with less is never this deposit.
    pub min_usdc: TokenAmount,
    /// A swap's premium, pure pool income; zero for a stake.
    pub premium: TokenAmount,
    /// A swap's ICP fees, the user's own line for its payout path's ledger fees; zero for a
    /// stake.
    pub icp_fees: TokenAmount,
    /// A stake's move fee: the cost of one Base to ICP move, Swapic's, never the pool's;
    /// zero for a swap.
    pub move_fee: TokenAmount,
    pub salt: [u8; 32],
}

impl DepositTerms {
    /// Whether every fee the kind has no place for is zero: a swap's move fee, a stake's
    /// premium and ICP fees. Only such terms have a memo.
    pub fn is_canonical(&self) -> bool {
        if self.kind.is_swap() {
            self.move_fee == TokenAmount::ZERO
        } else {
            self.premium == TokenAmount::ZERO && self.icp_fees == TokenAmount::ZERO
        }
    }

    /// The terms as their order's memo carries them.
    pub fn memo(&self) -> Result<Vec<u8>, memo::MemoError> {
        memo::encode(self)
    }

    /// The terms a memo holds.
    pub fn from_memo(bytes: &[u8]) -> Result<Self, memo::MemoError> {
        memo::decode(bytes)
    }

    /// The ref these terms commit to, `keccak256(memo)`; None for terms with no memo.
    pub fn pool_ref(&self) -> Option<PoolRef> {
        self.memo().ok().map(|bytes| memo::memo_ref(&bytes))
    }

    /// The registration these terms make of an arrival of `amount` on `rail` from
    /// `src_chain`, its payer as the proof shows it (none: the zero address), at `now`.
    pub fn registration(
        &self,
        amount: TokenAmount,
        src_chain: ChainId,
        src_payer: Option<EvmAddress>,
        rail: PoolRail,
        now: Timestamp,
    ) -> Registration {
        Registration {
            kind: self.kind,
            min_usdc: amount,
            premium: self.premium,
            move_fee: self.move_fee,
            src_chain,
            src_payer: src_payer.unwrap_or(EvmAddress::ZERO),
            rail,
            expires_at: now,
            salt: self.salt,
            icp_fees: self.kind.is_swap().then_some(self.icp_fees),
            least_usdc: self.min_usdc,
        }
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
    /// A controller sent what arrived back to its payer on Base (`resolve_deposit`): its
    /// arrival is an orphan held under its transfer's key, refunded from
    /// there.
    #[n(7)]
    Refunded,
}

/// What a controller decided for a deposit held for a human (`resolve_deposit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum DepositResolution {
    /// Back to its payout with fresh attempts: a started one whose transfer may have landed
    /// paying again with its own dedup pair (only on the ckUSDC ledger it started with, which
    /// deduplicates it), one whose transfer cannot land twice asked afresh, one that never
    /// started queued again at the back, a stake minting again.
    #[n(0)]
    Retry,
    /// What arrived back to its payer on Base, through the orphan refund path: only when
    /// the canister knows nothing of its payout went out on ICP.
    #[n(1)]
    Refund,
    /// Its payout's transfer as asked landed in ledger `block`, which the door read: paid,
    /// once.
    #[n(2)]
    MarkPaid {
        #[n(0)]
        block: u64,
    },
}

/// What a controller found of an ICP stake whose pull's answer was lost past the ledger's
/// deduplication window (`resolve_stake`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum StakeResolution {
    /// The pull landed in ledger `block`: its `shares`, priced when resolved, are minted.
    #[n(0)]
    Pulled {
        #[n(0)]
        block: u64,
        #[n(1)]
        shares: Shares,
    },
    /// It never landed: dropped, its room freed.
    #[n(1)]
    NotPulled,
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
    /// Its transfer to the recipient as last asked: none never. A
    /// payout started by a build before `TransferAsked` existed reads none here too; hfisl's
    /// log holds no Pool event, so no such payout exists.
    #[n(7)]
    pub sent: Option<Sent>,
}

/// A payout's transfer to its recipient as the canister asked it, recorded before the call
///: of which ledger, how much, and what it spends of the ICP pocket
/// once paid; and whether the ledger refused it for sure at that ask. Asked and not refused,
/// it may have landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Sent {
    #[n(0)]
    pub token: PrincipalBytes,
    #[n(1)]
    pub amount: TokenAmount,
    #[n(2)]
    pub spent: TokenAmount,
    #[n(3)]
    pub refused: bool,
    /// Whether an answer of it was ever lost (unknown, or too old to tell), kept for good
    ///: a later refusal does not prove the lost ask landed nowhere.
    #[n(4)]
    pub lost: crate::pool::payout::Flag,
}

impl PayoutProgress {
    /// Whether any ICPSwap leg of it moved money on ICP: approved (a fee spent), begun, or
    /// answered.
    pub fn moved_on_icp(&self) -> bool {
        [Some(self.swap), self.second, self.back]
            .into_iter()
            .flatten()
            .any(|leg| leg.begun || leg.approvals.unwrap_or(0) > 0 || leg.out.is_some())
    }

    /// Whether the canister knows nothing of it went out: never
    /// started; or started, no ICPSwap leg moved money, and no transfer to the recipient may
    /// have landed (none asked, or the last ask refused by the ledger for sure with no answer
    /// of it ever lost).
    pub fn nothing_went_out(&self) -> bool {
        self.attempt.is_none() || (!self.moved_on_icp() && self.cannot_land_twice())
    }

    /// Whether its transfer to the recipient cannot land twice if asked afresh: never asked,
    /// or refused by the ledger for sure at its last ask and no answer of it ever lost.
    pub fn cannot_land_twice(&self) -> bool {
        self.sent.is_none_or(|sent| sent.refused && !sent.lost.0)
    }

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

/// Why a held deposit's arrival is an orphan: a controller refunds it to its payer.
pub const DEPOSIT_REFUNDED: &str = "a held deposit a controller refunds to its payer";

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
    /// nothing.
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

    /// The ledger its payout's transfer to the recipient is on, once it started: the one it
    /// asked; else its path's, ckUSDC (`ckusdc`, the pool's own, when the attempt names none)
    /// or the token out through ICPSwap. None before it started, and for a stake.
    pub fn out_ledger(&self, ckusdc: Principal) -> Option<Principal> {
        let attempt = self.payout.attempt?;
        if let Some(sent) = self.payout.sent {
            return Some(sent.token.0);
        }
        match (attempt.path, self.terms.kind) {
            (PayoutPath::Ckusdc, _) => Some(attempt.token.map_or(ckusdc, |token| token.0)),
            (_, DepositKind::Swap { token_out, .. }) => Some(token_out),
            (_, DepositKind::Stake { .. }) => None,
        }
    }

    /// The orphan what arrived becomes when a controller refunds it (`resolve_deposit`):
    /// held under its transfer's key for its payer. None before it arrived.
    pub fn refund_orphan(&self) -> Option<(CreditKey, Orphan)> {
        let arrival = self.arrived?;
        Some((
            arrival.credit,
            Orphan {
                amount: arrival.amount,
                block: arrival.block,
                payer: Some(self.terms.src_payer),
                why: DEPOSIT_REFUNDED.to_string(),
                status: OrphanStatus::Held,
            },
        ))
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
        /// Swapic's, out of `amount`, booked to the ICP fee line when the pull lands.
        #[n(4)]
        move_fee: TokenAmount,
    },
    #[n(1)]
    Withdraw {
        #[n(0)]
        owner: IcpAccount,
        #[n(1)]
        burned: Shares,
        /// The ckUSDC the shares were worth less the move fee, reserved on ICP: the payout's
        /// whole budget, ledger fees included.
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

/// One balance check of the ICP pocket.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcpReadFailure {
    /// The ledger call failed or was refused.
    CallFailed,
    /// The answer is no balance the pool's lines hold (wider than 128 bits).
    BadAnswer,
}

impl IcpCheck {
    /// `observed - expected`, which a donation makes positive.
    pub fn surplus(&self) -> i128 {
        crate::books::surplus_of(self.observed, self.expected)
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
