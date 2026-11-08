use super::*;
use swapic_solana_wire::consts::UPGRADEABLE_LOADER;

const ADMIN: [u8; 32] = [3; 32];
const NOW: u64 = 1_800_000_000;

fn buffer(authority: &[u8; 32], program: &[u8]) -> Vec<u8> {
    let mut data = vec![1, 0, 0, 0, 1];
    data.extend_from_slice(authority);
    data.extend_from_slice(program);
    data
}

fn check(hash: &[u8; 32], owner: &[u8; 32], data: &[u8], space: u64, not_before: u64) -> Result<(), &'static str> {
    check_execution(not_before, NOW, hash, owner, data, &ADMIN, space)
}

#[test]
fn the_hash_ignores_the_trailing_zeros_of_the_padding() {
    assert_eq!(executable_hash(&[1, 2, 3, 0, 0]), executable_hash(&[1, 2, 3]));
    assert_ne!(executable_hash(&[1, 2, 3, 0, 1]), executable_hash(&[1, 2, 3]));
    assert_eq!(executable_hash(&[0, 0]), executable_hash(&[]));
}

#[test]
fn only_the_published_bytes_after_the_delay_under_the_admin_key_pass() {
    let program = [7u8; 100];
    let hash = executable_hash(&program);
    let data = buffer(&ADMIN, &program);
    let space = (PROGRAM_DATA_HEADER + 100) as u64;
    assert_eq!(check(&hash, &UPGRADEABLE_LOADER, &data, space, NOW), Ok(()));
    assert!(check(&hash, &UPGRADEABLE_LOADER, &data, space, NOW + 1).unwrap_err().contains("delay"));
    assert!(check(&hash, &[9; 32], &data, space, NOW).unwrap_err().contains("loader"));
    assert!(check(&hash, &UPGRADEABLE_LOADER, &buffer(&[4; 32], &program), space, NOW).unwrap_err().contains("authority"));
    let mut changed = program;
    changed[50] = 8;
    assert!(check(&hash, &UPGRADEABLE_LOADER, &buffer(&ADMIN, &changed), space, NOW).unwrap_err().contains("published"));
    // attack L11: a bigger build needs the account extended first
    assert!(check(&hash, &UPGRADEABLE_LOADER, &data, space - 1, NOW).unwrap_err().contains("extend"));
}

#[test]
fn a_buffer_with_no_authority_or_of_another_kind_reads_as_none() {
    assert_eq!(buffer_authority(&buffer(&ADMIN, &[])), Some(ADMIN));
    let mut none = buffer(&ADMIN, &[]);
    none[4] = 0;
    assert_eq!(buffer_authority(&none), None);
    let mut other = buffer(&ADMIN, &[]);
    other[0] = 3;
    assert_eq!(buffer_authority(&other), None);
    assert_eq!(buffer_authority(&[1, 0]), None);
}
