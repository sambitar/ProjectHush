use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use libsignal_protocol::PreKeyBundle;
use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const INVITE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitePayload {
    pub v: u32,
    pub relay: String,
    pub protocol_name: String,
    pub queue_id: String,
    pub queue_token: String,
    pub registration_id: u32,
    pub device_id: u32,
    pub identity_key: Vec<u8>,
    pub pre_key_id: Option<u32>,
    pub pre_key_public: Option<Vec<u8>>,
    pub signed_pre_key_id: u32,
    pub signed_pre_key_public: Vec<u8>,
    pub signed_pre_key_signature: Vec<u8>,
    pub kyber_pre_key_id: u32,
    pub kyber_pre_key_public: Vec<u8>,
    pub kyber_pre_key_signature: Vec<u8>,
}

impl InvitePayload {
    pub fn encode(&self) -> Result<String, Error> {
        let json = serde_json::to_vec(self).map_err(|_| Error::Corrupt)?;
        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(s: &str) -> Result<Self, Error> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s.trim())
            .map_err(|_| Error::Corrupt)?;
        let payload: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if payload.v != INVITE_VERSION {
            return Err(Error::UnsupportedInvite);
        }
        Ok(payload)
    }

    pub fn to_prekey_bundle(&self) -> Result<PreKeyBundle, Error> {
        use libsignal_protocol::{
            kem, DeviceId, IdentityKey, KyberPreKeyId, PreKeyId, PublicKey, SignedPreKeyId,
        };
        let device_id = DeviceId::try_from(self.device_id).map_err(|_| Error::Corrupt)?;
        let identity_key =
            IdentityKey::try_from(self.identity_key.as_slice()).map_err(|_| Error::Corrupt)?;
        let pre_key = match (self.pre_key_id, self.pre_key_public.as_ref()) {
            (Some(id), Some(pk)) => {
                let key = PublicKey::try_from(pk.as_slice()).map_err(|_| Error::Corrupt)?;
                Some((PreKeyId::from(id), key))
            }
            _ => None,
        };
        let signed_pk =
            PublicKey::try_from(self.signed_pre_key_public.as_slice()).map_err(|_| Error::Corrupt)?;
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
