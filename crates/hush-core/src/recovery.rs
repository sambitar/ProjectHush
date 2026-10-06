use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{
    decrypt, derive_master_key, encrypt, random_bytes, MASTER_KEY_LEN, SALT_LEN,
};
use crate::error::Error;

const RECOVERY_LOCK: &str = "recovery.lock.json";
const RECOVERY_WRAP: &str = "recovery.enc";

#[derive(Serialize, Deserialize)]
struct RecoveryLock {
    kdf: String,
    salt_hex: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

/// Human-readable groups (32 random bytes → 16 hex groups of 4).
pub fn generate_recovery_code() -> String {
    let bytes = random_bytes::<32>();
    let hex = hex::encode(bytes);
    hex.chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn recovery_files_exist(dir: &Path) -> bool {
    dir.join(RECOVERY_WRAP).exists() && dir.join(RECOVERY_LOCK).exists()
}

pub fn ensure_recovery_files(
    dir: &Path,
    master: &[u8; MASTER_KEY_LEN],
    code: &str,
) -> Result<(), Error> {
    let salt = random_bytes::<SALT_LEN>();
    let params = crate::crypto::argon2_params()?;
    let lock = RecoveryLock {
        kdf: "argon2id".into(),
        salt_hex: hex::encode(salt),
        m_cost: params.m_cost(),
        t_cost: params.t_cost(),
        p_cost: params.p_cost(),
    };
    fs::write(
        dir.join(RECOVERY_LOCK),
        serde_json::to_vec_pretty(&lock).map_err(|_| Error::Corrupt)?,
    )?;
    let wrap_key = recovery_wrap_key(code, &lock)?;
    let packed = encrypt(&wrap_key, master)?;
    fs::write(dir.join(RECOVERY_WRAP), packed)?;
    Ok(())
}

pub fn unwrap_master_with_recovery(
    dir: &Path,
    code: &str,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, Error> {
    let lock_path = dir.join(RECOVERY_LOCK);
    let wrap_path = dir.join(RECOVERY_WRAP);
    if !lock_path.exists() || !wrap_path.exists() {
        return Err(Error::NotFound);
    }
    let lock: RecoveryLock =
        serde_json::from_slice(&fs::read(&lock_path)?).map_err(|_| Error::Corrupt)?;
    let wrap_key = recovery_wrap_key(code, &lock)?;
    let packed = fs::read(&wrap_path)?;
    let master_bytes = decrypt(&wrap_key, &packed)?;
    if master_bytes.len() != MASTER_KEY_LEN {
        return Err(Error::WrongRecoveryCode);
    }
    let mut master = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    master.copy_from_slice(&master_bytes);
    Ok(master)
}

fn recovery_wrap_key(
    code: &str,
    lock: &RecoveryLock,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, Error> {
    if lock.kdf != "argon2id" {
        return Err(Error::Corrupt);
    }
    let normalized = normalize_recovery_code(code);
    if normalized.len() < 32 {
        return Err(Error::WrongRecoveryCode);
    }
    let salt = hex::decode(&lock.salt_hex).map_err(|_| Error::Corrupt)?;
    derive_master_key(
        &normalized,
        &salt,
        lock.m_cost,
        lock.t_cost,
        lock.p_cost,
    )
}
