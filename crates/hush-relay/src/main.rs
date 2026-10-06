use std::net::SocketAddr;
use std::path::Path;

use hush_core::default_relay_db_path;
use hush_relay::{RelayConfig, router};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::var("HUSH_RELAY_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let db_path = std::env::var("HUSH_RELAY_DB").unwrap_or_else(|_| {
        default_relay_db_path().to_string_lossy().into_owned()
    });
    if let Some(parent) = Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let app = router(&db_path, RelayConfig::default())?;
    let listener = TcpListener::bind(addr).await?;
    eprintln!("hush-relay listening on http://{addr} (db={db_path})");
    axum::serve(listener, app).await?;
    Ok(())
}
