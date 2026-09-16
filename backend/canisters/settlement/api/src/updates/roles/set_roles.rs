use candid::Principal;

/// Positional, as the endpoint takes them: `(quoter, watcher)`.
pub type Args = (Principal, Principal);
pub type Response = Result<(), String>;
