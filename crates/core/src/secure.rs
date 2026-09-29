//! Per-connection SQLite column encryption. The Kotlin side supplies a random
//! 32-byte key unwrapped by Keystore; this module never persists that key.
use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, KeyInit, Nonce,
};
use rusqlite::{
    functions::FunctionFlags,
    types::{Value, ValueRef},
    Connection,
};

const MAGIC: &[u8] = b"DMSG-S1";
const CHECK: &[u8] = b"dmsg local storage key v1";

fn seal(key: &[u8; 32], field: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| "storage random failure")?;
    let cipher = ChaCha20Poly1305::new(key.into());
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: field,
            },
        )
        .map_err(|_| "storage encryption failure")?;
    let mut out = Vec::with_capacity(MAGIC.len() + nonce.len() + encrypted.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    Ok(out)
}

fn unseal(key: &[u8; 32], field: &[u8], value: &[u8]) -> Result<Vec<u8>, String> {
    if value.len() < MAGIC.len() + 12 + 16 || !value.starts_with(MAGIC) {
        return Err("storage ciphertext format error".into());
    }
    let start = MAGIC.len();
    ChaCha20Poly1305::new(key.into())
        .decrypt(
            Nonce::from_slice(&value[start..start + 12]),
            Payload {
                msg: &value[start + 12..],
                aad: field,
            },
        )
        .map_err(|_| "storage authentication failed".into())
}

fn data(value: ValueRef<'_>) -> rusqlite::Result<&[u8]> {
    match value {
        ValueRef::Text(b) | ValueRef::Blob(b) => Ok(b),
        _ => Err(rusqlite::Error::UserFunctionError(
            "storage value type error".into(),
        )),
    }
}

/// Functions retain the key only for the lifetime of this connection. Neither
/// function is deterministic: a fresh nonce is generated for every write.
pub(crate) fn register(conn: &Connection, key: Option<[u8; 32]>) -> Result<(), String> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
    conn.create_scalar_function("dmsg_seal", 2, flags, move |ctx| {
        let field = data(ctx.get_raw(0))?;
        let value = ctx.get_raw(1);
        match key {
            Some(k) => seal(&k, field, data(value)?)
                .map(Value::Blob)
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into())),
            None => unchanged(value),
        }
    })
    .map_err(|_| "register storage seal failed")?;
    conn.create_scalar_function("dmsg_unseal", 2, flags, move |ctx| {
        let field = data(ctx.get_raw(0))?;
        let value = ctx.get_raw(1);
        match key {
            Some(k) => unseal(&k, field, data(value)?)
                .map(Value::Blob)
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into())),
            None => unchanged(value),
        }
    })
    .map_err(|_| "register storage unseal failed")?;
    Ok(())
}

fn unchanged(value: ValueRef<'_>) -> rusqlite::Result<Value> {
    match value {
        ValueRef::Text(v) => String::from_utf8(v.to_vec())
            .map(Value::Text)
            .map_err(|_| rusqlite::Error::UserFunctionError("storage text error".into())),
        ValueRef::Blob(v) => Ok(Value::Blob(v.to_vec())),
        _ => Err(rusqlite::Error::UserFunctionError(
            "storage value type error".into(),
        )),
    }
}

pub(crate) fn verifier(key: &[u8; 32]) -> Result<Vec<u8>, String> {
    seal(key, b"storage-check", CHECK)
}

pub(crate) fn verify(key: &[u8; 32], value: &[u8]) -> Result<(), String> {
    if unseal(key, b"storage-check", value).as_deref() == Ok(CHECK) {
        Ok(())
    } else {
        Err("wrong storage key or corrupted storage marker".into())
    }
}
