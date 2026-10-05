use libsignal_protocol::{
    GenericSignedPreKey, IdentityKey, IdentityKeyPair, InMemSignalProtocolStore, KyberPreKeyId,
    KyberPreKeyRecord, PreKeyId, PreKeyRecord, ProtocolAddress, SessionRecord, SignedPreKeyId,
    SignedPreKeyRecord,
};
use libsignal_protocol::{
    IdentityKeyStore, KyberPreKeyStore, PreKeyStore, SessionStore, SignedPreKeyStore,
};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::signal::device_id;

#[derive(Serialize, Deserialize)]
struct PersistedSignal {
    registration_id: u32,
    identity_key_pair: Vec<u8>,
    pre_keys: Vec<(u32, Vec<u8>)>,
    signed_pre_keys: Vec<(u32, Vec<u8>)>,
    kyber_pre_keys: Vec<(u32, Vec<u8>)>,
    sessions: Vec<(String, Vec<u8>)>,
    known_identities: Vec<(String, Vec<u8>)>,
}

pub async fn export_store(
    store: &InMemSignalProtocolStore,
    session_names: &[String],
) -> Result<Vec<u8>, Error> {
    let identity_key_pair = store
        .get_identity_key_pair()
        .await
        .map_err(|_| Error::Corrupt)?
        .serialize()
        .to_vec();
    let registration_id = store
        .get_local_registration_id()
        .await
        .map_err(|_| Error::Corrupt)?;

    let mut pre_keys = Vec::new();
    for id in store.all_pre_key_ids() {
        let record = store
            .get_pre_key(*id)
            .await
            .map_err(|_| Error::Corrupt)?;
        pre_keys.push((
            u32::from(*id),
            record.serialize().map_err(|_| Error::Corrupt)?,
        ));
    }

    let mut signed_pre_keys = Vec::new();
    for id in store.all_signed_pre_key_ids() {
        let record = store
            .get_signed_pre_key(*id)
            .await
            .map_err(|_| Error::Corrupt)?;
        signed_pre_keys.push((
            u32::from(*id),
            record.serialize().map_err(|_| Error::Corrupt)?,
        ));
    }

    let mut kyber_pre_keys = Vec::new();
    for id in store.all_kyber_pre_key_ids() {
        let record = store
            .get_kyber_pre_key(*id)
            .await
            .map_err(|_| Error::Corrupt)?;
        kyber_pre_keys.push((
            u32::from(*id),
            record.serialize().map_err(|_| Error::Corrupt)?,
        ));
    }

    let mut sessions = Vec::new();
    for name in session_names {
        let addr = ProtocolAddress::new(name.clone(), device_id());
        if let Some(record) = store
            .load_session(&addr)
            .await
            .map_err(|_| Error::Corrupt)?
        {
            sessions.push((
                name.clone(),
                record.serialize().map_err(|_| Error::Corrupt)?,
            ));
        }
    }

    let persisted = PersistedSignal {
        registration_id,
        identity_key_pair,
        pre_keys,
        signed_pre_keys,
        kyber_pre_keys,
        sessions,
        known_identities: Vec::new(),
    };
    serde_json::to_vec(&persisted).map_err(|_| Error::Corrupt)
}

pub async fn import_store(bytes: &[u8]) -> Result<InMemSignalProtocolStore, Error> {
    let persisted: PersistedSignal = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
    let identity_key_pair =
        IdentityKeyPair::try_from(persisted.identity_key_pair.as_slice()).map_err(|_| Error::Corrupt)?;
    let mut store =
        InMemSignalProtocolStore::new(identity_key_pair, persisted.registration_id)
            .map_err(|_| Error::Corrupt)?;

    for (id, bytes) in persisted.pre_keys {
        let record = PreKeyRecord::deserialize(&bytes).map_err(|_| Error::Corrupt)?;
        store
            .save_pre_key(PreKeyId::from(id), &record)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    for (id, bytes) in persisted.signed_pre_keys {
        let record = SignedPreKeyRecord::deserialize(&bytes).map_err(|_| Error::Corrupt)?;
        store
            .save_signed_pre_key(SignedPreKeyId::from(id), &record)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    for (id, bytes) in persisted.kyber_pre_keys {
        let record = KyberPreKeyRecord::deserialize(&bytes).map_err(|_| Error::Corrupt)?;
        store
            .save_kyber_pre_key(KyberPreKeyId::from(id), &record)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    for (name, bytes) in persisted.sessions {
        let addr = ProtocolAddress::new(name, device_id());
        let record = SessionRecord::deserialize(&bytes).map_err(|_| Error::Corrupt)?;
        store
            .store_session(&addr, &record)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    for (name, bytes) in persisted.known_identities {
        let addr = ProtocolAddress::new(name, device_id());
        let key = IdentityKey::try_from(bytes.as_slice()).map_err(|_| Error::Corrupt)?;
        store
            .save_identity(&addr, &key)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    Ok(store)
}
