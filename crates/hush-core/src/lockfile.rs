use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct LockFile {
    pub kdf: String,
    pub salt_hex: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}
