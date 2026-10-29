//! These types against the standard's own: `ICRC-21.did` copied below under its own names,
//! field for field (dfinity/wg-identity-authentication, `topics/ICRC-21/ICRC-21.did`, read
//! 2026-10-28). A signer's request must decode as ours, and our answer as the standard's.

use super::*;
use candid::types::internal::TypeContainer;
use candid::types::subtype::{subtype_with_config, Gamma, OptReport};
use candid::types::Type;
use candid::TypeEnv;

#[allow(non_camel_case_types, dead_code)]
mod standard {
    use candid::{CandidType, Deserialize, Nat};

    #[derive(CandidType, Deserialize)]
    pub struct icrc21_consent_message_metadata {
        pub language: String,
        pub utc_offset_minutes: Option<i16>,
    }

    #[derive(CandidType, Deserialize)]
    pub enum device_spec {
        GenericDisplay,
        FieldsDisplay,
    }

    #[derive(CandidType, Deserialize)]
    pub struct icrc21_consent_message_spec {
        pub metadata: icrc21_consent_message_metadata,
        pub device_spec: Option<device_spec>,
    }

    #[derive(CandidType, Deserialize)]
    pub struct icrc21_consent_message_request {
        pub method: String,
        pub arg: Vec<u8>,
        pub user_preferences: icrc21_consent_message_spec,
    }

    #[derive(CandidType, Deserialize)]
    pub struct TokenAmount {
        pub decimals: u8,
        pub amount: u64,
        pub symbol: String,
    }

    #[derive(CandidType, Deserialize)]
    pub struct TimestampSeconds {
        pub amount: u64,
    }

    #[derive(CandidType, Deserialize)]
    pub struct DurationSeconds {
        pub amount: u64,
    }

    #[derive(CandidType, Deserialize)]
    pub struct TextValue {
        pub content: String,
    }

    #[derive(CandidType, Deserialize)]
    pub enum Value {
        TokenAmount(TokenAmount),
        TimestampSeconds(TimestampSeconds),
        DurationSeconds(DurationSeconds),
        Text(TextValue),
    }

    #[derive(CandidType, Deserialize)]
    pub struct fields_display_message {
        pub intent: String,
        pub fields: Vec<(String, Value)>,
    }

    #[derive(CandidType, Deserialize)]
    pub enum icrc21_consent_message {
        GenericDisplayMessage(String),
        FieldsDisplayMessage(fields_display_message),
    }

    #[derive(CandidType, Deserialize)]
    pub struct icrc21_consent_info {
        pub consent_message: icrc21_consent_message,
        pub metadata: icrc21_consent_message_metadata,
    }

    #[derive(CandidType, Deserialize)]
    pub struct icrc21_error_info {
        pub description: String,
    }

    #[derive(CandidType, Deserialize)]
    pub enum icrc21_error {
        UnsupportedCanisterCall(icrc21_error_info),
        ConsentMessageUnavailable(icrc21_error_info),
        InsufficientPayment(icrc21_error_info),
        GenericError { error_code: Nat, description: String },
    }

    #[derive(CandidType, Deserialize)]
    pub enum icrc21_consent_message_response {
        Ok(icrc21_consent_info),
        Err(icrc21_error),
    }

    #[derive(CandidType, Deserialize)]
    pub struct supported_standard {
        pub name: String,
        pub url: String,
    }
}

/// `T`'s candid type, its definitions merged into `env`.
fn ty<T: CandidType>(env: &mut TypeEnv) -> Type {
    let mut container = TypeContainer::new();
    let t = container.add::<T>();
    env.merge_type(container.env, t)
}

/// Whether `sub` decodes as `sup`, candid's special `opt` rule an error.
fn fits(env: &TypeEnv, sub: &Type, sup: &Type) -> Result<(), String> {
    subtype_with_config(OptReport::Error, &mut Gamma::new(), env, sub, sup)
        .map_err(|error| error.to_string())
}

#[test]
fn a_signers_request_decodes_as_ours_and_our_answer_as_the_standards() {
    let mut env = TypeEnv::new();
    let theirs = ty::<standard::icrc21_consent_message_request>(&mut env);
    let ours = ty::<ConsentMessageRequest>(&mut env);
    assert_eq!(fits(&env, &theirs, &ours), Ok(()), "a signer's request");
    assert_eq!(fits(&env, &ours, &theirs), Ok(()), "the same record both ways");
    let ours = ty::<ConsentMessageResponse>(&mut env);
    let theirs = ty::<standard::icrc21_consent_message_response>(&mut env);
    assert_eq!(fits(&env, &ours, &theirs), Ok(()), "our answer");
    let ours = ty::<Vec<SupportedStandard>>(&mut env);
    let theirs = ty::<Vec<standard::supported_standard>>(&mut env);
    assert_eq!(fits(&env, &ours, &theirs), Ok(()), "ICRC-10's answer");
}

/// The check can fail: an answer with a case the standard does not have is caught.
#[test]
fn a_drifted_shape_is_caught() {
    #[allow(dead_code)]
    #[derive(CandidType)]
    enum Drifted {
        Ok(ConsentInfo),
        Refused(ErrorInfo),
    }
    let mut env = TypeEnv::new();
    let ours = ty::<Drifted>(&mut env);
    let theirs = ty::<standard::icrc21_consent_message_response>(&mut env);
    assert!(fits(&env, &ours, &theirs).is_err());
}
