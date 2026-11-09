//! Signing and recording a Solana transaction (design 5.2.3). Broadcast is the watcher's, as on EVM:
//! it reads the wire bytes from `signed_txs`. A signature is not deterministic, so one is taken once
//! per message and stored; a new attempt is a new message.

use super::Deployment;
use crate::ed25519;
use crate::guards::require_not_halted;
use crate::storage::events::append_event;
use settlement_api::types::solana::SolanaLifetime;
use swapic_solana_wire::base58;
use swapic_solana_wire::message::{
    compile_v0, compile_v1, wire_v0, wire_v1, Instruction, LookupTable, V1Config, MAX_TX_ACCOUNTS,
    MAX_V0_TX_BYTES, MAX_V1_TX_BYTES,
};
use types::events::EventType;
use types::solana::{SolanaKeyRole, SolanaPurpose, SolanaStep, SolanaTxVersion};

/// A lifetime the watcher named: a wrong one only makes the transaction invalid.
pub fn blockhash_of(lifetime: &SolanaLifetime) -> Result<[u8; 32], String> {
    base58::decode_pubkey(&lifetime.blockhash)
        .ok_or_else(|| "the blockhash is not base58".to_string())
}

/// A message ready to sign, its signers in key order.
pub struct Compiled {
    pub bytes: Vec<u8>,
    pub version: SolanaTxVersion,
    pub signers: Vec<SolanaKeyRole>,
}

/// Which of the canister's keys each of a message's signers is. A signer that is neither is refused:
/// the canister builds these messages, so a foreign signer is a bug.
fn roles_of(
    keys: &[[u8; 32]],
    required: usize,
    d: &Deployment,
) -> Result<Vec<SolanaKeyRole>, String> {
    keys.iter()
        .take(required)
        .map(|key| {
            if *key == d.a.canister {
                Ok(SolanaKeyRole::Payer)
            } else if *key == d.a.admin {
                Ok(SolanaKeyRole::Admin)
            } else {
                Err("a signer of this message is not one of the canister's keys".to_string())
            }
        })
        .collect()
}

/// A v1 message from the canister's payer key, every config value set.
pub fn compile_v1_from_payer(
    d: &Deployment,
    instructions: &[Instruction],
    cu_limit: u32,
    lifetime: &SolanaLifetime,
) -> Result<Compiled, String> {
    let config = V1Config {
        priority_lamports: d.config.max_priority_lamports,
        compute_unit_limit: cu_limit.min(d.config.max_cu_limit),
        loaded_data_limit: d.config.loaded_data_limit,
    };
    let message = compile_v1(
        &d.a.canister,
        instructions,
        &blockhash_of(lifetime)?,
        config,
    )
    .map_err(|e| format!("the v1 message does not compile: {e:?}"))?;
    let bytes = message.encode();
    let signatures = message.header.required_signatures as usize;
    if bytes.len() + signatures * 64 > MAX_V1_TX_BYTES {
        return Err(format!(
            "the v1 transaction is {} bytes",
            bytes.len() + signatures * 64
        ));
    }
    Ok(Compiled {
        signers: roles_of(&message.addresses, signatures, d)?,
        bytes,
        version: SolanaTxVersion::V1,
    })
}

/// A v0 message from the canister's payer key against the frozen Swapic table (design decision 8).
pub fn compile_v0_from_payer(
    d: &Deployment,
    table: &LookupTable,
    instructions: &[Instruction],
    lifetime: &SolanaLifetime,
) -> Result<Compiled, String> {
    let message = compile_v0(
        &d.a.canister,
        instructions,
        std::slice::from_ref(table),
        &blockhash_of(lifetime)?,
    )
    .map_err(|e| format!("the v0 message does not compile: {e:?}"))?;
    if message.wire_size() > MAX_V0_TX_BYTES || message.total_accounts() > MAX_TX_ACCOUNTS {
        return Err(format!(
            "the v0 transaction is {} bytes and {} accounts",
            message.wire_size(),
            message.total_accounts()
        ));
    }
    let required = message.header.required_signatures as usize;
    Ok(Compiled {
        signers: roles_of(&message.static_keys, required, d)?,
        bytes: message.encode(),
        version: SolanaTxVersion::V0,
    })
}

/// Signs with every key the message names, then writes the line the watcher broadcasts. Answers its
/// id in `signed_txs`. The halt is read again after the signatures: no await follows it.
pub async fn sign_and_record(
    d: &Deployment,
    purpose: SolanaPurpose,
    compiled: Compiled,
    last_valid_block_height: u64,
) -> Result<u64, String> {
    let mut signatures = Vec::with_capacity(compiled.signers.len());
    for role in &compiled.signers {
        signatures.push(ed25519::sign(*role, &d.config.key_name, compiled.bytes.clone()).await?);
    }
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let raw = match compiled.version {
        SolanaTxVersion::V0 => wire_v0(&signatures, &compiled.bytes),
        SolanaTxVersion::V1 => wire_v1(&compiled.bytes, &signatures),
    };
    record(
        purpose,
        compiled.version,
        signatures[0],
        raw,
        last_valid_block_height,
    )
}

/// Writes a signed transaction's line.
pub fn record(
    purpose: SolanaPurpose,
    version: SolanaTxVersion,
    signature: [u8; 64],
    raw: Vec<u8>,
    last_valid_block_height: u64,
) -> Result<u64, String> {
    append_event(EventType::Solana {
        step: SolanaStep::Signed {
            purpose,
            version,
            signature,
            raw,
            last_valid_block_height,
        },
    })
    .map(|index| index.get())
    .map_err(|e| e.to_string())
}
