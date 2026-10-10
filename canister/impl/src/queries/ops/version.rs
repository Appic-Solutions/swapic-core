use ic_cdk::query;

#[query]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
