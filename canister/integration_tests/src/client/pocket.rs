use candid::{decode_one, CandidType, Deserialize, Principal};
use pocket_ic::PocketIc;

/// `args` is pre-encoded so callers can pass any arity.
pub fn query<T: CandidType + for<'de> Deserialize<'de>>(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    method: &str,
    args: Vec<u8>,
) -> T {
    let raw = pic.query_call(canister, sender, method, args).unwrap();
    decode_one(&raw).unwrap()
}

/// The update twin of `query`; `args` is pre-encoded for the same reason.
pub fn update<T: CandidType + for<'de> Deserialize<'de>>(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    method: &str,
    args: Vec<u8>,
) -> T {
    let raw = pic.update_call(canister, sender, method, args).unwrap();
    decode_one(&raw).unwrap()
}
