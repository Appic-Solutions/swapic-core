//! What the entry doors keep between messages: the marker a claim or a pull holds while
//! its outcall or signature is out, and the attestation the watcher hands in for a burn.
//! Neither is a fold of the event log: a marker is scaffolding around one message chain,
//! and an attestation is rail data the mint transaction carries and the chain verifies.

#[cfg(test)]
mod tests;

use crate::numeric::Timestamp;
use minicbor::{Decode, Encode};
use std::time::Duration;
use thiserror::Error;

/// What a marker is standing in for.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum InFlightKind {
    /// A claim is reading the chain for the quote's deposit.
    #[n(0)]
    Claim,
    /// A gasless pull is being signed for the quote.
    #[n(1)]
    Pull,
}

/// Rule A8: the marker a door commits for a quote before the await it is about to make, so
/// a second call for the same quote is refused instead of buying a second outcall or
/// signing a second pull. Removed when the message chain ends, and stale once it has
/// outlived anything that could still be out.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct InFlight {
    #[n(0)]
    pub kind: InFlightKind,
    #[n(1)]
    pub since: Timestamp,
}

impl InFlight {
    /// Whether the marker has stood for `bound` or longer. What it marks is one outcall,
    /// capped at a minute by the system, or one signing round trip, so past a bound well
    /// above both it was left behind by a message that never came back, and the next
    /// caller may take its place.
    pub fn is_stale(&self, now: Timestamp, bound: Duration) -> bool {
        now.saturating_duration_since(self.since) >= bound
    }
}

crate::storable_as_cbor!(InFlight);

/// The most bytes a CCTP message may be. A v2 burn message is 376 bytes; four kilobytes
/// leaves room for every message body Circle defines.
pub const MAX_MESSAGE_BYTES: usize = 4_096;

/// The most bytes an attestation may be: 65 per attester, and Circle signs with a handful.
pub const MAX_ATTESTATION_BYTES: usize = 1_024;

/// Why an attestation was not taken in.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AttestationError {
    #[error("the message is {len} bytes, above the cap of {cap}")]
    MessageTooLong { len: usize, cap: usize },
    #[error("the attestation is {len} bytes, above the cap of {cap}")]
    AttestationTooLong { len: usize, cap: usize },
}

/// What Circle attested for a burn: the message the source chain emitted and the
/// signatures over it, which together are what the destination's `receiveMessage` takes.
/// A hint from outside, never money truth: a wrong pair makes the mint revert, and the
/// receipt is what decides.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Attestation {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub message: Vec<u8>,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub attestation: Vec<u8>,
    /// Canister time, when the watcher handed it in.
    #[n(2)]
    pub received_at: Timestamp,
}

impl Attestation {
    /// An attestation inside the bounds, or which bound it broke.
    pub fn new(
        message: Vec<u8>,
        attestation: Vec<u8>,
        received_at: Timestamp,
    ) -> Result<Self, AttestationError> {
        if message.len() > MAX_MESSAGE_BYTES {
            return Err(AttestationError::MessageTooLong {
                len: message.len(),
                cap: MAX_MESSAGE_BYTES,
            });
        }
        if attestation.len() > MAX_ATTESTATION_BYTES {
            return Err(AttestationError::AttestationTooLong {
                len: attestation.len(),
                cap: MAX_ATTESTATION_BYTES,
            });
        }
        Ok(Self {
            message,
            attestation,
            received_at,
        })
    }
}

crate::storable_as_cbor!(Attestation);
