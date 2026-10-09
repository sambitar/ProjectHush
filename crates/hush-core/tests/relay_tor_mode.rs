//! Tor relay mode: the switch moves the relay path between the clearnet base
//! and the onion address, and it is fail-closed — a missing SOCKS port or a
//! missing onion address is an error, never a silent fall back to clearnet.

use hush_core::Account;
use hush_relay::{RelayConfig, router};
use tempfile::TempDir;
use tokio::time::{Duration, sleep};

async fn spawn_relay() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

/// A syntactically valid v3 onion URL. The shape is what the validator checks;
/// nothing here can verify a checksum, and no test in this file needs a real
/// onion to exist — only that the switch sends traffic where it says.
fn test_onion() -> String {
    let label = "a".repeat(56);
    format!("http://{label}.onion")
}

/// A port with no listener: with Tor mode on and the SOCKS port pointed here,
/// a request must fail (the onion path is tried and dead) rather than succeed
/// over clearnet.
async fn dead_socks_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

#[tokio::test]
async fn tor_mode_is_fail_closed_and_persists() {
    let (relay, _dir) = spawn_relay().await;
    let vault = TempDir::new().unwrap();
    let mut alice = Account::create(vault.path(), "pw", &relay).await.unwrap();

    // Mode off: the clearnet path works against the in-process relay.
    alice.heartbeat_presence().await.expect("clearnet send");

    // Turning the mode on without an address on file is refused outright.
    let err = alice.set_relay_tor(true, None).await.unwrap_err();
    assert!(
        err.to_string().contains("onion address"),
        "unexpected error: {err}"
    );

    // Attaching an address records it, and the mode takes effect: the active
    // base is now the onion, and with no SOCKS port the request fails closed
    // naming the missing half.
    let onion = test_onion();
    alice.set_relay_tor(true, Some(&onion)).await.unwrap();
    assert_eq!(alice.relay(), onion);
    assert!(alice.relay_tor_mode());
    let err = alice.heartbeat_presence().await.unwrap_err();
    assert!(
        err.to_string().contains("Tor is not connected"),
        "a missing SOCKS port must fail closed, got: {err}"
    );

    // A SOCKS port with nothing listening is still not clearnet: the request
    // goes to the (dead) proxy and fails, it never succeeds over the base.
    let dead = dead_socks_port().await;
    alice.set_relay_socks(Some(format!("socks5h://127.0.0.1:{dead}")));
    assert!(
        alice.heartbeat_presence().await.is_err(),
        "a dead proxy must not fall back to clearnet"
    );

    // The switch off is the only path back: same vault, no proxy at all, and
    // the clearnet send works again.
    alice.set_relay_tor(false, None).await.unwrap();
    assert_eq!(alice.relay(), relay);
    alice.heartbeat_presence().await.expect("clearnet send again");

    // The mode is vault state: closing and reopening keeps it.
    alice.set_relay_tor(true, None).await.unwrap();
    drop(alice);
    let reopened = Account::open(vault.path(), "pw").await.unwrap();
    assert!(reopened.relay_tor_mode());
    assert_eq!(reopened.relay(), onion);
    assert_eq!(reopened.onion_relay(), Some(onion.as_str()));
}
