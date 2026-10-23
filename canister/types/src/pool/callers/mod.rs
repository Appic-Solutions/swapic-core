//! The per-caller limit on the doors anyone may call for free (Jeff, 2026-10-23):
//! `swap_from_icp` and `stake_icp` need no approval to be called, and each call writes two
//! permanent lines before its pull is refused. A caller is let through at most
//! [`PER_MINUTE`] calls a minute and [`PER_HOUR`] an hour, both doors together, each window
//! counted from its first call; a refused call counts for nothing and writes nothing.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.

#[cfg(test)]
mod tests;

use crate::numeric::Timestamp;
use minicbor::{Decode, Encode};
use std::time::Duration;
use thiserror::Error;

/// The most calls a caller makes a minute.
pub const PER_MINUTE: u8 = 5;

/// The most calls a caller makes an hour.
pub const PER_HOUR: u8 = 30;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3_600);

/// One caller's calls let through: how many in its current minute and hour, each window
/// counted from the call that opened it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct CallerCalls {
    #[n(0)]
    pub minute_from: Timestamp,
    #[n(1)]
    pub in_minute: u8,
    #[n(2)]
    pub hour_from: Timestamp,
    #[n(3)]
    pub in_hour: u8,
}

crate::storable_as_cbor!(CallerCalls);

/// Why a call is not let through.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CallRefusal {
    #[error("at most {PER_MINUTE} calls a minute from one caller: ask again later")]
    PerMinute,
    #[error("at most {PER_HOUR} calls an hour from one caller: ask again later")]
    PerHour,
}

/// The caller's record once one more call at `now` is let through, or why it is not.
pub fn admit(record: Option<CallerCalls>, now: Timestamp) -> Result<CallerCalls, CallRefusal> {
    let open = |from: Timestamp, span: Duration| now.saturating_duration_since(from) < span;
    let (minute_from, in_minute) = match record {
        Some(record) if open(record.minute_from, MINUTE) => (record.minute_from, record.in_minute),
        _ => (now, 0),
    };
    let (hour_from, in_hour) = match record {
        Some(record) if open(record.hour_from, HOUR) => (record.hour_from, record.in_hour),
        _ => (now, 0),
    };
    if in_hour >= PER_HOUR {
        return Err(CallRefusal::PerHour);
    }
    if in_minute >= PER_MINUTE {
        return Err(CallRefusal::PerMinute);
    }
    Ok(CallerCalls {
        minute_from,
        in_minute: in_minute + 1,
        hour_from,
        in_hour: in_hour + 1,
    })
}
