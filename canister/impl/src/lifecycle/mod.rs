mod init;
mod post_upgrade;

// the init arg's type reaches the crate root, where `export_candid!()` reads it
pub use init::InitArg;
