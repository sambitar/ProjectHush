//! Publishing an account's prekey material so a peer can open a session with it.
//!
//! An invite carries a bundle out of band, and that works because you exchange
//! the invite with the one device you are adding. The relay's published record
//! is the same thing for a peer that has an account id but no invite — it is not
//! a multi-device feature.
//!
//! The relay stores one record per account and verifies it before accepting it,
//! but it is not trusted with the contents: a peer re-checks the same signatures
//! independently. What the relay's check buys is that an account's published
//! prekeys cannot be replaced by someone who merely knows its public account id.

use libsignal_protocol::{
    kem, DeviceId, IdentityKey, KyberPreKeyId, PreKeyBundle, PreKeyId, PublicKey, SignedPreKeyId,
};
use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const PREKEY_RECORD_VERSION: u32 = 1;

/// One one-time prekey. Consumed by exactly one fetch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OneTimePreKey {
    pub key_id: u32,
    pub public_key: Vec<u8>,
}

/// An account's published prekey material: everything a peer needs to open a
/// session without having exchanged an invite with that account.
///
/// The one-time prekeys are a *pool* rather than a single key. A fetch takes one
/// and the relay drops it, so two peers never open sessions against the same
/// key. When the pool runs dry a fetch still succeeds using the signed prekey
/// alone — libsignal accepts a bundle with no one-time prekey — it simply loses
/// the extra forward secrecy for that first message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreKeyRecord {
    pub v: u32,
    /// The account's one device. Kept in the record so the wire format does not
    /// change shape, but deliberately not trusted: [`PreKeyRecord::to_prekey_bundle`]
    /// uses [`crate::device::DEVICE_ID`] instead, so a relay that altered this
    /// field could not redirect a session to a device that does not exist.
    pub device_id: u32,
    pub registration_id: u32,
    pub identity_key: Vec<u8>,
    pub signed_pre_key_id: u32,
    pub signed_pre_key_public: Vec<u8>,
    pub signed_pre_key_signature: Vec<u8>,
    pub kyber_pre_key_id: u32,
    pub kyber_pre_key_public: Vec<u8>,
    pub kyber_pre_key_signature: Vec<u8>,
    #[serde(default)]
    pub one_time_prekeys: Vec<OneTimePreKey>,
}

impl PreKeyRecord {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|_| Error::Corrupt)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
        if record.v != PREKEY_RECORD_VERSION {
            return Err(Error::Corrupt);
        }
        Ok(record)
    }

    /// Take the next one-time prekey, if the pool has one.
    pub fn pop_one_time_prekey(&mut self) -> Option<OneTimePreKey> {
        if self.one_time_prekeys.is_empty() {
            None
        } else {
            Some(self.one_time_prekeys.remove(0))
        }
    }

    /// Check the record is internally consistent and belongs to the identity key
    /// we already trust for this account.
    ///
    /// `expected_identity_key` is the key the contact pinned in their invite (or,
    /// for the relay, the key the account id decodes to). The two signature
    /// checks are then what prove the *publisher* held the identity private key:
    /// a valid XEd25519 signature over the signed prekey cannot be produced
    /// without it, and the identity key on its own is public. So knowing a
    /// public account id does not let anyone replace that account's prekeys.
    pub fn verify(&self, expected_identity_key: &[u8]) -> Result<(), Error> {
        if self.identity_key != expected_identity_key {
            return Err(Error::Corrupt);
        }
        let identity = IdentityKey::try_from(expected_identity_key).map_err(|_| Error::Corrupt)?;
        if !identity
            .public_key()
            .verify_signature(&self.signed_pre_key_public, &self.signed_pre_key_signature)
        {
            return Err(Error::Corrupt);
        }
        if !identity
            .public_key()
            .verify_signature(&self.kyber_pre_key_public, &self.kyber_pre_key_signature)
        {
            return Err(Error::Corrupt);
        }
        Ok(())
    }

    /// Build the libsignal bundle a peer hands to `process_prekey_bundle`.
    ///
    /// Uses the first one-time prekey if the relay returned one, and falls back
    /// to the signed prekey alone if not. The device id comes from
    /// [`crate::device::DEVICE_ID`] rather than the record: an account has one
    /// device, so the field is not worth trusting a relay over.
    pub fn to_prekey_bundle(&self) -> Result<PreKeyBundle, Error> {
        let device_id =
            DeviceId::try_from(crate::device::DEVICE_ID).map_err(|_| Error::Corrupt)?;
        let identity_key =
            IdentityKey::try_from(self.identity_key.as_slice()).map_err(|_| Error::Corrupt)?;
        let pre_key = match self.one_time_prekeys.first() {
            Some(otp) => {
                let key =
                    PublicKey::try_from(otp.public_key.as_slice()).map_err(|_| Error::Corrupt)?;
                Some((PreKeyId::from(otp.key_id), key))
            }
            None => None,
        };
        let signed_pk = PublicKey::try_from(self.signed_pre_key_public.as_slice())
            .map_err(|_| Error::Corrupt)?;
        let kyber_pk = kem::PublicKey::try_from(self.kyber_pre_key_public.as_slice())
            .map_err(|_| Error::Corrupt)?;
        PreKeyBundle::new(
            self.registration_id,
            device_id,
            pre_key,
            SignedPreKeyId::from(self.signed_pre_key_id),
            signed_pk,
            self.signed_pre_key_signature.clone(),
            KyberPreKeyId::from(self.kyber_pre_key_id),
            kyber_pk,
            self.kyber_pre_key_signature.clone(),
            identity_key,
        )
        .map_err(|_| Error::Corrupt)
    }
}
