use ic_cdk::query;

pub mod events;
pub mod state;

#[query]
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

ic_cdk::export_candid!();
