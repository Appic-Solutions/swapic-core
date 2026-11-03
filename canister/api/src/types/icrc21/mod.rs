//! ICRC-21 (canister call consent messages) and ICRC-10 (supported standards): the shapes of
//! `ICRC-21.did` (dfinity/wg-identity-authentication, `topics/ICRC-21/ICRC-21.did`)
//! under this crate's names. A signer decodes by shape, never by name. The
//! answer's variants are the cases this canister sends: a variant with fewer cases is a
//! subtype of the standard's, so every signer decodes it.

#[cfg(test)]
mod tests;

use candid::CandidType;
use serde::Deserialize;

/// `icrc21_consent_message_request`: the call a signer is about to make.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConsentMessageRequest {
    /// The method the signer would call on this canister.
    pub method: String,
    /// Its candid argument, byte for byte as the signer would send it.
    pub arg: Vec<u8>,
    pub user_preferences: ConsentMessageSpec,
}

/// `icrc21_consent_message_spec`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConsentMessageSpec {
    pub metadata: ConsentMessageMetadata,
    /// None: the canister picks (this one: the generic display).
    pub device_spec: Option<DeviceSpec>,
}

/// `icrc21_consent_message_metadata`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConsentMessageMetadata {
    /// A BCP-47 language tag.
    pub language: String,
    /// Minutes east of UTC. In a request, None is UTC; in an answer, None says the message
    /// holds no local time.
    pub utc_offset_minutes: Option<i16>,
}

/// The display a signer shows a message on.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceSpec {
    /// Markdown, no external resources.
    GenericDisplay,
    /// Titled fields of plain text.
    FieldsDisplay,
}

/// `icrc21_consent_message_response`. A variant of its own rather than a `Result`, so the
/// did gains no `Result_N` and every existing one keeps its number.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ConsentMessageResponse {
    Ok(ConsentInfo),
    Err(Icrc21Error),
}

/// `icrc21_consent_info`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConsentInfo {
    pub consent_message: ConsentMessage,
    pub metadata: ConsentMessageMetadata,
}

/// `icrc21_consent_message`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ConsentMessage {
    GenericDisplayMessage(String),
    FieldsDisplayMessage(FieldsDisplay),
}

/// A fields display's message: its intent and its titled values.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FieldsDisplay {
    pub intent: String,
    pub fields: Vec<(String, Value)>,
}

/// A field's value: the standard's `Value` less `TokenAmount` and `DurationSeconds`, which
/// this canister never sends (an amount is text it wrote exactly; no message holds a
/// duration).
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Value {
    TimestampSeconds(TimestampSeconds),
    Text(TextValue),
}

/// Unix seconds, which the signer writes in its user's zone.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestampSeconds {
    pub amount: u64,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TextValue {
    pub content: String,
}

/// `icrc21_error_info`: a description for developers, never shown to the user.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ErrorInfo {
    pub description: String,
}

/// `icrc21_error`, the two cases this canister answers.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Icrc21Error {
    /// A call the canister refuses: another method, or an argument its door would not take.
    UnsupportedCanisterCall(ErrorInfo),
    /// A call the canister cannot describe now: what reading it needs is not set yet.
    ConsentMessageUnavailable(ErrorInfo),
}

/// One standard `icrc10_supported_standards` lists.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SupportedStandard {
    pub name: String,
    pub url: String,
}
