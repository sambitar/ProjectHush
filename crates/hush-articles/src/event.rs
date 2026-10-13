//! NIP-01 events, and the NIP-19 handle that names one.
//!
//! The read lane consumes events ([`crate::nip23::RawEvent`]); this is the
//! outbound half — build, id, sign, and the `naddr` a reader can pass around.
//! It is the only module in the crate that produces bytes *for* a relay.
//!
//! The id is the part that has to be exactly right. NIP-01 defines it as the
//! SHA-256 of a compact JSON array of six values in a fixed order, so the
//! serialization is written out here rather than derived from a struct: a
//! struct's field order, or a field added to it later, would silently change
//! every id — and the failure mode is not a crash but a relay rejecting an
//! event for reasons it will not explain.
//!
//! Nothing here holds a key. The id is computed, handed to the vault to sign,
//! and the signature is attached: [`UnsignedEvent::signed`] is the seam.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The NIP-19 human-readable prefix for an addressable-event handle.
const NADDR_HRP: &str = "naddr";

/// A signed NIP-01 event: exactly the JSON a relay is sent.
///
/// Deserializable as well as serializable, because a relay echoes the event
/// back inside its `OK` frame and a test wants to read what it was sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    /// Hex, 64 bytes: the BIP340 signature over [`Event::id`].
    pub sig: String,
}

/// An event whose id is computed and which is not signed yet: what the daemon
/// hands to the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsignedEvent {
    /// Hex, 32 bytes: the SHA-256 of the NIP-01 serialization.
    pub id: String,
    /// The authoring key, hex, lowercased.
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl UnsignedEvent {
    /// Build an event and compute its id.
    ///
    /// The pubkey is lowercased here because the id is computed over it: a
    /// caller that handed in an upper-case hex key would otherwise get a
    /// different id for the same event, and a relay computing the id from the
    /// event as received would disagree with us about the signature.
    pub fn new(
        pubkey: impl Into<String>,
        created_at: u64,
        kind: u64,
        tags: Vec<Vec<String>>,
        content: impl Into<String>,
    ) -> Result<Self, String> {
        let pubkey = pubkey.into().trim().to_ascii_lowercase();
        let content = content.into();
        let id = event_id(&pubkey, created_at, kind, &tags, &content)?;
        Ok(Self {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content,
        })
    }

    /// The 32 bytes the public identity signs.
    ///
    /// Returned as a fixed-size array rather than a slice so the one thing that
    /// must not vary — the length — cannot: a signature over a truncated digest
    /// would be a signature over nothing.
    pub fn id_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = hex::decode(&self.id).map_err(|_| "the event id is not hex".to_string())?;
        bytes
            .try_into()
            .map_err(|_| "the event id is not 32 bytes".to_string())
    }

    /// Attach a BIP340 signature, producing the event a relay is sent.
    pub fn signed(self, signature: &[u8]) -> Result<Event, String> {
        if signature.len() != 64 {
            return Err(format!(
                "a BIP340 signature is 64 bytes, not {}",
                signature.len()
            ));
        }
        Ok(Event {
            id: self.id,
            pubkey: self.pubkey,
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags,
            content: self.content,
            sig: hex::encode(signature),
        })
    }
}

/// NIP-01's event id: the SHA-256 of the compact JSON array
/// `[0, pubkey, created_at, kind, tags, content]`.
///
/// The leading `0` is the spec's, and is what keeps an event id from ever
/// colliding with the id of the thing it is signed with.
pub fn event_id(
    pubkey: &str,
    created_at: u64,
    kind: u64,
    tags: &[Vec<String>],
    content: &str,
) -> Result<String, String> {
    let serialized = serde_json::to_string(&serde_json::json!([
        0, pubkey, created_at, kind, tags, content
    ]))
    .map_err(|e| e.to_string())?;
    Ok(hex::encode(Sha256::digest(serialized.as_bytes())))
}

/// NIP-19's `naddr`: a shareable handle for one addressable event.
///
/// A TLV list — `d`, then any relay hints, then the author key and the kind —
/// bech32-encoded under the `naddr` prefix. `bech32`, not `bech32m`: like
/// `npub`, the prefix predates the revised checksum and other clients expect
/// the original.
///
/// The relay hints are a courtesy, not a requirement: a reader that knows the
/// address can find the event on any relay it already reads. Passing the
/// allowlist makes the handle self-contained for someone with none.
pub fn naddr(kind: u64, pubkey: &str, d: &str, relays: &[String]) -> Result<String, String> {
    let mut tlv: Vec<u8> = Vec::new();
    // Type 0 is the `d` tag: the identifier within the author's namespace.
    push_entry(&mut tlv, 0, d.as_bytes())?;
    for relay in relays {
        push_entry(&mut tlv, 1, relay.as_bytes())?;
    }
    let author = hex::decode(pubkey.trim()).map_err(|_| "the pubkey is not hex".to_string())?;
    if author.len() != 32 {
        return Err("a public key is 32 bytes".to_string());
    }
    push_entry(&mut tlv, 2, &author)?;
    // Type 3 is the kind, as a 32-bit big-endian integer.
    push_entry(&mut tlv, 3, &(kind as u32).to_be_bytes())?;

    let hrp = bech32::Hrp::parse(NADDR_HRP).map_err(|e| e.to_string())?;
    bech32::encode::<bech32::Bech32>(hrp, &tlv).map_err(|e| e.to_string())
}

/// One TLV entry: a type, a length, and the value.
///
/// The length is a single byte in the format, so a value that does not fit is
/// refused rather than truncated — a truncated author key or `d` would encode
/// into a handle that resolves to the wrong event, or to nothing.
fn push_entry(out: &mut Vec<u8>, entry_type: u8, value: &[u8]) -> Result<(), String> {
    let len = u8::try_from(value.len())
        .map_err(|_| format!("a {entry_type} entry is longer than the format allows"))?;
    out.push(entry_type);
    out.push(len);
    out.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// The id is the one value a relay recomputes from scratch, so it is worth
    /// pinning against something outside this crate: these two ids were computed
    /// by a Python script written from the spec (`json.dumps` with compact
    /// separators and `ensure_ascii=False`, then `sha256`), not from this code.
    ///
    /// The second case is the interesting one — a quote, a backslash, a newline,
    /// a tab, a non-BMP character and a non-ASCII letter. That is exactly where
    /// two JSON writers can disagree while both looking correct, and a
    /// disagreement there produces an event every relay rejects without saying
    /// why.
    #[test]
    fn the_id_is_the_sha256_of_the_nip01_serialization() {
        let tags = vec![
            vec!["d".to_string(), "a-slug".to_string()],
            vec!["title".to_string(), "On relays".to_string()],
        ];
        assert_eq!(
            event_id(PUBKEY, 1_700_000_000, 30023, &tags, "the body").unwrap(),
            "98fd649cb183a2d2f63019c4af8f1f485d873ec6d463c39be682d9d3e363095d"
        );

        // Exactly what NIP-01 says to hash, byte for byte, for that same case.
        let expected_input = concat!(
            r#"[0,"79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798","#,
            r#"1700000000,30023,[["d","a-slug"],["title","On relays"]],"the body"]"#
        );
        assert_eq!(
            hex::encode(Sha256::digest(expected_input.as_bytes())),
            "98fd649cb183a2d2f63019c4af8f1f485d873ec6d463c39be682d9d3e363095d",
            "the serialization is byte-for-byte NIP-01"
        );

        let tricky_tags = vec![
            vec!["d".to_string(), "sl/ug".to_string()],
            vec!["t".to_string(), "café".to_string()],
        ];
        let tricky = "a \"quote\", a \\ backslash, a\nnewline, a\ttab, an emoji \u{1F600}, café";
        assert_eq!(
            event_id(PUBKEY, 1, 30023, &tricky_tags, tricky).unwrap(),
            "30e366b7c9faad0dd6200df3970b26b98a8d3729ccb49bd47c6929b2b53edadf",
            "escaping and non-ASCII agree with a second JSON writer"
        );
    }

    #[test]
    fn the_id_is_over_the_lowercased_pubkey_and_a_changed_body_moves_it() {
        let tags = vec![vec!["d".to_string(), "x".to_string()]];
        let lower = UnsignedEvent::new(PUBKEY, 10, 30023, tags.clone(), "body").unwrap();
        let upper =
            UnsignedEvent::new(PUBKEY.to_ascii_uppercase(), 10, 30023, tags, "body").unwrap();
        assert_eq!(
            lower.id, upper.id,
            "the same key in either case is the same event"
        );

        let edited = UnsignedEvent::new(PUBKEY, 10, 30023, lower.tags.clone(), "body!").unwrap();
        assert_ne!(lower.id, edited.id, "an edit is a different event");
    }

    #[test]
    fn an_event_is_only_signed_by_a_64_byte_signature() {
        let unsigned = UnsignedEvent::new(PUBKEY, 10, 1, vec![], "hi").unwrap();
        assert_eq!(unsigned.id_bytes().unwrap().len(), 32);

        let signed = unsigned.clone().signed(&[7u8; 64]).unwrap();
        assert_eq!(signed.sig, "07".repeat(64));
        assert_eq!(signed.id, unsigned.id);
        assert_eq!(signed.pubkey, unsigned.pubkey);

        let short = unsigned.signed(&[7u8; 63]).unwrap_err();
        assert!(short.contains("64 bytes"), "{short}");
    }

    /// A JSON round trip is what a relay does with the event before it checks
    /// the signature: it parses the frame. The field names and the number types
    /// have to survive that, or a valid signature is attached to a different id.
    #[test]
    fn a_signed_event_round_trips_through_json_unchanged() {
        let event = UnsignedEvent::new(PUBKEY, 1_700_000_000, 30023, vec![], "body")
            .unwrap()
            .signed(&[9u8; 64])
            .unwrap();
        let text = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&text).unwrap();
        assert_eq!(back, event);
        assert!(
            text.contains(r#""created_at":1700000000"#),
            "the timestamp is a JSON number: {text}"
        );
    }

    /// The `naddr` must decode back to exactly the address it names, and must
    /// agree with an encoder written from BIP-173 rather than from this code.
    ///
    /// The expected strings come from a Python script implementing BIP-173's
    /// bech32 (charset, generator, HRP expansion) and NIP-19's TLV from the
    /// specs. This is the check that matters for the handle: a wrong bech32
    /// checksum produces a string that looks entirely plausible and that no
    /// other client can read.
    #[test]
    fn an_naddr_matches_an_independent_encoder_and_decodes_back() {
        let relays = vec![
            "wss://relay.damus.io".to_string(),
            "wss://nos.lol".to_string(),
        ];
        let encoded = naddr(30023, PUBKEY, "a-slug", &relays).unwrap();
        assert_eq!(
            encoded,
            concat!(
                "naddr1qqrxzttnd36kwqg5waehxw309aex2mrp0yhxgctdw4eju6t0qyxhwumn8ghj7mn0",
                "wvhxcmmvqgs8n0nx0muaewav2ksx99wwsu9swq5mlndjmn3gm9vl9q2mzmup0xqrqsqqqa",
                "28stsc4l",
            )
        );
        assert_eq!(
            naddr(30023, PUBKEY, "a-slug", &[]).unwrap(),
            concat!(
                "naddr1qqrxzttnd36kwq3q0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hc",
                "z7vqxpqqqp65we53ls0",
            )
        );

        let decoded = decode(&encoded);
        assert_eq!(decoded.get(&0).unwrap(), &[b"a-slug".to_vec()]);
        assert_eq!(
            decoded.get(&1).unwrap(),
            &[
                b"wss://relay.damus.io".to_vec(),
                b"wss://nos.lol".to_vec()
            ]
        );
        assert_eq!(
            hex::encode(&decoded.get(&2).unwrap()[0]),
            PUBKEY,
            "the author key is carried whole"
        );
        assert_eq!(decoded.get(&3).unwrap()[0], 30023u32.to_be_bytes().to_vec());
    }

    #[test]
    fn an_naddr_without_relays_is_still_a_handle() {
        let encoded = naddr(30023, PUBKEY, "a-slug", &[]).unwrap();
        let decoded = decode(&encoded);
        assert_eq!(decoded.get(&0).unwrap(), &[b"a-slug".to_vec()]);
        assert!(!decoded.contains_key(&1), "no relay hints were given");
        assert_eq!(decoded.get(&3).unwrap()[0], 30023u32.to_be_bytes().to_vec());
    }

    #[test]
    fn a_key_that_is_not_32_bytes_is_refused_rather_than_encoded() {
        let short = naddr(30023, "79be66", "d", &[]).unwrap_err();
        assert!(short.contains("32 bytes"), "{short}");
        assert!(naddr(30023, "zz", "d", &[]).is_err(), "not hex at all");
    }

    /// A `d` or a relay longer than one length byte can carry is refused. The
    /// alternative is a truncated entry that decodes to a different address.
    #[test]
    fn an_entry_too_long_for_the_format_is_refused() {
        let long = "x".repeat(300);
        let error = naddr(30023, PUBKEY, &long, &[]).unwrap_err();
        assert!(error.contains("longer than the format allows"), "{error}");
    }

    /// A minimal `naddr` decoder, for the round-trip test only. Not part of the
    /// crate's surface: nothing in the product decodes one yet.
    fn decode(encoded: &str) -> std::collections::BTreeMap<u8, Vec<Vec<u8>>> {
        let (hrp, data) = bech32::decode(encoded).unwrap();
        assert_eq!(hrp.to_string(), NADDR_HRP);
        let mut out: std::collections::BTreeMap<u8, Vec<Vec<u8>>> =
            std::collections::BTreeMap::new();
        let mut i = 0;
        while i < data.len() {
            let entry_type = data[i];
            let len = data[i + 1] as usize;
            let value = data[i + 2..i + 2 + len].to_vec();
            out.entry(entry_type).or_default().push(value);
            i += 2 + len;
        }
        out
    }
}
