//! The program's upgrade through the canister and its delay (design 4.13, 8e). The canister signs
//! exactly one loader instruction, `Upgrade`, which it builds itself: no door takes loader bytes.
//!
//! What the delay holds against: a compromised service and mistakes. It does not hold against the
//! canister's controller, who can install new code that signs at once (design decision 14): the same
//! trust that already signs the Base pool's payouts.

#[cfg(test)]
mod tests;

/// A buffer account's header: the loader's tag (`u32`, 1), then its optional authority.
pub const BUFFER_HEADER: usize = 37;
/// A program data account's header.
pub const PROGRAM_DATA_HEADER: usize = 45;

/// The authority a buffer account names, if it reads as one.
pub fn buffer_authority(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() < BUFFER_HEADER || data[..4] != [1, 0, 0, 0] || data[4] != 1 {
        return None;
    }
    data[5..37].try_into().ok()
}

/// The hash `solana-verify get-executable-hash` prints: sha256 of the program's bytes with the padding
/// of trailing zeroes taken off.
pub fn executable_hash(program: &[u8]) -> [u8; 32] {
    let end = program
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |at| at + 1);
    super::sha256(&program[..end])
}

/// Every rule of an execution, pure: the delay passed, the buffer is the loader's and the admin key's,
/// it holds the published bytes, and the program data account is large enough.
#[allow(clippy::too_many_arguments)]
pub fn check_execution(
    not_before_s: u64,
    now_s: u64,
    build_hash: &[u8; 32],
    buffer_owner: &[u8; 32],
    buffer_data: &[u8],
    admin: &[u8; 32],
    program_data_space: u64,
) -> Result<(), &'static str> {
    use swapic_solana_wire::consts::UPGRADEABLE_LOADER;
    if now_s < not_before_s {
        return Err("the delay has not passed");
    }
    if buffer_owner != &UPGRADEABLE_LOADER {
        return Err("the buffer is not the loader's");
    }
    if buffer_authority(buffer_data) != Some(*admin) {
        return Err("the buffer's authority is not the admin key: its bytes could still change");
    }
    let program = &buffer_data[BUFFER_HEADER..];
    if executable_hash(program) != *build_hash {
        return Err("the buffer does not hold the published build");
    }
    if (PROGRAM_DATA_HEADER + program.len()) as u64 > program_data_space {
        return Err("the program data account is too small: extend it first");
    }
    Ok(())
}
