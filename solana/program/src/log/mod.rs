//! The program's log lines (design 4.11). Hints only: logs can be cut after a long route, so no
//! money decision reads them.

/// Writes one `Program data:` line.
#[inline(always)]
pub fn data(bytes: &[u8]) {
    #[cfg(target_os = "solana")]
    {
        let parts = [bytes];
        // SAFETY: the syscall reads a slice of slices, valid for the call
        unsafe {
            pinocchio::syscalls::sol_log_data(parts.as_ptr() as *const u8, parts.len() as u64)
        };
    }
    #[cfg(not(target_os = "solana"))]
    let _ = bytes;
}

const BURNED: u8 = 1;
const COMPLETED: u8 = 2;
const RELEASED: u8 = 3;
const RECLAIMED: u8 = 4;

pub fn burned(ref_bytes: &[u8; 32], domain: u8, action: u8, produced: u64, fee: u32, amount: u64) {
    let mut line = [0u8; 55];
    line[0] = BURNED;
    line[1..33].copy_from_slice(ref_bytes);
    line[33] = domain;
    line[34] = action;
    line[35..43].copy_from_slice(&produced.to_le_bytes());
    line[43..47].copy_from_slice(&fee.to_le_bytes());
    line[47..55].copy_from_slice(&amount.to_le_bytes());
    data(&line);
}

pub fn completed(ref_bytes: &[u8; 32], minted: u64, got: u64) {
    let mut line = [0u8; 49];
    line[0] = COMPLETED;
    line[1..33].copy_from_slice(ref_bytes);
    line[33..41].copy_from_slice(&minted.to_le_bytes());
    line[41..49].copy_from_slice(&got.to_le_bytes());
    data(&line);
}

pub fn released(ref_bytes: &[u8; 32], minted: u64, misrouted: bool) {
    let mut line = [0u8; 42];
    line[0] = RELEASED;
    line[1..33].copy_from_slice(ref_bytes);
    line[33..41].copy_from_slice(&minted.to_le_bytes());
    line[41] = misrouted as u8;
    data(&line);
}

pub fn reclaimed(lamports: u64) {
    let mut line = [0u8; 9];
    line[0] = RECLAIMED;
    line[1..9].copy_from_slice(&lamports.to_le_bytes());
    data(&line);
}
