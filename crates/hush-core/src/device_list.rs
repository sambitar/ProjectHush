use libsignal_protocol::{IdentityKey, IdentityKeyPair};
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const DEVICE_LIST_VERSION: u32 = 1;

/// XEd25519 signatures from libsignal are a fixed 64 bytes, so the signature can
/// be taken from the tail of the blob rather than found with a delimiter.
const SIGNATURE_LEN: usize = 64;

/// One device belonging to an account.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: u32,
    /// The device's own libsignal identity key, serialized.
    pub identity_key: Vec<u8>,
    pub registration_id: u32,
    /// That device's relay inbox. Peers deliver here.
    pub queue_id: String,
    pub queue_token: String,
    pub added_unix: u64,
}

/// The public, signed list of an account's devices.
///
/// Published to the relay so a peer can learn every place it has to deliver to.
/// Signed by **device 1's** identity key (Q11): the account id *is* device 1's
/// identity key hash (Q7), so a peer verifies with the one value it already
/// pinned and needs no extra trust material.
///
/// A linked device cannot sign this, because device 1's private key is never
/// transferred. That is deliberate, and it is why device 1 has to be present for
/// any change to the list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceList {
    pub v: u32,
    /// Device 1's identity key hash, which is also the `protocol_name` peers
    /// know this account by.
    pub account_id: String,
    pub devices: Vec<DeviceRecord>,
    /// Bumped on every revocation.
    ///
    /// Peers cache device lists, so they need a way to tell a stale cache from a
    /// current one. Authorization does *not* depend on this: only device 1 can
    /// produce a valid signature at all, and that is what stops a revoked device
    /// re-publishing itself.
    pub epoch: u64,
    pub updated_unix: u64,
}

impl DeviceList {
    pub fn new(account_id: String, devices: Vec<DeviceRecord>) -> Self {
        Self {
            v: DEVICE_LIST_VERSION,
            account_id,
            devices,
            epoch: 0,
            updated_unix: now_unix(),
        }
    }

    /// Add a device, replacing any existing record with the same id.
    ///
    /// Adding does not rotate the token epoch: the new device is handed the
    /// current token when it links, and only revocation needs to invalidate it.
    pub fn add(&mut self, record: DeviceRecord) {
        self.devices.retain(|d| d.device_id != record.device_id);
        self.devices.push(record);
        self.devices.sort_by_key(|d| d.device_id);
        self.updated_unix = now_unix();
    }

    /// Remove a device and bump the epoch.
    pub fn without(&self, device_id: u32) -> Self {
        let mut next = self.clone();
        next.devices.retain(|d| d.device_id != device_id);
        next.epoch += 1;
        next.updated_unix = now_unix();
        next
    }

    pub fn find(&self, device_id: u32) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| d.device_id == device_id)
    }

    /// Serialize, then append the signature over those exact bytes.
    ///
    /// The payload is compact JSON, so it carries no raw newline, but the
    /// signature is taken by length rather than by scanning for a delimiter: a
    /// delimiter that could appear inside the payload is a parsing bug waiting
    /// to happen.
    pub fn sign(&self, key: &IdentityKeyPair) -> Result<Vec<u8>, Error> {
        let payload = serde_json::to_vec(self).map_err(|_| Error::Corrupt)?;
        let mut csprng = OsRng.unwrap_err();
        let signature = key
            .private_key()
            .calculate_signature(&payload, &mut csprng)
            .map_err(|_| Error::Corrupt)?;
        if signature.len() != SIGNATURE_LEN {
            return Err(Error::Corrupt);
        }
        let mut out = payload;
        out.extend_from_slice(&signature);
        Ok(out)
    }

    /// Verify a signed blob against the account id a peer already pinned.
    ///
    /// Returns the list only if the signature is valid, so a relay that swapped
    /// in a list of its own cannot get it accepted.
    pub fn verify(signed: &[u8], expected_account_id: &str) -> Result<Self, Error> {
        if signed.len() <= SIGNATURE_LEN {
            return Err(Error::Corrupt);
        }
        let (payload, signature) = signed.split_at(signed.len() - SIGNATURE_LEN);
        let list: Self = serde_json::from_slice(payload).map_err(|_| Error::Corrupt)?;
        if list.v != DEVICE_LIST_VERSION {
            return Err(Error::Corrupt);
        }
        if list.account_id != expected_account_id {
            return Err(Error::Corrupt);
        }
        let key_bytes = hex::decode(expected_account_id).map_err(|_| Error::Corrupt)?;
        let key = IdentityKey::try_from(key_bytes.as_slice()).map_err(|_| Error::Corrupt)?;
        if !key.public_key().verify_signature(payload, signature) {
            return Err(Error::Corrupt);
        }
        Ok(list)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
