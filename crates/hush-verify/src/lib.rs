//! Signature verification shared by the Hush app, the daemon and the relay.
//!
//! This crate exists so that the code deciding whether a subscription is paid
//! and the code deciding whether an update is genuine are the same code, tested
//! once. It is deliberately a leaf: it depends on no other crate in the
//! workspace, so the relay can link it without pulling in the client stack and
//! the client can link it without pulling in the relay.
//!
//! Two things are verified here today:
//!
//! - **Licenses** ([`license`]) — a signed statement that a vault is entitled to
//!   the first-party relay and official updates. Verified offline, against a
//!   public key compiled into the app and configured on the relay, so no client
//!   has to phone home to find out whether it is paid up.
//!
//! Release artifacts are verified by the Tauri updater plugin, which does its
//! own minisign check inside the shell; that path does not route through this
//! crate.

pub mod license;

pub use license::{
    check_encoded_license, decode_license, encode_license, sign_license, verify_license, License,
    VerifyError, PREFIX,
};
