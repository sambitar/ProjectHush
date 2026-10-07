use std::time::SystemTime;

use libsignal_protocol::{
    DeviceId, IdentityKeyPair, InMemSignalProtocolStore, KeyPair, KyberPreKeyId,
    KyberPreKeyRecord, PreKeyBundle, PreKeyId, PreKeyRecord, ProtocolAddress,
    SignedPreKeyId, SignedPreKeyRecord, Timestamp,
};
use libsignal_protocol::{
    GenericSignedPreKey, IdentityKeyStore, KyberPreKeyStore, PreKeyStore, SignedPreKeyStore,
};
use rand::rngs::OsRng;
use rand::{Rng, RngCore, TryRngCore};

use crate::error::Error;
use crate::invite::InvitePayload;

pub fn protocol_name_from_identity(pair: &IdentityKeyPair) -> String {
    hex::encode(pair.identity_key().serialize())
}

/// Build the address of one device of an account.
///
/// `name` is the *account* id, not the device's own identity hash: every device
/// of an account shares it and the device id distinguishes them. That is exactly
/// what libsignal's `ProtocolAddress` models, and it is why a peer holds one
/// session per device under a single contact instead of a session per key.
pub fn protocol_address(name: &str, device_id: u32) -> Result<ProtocolAddress, Error> {
    let device_id = DeviceId::try_from(device_id).map_err(|_| Error::Corrupt)?;
    Ok(ProtocolAddress::new(name.to_string(), device_id))
}

pub fn new_protocol_store() -> Result<InMemSignalProtocolStore, Error> {
    let mut csprng = OsRng.unwrap_err();
    let identity_key = IdentityKeyPair::generate(&mut csprng);
    let registration_id: u32 = csprng.next_u32() & 0x3fff;
    InMemSignalProtocolStore::new(identity_key, registration_id).map_err(|_| Error::Corrupt)
}

pub async fn mint_prekey_bundle(
    store: &mut InMemSignalProtocolStore,
    device_id: u32,
) -> Result<PreKeyBundle, Error> {
    use libsignal_protocol::kem;

    let mut csprng = OsRng.unwrap_err();
    let pre_key_pair = KeyPair::generate(&mut csprng);
    let signed_pre_key_pair = KeyPair::generate(&mut csprng);
    let kyber_pre_key_pair = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut csprng);

    let signed_pre_key_public = signed_pre_key_pair.public_key.serialize();
    let signed_pre_key_signature = store
        .get_identity_key_pair()
        .await
        .map_err(|_| Error::Corrupt)?
        .private_key()
        .calculate_signature(&signed_pre_key_public, &mut csprng)
        .map_err(|_| Error::Corrupt)?;

    let kyber_pre_key_public = kyber_pre_key_pair.public_key.serialize();
    let kyber_pre_key_signature = store
        .get_identity_key_pair()
        .await
        .map_err(|_| Error::Corrupt)?
        .private_key()
        .calculate_signature(&kyber_pre_key_public, &mut csprng)
        .map_err(|_| Error::Corrupt)?;

    let device_id = DeviceId::try_from(device_id).map_err(|_| Error::Corrupt)?;
    let pre_key_id: u32 = csprng.random();
    let signed_pre_key_id: u32 = csprng.random();
    let kyber_pre_key_id: u32 = csprng.random();

    let pre_key_bundle = PreKeyBundle::new(
        store.get_local_registration_id().await.map_err(|_| Error::Corrupt)?,
        device_id,
        Some((PreKeyId::from(pre_key_id), pre_key_pair.public_key)),
        SignedPreKeyId::from(signed_pre_key_id),
        signed_pre_key_pair.public_key,
        signed_pre_key_signature.to_vec(),
        KyberPreKeyId::from(kyber_pre_key_id),
        kyber_pre_key_pair.public_key.clone(),
        kyber_pre_key_signature.to_vec(),
        *store
            .get_identity_key_pair()
            .await
            .map_err(|_| Error::Corrupt)?
            .identity_key(),
    )
    .map_err(|_| Error::Corrupt)?;

    store
        .save_pre_key(
            PreKeyId::from(pre_key_id),
            &PreKeyRecord::new(PreKeyId::from(pre_key_id), &pre_key_pair),
        )
        .await
        .map_err(|_| Error::Corrupt)?;

    let timestamp = Timestamp::from_epoch_millis(csprng.random_range(1_000_000..2_000_000));

    store
        .save_signed_pre_key(
            SignedPreKeyId::from(signed_pre_key_id),
            &SignedPreKeyRecord::new(
                SignedPreKeyId::from(signed_pre_key_id),
                timestamp,
                &signed_pre_key_pair,
                &signed_pre_key_signature,
            ),
        )
        .await
        .map_err(|_| Error::Corrupt)?;

    store
        .save_kyber_pre_key(
            KyberPreKeyId::from(kyber_pre_key_id),
            &KyberPreKeyRecord::new(
                KyberPreKeyId::from(kyber_pre_key_id),
                Timestamp::from_epoch_millis(43),
                &kyber_pre_key_pair,
                &kyber_pre_key_signature,
            ),
        )
        .await
        .map_err(|_| Error::Corrupt)?;

    Ok(pre_key_bundle)
}

/// Mint a device's publishable prekey record: a fresh pool of one-time prekeys
/// plus the signed and kyber prekeys that carry the forward-secrecy guarantee.
///
/// The private halves are saved into the store, because that is what a peer's
/// first message will be decrypted against. Only the public halves go on the
/// wire.
pub async fn mint_publishable_prekeys(
    store: &mut InMemSignalProtocolStore,
    device_id: u32,
    one_time_count: usize,
) -> Result<crate::prekey::PreKeyRecord, Error> {
    use libsignal_protocol::kem;
    // Aliased: libsignal already has a `PreKeyRecord`, and this one is the
    // publishable *set* of keys rather than a single prekey.
    use crate::prekey::{OneTimePreKey, PreKeyRecord as PublishablePreKeys};

    let mut csprng = OsRng.unwrap_err();
    let identity_key_pair = store
        .get_identity_key_pair()
        .await
        .map_err(|_| Error::Corrupt)?;
    let identity_key = identity_key_pair.identity_key().serialize().to_vec();

    let mut one_time_prekeys = Vec::with_capacity(one_time_count);
    for _ in 0..one_time_count {
        let key_pair = KeyPair::generate(&mut csprng);
        let key_id: u32 = csprng.random();
        store
            .save_pre_key(
                PreKeyId::from(key_id),
                &PreKeyRecord::new(PreKeyId::from(key_id), &key_pair),
            )
            .await
            .map_err(|_| Error::Corrupt)?;
        one_time_prekeys.push(OneTimePreKey {
            key_id,
            public_key: key_pair.public_key.serialize().to_vec(),
        });
    }

    let signed_pre_key_pair = KeyPair::generate(&mut csprng);
    let signed_pre_key_public = signed_pre_key_pair.public_key.serialize();
    let signed_pre_key_signature = identity_key_pair
        .private_key()
        .calculate_signature(&signed_pre_key_public, &mut csprng)
        .map_err(|_| Error::Corrupt)?;
    let signed_pre_key_id: u32 = csprng.random();
    store
        .save_signed_pre_key(
            SignedPreKeyId::from(signed_pre_key_id),
            &SignedPreKeyRecord::new(
                SignedPreKeyId::from(signed_pre_key_id),
                Timestamp::from_epoch_millis(csprng.random_range(1_000_000..2_000_000)),
                &signed_pre_key_pair,
                &signed_pre_key_signature,
            ),
        )
        .await
        .map_err(|_| Error::Corrupt)?;

    let kyber_pre_key_pair = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut csprng);
    let kyber_pre_key_public = kyber_pre_key_pair.public_key.serialize();
    let kyber_pre_key_signature = identity_key_pair
        .private_key()
        .calculate_signature(&kyber_pre_key_public, &mut csprng)
        .map_err(|_| Error::Corrupt)?;
    let kyber_pre_key_id: u32 = csprng.random();
    store
        .save_kyber_pre_key(
            KyberPreKeyId::from(kyber_pre_key_id),
            &KyberPreKeyRecord::new(
                KyberPreKeyId::from(kyber_pre_key_id),
                Timestamp::from_epoch_millis(43),
                &kyber_pre_key_pair,
                &kyber_pre_key_signature,
            ),
        )
        .await
        .map_err(|_| Error::Corrupt)?;

    Ok(PublishablePreKeys {
        v: crate::prekey::PREKEY_RECORD_VERSION,
        device_id,
        registration_id: store
            .get_local_registration_id()
            .await
            .map_err(|_| Error::Corrupt)?,
        identity_key,
        signed_pre_key_id,
        signed_pre_key_public: signed_pre_key_public.to_vec(),
        signed_pre_key_signature: signed_pre_key_signature.to_vec(),
        kyber_pre_key_id,
        kyber_pre_key_public: kyber_pre_key_public.to_vec(),
        kyber_pre_key_signature: kyber_pre_key_signature.to_vec(),
        one_time_prekeys,
    })
}

pub fn invite_from_bundle(
    relay: &str,
    protocol_name: &str,
    queue_id: &str,
    queue_token: &str,
    handle: Option<&str>,
    bundle: &PreKeyBundle,
) -> Result<InvitePayload, Error> {
    let pre_key_id = bundle.pre_key_id().map_err(|_| Error::Corrupt)?;
    let pre_key_public = bundle.pre_key_public().map_err(|_| Error::Corrupt)?;
    Ok(InvitePayload {
        v: crate::invite::INVITE_VERSION,
        relay: relay.to_string(),
        protocol_name: protocol_name.to_string(),
        queue_id: queue_id.to_string(),
        queue_token: queue_token.to_string(),
        registration_id: bundle.registration_id().map_err(|_| Error::Corrupt)?,
        device_id: u32::from(bundle.device_id().map_err(|_| Error::Corrupt)?),
        identity_key: bundle
            .identity_key()
            .map_err(|_| Error::Corrupt)?
            .serialize()
            .to_vec(),
        pre_key_id: pre_key_id.map(u32::from),
        pre_key_public: pre_key_public.map(|k| k.serialize().to_vec()),
        signed_pre_key_id: u32::from(bundle.signed_pre_key_id().map_err(|_| Error::Corrupt)?),
        signed_pre_key_public: bundle
            .signed_pre_key_public()
            .map_err(|_| Error::Corrupt)?
            .serialize()
            .to_vec(),
        signed_pre_key_signature: bundle
            .signed_pre_key_signature()
            .map_err(|_| Error::Corrupt)?
            .to_vec(),
        kyber_pre_key_id: u32::from(bundle.kyber_pre_key_id().map_err(|_| Error::Corrupt)?),
        kyber_pre_key_public: bundle
            .kyber_pre_key_public()
            .map_err(|_| Error::Corrupt)?
            .serialize()
            .to_vec(),
        kyber_pre_key_signature: bundle
            .kyber_pre_key_signature()
            .map_err(|_| Error::Corrupt)?
            .to_vec(),
        handle: handle.map(|h| h.to_string()),
    })
}

pub async fn process_contact_bundle(
    store: &mut InMemSignalProtocolStore,
    local_name: &str,
    local_device_id: u32,
    remote_name: &str,
    remote_device_id: u32,
    bundle: &PreKeyBundle,
) -> Result<(), Error> {
    use libsignal_protocol::process_prekey_bundle;

    let local = protocol_address(local_name, local_device_id)?;
    let remote = protocol_address(remote_name, remote_device_id)?;
    let mut csprng = OsRng.unwrap_err();
    process_prekey_bundle(
        &remote,
        &local,
        &mut store.session_store,
        &mut store.identity_store,
        bundle,
        SystemTime::now(),
        &mut csprng,
    )
    .await
    .map_err(|_| Error::Session)?;
    Ok(())
}

pub async fn encrypt_message(
    store: &mut InMemSignalProtocolStore,
    local_name: &str,
    local_device_id: u32,
    remote_name: &str,
    remote_device_id: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    use libsignal_protocol::message_encrypt;

    let local = protocol_address(local_name, local_device_id)?;
    let remote = protocol_address(remote_name, remote_device_id)?;
    let mut csprng = OsRng.unwrap_err();
    let msg = message_encrypt(
        plaintext,
        &remote,
        &local,
        &mut store.session_store,
        &mut store.identity_store,
        SystemTime::now(),
        &mut csprng,
    )
    .await
    .map_err(|_| Error::Session)?;
    Ok(msg.serialize().to_vec())
}

pub async fn decrypt_message(
    store: &mut InMemSignalProtocolStore,
    local_name: &str,
    local_device_id: u32,
    remote_name: &str,
    remote_device_id: u32,
    ciphertext: &[u8],
) -> Result<Vec<u8>, Error> {
    use libsignal_protocol::message_decrypt;

    let local = protocol_address(local_name, local_device_id)?;
    let remote = protocol_address(remote_name, remote_device_id)?;
    let mut csprng = OsRng.unwrap_err();
    let msg = parse_ciphertext(ciphertext)?;
    message_decrypt(
        &msg,
        &remote,
        &local,
        &mut store.session_store,
        &mut store.identity_store,
        &mut store.pre_key_store,
        &store.signed_pre_key_store,
        &mut store.kyber_pre_key_store,
        &mut csprng,
    )
    .await
    .map_err(|_| Error::Session)
}

fn parse_ciphertext(bytes: &[u8]) -> Result<libsignal_protocol::CiphertextMessage, Error> {
    use libsignal_protocol::{CiphertextMessage, PreKeySignalMessage, SignalMessage};
    if let Ok(m) = PreKeySignalMessage::try_from(bytes) {
        return Ok(CiphertextMessage::PreKeySignalMessage(m));
    }
    if let Ok(m) = SignalMessage::try_from(bytes) {
        return Ok(CiphertextMessage::SignalMessage(m));
    }
    Err(Error::Corrupt)
}
