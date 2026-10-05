use std::net::SocketAddr;

use hush_relay::{RelayConfig, router};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::var("HUSH_RELAY_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let db_path = std::env::var("HUSH_RELAY_DB").unwrap_or_else(|_| "relay.db".into());

    let app = router(&db_path, RelayConfig::default())?;
    let listener = TcpListener::bind(addr).await?;
    eprintln!("hush-relay listening on http://{addr} (db={db_path})");
    axum::serve(listener, app).await?;
    Ok(())
}
