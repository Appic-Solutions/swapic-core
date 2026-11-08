//! Transaction messages: parse and compile v0 (the only form a user signs), and encode and parse
//! v1 (the only form the canister's arrivals use, SIMD-0385).

use crate::Pubkey;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;

pub const MAX_V0_TX_BYTES: usize = 1232;
pub const MAX_V1_TX_BYTES: usize = 4096;
/// No transaction of either form may lock more accounts than this today.
pub const MAX_TX_ACCOUNTS: usize = 64;
pub const VERSION_V0_BYTE: u8 = 0x80;
pub const VERSION_V1_BYTE: u8 = 129;

/// Why bytes are not a message.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MessageError {
    Truncated,
    TrailingBytes,
    NotV0,
    NotV1,
    BadHeader,
    BadIndex,
    TooManyInstructions,
    TooManyAccounts,
    DuplicateAccount,
    PayerNotSigner,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Header {
    pub required_signatures: u8,
    pub readonly_signed: u8,
    pub readonly_unsigned: u8,
}

/// One instruction by index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledInstruction {
    pub program_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableLookup {
    pub table: Pubkey,
    pub writable: Vec<u8>,
    pub readonly: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V0Message {
    pub header: Header,
    pub static_keys: Vec<Pubkey>,
    pub blockhash: Pubkey,
    pub instructions: Vec<CompiledInstruction>,
    pub lookups: Vec<TableLookup>,
}

/// One account of an instruction, by address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountMeta {
    pub pubkey: Pubkey,
    pub signer: bool,
    pub writable: bool,
}

impl AccountMeta {
    pub const fn readonly(pubkey: Pubkey) -> Self {
        Self { pubkey, signer: false, writable: false }
    }
    pub const fn writable(pubkey: Pubkey) -> Self {
        Self { pubkey, signer: false, writable: true }
    }
    pub const fn signer(pubkey: Pubkey) -> Self {
        Self { pubkey, signer: true, writable: false }
    }
    pub const fn writable_signer(pubkey: Pubkey) -> Self {
        Self { pubkey, signer: true, writable: true }
    }
}

/// One instruction, by address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub program: Pubkey,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

/// A lookup table with its entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupTable {
    pub address: Pubkey,
    pub entries: Vec<Pubkey>,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MessageError> {
        if self.bytes.len() < n {
            return Err(MessageError::Truncated);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, MessageError> {
        Ok(self.take(1)?[0])
    }
    fn key(&mut self) -> Result<Pubkey, MessageError> {
        Ok(self.take(32)?.try_into().expect("32 bytes"))
    }
    /// Compact-u16: at most three bytes, canonical.
    fn short(&mut self) -> Result<usize, MessageError> {
        let mut value = 0usize;
        for i in 0..3 {
            let byte = self.u8()? as usize;
            if i == 2 && byte > 3 {
                return Err(MessageError::Truncated);
            }
            value |= (byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                if i > 0 && byte == 0 {
                    return Err(MessageError::Truncated);
                }
                return Ok(value);
            }
        }
        Err(MessageError::Truncated)
    }
}

fn push_short(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

impl V0Message {
    /// Strict: no trailing bytes, sane header, every index inside the key space.
    pub fn parse(message: &[u8]) -> Result<Self, MessageError> {
        let mut r = Reader { bytes: message };
        if r.u8()? != VERSION_V0_BYTE {
            return Err(MessageError::NotV0);
        }
        let header =
            Header { required_signatures: r.u8()?, readonly_signed: r.u8()?, readonly_unsigned: r.u8()? };
        let key_count = r.short()?;
        let mut static_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            static_keys.push(r.key()?);
        }
        if header.required_signatures == 0
            || header.readonly_signed >= header.required_signatures
            || header.required_signatures as usize > key_count
            || header.readonly_unsigned as usize > key_count - header.required_signatures as usize
        {
            return Err(MessageError::BadHeader);
        }
        let blockhash = r.key()?;
        let instruction_count = r.short()?;
        let mut instructions = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            let program_index = r.u8()?;
            let account_count = r.short()?;
            let accounts = r.take(account_count)?.to_vec();
            let data_len = r.short()?;
            let data = r.take(data_len)?.to_vec();
            instructions.push(CompiledInstruction { program_index, accounts, data });
        }
        let lookup_count = r.short()?;
        let mut lookups = Vec::with_capacity(lookup_count);
        for _ in 0..lookup_count {
            let table = r.key()?;
            let writable_len = r.short()?;
            let writable = r.take(writable_len)?.to_vec();
            let readonly_len = r.short()?;
            let readonly = r.take(readonly_len)?.to_vec();
            lookups.push(TableLookup { table, writable, readonly });
        }
        if !r.bytes.is_empty() {
            return Err(MessageError::TrailingBytes);
        }
        let parsed = Self { header, static_keys, blockhash, instructions, lookups };
        let unique: BTreeSet<&Pubkey> = parsed.static_keys.iter().collect();
        if unique.len() != parsed.static_keys.len() {
            return Err(MessageError::DuplicateAccount);
        }
        let total = parsed.total_accounts();
        for ix in &parsed.instructions {
            // a program is always a static key: a lookup table cannot name one
            if ix.program_index == 0 || ix.program_index as usize >= parsed.static_keys.len() {
                return Err(MessageError::BadIndex);
            }
            if ix.accounts.iter().any(|a| *a as usize >= total) {
                return Err(MessageError::BadIndex);
            }
        }
        Ok(parsed)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.push(VERSION_V0_BYTE);
        out.extend_from_slice(&[
            self.header.required_signatures,
            self.header.readonly_signed,
            self.header.readonly_unsigned,
        ]);
        push_short(&mut out, self.static_keys.len());
        for key in &self.static_keys {
            out.extend_from_slice(key);
        }
        out.extend_from_slice(&self.blockhash);
        push_short(&mut out, self.instructions.len());
        for ix in &self.instructions {
            out.push(ix.program_index);
            push_short(&mut out, ix.accounts.len());
            out.extend_from_slice(&ix.accounts);
            push_short(&mut out, ix.data.len());
            out.extend_from_slice(&ix.data);
        }
        push_short(&mut out, self.lookups.len());
        for lookup in &self.lookups {
            out.extend_from_slice(&lookup.table);
            push_short(&mut out, lookup.writable.len());
            out.extend_from_slice(&lookup.writable);
            push_short(&mut out, lookup.readonly.len());
            out.extend_from_slice(&lookup.readonly);
        }
        out
    }

    pub fn total_accounts(&self) -> usize {
        self.static_keys.len()
            + self.lookups.iter().map(|l| l.writable.len() + l.readonly.len()).sum::<usize>()
    }

    /// True when the key at this index signs.
    pub fn is_signer(&self, index: usize) -> bool {
        index < self.header.required_signatures as usize
    }

    /// True when the static key at this index may be written.
    pub fn is_static_writable(&self, index: usize) -> bool {
        let keys = self.static_keys.len();
        let signers = self.header.required_signatures as usize;
        if index >= keys {
            return false;
        }
        if index < signers {
            index < signers - self.header.readonly_signed as usize
        } else {
            index < keys - self.header.readonly_unsigned as usize
        }
    }

    /// The whole size on the wire with its signatures.
    pub fn wire_size(&self) -> usize {
        let signatures = self.header.required_signatures as usize;
        let mut prefix = Vec::with_capacity(3);
        push_short(&mut prefix, signatures);
        prefix.len() + signatures * 64 + self.encode().len()
    }
}

/// A signed transaction of the legacy wire shape (v0 included), borrowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedV0<'a> {
    pub signatures: Vec<[u8; 64]>,
    pub message: &'a [u8],
}

impl<'a> SignedV0<'a> {
    pub fn parse(wire: &'a [u8]) -> Result<Self, MessageError> {
        let mut r = Reader { bytes: wire };
        let count = r.short()?;
        let mut signatures = Vec::with_capacity(count);
        for _ in 0..count {
            signatures.push(r.take(64)?.try_into().expect("64 bytes"));
        }
        Ok(Self { signatures, message: r.bytes })
    }
}

pub fn wire_v0(signatures: &[[u8; 64]], message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + signatures.len() * 64 + message.len());
    push_short(&mut out, signatures.len());
    for signature in signatures {
        out.extend_from_slice(signature);
    }
    out.extend_from_slice(message);
    out
}

/// Sorts a message's accounts by the order every Solana message wants: signers first (the payer
/// the very first), writable before readonly in each class, then the entries a table provides.
struct KeySpace {
    /// (address, signer, writable, invoked)
    entries: Vec<(Pubkey, bool, bool, bool)>,
}

impl KeySpace {
    fn of(payer: &Pubkey, instructions: &[Instruction]) -> Self {
        let mut entries: Vec<(Pubkey, bool, bool, bool)> = Vec::with_capacity(64);
        entries.push((*payer, true, true, false));
        let mut add = |key: &Pubkey, signer: bool, writable: bool, invoked: bool| {
            if let Some(found) = entries.iter_mut().find(|e| e.0 == *key) {
                found.1 |= signer;
                found.2 |= writable;
                found.3 |= invoked;
            } else {
                entries.push((*key, signer, writable, invoked));
            }
        };
        for ix in instructions {
            add(&ix.program, false, false, true);
            for meta in &ix.accounts {
                add(&meta.pubkey, meta.signer, meta.writable, false);
            }
        }
        Self { entries }
    }
}

/// Compiles a v0 message. An account that is neither a signer nor invoked comes from the first
/// table that holds it; everything else stays a static key.
pub fn compile_v0(
    payer: &Pubkey,
    instructions: &[Instruction],
    tables: &[LookupTable],
    blockhash: &Pubkey,
) -> Result<V0Message, MessageError> {
    let space = KeySpace::of(payer, instructions);
    let table_of = |key: &Pubkey| -> Option<(usize, u8)> {
        tables.iter().enumerate().find_map(|(t, table)| {
            let index = table.entries.iter().position(|e| e == key)?;
            u8::try_from(index).ok().map(|i| (t, i))
        })
    };

    let mut signers_w = Vec::new();
    let mut signers_r = Vec::new();
    let mut plain_w = Vec::new();
    let mut plain_r = Vec::new();
    let mut lookups: Vec<TableLookup> = tables
        .iter()
        .map(|t| TableLookup { table: t.address, writable: Vec::new(), readonly: Vec::new() })
        .collect();
    let mut loaded_w: Vec<Vec<Pubkey>> = tables.iter().map(|_| Vec::new()).collect();
    let mut loaded_r: Vec<Vec<Pubkey>> = tables.iter().map(|_| Vec::new()).collect();

    for (key, signer, writable, invoked) in &space.entries {
        match (*signer, *writable) {
            (true, true) => signers_w.push(*key),
            (true, false) => signers_r.push(*key),
            (false, writable) => {
                let from_table = if *invoked { None } else { table_of(key) };
                match (from_table, writable) {
                    (Some((t, index)), true) => {
                        lookups[t].writable.push(index);
                        loaded_w[t].push(*key);
                    }
                    (Some((t, index)), false) => {
                        lookups[t].readonly.push(index);
                        loaded_r[t].push(*key);
                    }
                    (None, true) => plain_w.push(*key),
                    (None, false) => plain_r.push(*key),
                }
            }
        }
    }
    if signers_w.first() != Some(payer) {
        return Err(MessageError::PayerNotSigner);
    }
    let required = signers_w.len() + signers_r.len();
    let readonly_signed = signers_r.len();
    let readonly_unsigned = plain_r.len();
    let mut static_keys = signers_w;
    static_keys.append(&mut signers_r);
    static_keys.append(&mut plain_w);
    static_keys.append(&mut plain_r);

    // the key space: static keys, then each table's writable entries, then each table's readonly ones
    let mut space_keys = static_keys.clone();
    for keys in &loaded_w {
        space_keys.extend_from_slice(keys);
    }
    for keys in &loaded_r {
        space_keys.extend_from_slice(keys);
    }
    if space_keys.len() > 256 {
        return Err(MessageError::TooManyAccounts);
    }
    let index_of = |key: &Pubkey| -> Result<u8, MessageError> {
        space_keys.iter().position(|k| k == key).map(|i| i as u8).ok_or(MessageError::BadIndex)
    };
    let mut compiled = Vec::with_capacity(instructions.len());
    for ix in instructions {
        compiled.push(CompiledInstruction {
            program_index: index_of(&ix.program)?,
            accounts: ix.accounts.iter().map(|m| index_of(&m.pubkey)).collect::<Result<_, _>>()?,
            data: ix.data.clone(),
        });
    }
    lookups.retain(|l| !l.writable.is_empty() || !l.readonly.is_empty());
    Ok(V0Message {
        header: Header {
            required_signatures: required as u8,
            readonly_signed: readonly_signed as u8,
            readonly_unsigned: readonly_unsigned as u8,
        },
        static_keys,
        blockhash: *blockhash,
        instructions: compiled,
        lookups,
    })
}

/// The config values of a v1 message. The canister sets all of them, every time: a v1 message with
/// no compute limit gets a limit of 0, and one with no data limit is costed at 0 bytes loaded.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct V1Config {
    pub priority_lamports: u64,
    pub compute_unit_limit: u32,
    pub loaded_data_limit: u32,
}

const V1_CONFIG_MASK: u32 = 0b1111;

/// A v1 message: no lookup tables, its budget in its config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1Message {
    pub header: Header,
    pub config: V1Config,
    pub blockhash: Pubkey,
    pub addresses: Vec<Pubkey>,
    pub instructions: Vec<CompiledInstruction>,
}

impl V1Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2048);
        out.push(VERSION_V1_BYTE);
        out.extend_from_slice(&[
            self.header.required_signatures,
            self.header.readonly_signed,
            self.header.readonly_unsigned,
        ]);
        out.extend_from_slice(&V1_CONFIG_MASK.to_le_bytes());
        out.extend_from_slice(&self.blockhash);
        out.push(self.instructions.len() as u8);
        out.push(self.addresses.len() as u8);
        for address in &self.addresses {
            out.extend_from_slice(address);
        }
        out.extend_from_slice(&self.config.priority_lamports.to_le_bytes());
        out.extend_from_slice(&self.config.compute_unit_limit.to_le_bytes());
        out.extend_from_slice(&self.config.loaded_data_limit.to_le_bytes());
        for ix in &self.instructions {
            out.push(ix.program_index);
            out.push(ix.accounts.len() as u8);
            out.extend_from_slice(&(ix.data.len() as u16).to_le_bytes());
        }
        for ix in &self.instructions {
            out.extend_from_slice(&ix.accounts);
            out.extend_from_slice(&ix.data);
        }
        out
    }

    /// Parses a whole signed v1 transaction: the message, then its signatures last. Only the mask the
    /// canister writes is accepted, so anything else is not Swapic's own.
    pub fn parse_wire(wire: &[u8]) -> Result<(Self, Vec<[u8; 64]>), MessageError> {
        let mut r = Reader { bytes: wire };
        if r.u8()? != VERSION_V1_BYTE {
            return Err(MessageError::NotV1);
        }
        let header =
            Header { required_signatures: r.u8()?, readonly_signed: r.u8()?, readonly_unsigned: r.u8()? };
        let mask = u32::from_le_bytes(r.take(4)?.try_into().expect("4 bytes"));
        if mask != V1_CONFIG_MASK {
            return Err(MessageError::BadHeader);
        }
        let blockhash = r.key()?;
        let instruction_count = r.u8()? as usize;
        let address_count = r.u8()? as usize;
        let mut addresses = Vec::with_capacity(address_count);
        for _ in 0..address_count {
            addresses.push(r.key()?);
        }
        let priority_lamports = u64::from_le_bytes(r.take(8)?.try_into().expect("8 bytes"));
        let compute_unit_limit = u32::from_le_bytes(r.take(4)?.try_into().expect("4 bytes"));
        let loaded_data_limit = u32::from_le_bytes(r.take(4)?.try_into().expect("4 bytes"));
        let mut headers = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            let program_index = r.u8()?;
            let accounts = r.u8()? as usize;
            let data = u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")) as usize;
            headers.push((program_index, accounts, data));
        }
        let mut instructions = Vec::with_capacity(instruction_count);
        for (program_index, accounts, data) in headers {
            instructions.push(CompiledInstruction {
                program_index,
                accounts: r.take(accounts)?.to_vec(),
                data: r.take(data)?.to_vec(),
            });
        }
        let mut signatures = Vec::with_capacity(header.required_signatures as usize);
        for _ in 0..header.required_signatures {
            signatures.push(r.take(64)?.try_into().expect("64 bytes"));
        }
        if !r.bytes.is_empty() {
            return Err(MessageError::TrailingBytes);
        }
        let config = V1Config { priority_lamports, compute_unit_limit, loaded_data_limit };
        Ok((Self { header, config, blockhash, addresses, instructions }, signatures))
    }
}

/// The whole v1 transaction: signatures come after the message (SIMD-0385).
pub fn wire_v1(message: &[u8], signatures: &[[u8; 64]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + signatures.len() * 64);
    out.extend_from_slice(message);
    for signature in signatures {
        out.extend_from_slice(signature);
    }
    out
}

pub fn compile_v1(
    payer: &Pubkey,
    instructions: &[Instruction],
    blockhash: &Pubkey,
    config: V1Config,
) -> Result<V1Message, MessageError> {
    let v0 = compile_v0(payer, instructions, &[], blockhash)?;
    if v0.static_keys.len() > MAX_TX_ACCOUNTS {
        return Err(MessageError::TooManyAccounts);
    }
    if v0.instructions.len() > MAX_TX_ACCOUNTS {
        return Err(MessageError::TooManyInstructions);
    }
    Ok(V1Message {
        header: v0.header,
        config,
        blockhash: v0.blockhash,
        addresses: v0.static_keys,
        instructions: v0.instructions,
    })
}

#[cfg(test)]
mod tests;
