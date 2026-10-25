use libsignal_protocol::{
    DeviceId, GenericSignedPreKey, IdentityKey, IdentityKeyPair, InMemSignalProtocolStore,
    KyberPreKeyId, KyberPreKeyRecord, PreKeyId, PreKeyRecord, ProtocolAddress, SessionRecord,
    SignedPreKeyId, SignedPreKeyRecord,
};
use libsignal_protocol::{
    IdentityKeyStore, KyberPreKeyStore, PreKeyStore, SessionStore, SignedPreKeyStore,
};
use serde::{Deserialize, Serialize};

use crate::error::Error;

#[derive(Serialize, Deserialize)]
struct PersistedSignal {
    registration_id: u32,
    identity_key_pair: Vec<u8>,
    pre_keys: Vec<(u32, Vec<u8>)>,
    signed_pre_keys: Vec<(u32, Vec<u8>)>,
    kyber_pre_keys: Vec<(u32, Vec<u8>)>,
    sessions: Vec<SessionEntry>,
    known_identities: Vec<IdentityEntry>,
}

/// One persisted session.
///
/// The address a session lives under is an account id *and* a device id, and both
/// have to survive a reload or a device could end up ratcheting with the wrong
/// one. The legacy shape is still read: before multi-device there was only ever
/// device 1, so the device id was not stored, and a vault written then must not
/// lose its sessions on the next unlock.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SessionEntry {
    Named {
        name: String,
        device_id: u32,
        record: Vec<u8>,
    },
    Legacy((String, Vec<u8>)),
}

impl SessionEntry {
    fn into_parts(self) -> (String, u32, Vec<u8>) {
        match self {
            SessionEntry::Named {
                name,
                device_id,
                record,
            } => (name, device_id, record),
            SessionEntry::Legacy((name, record)) => (name, 1, record),
        }
    }
}

/// One persisted pinned identity key. Same shape story as `SessionEntry`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum IdentityEntry {
    Named {
        name: String,
        device_id: u32,
        key: Vec<u8>,
    },
    Legacy((String, Vec<u8>)),
}

impl IdentityEntry {
    fn into_parts(self) -> (String, u32, Vec<u8>) {
        match self {
            IdentityEntry::Named {
                name,
                device_id,
                key,
            } => (name, device_id, key),
            IdentityEntry::Legacy((name, key)) => (name, 1, key),
        }
    }
}

pub async fn export_store(
    store: &InMemSignalProtocolStore,
    session_addresses: &[(String, u32)],
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
        let record = store.get_pre_key(*id).await.map_err(|_| Error::Corrupt)?;
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
    for (name, device_id) in session_addresses {
        let addr = ProtocolAddress::new(
            name.clone(),
            DeviceId::try_from(*device_id).map_err(|_| Error::Corrupt)?,
        );
        if let Some(record) = store
            .load_session(&addr)
            .await
            .map_err(|_| Error::Corrupt)?
        {
            sessions.push(SessionEntry::Named {
                name: name.clone(),
                device_id: *device_id,
                record: record.serialize().map_err(|_| Error::Corrupt)?,
            });
        }
    }

    let mut known_identities = Vec::new();
    for (name, device_id) in session_addresses {
        let addr = ProtocolAddress::new(
            name.clone(),
            DeviceId::try_from(*device_id).map_err(|_| Error::Corrupt)?,
        );
        if let Some(key) = store
            .get_identity(&addr)
            .await
            .map_err(|_| Error::Corrupt)?
        {
            known_identities.push(IdentityEntry::Named {
                name: name.clone(),
                device_id: *device_id,
                key: key.serialize().to_vec(),
            });
        }
    }

    let persisted = PersistedSignal {
        registration_id,
        identity_key_pair,
        pre_keys,
        signed_pre_keys,
        kyber_pre_keys,
        sessions,
        known_identities,
    };
    serde_json::to_vec(&persisted).map_err(|_| Error::Corrupt)
}

pub async fn import_store(bytes: &[u8]) -> Result<InMemSignalProtocolStore, Error> {
    let persisted: PersistedSignal = serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)?;
    let identity_key_pair = IdentityKeyPair::try_from(persisted.identity_key_pair.as_slice())
        .map_err(|_| Error::Corrupt)?;
    let mut store = InMemSignalProtocolStore::new(identity_key_pair, persisted.registration_id)
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
    for entry in persisted.sessions {
        let (name, device_id, bytes) = entry.into_parts();
        let addr = ProtocolAddress::new(
            name,
            DeviceId::try_from(device_id).map_err(|_| Error::Corrupt)?,
        );
        let record = SessionRecord::deserialize(&bytes).map_err(|_| Error::Corrupt)?;
        store
            .store_session(&addr, &record)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    for entry in persisted.known_identities {
        let (name, device_id, bytes) = entry.into_parts();
        let addr = ProtocolAddress::new(
            name,
            DeviceId::try_from(device_id).map_err(|_| Error::Corrupt)?,
        );
        let key = IdentityKey::try_from(bytes.as_slice()).map_err(|_| Error::Corrupt)?;
        store
            .save_identity(&addr, &key)
            .await
            .map_err(|_| Error::Corrupt)?;
    }
    Ok(store)
}
