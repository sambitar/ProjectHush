//! What a member's connection to the public network *is*.
//!
//! Three things, kept apart because they have different lifetimes. The
//! [`Session`] is who is speaking and the tokens that prove it — it moves when
//! the tokens rotate. The [`Publication`] is where their posts are filed — it is
//! written once and then addressed. [`Connection`] is the pair as one stored
//! value, which is the shape the daemon hands to the vault.
//!
//! Nothing here holds a private key. That is the point of the ATProto model and
//! the reason it replaces what came before: the identity is a DID the member
//! already owns on a PDS, and this app is a guest with a scoped app password,
//! not the holder of the key that defines them. A member can revoke Hush from
//! their ATProto account settings and the vault has nothing left to leak.

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// One ATProto session: which identity, which host, and the tokens that speak
/// for it.
///
/// `did` is the stable identity every record is written under, and the only
/// field a reader can trust to keep meaning the same thing: a handle is a label
/// that can move and be re-pointed, so it is carried for display and never used
/// to address anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The account's DID. The `repo` every record is written to.
    pub did: String,
    /// The handle at sign-in, for display only.
    #[serde(default)]
    pub handle: String,
    /// The PDS base, e.g. `https://bsky.social`, with no trailing slash.
    pub pds: String,
    /// The app password.
    ///
    /// Kept so an expired or revoked refresh token costs a re-auth rather than
    /// a re-sign-in. An app password is revocable and scoped by the member, and
    /// the vault holds this encrypted at rest — the same trust level as the
    /// vault's own keys, which are already on this disk.
    #[serde(default)]
    pub password: String,
    /// Short-lived bearer token for repo writes. Rotated by [`Session::with`].
    #[serde(default)]
    pub access_jwt: String,
    /// Long-lived token used only to mint a new access token.
    #[serde(default)]
    pub refresh_jwt: String,
}

impl Session {
    /// Whether this session has what a write needs. A session with a DID and a
    /// PDS but no password is one restored from a partial write, and it cannot
    /// refresh itself — better to say so than to fail at the first request.
    pub fn can_write(&self) -> bool {
        !self.did.trim().is_empty()
            && !self.pds.trim().is_empty()
            && !self.password.trim().is_empty()
    }

    /// The same session with new tokens, keeping the identity and credentials
    /// that did not change.
    ///
    /// A refresh answer may also carry a rotated handle, so it is taken when
    /// present: a member who renamed on their PDS should see the new one.
    pub fn with(&self, access_jwt: String, refresh_jwt: String, handle: Option<String>) -> Self {
        Self {
            access_jwt,
            refresh_jwt,
            handle: handle.filter(|h| !h.trim().is_empty()).unwrap_or_else(|| self.handle.clone()),
            ..self.clone()
        }
    }
}

/// Where a member's posts are filed: the `site.standard.publication` record.
///
/// Standard.site needs a publication for a document to belong to, and a
/// publication needs a web address to combine with each document's `path`. That
/// address is the member's own — Hush does not host it, and cannot invent it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// The `at://` URI of the record on the member's PDS.
    pub uri: String,
    /// The name shown as the masthead of the blog.
    pub name: String,
    /// The base web URL, e.g. `https://ada.example`. Combined with a document's
    /// `path` to build a reader's link.
    pub url: String,
}

/// The `rkey` a publication is written at.
///
/// Fixed rather than minted: a member has one publication, and re-running setup
/// must replace it rather than pile up a second one beside it. A TID would make
/// every re-connect a new blog.
pub const PUBLICATION_RKEY: &str = "self";

/// The vault sidecar the connection lives in.
///
/// Separate from [`crate::STATE_FILE`] because the two have different owners and
/// different lifetimes: the connection is credentials, written once at connect
/// and read on every write, while the lane's state is a cache rewritten on every
/// refresh. One file would make a refresh able to corrupt a session.
pub const CONNECTION_FILE: &str = "atproto-session.enc";

/// Everything this vault has connected to on the public network.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// `None` before the member has connected an ATProto account.
    #[serde(default)]
    pub session: Option<Session>,
    /// `None` until a publication has been written to their PDS.
    #[serde(default)]
    pub publication: Option<Publication>,
}

impl Connection {
    /// The stored form, for the vault's encrypted sidecar.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|e| Error::Json(e.to_string()))
    }

    /// Read the stored form. A missing sidecar is an unconnected vault, not a
    /// fault — the same rule the zines sidecar follows, so the state before the
    /// first connect is expressible without an error path.
    pub fn decode(raw: Option<&[u8]>) -> Result<Self, Error> {
        match raw {
            None => Ok(Self::default()),
            Some(bytes) if bytes.is_empty() => Ok(Self::default()),
            Some(bytes) => serde_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string())),
        }
    }

    /// The session, or a refusal naming what is missing. A write with no
    /// connection is a caller error, and saying "not connected" is better than
    /// a request to an empty host.
    pub fn session(&self) -> Result<&Session, Error> {
        self.session
            .as_ref()
            .filter(|s| s.can_write())
            .ok_or_else(|| Error::Invalid("no ATProto account is connected".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            did: "did:plc:abc".into(),
            handle: "ada.test".into(),
            pds: "https://pds.example".into(),
            password: "app-pw".into(),
            access_jwt: "a1".into(),
            refresh_jwt: "r1".into(),
        }
    }

    #[test]
    fn a_session_without_a_password_cannot_write() {
        let mut s = session();
        assert!(s.can_write());
        s.password.clear();
        assert!(!s.can_write(), "a session that cannot refresh is not writable");
        s.password = "app-pw".into();
        s.did.clear();
        assert!(!s.can_write());
        s.did = "did:plc:abc".into();
        s.pds.clear();
        assert!(!s.can_write());
    }

    #[test]
    fn rotating_tokens_keeps_the_identity() {
        let next = session().with("a2".into(), "r2".into(), Some("ada.new".into()));
        assert_eq!(next.access_jwt, "a2");
        assert_eq!(next.refresh_jwt, "r2");
        assert_eq!(next.handle, "ada.new");
        assert_eq!(next.did, "did:plc:abc", "a token rotation is not a new account");
        assert_eq!(next.password, "app-pw");
    }

    #[test]
    fn a_refresh_answer_without_a_handle_keeps_the_old_one() {
        let next = session().with("a2".into(), "r2".into(), None);
        assert_eq!(next.handle, "ada.test");
        let next = session().with("a2".into(), "r2".into(), Some("  ".into()));
        assert_eq!(next.handle, "ada.test", "a blank handle is not a rename");
    }

    #[test]
    fn a_missing_sidecar_is_an_unconnected_vault() {
        let empty = Connection::decode(None).unwrap();
        assert!(empty.session.is_none());
        assert!(Connection::decode(Some(b"")).unwrap().session.is_none());
        assert!(
            matches!(empty.session(), Err(Error::Invalid(_))),
            "a write with no connection is refused with a reason"
        );
    }

    #[test]
    fn a_connection_round_trips_through_its_stored_form() {
        let c = Connection {
            session: Some(session()),
            publication: Some(Publication {
                uri: "at://did:plc:abc/site.standard.publication/self".into(),
                name: "Ada's zine".into(),
                url: "https://ada.example".into(),
            }),
        };
        let back = Connection::decode(Some(&c.encode().unwrap())).unwrap();
        assert_eq!(back, c);
    }
}
