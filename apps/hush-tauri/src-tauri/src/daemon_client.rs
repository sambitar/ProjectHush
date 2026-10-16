//! Daemon API client for the Tauri shell.
//!
//! The daemon listens on a Unix socket by default (DR-02) and the webview
//! cannot `fetch()` a socket, so requests are proxied through Rust: JS calls
//! the `hush_api` command and this module speaks HTTP/1.1 over the socket.
//!
//! When `HUSH_API_URL` is an `http://` URL the UI keeps using plain `fetch`
//! and this module is not involved.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Uri};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tower_service::Service;

/// Socket file name; mirrors `hush_core::config::SOCKET_FILE_NAME`.
const SOCKET_FILE_NAME: &str = "hush-daemon.sock";

/// How the UI should reach the daemon.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Transport {
    Unix { path: String },
    Tcp { base: String },
}

/// The transport named by the environment, if any.
///
/// The run scripts set one, and so does anyone who starts a daemon by hand. A
/// bundled copy of the app has neither, and starts its own daemon instead — see
/// `daemon_supervisor`, which uses this to keep one owner per socket.
pub fn env_transport() -> Option<Transport> {
    if let Ok(url) = std::env::var("HUSH_API_URL") {
        let url = url.trim().to_string();
        if let Some(path) = url.strip_prefix("unix://") {
            return Some(Transport::Unix {
                path: path.to_string(),
            });
        }
        if !url.is_empty() {
            return Some(Transport::Tcp { base: url });
        }
    }
    if let Ok(path) = std::env::var("HUSH_DAEMON_SOCKET") {
        if !path.trim().is_empty() {
            return Some(Transport::Unix {
                path: path.trim().to_string(),
            });
        }
    }
    None
}

/// Where a daemon on this machine is reached when nothing says otherwise.
pub fn default_transport() -> Transport {
    Transport::Unix {
        path: default_socket_path().display().to_string(),
    }
}

/// Resolve the transport from the environment, matching `hush_core::config`
/// precedence for the socket path: `HUSH_API_URL` > `HUSH_DAEMON_SOCKET` >
/// runtime-dir default.
pub fn transport_from_env() -> Transport {
    env_transport().unwrap_or_else(default_transport)
}

/// Mirror of `hush_core::paths::default_runtime_dir` — kept in sync deliberately
/// so the UI crate does not have to link the crypto stack.
fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("HUSH_RUNTIME_DIR") {
        return PathBuf::from(p).join(SOCKET_FILE_NAME);
    }
    if let Ok(p) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(p).join("hush").join(SOCKET_FILE_NAME);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    PathBuf::from(format!("/tmp/hush-{}/{}", user, SOCKET_FILE_NAME))
}

#[derive(Debug, Serialize)]
pub struct ApiReply {
    pub status: u16,
    pub body: String,
}

pub struct DaemonClient {
    client: Client<UnixConnector, Full<Bytes>>,
}

impl DaemonClient {
    pub fn new(path: &Path) -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build(UnixConnector {
                path: Arc::new(path.to_path_buf()),
            }),
        }
    }

    pub async fn request(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<String>,
    ) -> Result<ApiReply, String> {
        let uri = format!("http://localhost{}", path_and_query)
            .parse::<Uri>()
            .map_err(|e| format!("bad request path: {e}"))?;
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| e.to_string())?;
        let res = self
            .client
            .request(req)
            .await
            .map_err(|e| format!("daemon unreachable: {e}"))?;
        let status = res.status().as_u16();
        let bytes = res
            .into_body()
            .collect()
            .await
            .map_err(|e| format!("bad daemon response: {e}"))?
            .to_bytes();
        Ok(ApiReply {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

#[derive(Clone)]
struct UnixConnector {
    path: Arc<PathBuf>,
}

/// Wrapper so `hyper`'s `Connection` marker can be implemented for a Unix stream.
struct UnixStreamIo(UnixStream);

impl AsyncRead for UnixStreamIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for UnixStreamIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

impl Connection for UnixStreamIo {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl Service<Uri> for UnixConnector {
    type Response = TokioIo<UnixStreamIo>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move {
            let stream = UnixStream::connect(path.as_path()).await?;
            Ok(TokioIo::new(UnixStreamIo(stream)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::json;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hush-proxy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[tokio::test]
    async fn proxies_get_and_post_over_unix_socket() {
        let socket = temp_socket("proxy.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        let app = Router::new()
            .route(
                "/api/status",
                get(|| async { Json(json!({ "locked": true, "relay": "http://relay" })) }),
            )
            .route(
                "/api/unlock",
                axum::routing::post(|body: String| async move {
                    Json(json!({ "echo": body }))
                }),
            )
            .route(
                "/api/lock",
                axum::routing::post(|| async { axum::http::StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/boom",
                get(|| async {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": "nope" })),
                    )
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = DaemonClient::new(&socket);

        let status = client.request("GET", "/api/status", None).await.unwrap();
        assert_eq!(status.status, 200);
        assert!(status.body.contains("\"locked\":true"), "{}", status.body);
        assert!(status.body.contains("http://relay"), "{}", status.body);

        let posted = client
            .request("POST", "/api/unlock", Some("{\"passphrase\":\"x\"}".into()))
            .await
            .unwrap();
        assert_eq!(posted.status, 200);
        assert!(posted.body.contains("passphrase"), "{}", posted.body);

        let empty = client.request("POST", "/api/lock", None).await.unwrap();
        assert_eq!(empty.status, 204);
        assert!(empty.body.is_empty());

        let err = client.request("GET", "/api/boom", None).await.unwrap();
        assert_eq!(err.status, 401);
        assert!(err.body.contains("nope"), "{}", err.body);

        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test]
    async fn missing_socket_reports_unreachable() {
        let socket = temp_socket("absent.sock");
        let _ = std::fs::remove_file(&socket);
        let client = DaemonClient::new(&socket);
        let err = client.request("GET", "/api/status", None).await.unwrap_err();
        assert!(err.contains("daemon unreachable"), "{err}");
    }
}
