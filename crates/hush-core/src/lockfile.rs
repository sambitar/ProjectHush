use serde::{Deserialize, Serialize};

/// Vault unlock header.
///
/// Format v1 derived the master key straight from the passphrase. That made a
/// passphrase change impossible to do cheaply: the master key encrypts
/// `identity.secret`, every blob, `account.enc` and `chat.enc`, so rotating the
/// passphrase meant re-encrypting all of it — and it would have invalidated any
/// recovery wrap, because a recovery code pins the master key.
///
/// Format v2 generates the master key at random and stores it wrapped by a key
/// derived from the passphrase. Rotating the passphrase then rewrites this file
/// only. `wrapped_master` is absent in v1 vaults, which is how the two are told
/// apart on open.
#[derive(Serialize, Deserialize)]
pub struct LockFile {
    pub kdf: String,
    pub salt_hex: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_master: Option<String>,
}
