//! ICRC-21 consent messages for the five doors a wallet calls for its user: `swap_from_icp`,
//! `swap_from_icp_ck`, `stake_icp`, `withdraw_icp` and `withdraw_evm`. The argument is decoded exactly as the
//! door decodes it (its types, its decoder quotas), the door's own wire conversion reads it,
//! and a swap's payout plan is read by the canister's own reader, so the message states what
//! the door will act on. The facts are one list, rendered twice: Markdown for a generic
//! display, plain fields for a fields display, in the wording Jeff approved.
//! Pure: nothing is written, awaited or called.
//!
//! The text never carries a string the caller wrote: every value is a number, a principal,
//! an address or an instant this module rendered.

#[cfg(test)]
mod tests;

pub mod text;
pub mod tokens;

use crate::pool::icp::{StakeRequest, WithdrawRequest};
use crate::pool::{ck, icp_swap, pay, withdraw};
use candid::de::DecoderConfig;
use candid::utils::ArgumentDecoder;
use candid::Principal;
use settlement_api::types::icrc21::{
    ConsentInfo, ConsentMessage, ConsentMessageMetadata, ConsentMessageRequest,
    ConsentMessageResponse, DeviceSpec, ErrorInfo, FieldsDisplay, Icrc21Error, SupportedStandard,
    TextValue, TimestampSeconds, Value,
};
use settlement_api::types::pool::{
    CkSwapTerms, EvmWithdrawal, IcpSwapTerms, PoolError, StakeIcpArgs, WithdrawIcpArgs,
};
use std::fmt;
use text::{
    AccountText, Amount, ChainText, LocalTime, TokenName, UtcOffset, CKUSDC, NATIVE_DECIMALS, USDC,
    USDC_DECIMALS,
};
use thiserror::Error;
use types::pool::{IcpAccount, PoolAccount};
use types::swapic::NATIVE;
use types::{ChainId, EvmAddress, Timestamp, TokenAmount};

/// The skipping quota every door decodes its argument with: `ic-cdk` 0.17's own for an
/// `#[update]` that names none (no decoding quota), so a message reads what the door reads.
pub const DOOR_SKIPPING_QUOTA: usize = 10_000;

/// The most bytes of a refusal's description, and of the text it quotes (F3).
pub const MAX_DESCRIPTION_BYTES: usize = 300;

/// The language every message is written in; a request for another gets it too, as ICRC-21
/// prefers a message to an error.
pub const LANGUAGE: &str = "en";

/// A door that has a consent message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    SwapFromIcp,
    SwapFromIcpCk,
    StakeIcp,
    WithdrawIcp,
    WithdrawEvm,
}

impl Door {
    pub fn of(method: &str) -> Option<Self> {
        match method {
            "swap_from_icp" => Some(Self::SwapFromIcp),
            "swap_from_icp_ck" => Some(Self::SwapFromIcpCk),
            "stake_icp" => Some(Self::StakeIcp),
            "withdraw_icp" => Some(Self::WithdrawIcp),
            "withdraw_evm" => Some(Self::WithdrawEvm),
            _ => None,
        }
    }

    pub const fn method(self) -> &'static str {
        match self {
            Self::SwapFromIcp => "swap_from_icp",
            Self::SwapFromIcpCk => "swap_from_icp_ck",
            Self::StakeIcp => "stake_icp",
            Self::WithdrawIcp => "withdraw_icp",
            Self::WithdrawEvm => "withdraw_evm",
        }
    }
}

impl fmt::Display for Door {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.method())
    }
}

/// Why no message describes a call.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Refusal {
    #[error(
        "{method} has no consent message: Swapic describes only swap_from_icp, \
         swap_from_icp_ck, stake_icp, withdraw_icp and withdraw_evm"
    )]
    NoMessage { method: String },
    /// Candid's own reason, bounded.
    #[error("the argument of {door} does not decode: {reason}")]
    Undecodable { door: Door, reason: String },
    /// What the door's own conversion or plan reader answers.
    #[error("{door}: {error:?}")]
    Refused { door: Door, error: PoolError },
    /// The canister lacks what reading the call needs: its EVM address, or a pool chain.
    #[error("{door}: {error:?}")]
    Unavailable { door: Door, error: PoolError },
}

impl From<Refusal> for Icrc21Error {
    fn from(refusal: Refusal) -> Self {
        let info = ErrorInfo {
            description: bounded(&refusal.to_string()),
        };
        match refusal {
            Refusal::NoMessage { .. } | Refusal::Undecodable { .. } | Refusal::Refused { .. } => {
                Self::UnsupportedCanisterCall(info)
            }
            Refusal::Unavailable { .. } => Self::ConsentMessageUnavailable(info),
        }
    }
}

/// `text` cut to [`MAX_DESCRIPTION_BYTES`] on a character boundary, marked when cut.
pub fn bounded(text: &str) -> String {
    if text.len() <= MAX_DESCRIPTION_BYTES {
        return text.to_string();
    }
    let end = (0..=MAX_DESCRIPTION_BYTES)
        .rev()
        .find(|end| text.is_char_boundary(*end))
        .unwrap_or(0);
    format!("{}...", &text[..end])
}

/// What the canister knows that a message names things by, read once per request.
pub struct Known<'a> {
    /// The chain table, the pool chain and Eco's Portal: what reads a plan and names a
    /// configured chain's USDC.
    pub config: &'a types::Config,
    /// The canister's EVM address: the payer every payout plan names.
    pub canister: Option<EvmAddress>,
    /// The pool's ckUSDC ledger: named ckUSDC, and the token a withdrawal pays without a
    /// swap (as the door's path decides it).
    pub ckusdc: Option<Principal>,
    /// Who asks: the call's own caller (a hot signer), or anonymous (a cold signer).
    pub caller: Principal,
}

impl Known<'_> {
    /// An ICP ledger's token: the pool's ckUSDC, a built-in one, any other by its ledger.
    pub fn icp_token(&self, ledger: Principal) -> TokenName {
        if Some(ledger) == self.ckusdc {
            return CKUSDC;
        }
        tokens::icp_token(ledger).unwrap_or(TokenName::IcpLedger(ledger))
    }

    /// An EVM token on `chain`: a named chain's native coin, a built-in one, the chain's USDC
    /// as the config names it, any other by its contract.
    pub fn evm_token(&self, chain: ChainId, token: EvmAddress) -> TokenName {
        if token == NATIVE {
            return match chain.native_symbol() {
                Some(symbol) => TokenName::Known {
                    symbol,
                    decimals: NATIVE_DECIMALS,
                },
                None => TokenName::NativeOf(chain),
            };
        }
        if let Some(named) = tokens::evm_token(chain, token) {
            return named;
        }
        match self.config.evm_chains.get(chain) {
            Some(evm) if evm.usdc == token => USDC,
            _ => TokenName::Evm(token),
        }
    }

    /// `account` as a message names it: "your account" when it is `yours`, the caller's.
    pub fn account(&self, account: IcpAccount, yours: IcpAccount) -> AccountText {
        if account.owner == yours.owner
            && text::normal(account.subaccount) == text::normal(yours.subaccount)
        {
            AccountText::Yours {
                subaccount: yours.subaccount,
            }
        } else {
            AccountText::Other {
                account,
                asker: self.caller,
            }
        }
    }
}

/// What a message states: its intent and its lines, in order. Both displays come from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    pub intent: String,
    pub lines: Vec<Line>,
}

/// One stated fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub label: &'static str,
    pub value: Shown,
}

/// A line's value: text this module wrote, or an instant each display writes its own way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Text(String),
    Time(Timestamp),
}

impl Line {
    fn text(label: &'static str, value: impl fmt::Display) -> Self {
        Self {
            label,
            value: Shown::Text(value.to_string()),
        }
    }

    fn time(label: &'static str, at: Timestamp) -> Self {
        Self {
            label,
            value: Shown::Time(at),
        }
    }
}

impl Consent {
    /// The generic display's Markdown: a title, then one `- Label: value` line per fact.
    pub fn markdown(&self, offset: UtcOffset) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|line| {
                let value = match &line.value {
                    Shown::Text(value) => value.clone(),
                    Shown::Time(at) => LocalTime { at: *at, offset }.to_string(),
                };
                format!("- {}: {value}", line.label)
            })
            .collect();
        format!("## {}\n\n{}", self.intent, lines.join("\n"))
    }

    /// The fields display's message: the same facts as plain fields.
    pub fn fields(&self) -> FieldsDisplay {
        FieldsDisplay {
            intent: self.intent.clone(),
            fields: self
                .lines
                .iter()
                .map(|line| {
                    let value = match &line.value {
                        Shown::Text(value) => Value::Text(TextValue {
                            content: value.clone(),
                        }),
                        Shown::Time(at) => Value::TimestampSeconds(TimestampSeconds {
                            amount: at.as_secs().get(),
                        }),
                    };
                    (line.label.to_string(), value)
                })
                .collect(),
        }
    }
}

/// The answer to a consent request: the message in the display asked (the generic one when
/// none is), or why there is none. Total: no input traps it.
pub fn answer(request: &ConsentMessageRequest, known: &Known) -> ConsentMessageResponse {
    let consent = match consent(&request.method, &request.arg, known) {
        Ok(consent) => consent,
        Err(refusal) => return ConsentMessageResponse::Err(refusal.into()),
    };
    let preferences = &request.user_preferences;
    let (consent_message, utc_offset_minutes) = match preferences.device_spec {
        // its deadline is a timestamp the signer writes in its own zone
        Some(DeviceSpec::FieldsDisplay) => {
            (ConsentMessage::FieldsDisplayMessage(consent.fields()), None)
        }
        Some(DeviceSpec::GenericDisplay) | None => {
            let offset = UtcOffset::of(preferences.metadata.utc_offset_minutes);
            (
                ConsentMessage::GenericDisplayMessage(consent.markdown(offset)),
                Some(offset.minutes()),
            )
        }
    };
    ConsentMessageResponse::Ok(ConsentInfo {
        consent_message,
        metadata: ConsentMessageMetadata {
            language: LANGUAGE.to_string(),
            utc_offset_minutes,
        },
    })
}

/// What a call to `method` with `arg` states, or why nothing does.
pub fn consent(method: &str, arg: &[u8], known: &Known) -> Result<Consent, Refusal> {
    let door = Door::of(method).ok_or_else(|| Refusal::NoMessage {
        method: bounded(method),
    })?;
    match door {
        Door::SwapFromIcp => {
            let (terms,): (IcpSwapTerms,) = decode(door, arg)?;
            swap(door, terms, known)
        }
        Door::SwapFromIcpCk => {
            let (terms,): (CkSwapTerms,) = decode(door, arg)?;
            swap_ck(door, terms, known)
        }
        Door::StakeIcp => {
            let (args,): (StakeIcpArgs,) = decode(door, arg)?;
            stake(door, args, known)
        }
        Door::WithdrawIcp => {
            let (args,): (WithdrawIcpArgs,) = decode(door, arg)?;
            withdraw_icp(door, args, known)
        }
        Door::WithdrawEvm => {
            // the signature proves the staker to the door; the message states what it signed
            let (withdrawal, _signature): (EvmWithdrawal, Vec<u8>) = decode(door, arg)?;
            withdraw_evm(door, withdrawal, known)
        }
    }
}

/// The ICRC-10 answer: this standard and ICRC-21.
pub fn supported_standards() -> Vec<SupportedStandard> {
    [
        (
            "ICRC-10",
            "https://github.com/dfinity/ICRC/blob/main/ICRCs/ICRC-10/ICRC-10.md",
        ),
        (
            "ICRC-21",
            "https://github.com/dfinity/ICRC/blob/main/ICRCs/ICRC-21/ICRC-21.md",
        ),
    ]
    .into_iter()
    .map(|(name, url)| SupportedStandard {
        name: name.to_string(),
        url: url.to_string(),
    })
    .collect()
}

/// The door's argument, decoded as `ic-cdk` decodes it for the door.
fn decode<'a, T: ArgumentDecoder<'a>>(door: Door, arg: &'a [u8]) -> Result<T, Refusal> {
    let mut config = DecoderConfig::new();
    config.set_skipping_quota(DOOR_SKIPPING_QUOTA);
    candid::utils::decode_args_with_config(arg, &config).map_err(|error| Refusal::Undecodable {
        door,
        reason: bounded(&error.to_string()),
    })
}

fn refused(door: Door) -> impl Fn(PoolError) -> Refusal {
    move |error| Refusal::Refused { door, error }
}

fn unavailable(door: Door) -> impl Fn(PoolError) -> Refusal {
    move |error| Refusal::Unavailable { door, error }
}

/// An EVM token's own line: its symbol and contract, the chain's own coin, or its contract.
fn token_line(chain: ChainId, token: EvmAddress, name: TokenName) -> String {
    match name {
        TokenName::Known { symbol, .. } if token == NATIVE => {
            format!("{symbol} ({}'s own coin)", ChainText(chain))
        }
        TokenName::Known { symbol, .. } => format!("{symbol} ({token})"),
        TokenName::NativeOf(_) => format!("{}'s own coin", ChainText(chain)),
        TokenName::IcpLedger(_) | TokenName::Evm(_) => token.to_string(),
    }
}

/// `swap_from_icp_ck`: the words of `swap_from_icp`, with what this route adds said plainly:
/// its network fee, and that a refund after the minter burned keeps it.
fn swap_ck(door: Door, terms: CkSwapTerms, known: &Known) -> Result<Consent, Refusal> {
    let request = ck::request_of(known.caller, terms).map_err(refused(door))?;
    let sent = known.icp_token(request.token_in);
    let chain = ChainText(request.terms.dst_chain);
    let gets = Amount {
        units: request.terms.min_out,
        token: known.evm_token(request.terms.dst_chain, request.terms.token_out),
    };
    let refund = known.account(request.refund_to, request.from);
    Ok(Consent {
        intent: format!("Swap {} to {chain}", sent.short()),
        lines: vec![
            Line::text(
                "You send",
                format!(
                    "{}, plus the ledger fee",
                    Amount {
                        units: request.amount_in,
                        token: sent
                    }
                ),
            ),
            Line::text(
                "From",
                AccountText::Yours {
                    subaccount: request.from.subaccount,
                },
            ),
            Line::text("Recipient gets at least", format!("{gets} on {chain}")),
            Line::text(
                "Token",
                token_line(request.terms.dst_chain, request.terms.token_out, gets.token),
            ),
            Line::text("Recipient", request.terms.recipient),
            Line::text(
                "Network fee",
                Amount {
                    units: request.network_fee,
                    token: CKUSDC,
                },
            ),
            Line::text("Route", "the ckETH minter pays the recipient"),
            Line::time("Deadline", request.terms.deadline),
            Line::text(
                "If it can't finish",
                format!(
                    "refunded to {refund}, in ckUSDC or the token you sent, less ledger fees, \
                     and less the network fee if the minter already burned it"
                ),
            ),
        ],
    })
}

fn swap(door: Door, terms: IcpSwapTerms, known: &Known) -> Result<Consent, Refusal> {
    let request = icp_swap::request_of(known.caller, terms).map_err(refused(door))?;
    let order = icp_swap::decode_plan(&request.plan).map_err(refused(door))?;
    let canister = known.canister.ok_or_else(|| Refusal::Unavailable {
        door,
        error: PoolError::Unavailable("the canister's address is not derived yet".to_string()),
    })?;
    let payer = pay::payer_of(known.config, canister, None).map_err(unavailable(door))?;
    let (_, delivery) = icp_swap::plan_delivery(&order, &payer.chains).map_err(refused(door))?;
    let sent = known.icp_token(request.token_in);
    let chain = ChainText(delivery.chain);
    let gets = Amount {
        units: delivery.min_out,
        token: known.evm_token(delivery.chain, delivery.token),
    };
    let refund = known.account(request.refund_to, request.from);
    Ok(Consent {
        intent: format!("Swap {} to {chain}", sent.short()),
        lines: vec![
            Line::text(
                "You send",
                format!(
                    "{}, plus the ledger fee",
                    Amount {
                        units: request.amount_in,
                        token: sent
                    }
                ),
            ),
            Line::text(
                "From",
                AccountText::Yours {
                    subaccount: request.from.subaccount,
                },
            ),
            Line::text("Recipient gets at least", format!("{gets} on {chain}")),
            Line::text(
                "Token",
                token_line(delivery.chain, delivery.token, gets.token),
            ),
            Line::text("Recipient", delivery.recipient),
            Line::time("Deadline", request.deadline),
            Line::text(
                "If it can't finish",
                format!("refunded to {refund}, in ckUSDC or the token you sent, less ledger fees"),
            ),
        ],
    })
}

fn stake(door: Door, args: StakeIcpArgs, known: &Known) -> Result<Consent, Refusal> {
    let request = StakeRequest::of(known.caller, args).map_err(refused(door))?;
    let from = IcpAccount {
        owner: known.caller,
        subaccount: request.from_subaccount,
    };
    let owner = match request.owner {
        PoolAccount::Icp(account) => known.account(account, from).to_string(),
        PoolAccount::Evm(address) => format!("the EVM address {address}"),
    };
    Ok(Consent {
        intent: "Stake ckUSDC in the Swapic pool".to_string(),
        lines: vec![
            Line::text(
                "You stake",
                format!(
                    "{}, plus the ledger fee",
                    Amount {
                        units: request.amount,
                        token: CKUSDC
                    }
                ),
            ),
            Line::text(
                "From",
                AccountText::Yours {
                    subaccount: request.from_subaccount,
                },
            ),
            // the fee the request names, which may be up to all but one unit of the stake
            Line::text(
                "Kept",
                format!(
                    "{} Swapic fee; {} is staked",
                    Amount {
                        units: request.move_fee,
                        token: CKUSDC
                    },
                    Amount {
                        units: request
                            .amount
                            .checked_sub(request.move_fee)
                            .unwrap_or(TokenAmount::ZERO),
                        token: CKUSDC
                    }
                ),
            ),
            Line::text("Stake owned by", owner),
        ],
    })
}

fn withdraw_icp(door: Door, args: WithdrawIcpArgs, known: &Known) -> Result<Consent, Refusal> {
    let request = WithdrawRequest::of(known.caller, args).map_err(refused(door))?;
    let from_stake = match text::normal(request.from) {
        None => "your stake".to_string(),
        Some(bytes) => format!("your stake (subaccount {})", hex::encode(bytes)),
    };
    let mut lines = vec![
        Line::text(
            "You withdraw",
            format!(
                "{} from {from_stake}",
                Amount {
                    units: request.usdc,
                    token: USDC
                }
            ),
        ),
        Line::text("Paid to", known.account(request.to, request.owner())),
        Line::text(
            "Paid at least",
            Amount {
                units: TokenAmount::new(request.min_out),
                token: known.icp_token(request.token_out),
            },
        ),
        Line::text(
            "Fees",
            format!(
                "{} Swapic fee, and ledger fees, come out of the {}",
                Amount {
                    units: request.move_fee,
                    token: USDC
                },
                text::decimal(request.usdc, USDC_DECIMALS)
            ),
        ),
    ];
    // the door pays ckUSDC without a swap only on the pool's own ckUSDC ledger
    if Some(request.token_out) != known.ckusdc {
        lines.push(Line::text(
            "If the swap pays less",
            "all of it is paid as ckUSDC",
        ));
    }
    Ok(Consent {
        intent: "Withdraw from the Swapic pool on ICP".to_string(),
        lines,
    })
}

fn withdraw_evm(door: Door, args: EvmWithdrawal, known: &Known) -> Result<Consent, Refusal> {
    let request = withdraw::request_of(args).map_err(refused(door))?;
    let terms = request.payout;
    let chain = ChainText(terms.dst_chain);
    let usdc = |units| Amount { units, token: USDC };
    let gets = Amount {
        units: terms.min_out,
        token: known.evm_token(terms.dst_chain, terms.token_out),
    };
    Ok(Consent {
        intent: format!("Withdraw from the Swapic pool to {chain}"),
        lines: vec![
            Line::text(
                "Staker",
                format!("{} (their signature allows this)", request.staker),
            ),
            Line::text("Withdraw", format!("{} from the stake", usdc(request.usdc))),
            Line::text(
                "Kept",
                format!(
                    "{} pool fee, {} network fee",
                    usdc(request.move_fee),
                    usdc(request.network_fee)
                ),
            ),
            Line::text("Recipient gets at least", format!("{gets} on {chain}")),
            Line::text(
                "Token",
                token_line(terms.dst_chain, terms.token_out, gets.token),
            ),
            Line::text("Recipient", terms.recipient),
            Line::time("Deadline", terms.deadline),
            Line::text(
                "If it can't finish",
                "the withdrawal is undone and the stake is restored",
            ),
        ],
    })
}
