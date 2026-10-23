use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use libsignal_protocol::PreKeyBundle;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::prekey::PreKeyRecord;

/// The current invite format.
///
/// v1 wrapped a JSON document (with byte fields as arrays of numbers) in
/// base64 — a 9 KB string for one invite. v2 is the same fields as plain JSON
/// with the byte fields base64-encoded, which lands near 3 KB with no change
/// to what the invite means. v1 strings remain decodable: see [`InvitePayload::decode`].
pub const INVITE_VERSION: u32 = 2;

/// Base64 (URL-safe, unpadded) for the byte fields, so a JSON invite stays
/// URL-safe ASCII end to end.
mod b64_bytes {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        URL_SAFE_NO_PAD.encode(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        URL_SAFE_NO_PAD
            .decode(s.trim())
            .map_err(serde::de::Error::custom)
    }
}

/// Same encoding for the optional byte fields.
mod b64_bytes_opt {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => URL_SAFE_NO_PAD.encode(bytes).serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        match s {
            Some(text) => URL_SAFE_NO_PAD
                .decode(text.trim())
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitePayload {
    pub v: u32,
    pub relay: String,
    pub protocol_name: String,
    pub queue_id: String,
    pub queue_token: String,
    pub registration_id: u32,
    pub device_id: u32,
    #[serde(with = "b64_bytes")]
    pub identity_key: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_key_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "b64_bytes_opt")]
    pub pre_key_public: Option<Vec<u8>>,
    pub signed_pre_key_id: u32,
    #[serde(with = "b64_bytes")]
    pub signed_pre_key_public: Vec<u8>,
    #[serde(with = "b64_bytes")]
    pub signed_pre_key_signature: Vec<u8>,
    pub kyber_pre_key_id: u32,
    #[serde(with = "b64_bytes")]
    pub kyber_pre_key_public: Vec<u8>,
    #[serde(with = "b64_bytes")]
    pub kyber_pre_key_signature: Vec<u8>,
    /// The sender's chosen mail handle (local part only), if they set one.
    /// Optional so invites minted before mail still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The sender's vault name — the suffix of their mail address, so a
    /// contact renders `handle@vaultname` and not `handle@`-something-guessed.
    /// Optional so invites minted before vault naming still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// Whether this build can receive a chunked `FileOffer`.
    ///
    /// Optional and defaulted so every invite minted before large-file transfer
    /// decodes as `false`; the sender then keeps the inline file path.
    #[serde(default)]
    pub supports_file_transfer: bool,
}

/// The v1 wire shape: byte fields as JSON arrays of numbers. Kept only so
/// invites minted before v2 — including a vault's cached invite string — still
/// decode. Never written again.
#[derive(Deserialize)]
struct LegacyInvite {
    v: u32,
    relay: String,
    protocol_name: String,
    queue_id: String,
    queue_token: String,
    registration_id: u32,
    device_id: u32,
    identity_key: Vec<u8>,
    pre_key_id: Option<u32>,
    pre_key_public: Option<Vec<u8>>,
    signed_pre_key_id: u32,
    signed_pre_key_public: Vec<u8>,
    signed_pre_key_signature: Vec<u8>,
    kyber_pre_key_id: u32,
    kyber_pre_key_public: Vec<u8>,
    kyber_pre_key_signature: Vec<u8>,
    #[serde(default)]
    handle: Option<String>,
    #[serde(default)]
    vault: Option<String>,
}

impl From<LegacyInvite> for InvitePayload {
    fn from(l: LegacyInvite) -> Self {
        Self {
            v: l.v,
            relay: l.relay,
            protocol_name: l.protocol_name,
            queue_id: l.queue_id,
            queue_token: l.queue_token,
            registration_id: l.registration_id,
            device_id: l.device_id,
            identity_key: l.identity_key,
            pre_key_id: l.pre_key_id,
            pre_key_public: l.pre_key_public,
            signed_pre_key_id: l.signed_pre_key_id,
            signed_pre_key_public: l.signed_pre_key_public,
            signed_pre_key_signature: l.signed_pre_key_signature,
            kyber_pre_key_id: l.kyber_pre_key_id,
            kyber_pre_key_public: l.kyber_pre_key_public,
            kyber_pre_key_signature: l.kyber_pre_key_signature,
            handle: l.handle,
            vault: l.vault,
            supports_file_transfer: false,
        }
    }
}

impl InvitePayload {
    /// The invite string is the JSON document itself. No outer base64: the
    /// JSON is already URL-safe ASCII, and wrapping it in base64 bought a
    /// larger string for nothing.
    pub fn encode(&self) -> Result<String, Error> {
        let json = serde_json::to_vec(self).map_err(|_| Error::Corrupt)?;
        String::from_utf8(json).map_err(|_| Error::Corrupt)
    }

    /// Decode either format. The leading byte picks the reader: a v1 string is
    /// base64 and the base64 alphabet has no `{`, while a v2 string *is* the
    /// JSON document and must begin with one.
    pub fn decode(s: &str) -> Result<Self, Error> {
        let s = s.trim();
        if s.starts_with('{') {
            let payload: Self = serde_json::from_slice(s.as_bytes()).map_err(|_| Error::Corrupt)?;
            if payload.v != INVITE_VERSION {
                return Err(Error::UnsupportedInvite);
            }
            return Ok(payload);
        }
        let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| Error::Corrupt)?;
        let legacy: LegacyInvite = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if legacy.v != 1 {
            return Err(Error::UnsupportedInvite);
        }
        Ok(legacy.into())
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

/// The bech32 human-readable part: every pointer invite renders as
/// `hush1…`, address-style, with a checksum so a hand-typed code fails
/// loudly instead of adding the wrong person.
const POINTER_HRP: &str = "hush";

/// The relay a pointer invite names implicitly: the build's default relay
/// ([`crate::config::DEFAULT_RELAY_URL`] — the production relay in release
/// builds, the local lab in debug builds). A code that points there omits
/// the relay entirely, which is what keeps it address-shaped; anything else
/// carries its relay explicitly inside the code.
pub fn default_relay() -> &'static str {
    crate::config::DEFAULT_RELAY_URL
}

/// An address-shaped invite (format v3).
///
/// Instead of carrying the whole prekey bundle inline, it names the account —
/// the protocol name *is* the hex of the identity key — and the peer fetches
/// the signed prekey record from the relay's directory (`GET /k/{account_id}`)
/// at accept time. The record self-authenticates: the relay checks its
/// signature against the identity key the name decodes to, and the client
/// checks it again. The out-of-band channel that carried the code still pins
/// the identity; the relay can refuse to serve (as it can refuse delivery) but
/// cannot forge or swap the keys.
///
/// The queue credentials still travel in the code: the delivery token is
/// public by design but must not be learnable from the name alone, or anyone
/// who guesses a name could write to that inbox.
pub struct ShortInvite {
    /// `None` when the account homes on [`DEFAULT_RELAY`] — omitting it is
    /// what keeps a default-relay code short.
    pub relay: Option<String>,
    /// The account's protocol name: `hex(identity key)`, 66 characters.
    pub protocol_name: String,
    /// The account's inbox queue id (a UUID string).
    pub queue_id: String,
    /// The inbox's delivery token (64 hex characters).
    pub queue_token: String,
    pub handle: Option<String>,
    pub vault: Option<String>,
}

impl ShortInvite {
    /// The relay this account actually lives on.
    pub fn effective_relay(&self) -> String {
        self.relay
            .clone()
            .unwrap_or_else(|| default_relay().to_string())
    }

/// Encode as a bech32 string: `hush1…`, one line, checksummed.
///
/// Hand-rolled rather than the `bech32` crate because the crate enforces
/// BIP-173's 90-character *address* limit, and this payload needs ~170.
/// Lightning's bolt11 makes the same exception for the same reason. The
/// checksum, charset, and case rules are still exactly BIP-173.
pub fn encode(&self) -> Result<String, Error> {
    let identity = hex::decode(&self.protocol_name).map_err(|_| Error::Corrupt)?;
    let queue_id = Uuid::parse_str(&self.queue_id)
        .map_err(|_| Error::Corrupt)?
        .as_bytes()
        .to_vec();
    let token = hex::decode(&self.queue_token).map_err(|_| Error::Corrupt)?;

    let mut flags = 0u8;
    if self.relay.is_some() {
        flags |= 1;
    }
    if self.handle.is_some() {
        flags |= 2;
    }
    if self.vault.is_some() {
        flags |= 4;
    }

    let mut body = vec![3u8, flags];
    body.extend_from_slice(&identity);
    body.extend_from_slice(&queue_id);
    body.extend_from_slice(&token);
    for opt in [self.relay.as_deref(), self.handle.as_deref(), self.vault.as_deref()] {
        if let Some(text) = opt {
            let bytes = text.as_bytes();
            if bytes.len() > 255 {
                return Err(Error::Corrupt);
            }
            body.push(bytes.len() as u8);
            body.extend_from_slice(bytes);
        }
    }

    Ok(bech32_encode(POINTER_HRP, &body))
}

/// Decode a `hush1…` code. The bech32 checksum runs before anything else,
/// so a mistyped code is rejected as garbage, never as a different person.
pub fn decode(s: &str) -> Result<Self, Error> {
    let (hrp, bytes) = bech32_decode(s.trim())?;
    if hrp != POINTER_HRP {
        return Err(Error::Corrupt);
    }
    if bytes.len() < 2 + 33 + 16 + 32 || bytes[0] != 3 {
        return Err(Error::Corrupt);
    }
    let flags = bytes[1];
    // A plain cursor threaded through explicit &mut parameters: two closures
    // sharing one captured index were too subtle to trust.
    let mut at: usize = 2;
    let take = |n: usize, at: &mut usize, src: &[u8]| -> Vec<u8> {
        let out = src[*at..*at + n].to_vec();
        *at += n;
        out
    };
    let identity = take(33, &mut at, &bytes);
    let queue_id = take(16, &mut at, &bytes);
    let token = take(32, &mut at, &bytes);
    let next = |at: &mut usize, src: &[u8]| -> Result<Option<String>, Error> {
        if *at >= src.len() {
            return Ok(None);
        }
        let len = src[*at] as usize;
        *at += 1;
        if *at + len > src.len() {
            return Err(Error::Corrupt);
        }
        let text = String::from_utf8(src[*at..*at + len].to_vec())
            .map_err(|_| Error::Corrupt)?;
        *at += len;
        Ok(Some(text))
    };
    let relay = if flags & 1 != 0 { next(&mut at, &bytes)? } else { None };
    let handle = if flags & 2 != 0 { next(&mut at, &bytes)? } else { None };
    let vault = if flags & 4 != 0 { next(&mut at, &bytes)? } else { None };

    Ok(Self {
        relay,
        protocol_name: hex::encode(&identity),
        queue_id: Uuid::from_bytes(queue_id.try_into().map_err(|_| Error::Corrupt)?).to_string(),
        queue_token: hex::encode(&token),
        handle,
        vault,
    })
}

    /// Fetch the account's prekey record from its relay directory and fold it
    /// into a full payload, so the existing contact pipeline (relay match,
    /// bundle processing, contact record) runs unchanged.
    pub async fn resolve(self, relay_client: &crate::relay_client::RelayClient) -> Result<InvitePayload, Error> {
        let identity = hex::decode(&self.protocol_name).map_err(|_| Error::Corrupt)?;
        let bytes = relay_client.get_prekey_record(&self.protocol_name).await?;
        let record = PreKeyRecord::decode(&bytes)?;
        record.verify(&identity)?;
        let (pre_key_id, pre_key_public) = match record.one_time_prekeys.first() {
            Some(otp) => (Some(otp.key_id), Some(otp.public_key.clone())),
            None => (None, None),
        };
        Ok(InvitePayload {
            v: INVITE_VERSION,
            relay: self.effective_relay(),
            protocol_name: self.protocol_name,
            queue_id: self.queue_id,
            queue_token: self.queue_token,
            registration_id: record.registration_id,
            device_id: record.device_id,
            identity_key: record.identity_key,
            pre_key_id,
            pre_key_public,
            signed_pre_key_id: record.signed_pre_key_id,
            signed_pre_key_public: record.signed_pre_key_public,
            signed_pre_key_signature: record.signed_pre_key_signature,
            kyber_pre_key_id: record.kyber_pre_key_id,
            kyber_pre_key_public: record.kyber_pre_key_public,
            kyber_pre_key_signature: record.kyber_pre_key_signature,
            handle: self.handle,
            vault: self.vault,
            supports_file_transfer: record.supports_file_transfer,
        })
    }
}

/// --- bech32 (BIP-173), hand-rolled for payloads past the 90-char address cap.
///
/// Spec: https://github.com/bitcoin/bips/blob/master/bip-0173.mediawiki

const BECH32_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn bech32_polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
    let mut chk: u32 = 1;
    for v in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ff_ffff) << 5) ^ (*v as u32);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn bech32_hrp_expand(hrp: &str) -> Vec<u8> {
    let mut out: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
    out.push(0);
    out.extend(hrp.bytes().map(|b| b & 31));
    out
}

fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let maxv = (1u32 << to) - 1;
    // The accumulator is masked to from+to bits every step: without the mask
    // it keeps every input bit ever fed in, and a long payload overflows the
    // u32, corrupting everything past the first four bytes. The short BIP-173
    // vectors never caught this because their payloads are tiny.
    let max_acc = (1u32 << (from + to)) - 1;
    let mut out = Vec::new();
    for value in data {
        let v = *value as u32;
        if (v >> from) != 0 {
            return None;
        }
        acc = ((acc << from) | v) & max_acc;
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & maxv) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
        return None;
    }
    Some(out)
}

fn bech32_encode(hrp: &str, bytes: &[u8]) -> String {
    let data = convert_bits(bytes, 8, 5, true).unwrap_or_default();
    let mut values = bech32_hrp_expand(hrp);
    values.extend_from_slice(&data);
    values.extend_from_slice(&[0; 6]);
    let polymod = bech32_polymod(&values) ^ 1;
    let mut out = String::with_capacity(hrp.len() + 1 + data.len() + 6);
    out.push_str(hrp);
    out.push('1');
    for d in &data {
        out.push(BECH32_CHARSET[*d as usize] as char);
    }
    for i in 0..6 {
        out.push(BECH32_CHARSET[((polymod >> (5 * (5 - i))) & 31) as usize] as char);
    }
    out
}

fn bech32_decode(s: &str) -> Result<(String, Vec<u8>), Error> {
    // Either all lowercase or all uppercase (BIP-173 case rule); normalize.
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper {
        return Err(Error::Corrupt);
    }
    let s = s.to_ascii_lowercase();
    let sep = s.rfind('1').ok_or(Error::Corrupt)?;
    if sep == 0 || sep + 7 > s.len() {
        return Err(Error::Corrupt);
    }
    let hrp = s[..sep].to_string();
    if !hrp.bytes().all(|b| (33..=126).contains(&b)) {
        return Err(Error::Corrupt);
    }
    let mut data = Vec::with_capacity(s.len() - sep - 1);
    for c in s[sep + 1..].bytes() {
        let v = BECH32_CHARSET
            .iter()
            .position(|&x| x == c)
            .ok_or(Error::Corrupt)? as u8;
        data.push(v);
    }
    let mut values = bech32_hrp_expand(&hrp);
    values.extend_from_slice(&data);
    if bech32_polymod(&values) != 1 {
        return Err(Error::Corrupt);
    }
    let payload = convert_bits(&data[..data.len() - 6], 5, 8, false).ok_or(Error::Corrupt)?;
    Ok((hrp, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(v: u32) -> InvitePayload {
        InvitePayload {
            v,
            relay: "https://hushrelay.space".into(),
            protocol_name: "ab12cd34".into(),
            queue_id: "queue-uuid".into(),
            queue_token: "token".into(),
            registration_id: 42,
            device_id: 1,
            identity_key: vec![1u8; 33],
            pre_key_id: Some(7),
            pre_key_public: Some(vec![2u8; 33]),
            signed_pre_key_id: 3,
            signed_pre_key_public: vec![4u8; 33],
            signed_pre_key_signature: vec![5u8; 64],
            kyber_pre_key_id: 9,
            kyber_pre_key_public: vec![6u8; 1568],
            kyber_pre_key_signature: vec![7u8; 64],
            handle: Some("sam".into()),
            vault: Some("hush".into()),
            supports_file_transfer: true,
        }
    }

    #[test]
    fn v2_round_trips_as_plain_json() {
        let invite = sample(INVITE_VERSION);
        let s = invite.encode().unwrap();
        assert!(s.starts_with('{'), "v2 is the JSON itself");
        let back = InvitePayload::decode(&s).unwrap();
        assert_eq!(back.v, INVITE_VERSION);
        assert_eq!(back.identity_key, invite.identity_key);
        assert_eq!(back.kyber_pre_key_public, invite.kyber_pre_key_public);
        assert_eq!(back.handle.as_deref(), Some("sam"));
        assert!(back.supports_file_transfer);
    }

    /// A v2 invite minted before large-file transfer must decode as `false` so
    /// the sender keeps the inline path instead of sending an offer the peer
    /// cannot read.
    #[test]
    fn a_v2_invite_without_the_file_transfer_flag_decodes_as_false() {
        let invite = sample(INVITE_VERSION);
        let mut value: serde_json::Value =
            serde_json::from_str(&invite.encode().unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("supports_file_transfer");
        let back =
            InvitePayload::decode(&serde_json::to_string(&value).unwrap()).unwrap();
        assert!(!back.supports_file_transfer);
    }

    #[test]
    fn v1_string_still_decodes() {
        // A v1 invite: base64 of JSON whose byte fields are number arrays.
        let legacy = r#"{"v":1,"relay":"https://hushrelay.space","protocol_name":"ab12cd34","queue_id":"q","queue_token":"t","registration_id":42,"device_id":1,"identity_key":[1,2,3],"pre_key_id":7,"pre_key_public":[4,5,6],"signed_pre_key_id":3,"signed_pre_key_public":[7,8,9],"signed_pre_key_signature":[10,11],"kyber_pre_key_id":9,"kyber_pre_key_public":[12],"kyber_pre_key_signature":[13],"handle":"sam","vault":"hush"}"#;
        let s = URL_SAFE_NO_PAD.encode(legacy.as_bytes());
        let back = InvitePayload::decode(&s).unwrap();
        assert_eq!(back.v, 1);
        assert_eq!(back.identity_key, vec![1, 2, 3]);
        assert_eq!(back.kyber_pre_key_public, vec![12]);
        assert!(!back.supports_file_transfer);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let invite = sample(99);
        let s = invite.encode().unwrap();
        assert!(matches!(
            InvitePayload::decode(&s),
            Err(Error::UnsupportedInvite)
        ));
    }

    fn sample_pointer() -> ShortInvite {
        ShortInvite {
            relay: None,
            protocol_name: "05".to_string() + &"ab".repeat(32),
            queue_id: "0b9e6b1e-6f4e-4a2e-9d1c-3f7a5b8c2d4e".into(),
            queue_token: "cd".repeat(32),
            handle: Some("sandy".into()),
            vault: Some("quiet-harbor".into()),
        }
    }

    #[test]
    fn pointer_round_trips_and_stays_address_shaped() {
        let pointer = sample_pointer();
        let s = pointer.encode().unwrap();
        assert!(s.starts_with("hush1"), "renders as hush1…, got {s}");
        assert!(s.len() < 200, "an address-shaped code stays short: {len}", len = s.len());
        let back = ShortInvite::decode(&s).unwrap();
        assert_eq!(back.protocol_name, pointer.protocol_name);
        assert_eq!(back.queue_id, pointer.queue_id);
        assert_eq!(back.queue_token, pointer.queue_token);
        assert_eq!(back.handle.as_deref(), Some("sandy"));
        assert_eq!(back.vault.as_deref(), Some("quiet-harbor"));
        // relay omitted: the default is implicit
        assert!(back.relay.is_none());
        assert_eq!(back.effective_relay(), default_relay());
    }

    #[test]
    fn pointer_with_explicit_relay_round_trips() {
        let mut pointer = sample_pointer();
        pointer.relay = Some("http://127.0.0.1:8787".into());
        let s = pointer.encode().unwrap();
        let back = ShortInvite::decode(&s).unwrap();
        assert_eq!(back.effective_relay(), "http://127.0.0.1:8787");
    }

    #[test]
    fn a_flipped_bit_is_a_checksum_failure_not_a_different_person() {
        let pointer = sample_pointer();
        let s = pointer.encode().unwrap();
        // flip one character in the data part
        let mut bad: Vec<char> = s.chars().collect();
        let last = bad.len() - 1;
        bad[last] = if bad[last] == 'q' { 'p' } else { 'q' };
        let bad: String = bad.into_iter().collect();
        assert!(matches!(ShortInvite::decode(&bad), Err(Error::Corrupt)));
    }

    #[test]
    fn bech32_matches_the_bip173_vectors() {
        // Empty-payload vector from the BIP-173 test set.
        assert_eq!(bech32_encode("a", &[]), "a12uel5l");
        // A valid string from the BIP-173 spec (its payload is the full
        // charset, 5-bit values 0..31): must decode cleanly and re-encode
        // to itself.
        let spec = "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw";
        let (hrp, payload) = bech32_decode(spec).unwrap();
        assert_eq!(hrp, "abcdef");
        let fes: Vec<u8> = (0..32).collect();
        let bytes = convert_bits(&fes, 5, 8, false).unwrap();
        assert_eq!(payload, bytes);
        // Round trip through the decoder, and case normalization.
        let (hrp, payload) = bech32_decode("A12UEL5L").unwrap();
        assert_eq!(hrp, "a");
        assert!(payload.is_empty());
        assert!(bech32_decode("a12uel5lx").is_err(), "bad checksum");
        assert!(bech32_decode("a12uel5b").is_err(), "bad checksum");
        assert!(bech32_decode("1qzzf2").is_err(), "empty hrp");
        // Mixed case is refused per spec.
        assert!(bech32_decode("A12uel5L").is_err());
    }

    #[test]
    fn convert_bits_survives_a_full_size_payload() {
        // 102 bytes is the pointer invite's body. The accumulator mask bug
        // corrupted everything past ~four bytes here; the empty BIP-173
        // vectors could not see it.
        let body: Vec<u8> = (0..102).map(|i| (i * 37 + 11) as u8).collect();
        let fes = convert_bits(&body, 8, 5, true).unwrap();
        let back = convert_bits(&fes, 5, 8, false).unwrap();
        assert_eq!(back, body);
    }
}
