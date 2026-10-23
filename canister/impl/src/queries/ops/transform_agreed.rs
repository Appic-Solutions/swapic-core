use ic_cdk::api::management_canister::http_request::{HttpResponse, TransformArgs};

/// The transform every node runs on its own answer to an agreed read (M-03,
/// `rpc::rpc_batch_agreed`): what the canister reads of it, and nothing a provider writes
/// beside. Hidden from the candid interface: only the system calls it.
#[ic_cdk::query(hidden = true)]
pub fn transform_agreed(args: TransformArgs) -> HttpResponse {
    crate::rpc::agreed_response(args.response, &args.context)
}
