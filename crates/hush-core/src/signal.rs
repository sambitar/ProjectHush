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

pub fn device_id() -> DeviceId {
    DeviceId::try_from(1u32).expect("device 1")
}

pub fn protocol_address(name: &str) -> ProtocolAddress {
    ProtocolAddress::new(name.to_string(), device_id())
}

pub fn new_protocol_store() -> Result<InMemSignalProtocolStore, Error> {
    let mut csprng = OsRng.unwrap_err();
    let identity_key = IdentityKeyPair::generate(&mut csprng);
    let registration_id: u32 = csprng.next_u32() & 0x3fff;
    InMemSignalProtocolStore::new(identity_key, registration_id).map_err(|_| Error::Corrupt)
}

pub async fn mint_prekey_bundle(
    store: &mut InMemSignalProtocolStore,
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

    let device_id = device_id();
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

pub fn invite_from_bundle(
    relay: &str,
    protocol_name: &str,
    queue_id: &str,
    queue_token: &str,
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
        pre_key_id: pre_key_id.map(|id| u32::from(id)),
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
    })
}

pub async fn process_contact_bundle(
    store: &mut InMemSignalProtocolStore,
    local_name: &str,
    remote_name: &str,
    bundle: &PreKeyBundle,
) -> Result<(), Error> {
    use libsignal_protocol::process_prekey_bundle;

    let local = protocol_address(local_name);
    let remote = protocol_address(remote_name);
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
    remote_name: &str,
    text: &str,
) -> Result<Vec<u8>, Error> {
    use libsignal_protocol::message_encrypt;

    let local = protocol_address(local_name);
    let remote = protocol_address(remote_name);
    let mut csprng = OsRng.unwrap_err();
    let msg = message_encrypt(
        text.as_bytes(),
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
    remote_name: &str,
    ciphertext: &[u8],
) -> Result<Vec<u8>, Error> {
    use libsignal_protocol::message_decrypt;

    let local = protocol_address(local_name);
    let remote = protocol_address(remote_name);
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
