//! What can go wrong on the way to a PDS.
//!
//! Every variant carries enough to say *which* thing failed, because the two
//! failures a member actually meets are different problems with different
//! fixes: "the PDS said no" (a bad app password, a revoked session) and "the
//! PDS was not reached at all" (Tor down, wrong host). Collapsing them into one
//! string is how a UI ends up telling someone to check their password when the
//! real answer is that the proxy is not running.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The PDS answered, and the answer was a refusal. ATProto reports these as
    /// `{"error": "...", "message": "..."}` with a 4xx.
    #[error("the PDS refused the request ({status}): {message}")]
    Refused { status: u16, message: String },

    /// The PDS was never reached: DNS, TLS, connection, or the SOCKS proxy.
    #[error("could not reach the PDS: {0}")]
    Transport(String),

    /// A response that was not the JSON shape the lexicon promises. Treated as
    /// a hard failure rather than a tolerated one: guessing at a malformed
    /// session response is how a token ends up attached to the wrong account.
    #[error("the PDS answered something that is not ATProto JSON: {0}")]
    Json(String),

    /// The SOCKS proxy value is not one this crate will put Tor in front of.
    #[error("bad SOCKS proxy: {0}")]
    Proxy(String),

    /// The caller handed over something that cannot be published: an empty
    /// title, a missing publication, a record with no text.
    #[error("{0}")]
    Invalid(String),
}
