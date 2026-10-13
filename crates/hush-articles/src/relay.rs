//! The Tor transport: NIP-01 over a WebSocket, dialled through SOCKS.
//!
//! This is the only module in the crate that touches the network, and it is the
//! implementation of [`RelaySource`] the daemon builds. Everything else — the
//! mapping, the gate, the cache, the index — is a pure pipeline that this feeds.
//!
//! The wire is the small part of NIP-01 a read needs: send `REQ`, collect
//! `EVENT` frames until `EOSE`, send `CLOSE`. Nothing here decides what is an
//! article; the gate does. And the filter can only ask for `kind:30023`, so a
//! short note is never requested in the first place.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{client_async_tls_with_config, MaybeTlsStream, WebSocketStream};

use crate::event::Event;
use crate::nip23::RawEvent;
use crate::source::{Filter, PublishReport, RelaySink, RelaySource};

/// How long one relay has to answer before it is abandoned. Relays are read in
/// parallel, so this is the whole fetch's budget, not each message's.
const RELAY_TIMEOUT: Duration = Duration::from_secs(20);

/// The subscription id. One read, one relay, one socket, so a fixed id cannot
/// collide with anything.
const SUBSCRIPTION: &str = "hush-neighborhoods";

/// A relay socket. Both routes dial a `TcpStream`; the TLS wrap is the library's.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How a relay is dialled.
#[derive(Clone, Debug)]
enum Route {
    /// Through the SOCKS proxy at this address (Tor). The only route the daemon
    /// builds, so there is no clearnet path by construction.
    Tor(String),
    /// Straight out. A local relay in a test, never a product route.
    Direct,
}

/// A NIP-01 reader over Tor.
pub struct RelayClient {
    route: Route,
    timeout: Duration,
}

impl RelayClient {
    /// Read through the SOCKS proxy at `proxy` — `host:port`, optionally with a
    /// `socks5h://` scheme (Tor's usual `127.0.0.1:9050`).
    pub fn via_tor(proxy: impl Into<String>) -> Self {
        Self {
            route: Route::Tor(proxy.into()),
            timeout: RELAY_TIMEOUT,
        }
    }

    /// Dial relays directly. For a local relay in a test: the daemon never builds
    /// this, so a neighborhood read cannot leave without Tor.
    #[doc(hidden)]
    pub fn direct() -> Self {
        Self {
            route: Route::Direct,
            timeout: RELAY_TIMEOUT,
        }
    }

    /// Dial one relay: the route's socket, then the library's TLS wrap.
    ///
    /// The `None` connector takes the library's default — webpki roots for TLS,
    /// plain when the URL is `ws://`. The scheme in `url` decides.
    async fn dial(&self, url: &str) -> Result<Socket, String> {
        let (host, port) = split_relay_url(url)?;
        let socket = match &self.route {
            Route::Tor(proxy) => crate::socks::connect(proxy, &host, port)
                .await
                .map_err(|e| format!("{url}: {e}"))?,
            Route::Direct => TcpStream::connect((host.as_str(), port))
                .await
                .map_err(|e| format!("{url}: {e}"))?,
        };
        let (socket, _) = client_async_tls_with_config(url, socket, None, None)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        Ok(socket)
    }

    /// Read one relay: dial, subscribe, collect until `EOSE`, close.
    async fn read_one(&self, url: &str, filter: &Filter) -> Result<Vec<RawEvent>, String> {
        let socket = self.dial(url).await?;
        read_until_eose(socket, filter)
            .await
            .map_err(|e| format!("{url}: {e}"))
    }

    /// Send one event to one relay and read its answer.
    async fn publish_one(&self, url: &str, event: &Event) -> Result<Answer, String> {
        let socket = self.dial(url).await?;
        publish_until_ok(socket, event)
            .await
            .map_err(|e| format!("{url}: {e}"))
    }
}

#[async_trait]
impl RelaySource for RelayClient {
    async fn fetch(&self, relays: &[String], filter: &Filter) -> Result<Vec<RawEvent>, String> {
        let reads = futures_util::future::join_all(relays.iter().map(|url| async move {
            let result = tokio::time::timeout(self.timeout, self.read_one(url, filter)).await;
            (url, result)
        }));

        let mut events = Vec::new();
        let mut failures = Vec::new();
        for (url, result) in reads.await {
            match result {
                Ok(Ok(mut batch)) => events.append(&mut batch),
                Ok(Err(message)) => failures.push(message),
                Err(_) => failures.push(format!("{url}: no answer within {:?}", self.timeout)),
            }
        }

        // One dead relay is not a failed refresh when another answered: the
        // allowlist is a set of independent sources and a member being down is
        // not the lane's problem. Only when nothing came back at all is it an
        // error the caller should see.
        if events.is_empty() && !failures.is_empty() {
            return Err(failures.join("; "));
        }
        Ok(events)
    }
}

#[async_trait]
impl RelaySink for RelayClient {
    /// Send the event to every relay at once and collect the verdicts.
    ///
    /// `Err` only when no relay took it. A relay that is down, that says `false`,
    /// or that says nothing at all is a refusal like any other: an event one
    /// relay holds is published, and the author should not be told otherwise
    /// because a second relay is having a bad day.
    async fn publish(&self, relays: &[String], event: &Event) -> Result<PublishReport, String> {
        let id = event.id.clone();
        let sends = futures_util::future::join_all(relays.iter().map(|url| async move {
            let result = tokio::time::timeout(self.timeout, self.publish_one(url, event)).await;
            (url, result)
        }));

        let mut report = PublishReport::default();
        for (url, result) in sends.await {
            match result {
                Ok(Ok(Answer::Accepted)) => report.accepted.push(url.clone()),
                Ok(Ok(Answer::Refused(reason))) => report.refused.push(blame(url, &reason)),
                Ok(Ok(Answer::Other)) => {
                    report.refused.push(blame(url, "the relay did not answer `OK`"))
                }
                Ok(Err(message)) => report.refused.push(message),
                Err(_) => report.refused.push(format!(
                    "{url}: no answer within {:?} for {id}",
                    self.timeout
                )),
            }
        }

        if report.published() {
            Ok(report)
        } else {
            Err(report.summary())
        }
    }
}

/// Name a relay and, when it gave one, the reason it gave.
fn blame(url: &str, reason: &str) -> String {
    if reason.is_empty() {
        url.to_string()
    } else {
        format!("{url}: {reason}")
    }
}

/// Send one `EVENT` and wait for the `OK` that answers it.
///
/// The relay's answer is matched on the event id, so a relay that is answering
/// some earlier publish of ours is not mistaken for this one. A close before an
/// answer is `Other`: the relay never took it, which is a refusal, not a
/// transport error worth distinguishing.
async fn publish_until_ok(mut socket: Socket, event: &Event) -> Result<Answer, String> {
    let frame = serde_json::json!(["EVENT", event]);
    socket
        .send(Message::text(frame.to_string()))
        .await
        .map_err(|e| e.to_string())?;

    let answer = loop {
        let Some(message) = socket.next().await else {
            break Answer::Other;
        };
        let message = message.map_err(|e| e.to_string())?;
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => {
                String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?
            }
            Message::Close(_) => break Answer::Other,
            // Ping and Pong are answered by the stream itself.
            _ => continue,
        };
        match classify_answer(&text, &event.id) {
            Answer::Other => continue,
            answer => break answer,
        }
    };

    let _ = socket.close(None).await;
    Ok(answer)
}

/// The `REQ` frame for one filter: NIP-01's subscription, with the tag filters
/// as `#<name>` keys.
///
/// Built as a map rather than a literal so a filter with no tag filters sends no
/// `#` keys at all: `"#A": []` is a valid NIP-01 filter that matches nothing, and
/// asking a relay a question with a known-empty answer is still a request.
fn subscription_frame(filter: &Filter) -> String {
    let mut body = serde_json::Map::new();
    body.insert(
        "kinds".to_string(),
        serde_json::json!(filter.kinds),
    );
    for (name, values) in &filter.tags {
        body.insert(format!("#{name}"), serde_json::json!(values));
    }
    body.insert("limit".to_string(), serde_json::json!(filter.limit));
    serde_json::json!(["REQ", SUBSCRIPTION, serde_json::Value::Object(body)]).to_string()
}

/// Subscribe, collect `EVENT` frames until `EOSE`, then close.
async fn read_until_eose(mut socket: Socket, filter: &Filter) -> Result<Vec<RawEvent>, String> {
    socket
        .send(Message::text(subscription_frame(filter)))
        .await
        .map_err(|e| e.to_string())?;

    let mut events = Vec::new();
    while let Some(message) = socket.next().await {
        let message = message.map_err(|e| e.to_string())?;
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => {
                String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?
            }
            Message::Close(_) => break,
            // Ping and Pong are answered by the stream itself.
            _ => continue,
        };
        match classify(&text) {
            Incoming::Event(event) => events.push(event),
            Incoming::EndOfStored => break,
            Incoming::Closed(reason) => {
                // A relay that closes the subscription after serving events has
                // still answered; one that closes without serving any has not.
                if events.is_empty() {
                    return Err(reason);
                }
                break;
            }
            Incoming::Other => {}
        }
    }

    let _ = socket
        .send(Message::text(format!("[\"CLOSE\",\"{SUBSCRIPTION}\"]")))
        .await;
    let _ = socket.close(None).await;
    Ok(events)
}

/// The relay frames this lane acts on. Everything else is ignored.
enum Incoming {
    Event(RawEvent),
    EndOfStored,
    Closed(String),
    Other,
}

/// What a relay said about one event it was sent.
enum Answer {
    /// `OK` with `true`. A relay that already had the event lands here too:
    /// NIP-23 is editable by address, so a re-export is the ordinary case and
    /// its `duplicate:` reason is informational, not a refusal.
    Accepted,
    /// `OK` with `false`.
    Refused(String),
    /// Not an answer to this event: another frame, another id, or no `OK`.
    Other,
}

/// Read one relay's answer to one `EVENT`.
fn classify_answer(text: &str, id: &str) -> Answer {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Answer::Other;
    };
    let Some(items) = value.as_array() else {
        return Answer::Other;
    };
    if items.first().and_then(serde_json::Value::as_str) != Some("OK") {
        return Answer::Other;
    }
    // The id has to be ours: a relay may be answering something else it queued.
    if items.get(1).and_then(serde_json::Value::as_str) != Some(id) {
        return Answer::Other;
    }
    let reason = items
        .get(3)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    match items.get(2).and_then(serde_json::Value::as_bool) {
        Some(true) => Answer::Accepted,
        Some(false) => Answer::Refused(reason),
        None => Answer::Other,
    }
}

fn classify(text: &str) -> Incoming {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Incoming::Other;
    };
    let Some(items) = value.as_array() else {
        return Incoming::Other;
    };
    match items.first().and_then(serde_json::Value::as_str) {
        Some("EVENT") => match items.get(2) {
            Some(event) => match serde_json::from_value::<RawEvent>(event.clone()) {
                Ok(event) => Incoming::Event(event),
                Err(_) => Incoming::Other,
            },
            None => Incoming::Other,
        },
        Some("EOSE") => Incoming::EndOfStored,
        Some("CLOSED") => Incoming::Closed(
            items
                .get(2)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the relay closed the subscription")
                .to_string(),
        ),
        _ => Incoming::Other,
    }
}

/// The host and port of a relay URL. `wss://` is 443 and `ws://` is 80 when the
/// URL names no port.
fn split_relay_url(url: &str) -> Result<(String, u16), String> {
    let trimmed = url.trim();
    let (tls, rest) = match trimmed.strip_prefix("wss://") {
        Some(rest) => (true, rest),
        None => match trimmed.strip_prefix("ws://") {
            Some(rest) => (false, rest),
            None => return Err(format!("`{url}` is not a websocket relay URL")),
        },
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(format!("`{url}` has no host"));
    }
    // An IPv6 authority is bracketed; a DNS name is the ordinary case.
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, remainder) = rest
            .split_once(']')
            .ok_or_else(|| format!("`{url}` has an unterminated host"))?;
        let port = match remainder.strip_prefix(':') {
            Some(port) => port
                .parse()
                .map_err(|_| format!("`{url}` has no usable port"))?,
            None => default_port(tls),
        };
        return Ok((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Ok((
            host.to_string(),
            port.parse()
                .map_err(|_| format!("`{url}` has no usable port"))?,
        )),
        Some(_) => Err(format!("`{url}` has no host")),
        None => Ok((authority.to_string(), default_port(tls))),
    }
}

fn default_port(tls: bool) -> u16 {
    if tls {
        443
    } else {
        80
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nip23::KIND_LONG_FORM;
    use tokio::net::TcpListener;

    fn event_frame(d: &str, content: &str) -> String {
        serde_json::json!([
            "EVENT",
            SUBSCRIPTION,
            {
                "id": format!("id-{d}"),
                "pubkey": "aa",
                "created_at": 10,
                "kind": KIND_LONG_FORM,
                "tags": [["d", d], ["title", "A title"]],
                "content": content,
                "sig": "00",
            }
        ])
        .to_string()
    }

    fn as_text(message: &Message) -> Option<&str> {
        match message {
            Message::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// A one-shot relay: it checks the `REQ`, serves `frames`, then waits for the
    /// client to close. Returns its URL.
    async fn spawn_relay(frames: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let request = socket.next().await.unwrap().unwrap();
            let request: serde_json::Value =
                serde_json::from_str(as_text(&request).expect("a text frame")).unwrap();
            assert_eq!(request[0], "REQ", "the client subscribes");
            assert_eq!(request[1], SUBSCRIPTION);
            assert_eq!(
                request[2]["kinds"][0], KIND_LONG_FORM,
                "only long-form is ever asked for"
            );

            for frame in frames {
                socket.send(Message::text(frame)).await.unwrap();
            }
            // The client closes once it has what it came for.
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        format!("ws://{addr}")
    }

    #[tokio::test]
    async fn a_relay_is_read_until_eose() {
        let url = spawn_relay(vec![
            event_frame("one", "body one"),
            event_frame("two", "body two"),
            serde_json::json!(["EOSE", SUBSCRIPTION]).to_string(),
            // Frames after EOSE must not be read: the read is over.
            event_frame("three", "body three"),
        ])
        .await;

        let events = RelayClient::direct()
            .fetch(&[url], &Filter::long_form(10))
            .await
            .expect("the relay answers");

        assert_eq!(events.len(), 2, "the read stops at EOSE");
        assert_eq!(events[0].tag("d"), Some("one"));
        assert_eq!(events[1].tag("d"), Some("two"));
    }

    #[tokio::test]
    async fn a_dead_relay_is_an_error_only_when_nothing_came_back() {
        // A port that was bound and released: nothing is listening on it.
        let dead = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            drop(listener);
            format!("ws://{addr}")
        };
        let live = spawn_relay(vec![
            event_frame("one", "body"),
            serde_json::json!(["EOSE", SUBSCRIPTION]).to_string(),
        ])
        .await;

        // One down, one up: the lane reads what it can.
        let events = RelayClient::direct()
            .fetch(&[dead.clone(), live], &Filter::long_form(10))
            .await
            .expect("a dead relay beside a live one is not a failure");
        assert_eq!(events.len(), 1);

        // Nothing up: the caller is told why.
        let error = RelayClient::direct()
            .fetch(&[dead], &Filter::long_form(10))
            .await
            .expect_err("every relay down is an error");
        assert!(!error.is_empty(), "the failure names the relay");
    }

    #[tokio::test]
    async fn a_relay_that_closes_without_serving_anything_is_an_error() {
        let url = spawn_relay(vec![
            serde_json::json!(["CLOSED", SUBSCRIPTION, "rate limited"]).to_string(),
        ])
        .await;

        let error = RelayClient::direct()
            .fetch(&[url], &Filter::long_form(10))
            .await
            .expect_err("a close with no events is a failure");
        assert!(error.contains("rate limited"), "{error}");
    }

    /// A one-shot relay for a reply read: it checks that the `REQ` asks for the
    /// thread by its root address, then serves `frames`.
    async fn spawn_comment_relay(scope: &str, frames: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let scope = scope.to_string();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let request = socket.next().await.unwrap().unwrap();
            let request: serde_json::Value =
                serde_json::from_str(as_text(&request).expect("a text frame")).unwrap();
            assert_eq!(request[0], "REQ");
            assert_eq!(request[2]["kinds"][0], crate::nip22::KIND_COMMENT);
            assert_eq!(
                request[2]["#A"][0], scope,
                "the whole thread is asked for by its root address"
            );
            assert!(
                request[2]["#a"].is_null(),
                "the parent tag would return only the top-level comments"
            );

            for frame in frames {
                socket.send(Message::text(frame)).await.unwrap();
            }
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        format!("ws://{addr}")
    }

    fn comment_frame(root: &str, parent: Option<&str>, content: &str) -> String {
        let mut tags = vec![
            serde_json::json!(["A", root]),
            serde_json::json!(["K", KIND_LONG_FORM.to_string()]),
        ];
        match parent {
            Some(id) => {
                tags.push(serde_json::json!(["e", id]));
                tags.push(serde_json::json!(["k", crate::nip22::KIND_COMMENT.to_string()]));
            }
            None => tags.push(serde_json::json!(["k", KIND_LONG_FORM.to_string()])),
        }
        serde_json::json!([
            "EVENT",
            SUBSCRIPTION,
            {
                "id": format!("id-{content}"),
                "pubkey": "bb",
                "created_at": 20,
                "kind": crate::nip22::KIND_COMMENT,
                "tags": tags,
                "content": content,
                "sig": "00",
            }
        ])
        .to_string()
    }

    #[tokio::test]
    async fn a_reply_read_asks_for_the_thread_by_its_root_address() {
        let scope = "30023:ab:one";
        let url = spawn_comment_relay(
            scope,
            vec![
                comment_frame(scope, None, "top level"),
                comment_frame(scope, Some("id-top level"), "a reply"),
                serde_json::json!(["EOSE", SUBSCRIPTION]).to_string(),
            ],
        )
        .await;

        let events = RelayClient::direct()
            .fetch(&[url], &Filter::comments(&[scope.to_string()], 10))
            .await
            .expect("the relay answers");
        assert_eq!(events.len(), 2, "the thread comes back whole");
    }

    /// A filter with no tag filters must not send a `#` key: `"#A": []` is a
    /// valid NIP-01 filter that matches nothing, and asking a relay a question
    /// with a known-empty answer is still a request to a public relay.
    #[test]
    fn the_subscription_frame_carries_only_the_filters_it_was_given() {
        let plain: serde_json::Value =
            serde_json::from_str(&subscription_frame(&Filter::long_form(7))).unwrap();
        assert_eq!(plain[0], "REQ");
        assert_eq!(plain[1], SUBSCRIPTION);
        assert_eq!(plain[2]["kinds"][0], KIND_LONG_FORM);
        assert_eq!(plain[2]["limit"], 7);
        assert_eq!(plain[2].as_object().unwrap().len(), 2, "{plain}");

        let scoped = Filter::comments(&["30023:ab:one".to_string()], 9);
        let framed: serde_json::Value =
            serde_json::from_str(&subscription_frame(&scoped)).unwrap();
        assert_eq!(framed[2]["#A"][0], "30023:ab:one");
        assert_eq!(framed[2]["limit"], 9);
    }

    #[test]
    fn a_relay_url_names_a_host_and_a_port() {
        assert_eq!(
            split_relay_url("wss://relay.example").unwrap(),
            ("relay.example".to_string(), 443)
        );
        assert_eq!(
            split_relay_url("ws://relay.example:8080/path").unwrap(),
            ("relay.example".to_string(), 8080)
        );
        assert_eq!(
            split_relay_url("wss://relay.example:8443/").unwrap(),
            ("relay.example".to_string(), 8443)
        );
        assert_eq!(
            split_relay_url("ws://[::1]:7000").unwrap(),
            ("::1".to_string(), 7000)
        );
        assert!(split_relay_url("https://relay.example").is_err());
        assert!(split_relay_url("wss://").is_err());
    }

    #[test]
    fn only_the_four_shapes_this_lane_acts_on_are_classified() {
        assert!(matches!(
            classify(&event_frame("d", "body")),
            Incoming::Event(_)
        ));
        assert!(matches!(
            classify(&serde_json::json!(["EOSE", SUBSCRIPTION]).to_string()),
            Incoming::EndOfStored
        ));
        assert!(matches!(
            classify(&serde_json::json!(["CLOSED", SUBSCRIPTION, "no"]).to_string()),
            Incoming::Closed(_)
        ));
        assert!(matches!(classify("not json"), Incoming::Other));
        assert!(matches!(
            classify(&serde_json::json!(["NOTICE", "hello"]).to_string()),
            Incoming::Other
        ));
    }

    // ---- publishing -------------------------------------------------------

    use crate::event::UnsignedEvent;

    const PUBKEY: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// A signed article, the thing an export hands the sink.
    fn signed_event(d: &str, content: &str) -> Event {
        UnsignedEvent::new(
            PUBKEY,
            1_700_000_000,
            KIND_LONG_FORM,
            vec![
                vec!["d".to_string(), d.to_string()],
                vec!["title".to_string(), "A title".to_string()],
            ],
            content,
        )
        .unwrap()
        .signed(&[9u8; 64])
        .unwrap()
    }

    /// A client that gives up quickly, so a test of a relay that never answers
    /// does not spend the product timeout waiting.
    fn impatient() -> RelayClient {
        RelayClient {
            route: Route::Direct,
            timeout: Duration::from_millis(250),
        }
    }

    /// What a test relay does when it is handed an event.
    enum Reply {
        /// The ordinary case: `OK` with `true` and no reason.
        Accept,
        /// A relay that already holds this revision. `OK` with `true` — NIP-23
        /// is editable by address, so this is an ordinary re-export.
        AcceptDuplicate,
        /// A relay that will not take it, with the reason it gives.
        Refuse(&'static str),
        /// A relay that reads the frame and says nothing at all.
        Silence,
        /// A relay that answers `OK` about an event that is not the one sent.
        AnswerAboutAnotherEvent,
    }

    /// A one-shot relay for publishing: it reads one `EVENT`, replies per
    /// `reply`, then holds the socket until the client closes it. Returns its
    /// URL and a handle to the event it was handed.
    async fn spawn_sink(
        reply: Reply,
    ) -> (String, tokio::task::JoinHandle<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let frame = socket.next().await.unwrap().unwrap();
            let frame: serde_json::Value =
                serde_json::from_str(as_text(&frame).expect("a text frame")).unwrap();
            assert_eq!(frame[0], "EVENT", "the client publishes, not subscribes");
            let event = frame[1].clone();
            let id = event["id"].as_str().unwrap();

            let answer = match reply {
                Reply::Accept => Some(serde_json::json!(["OK", id, true, ""]).to_string()),
                Reply::AcceptDuplicate => Some(
                    serde_json::json!(["OK", id, true, "duplicate: already have this event"])
                        .to_string(),
                ),
                Reply::Refuse(reason) => {
                    Some(serde_json::json!(["OK", id, false, reason]).to_string())
                }
                Reply::Silence => None,
                Reply::AnswerAboutAnotherEvent => {
                    Some(serde_json::json!(["OK", "not-our-id", true, ""]).to_string())
                }
            };
            if let Some(answer) = answer {
                socket.send(Message::text(answer)).await.unwrap();
            }
            // The wrong-id case has to end the stream: a client that is waiting
            // for an answer about its own event otherwise waits out the timeout,
            // which is a different outcome from "answered, but not about ours".
            if matches!(reply, Reply::AnswerAboutAnotherEvent) {
                let _ = socket.close(None).await;
            }

            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            event
        });
        (format!("ws://{addr}"), handle)
    }

    #[tokio::test]
    async fn a_published_event_is_sent_whole_and_a_yes_is_a_publish() {
        let (url, relay) = spawn_sink(Reply::Accept).await;
        let event = signed_event("a-slug", "the body");

        let report = RelayClient::direct()
            .publish(std::slice::from_ref(&url), &event)
            .await
            .expect("the relay took it");

        assert!(report.published());
        assert_eq!(report.accepted, vec![url.clone()]);
        assert!(report.refused.is_empty());

        // What the relay actually received: the event as built, not a summary
        // of it. A relay recomputes the id from these fields, so every one has
        // to arrive.
        let received = relay.await.unwrap();
        assert_eq!(received["id"], event.id);
        assert_eq!(received["pubkey"], event.pubkey);
        assert_eq!(received["created_at"], 1_700_000_000u64);
        assert_eq!(received["kind"], KIND_LONG_FORM);
        assert_eq!(received["content"], "the body");
        assert_eq!(received["tags"][0][0], "d");
        assert_eq!(received["tags"][0][1], "a-slug");
        assert_eq!(received["sig"], event.sig);
    }

    #[tokio::test]
    async fn a_relay_that_already_has_the_revision_still_counts_as_published() {
        let (url, _relay) = spawn_sink(Reply::AcceptDuplicate).await;
        let event = signed_event("a-slug", "the body");

        let report = RelayClient::direct()
            .publish(std::slice::from_ref(&url), &event)
            .await
            .expect("a duplicate is not a failure");
        assert_eq!(report.accepted, vec![url]);
    }

    #[tokio::test]
    async fn a_refusal_names_the_relay_and_the_reason_it_gave() {
        let (url, _relay) = spawn_sink(Reply::Refuse("blocked: not allowed")).await;
        let event = signed_event("a-slug", "the body");

        let error = RelayClient::direct()
            .publish(std::slice::from_ref(&url), &event)
            .await
            .expect_err("no relay took it");
        assert!(error.contains(&url), "{error}");
        assert!(error.contains("blocked: not allowed"), "{error}");
    }

    #[tokio::test]
    async fn one_relay_refusing_beside_one_accepting_is_still_a_publish() {
        let (good, _good_relay) = spawn_sink(Reply::Accept).await;
        let (bad, _bad_relay) = spawn_sink(Reply::Refuse("rate limited")).await;
        let event = signed_event("a-slug", "the body");

        let report = RelayClient::direct()
            .publish(&[bad.clone(), good.clone()], &event)
            .await
            .expect("one relay holding it is published");

        assert_eq!(report.accepted, vec![good]);
        assert_eq!(report.refused.len(), 1);
        assert!(report.refused[0].contains("rate limited"), "{report:?}");
        assert!(report.summary().contains("published to 1 relay"), "{report:?}");
    }

    #[tokio::test]
    async fn an_answer_about_another_event_is_not_mistaken_for_ours() {
        let (url, _relay) = spawn_sink(Reply::AnswerAboutAnotherEvent).await;
        let event = signed_event("a-slug", "the body");

        let error = impatient()
            .publish(std::slice::from_ref(&url), &event)
            .await
            .expect_err("an answer to a different event is not an answer");
        assert!(error.contains("did not answer `OK`"), "{error}");
    }

    #[tokio::test]
    async fn a_relay_that_says_nothing_is_a_refusal_rather_than_a_hang() {
        let (url, _relay) = spawn_sink(Reply::Silence).await;
        let event = signed_event("a-slug", "the body");

        let error = impatient()
            .publish(std::slice::from_ref(&url), &event)
            .await
            .expect_err("silence is not acceptance");
        assert!(error.contains("no answer within"), "{error}");
    }

    #[test]
    fn only_an_ok_about_our_own_event_is_an_answer() {
        assert!(matches!(
            classify_answer(r#"["OK","abc",true,""]"#, "abc"),
            Answer::Accepted
        ));
        assert!(matches!(
            classify_answer(r#"["OK","abc",true,"duplicate: have it"]"#, "abc"),
            Answer::Accepted
        ));
        match classify_answer(r#"["OK","abc",false,"blocked: no"]"#, "abc") {
            Answer::Refused(reason) => assert_eq!(reason, "blocked: no"),
            _ => panic!("a false is a refusal"),
        }

        // Not an answer to our event, however plausible it looks.
        assert!(matches!(
            classify_answer(r#"["OK","other",true,""]"#, "abc"),
            Answer::Other
        ));
        assert!(matches!(
            classify_answer(r#"["OK","abc","yes",""]"#, "abc"),
            Answer::Other
        ));
        assert!(matches!(
            classify_answer(r#"["NOTICE","hello"]"#, "abc"),
            Answer::Other
        ));
        assert!(matches!(classify_answer("not json", "abc"), Answer::Other));
        assert!(matches!(classify_answer("[]", "abc"), Answer::Other));
    }

    #[test]
    fn a_relay_with_nothing_to_add_is_named_alone() {
        assert_eq!(blame("ws://r", ""), "ws://r");
        assert_eq!(blame("ws://r", "blocked"), "ws://r: blocked");
    }
}
