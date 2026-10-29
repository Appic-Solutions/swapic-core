//! How a consent message writes what it states: amounts exactly (integers only, never a
//! float), an instant in UTC or the offset asked, accounts and chains in full. Every function
//! here is total: nothing a caller sends makes one trap.

#[cfg(test)]
mod tests;

use candid::Principal;
use std::fmt;
use types::pool::IcpAccount;
use types::{ChainId, EvmAddress, Timestamp, TokenAmount};

/// How a message names a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenName {
    /// A token whose decimals the canister knows: its own unit, a chain's native coin, or
    /// the built-in list.
    Known { symbol: &'static str, decimals: u8 },
    /// An ICP token the canister knows only by its ledger.
    IcpLedger(Principal),
    /// An EVM token the canister knows only by its contract.
    Evm(EvmAddress),
    /// The native coin of a chain the canister has no name for.
    NativeOf(ChainId),
}

/// Micro USDC: the unit the pool and the books count USDC and ckUSDC in.
pub const USDC_DECIMALS: u8 = 6;
/// ckUSDC, which the pool counts in micro USDC.
pub const CKUSDC: TokenName = TokenName::Known {
    symbol: "ckUSDC",
    decimals: USDC_DECIMALS,
};
/// USDC on a chain the canister signs on, and the pool's value, in micro USDC.
pub const USDC: TokenName = TokenName::Known {
    symbol: "USDC",
    decimals: USDC_DECIMALS,
};
/// What an EVM chain's native coin is counted in: wei, 10^-18.
pub const NATIVE_DECIMALS: u8 = 18;

impl TokenName {
    /// The token in a message's title: its symbol, or what kind of token it is.
    pub fn short(self) -> &'static str {
        match self {
            TokenName::Known { symbol, .. } => symbol,
            TokenName::IcpLedger(_) => "an ICP token",
            TokenName::Evm(_) => "a token",
            TokenName::NativeOf(_) => "the native coin",
        }
    }
}

/// An amount in its token's smallest units, as a message writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Amount {
    pub units: TokenAmount,
    pub token: TokenName,
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.token {
            TokenName::Known { symbol, decimals } => {
                write!(f, "{} {symbol}", decimal(self.units, decimals))
            }
            TokenName::IcpLedger(ledger) => write!(f, "{} units of token {ledger}", self.units),
            TokenName::Evm(token) => write!(f, "{} units of token {token}", self.units),
            TokenName::NativeOf(chain) => {
                write!(f, "{} units of {}'s own coin", self.units, ChainText(chain))
            }
        }
    }
}

/// `units` with `decimals` digits after the point, exactly: trailing zeros and a bare point
/// dropped.
pub fn decimal(units: TokenAmount, decimals: u8) -> String {
    let digits = units.to_string();
    let decimals = usize::from(decimals);
    if decimals == 0 {
        return digits;
    }
    let padded = format!("{digits:0>width$}", width = decimals + 1);
    let (whole, fraction) = padded.split_at(padded.len() - decimals);
    match fraction.trim_end_matches('0') {
        "" => whole.to_string(),
        fraction => format!("{whole}.{fraction}"),
    }
}

/// A chain by its name where the canister knows it, else by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainText(pub ChainId);

impl fmt::Display for ChainText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "chain {}", self.0),
        }
    }
}

/// `subaccount` with 32 zero bytes read as none, as ICRC-1 reads them.
pub fn normal(subaccount: Option<[u8; 32]>) -> Option<[u8; 32]> {
    subaccount.filter(|bytes| *bytes != [0; 32])
}

/// An ICP account as a message names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountText {
    /// The caller's own account: no principal, since a cold signer asks anonymously.
    Yours { subaccount: Option<[u8; 32]> },
    /// Any other account, by its principal in full; marked when the asking wallet owns it.
    Other {
        account: IcpAccount,
        asker: Principal,
    },
}

impl fmt::Display for AccountText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountText::Yours { subaccount } => match normal(*subaccount) {
                None => f.write_str("your account"),
                Some(bytes) => write!(f, "your account (subaccount {})", hex::encode(bytes)),
            },
            AccountText::Other { account, asker } => {
                write!(f, "{}", account.owner)?;
                let wallet = account.owner == *asker && *asker != Principal::anonymous();
                match (wallet, normal(account.subaccount)) {
                    (false, None) => Ok(()),
                    (true, None) => f.write_str(" (this wallet)"),
                    (false, Some(bytes)) => write!(f, " (subaccount {})", hex::encode(bytes)),
                    (true, Some(bytes)) => {
                        write!(f, " (this wallet, subaccount {})", hex::encode(bytes))
                    }
                }
            }
        }
    }
}

/// Minutes east of UTC a message writes its instants in: the offset asked, within 14 hours
/// either way, else UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcOffset(i16);

impl UtcOffset {
    /// The widest offset any zone has: 14 hours.
    pub const MAX_MINUTES: u16 = 14 * 60;

    pub fn of(asked: Option<i16>) -> Self {
        Self(
            asked
                .filter(|minutes| minutes.unsigned_abs() <= Self::MAX_MINUTES)
                .unwrap_or(0),
        )
    }

    pub const fn minutes(self) -> i16 {
        self.0
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// An instant as a message writes it, to the minute and with its seconds when they are not
/// zero (a deadline is never rounded): `28 Oct 2026, 12:15 UTC`,
/// `28 Oct 2026, 16:45:30 UTC+04:00`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub at: Timestamp,
    pub offset: UtcOffset,
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let utc = i64::try_from(self.at.as_secs().get())
            .expect("BUG: u64 nanoseconds are at most 18446744073 seconds");
        let local = utc + i64::from(self.offset.0) * 60;
        let (year, month, day) = civil(local.div_euclid(86_400));
        let month = usize::try_from(month - 1)
            .ok()
            .and_then(|index| MONTHS.get(index))
            .expect("BUG: civil answers a month from 1 to 12");
        let second = local.rem_euclid(86_400);
        write!(
            f,
            "{day} {month} {year}, {:02}:{:02}",
            second / 3_600,
            second % 3_600 / 60
        )?;
        if second % 60 != 0 {
            write!(f, ":{:02}", second % 60)?;
        }
        f.write_str(" UTC")?;
        if self.offset.0 != 0 {
            let sign = if self.offset.0 > 0 { '+' } else { '-' };
            let minutes = self.offset.0.unsigned_abs();
            write!(f, "{sign}{:02}:{:02}", minutes / 60, minutes % 60)?;
        }
        Ok(())
    }
}

/// The date `days` after 1970-01-01, proleptic Gregorian (H. Hinnant's `civil_from_days`):
/// (year, month 1 to 12, day 1 to 31).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
