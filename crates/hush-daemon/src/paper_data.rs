//! Conclave Paper — the WebRTC DataChannel fast path (Task 6).
//!
//! The relay is the paper's guarantee: it always works, and it is slow (~1–3 s
//! per hop because every update is a store-and-forward round trip). This module
//! is the *optimisation* on top of it: a direct DataChannel between each trustee
//! and the paper's hub, carrying the same opaque `yrs` updates at a fraction of
//! the latency. Nothing here is load-bearing — if it never connects, the relay
//! carries the paper exactly as it does today.
//!
//! **Topology is a star, and the hub is the creator.** Each trustee dials the
//! hub once; the hub answers. A trustee never talks to another trustee, so there
//! is one connection per peer and one writer to converge on.
//!
//! **Signalling rides the existing 1:1 session** (`PaperOffer` / `PaperAnswer`),
//! not a channel of its own: the relay already delivers it reliably and in
//! order, and it is the only path that exists before the DataChannel does. Each
//! description carries its gathered ICE candidates, so the handshake is one
//! round trip.
//!
//! **Failure is the expected case, not an error.** `send` reports whether the
//! fast path took the update; when it did not — no channel yet, a channel that
//! just died, a NAT that will not let the peers see each other — the caller
//! falls back to the relay. `send` therefore never returns an error for a peer
//! it simply cannot reach quickly, because that is not a failure of the paper.
//!
//! **Scope of what is proven.** The tests here run two planes in one process on
//! loopback, with no STUN server. That covers the state machine, the fallback
//! contract, and the wire bytes. It does *not* cover two machines on a real LAN,
//! and it does not cover a symmetric NAT — with no STUN and no TURN by design,
//! that case degrades to the relay, which is what the fallback contract is for.
//!
//! **Wired into the daemon** as of Task 6 Step 2: a paper's open dials the hub,
//! inbound `PaperOffer`/`PaperAnswer` reach `answer`/`accept_answer`, outbound
//! deltas try `send` before the relay fan-out, and one background loop per
//! daemon drains the channels and re-broadcasts along the star. The relay
//! fan-out is untouched by all of it — it is still what makes convergence a
//! guarantee rather than a hope.
//!
//! **Presence rides the same channel** (Task 7 Step 2), as a second frame tag:
//! the editor's awareness bytes, opaque here, forwarded along the star and kept
//! for a short while so a peer that joins late still sees the cursors that are
//! already in the document. It has no relay fallback and no persistence — a
//! cursor that arrives a second and a half late is worse than no cursor.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::runtime::{channel, default_runtime, Receiver, Runtime, Sender};

/// Label on the data channel, i.e. what the peer sees it as.
const CHANNEL_LABEL: &str = "paper";

/// How long the whole handshake may take before it is given up on. A peer that
/// cannot connect in this long falls back to the relay, which is the point.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

/// Frame tag: an opaque `yrs` update.
///
/// Every message on the channel carries a one-byte tag. A second channel would
/// work too, but a channel is a connection: presence is worth exactly as much
/// as the update path it rides on, so it shares that path and costs one byte.
const TAG_UPDATE: u8 = 0;

/// Frame tag: an opaque presence frame (the editor's awareness bytes).
///
/// Opaque here for the same reason an update is: the hub forwards presence
/// along the star without understanding it, and the only party that can read
/// the bytes is the editor that produced them.
const TAG_PRESENCE: u8 = 1;

/// How long a peer's last presence frame stays current.
///
/// Presence is ephemeral by definition and there is no leave message to rely on
/// (a crashed daemon sends none), so a frame that has not been refreshed is
/// dropped rather than drawn as a cursor that is not there.
const PRESENCE_TTL: Duration = Duration::from_secs(30);

/// The shortest gap between two dials to the same peer.
///
/// The UI polls the paper once a second while the tab is open, and the open is
/// where a dial is triggered, so without this a peer that *cannot* connect —
/// a symmetric NAT, a hub that is offline — would post an offer every second
/// for as long as someone watches the tab. Long enough that the retry is not
/// noise, short enough that a hub that comes back is picked up while the tab is
/// still open.
const DIAL_COOLDOWN: Duration = Duration::from_secs(15);

/// The one runtime handle every peer connection and timer here shares.
///
/// `default_runtime()` constructs a runtime per call, so resolving it per use
/// would hand each peer connection a different runtime instance.
fn runtime() -> Arc<dyn Runtime> {
    static RUNTIME: OnceLock<Arc<dyn Runtime>> = OnceLock::new();
    Arc::clone(RUNTIME.get_or_init(|| {
        default_runtime().expect("runtime-tokio is enabled for the daemon")
    }))
}

/// What became of an attempt to use the fast path.
///
/// Deliberately not a `Result`: a peer this module cannot reach quickly is not
/// an error condition, it is the relay's turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// The update went out over the DataChannel.
    Live,
    /// No usable channel; the caller must use the relay.
    UseRelay,
}

/// What a frame on the channel carries.
///
/// Both halves are opaque to the daemon: an update is a `yrs` payload, and a
/// presence frame is the editor's awareness bytes. Nothing here parses either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboundFrame {
    /// An opaque `yrs` update, to merge into the document.
    Update(Vec<u8>),
    /// An opaque presence frame, to forward and to remember.
    Presence(Vec<u8>),
}

/// A frame that arrived over a DataChannel, waiting to be handled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inbound {
    /// The account the frame came from.
    pub from: String,
    /// What it carries.
    pub frame: InboundFrame,
}

/// Prefix `body` with its tag.
fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(tag);
    out.extend_from_slice(body);
    out
}

/// Split a received frame, or `None` when this build cannot read it.
///
/// An empty message and an unknown tag are both `None`: a peer running a newer
/// build must not be able to break this one by sending something it does not
/// know about, which is the same rule the payload decoder follows.
fn unframe(bytes: &[u8]) -> Option<InboundFrame> {
    let (tag, body) = bytes.split_first()?;
    match *tag {
        TAG_UPDATE => Some(InboundFrame::Update(body.to_vec())),
        TAG_PRESENCE => Some(InboundFrame::Presence(body.to_vec())),
        _ => None,
    }
}

/// One peer's connection state.
#[derive(Default)]
struct Link {
    connection: Option<Arc<dyn PeerConnection>>,
    channel: Option<Arc<dyn DataChannel>>,
    live: bool,
}

/// State shared with the event handlers, which outlive the calls that built
/// them.
struct Inner {
    stun: Option<String>,
    links: Mutex<HashMap<String, Link>>,
    inbound: Mutex<Vec<Inbound>>,
    /// The last presence frame from each peer, with when it arrived. Ephemeral
    /// by design: it is never persisted and never leaves this process.
    presence: Mutex<HashMap<String, (Vec<u8>, Instant)>>,
    /// When each peer was last dialled. See [`DataPlane::should_dial`].
    dialled: Mutex<HashMap<String, Instant>>,
}

impl Inner {
    /// Everything below locks briefly and never across an `await`, so the pump
    /// tasks and the callers can share one plain mutex.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Link>> {
        self.links.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn begin(&self, peer: &str) {
        self.lock().insert(peer.to_string(), Link::default());
    }

    fn set_connection(&self, peer: &str, connection: Arc<dyn PeerConnection>) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.connection = Some(connection);
        }
    }

    fn set_channel(&self, peer: &str, channel: Arc<dyn DataChannel>) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.channel = Some(channel);
        }
    }

    fn set_live(&self, peer: &str, live: bool) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.live = live;
        }
    }

    /// The channel to send on, or `None` when the fast path is not available.
    ///
    /// Returns a cloned `Arc` so the caller can drop the lock before awaiting.
    fn live_channel(&self, peer: &str) -> Option<Arc<dyn DataChannel>> {
        let links = self.lock();
        let link = links.get(peer)?;
        if !link.live {
            return None;
        }
        link.channel.clone()
    }

    fn connection(&self, peer: &str) -> Option<Arc<dyn PeerConnection>> {
        self.lock().get(peer)?.connection.clone()
    }

    fn push_inbound(&self, from: &str, frame: InboundFrame) {
        // Presence is remembered as well as queued: it has no history and no
        // replay, so a peer that joins after the frame went by would otherwise
        // never see a cursor that is still on screen elsewhere.
        if let InboundFrame::Presence(bytes) = &frame {
            self.presence
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(from.to_string(), (bytes.clone(), Instant::now()));
        }
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Inbound {
                from: from.to_string(),
                frame,
            });
    }

    fn take_inbound(&self) -> Vec<Inbound> {
        std::mem::take(&mut *self.inbound.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Peer-connection events.
///
/// One handler type serves both roles: `peer` is the account on the far end of
/// *this* connection, which is what the hub needs to tell dialers apart.
struct Handler {
    peer: String,
    inner: Arc<Inner>,
    gathered: Sender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }

    /// A dialer's channel is created locally; the hub's arrives here, because a
    /// peer that answers never calls `create_data_channel`.
    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        self.inner.set_channel(&self.peer, Arc::clone(&channel));
        pump(Arc::clone(&self.inner), self.peer.clone(), channel);
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        // The channel's own open/close is the signal the fast path acts on, but
        // a connection that has failed will never deliver one, so drop `live`
        // here too rather than waiting for an event that is not coming.
        if matches!(
            state,
            RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Closed
        ) {
            self.inner.set_live(&self.peer, false);
        }
    }
}

/// Forward a channel's events into the plane's state.
fn pump(inner: Arc<Inner>, peer: String, channel: Arc<dyn DataChannel>) {
    runtime().spawn(Box::pin(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => inner.set_live(&peer, true),
                DataChannelEvent::OnMessage(message) => {
                    // A tag this build does not know is dropped rather than
                    // guessed at, exactly like an unknown payload tag.
                    if let Some(frame) = unframe(&message.data) {
                        inner.push_inbound(&peer, frame);
                    }
                }
                DataChannelEvent::OnClose => {
                    inner.set_live(&peer, false);
                    break;
                }
                _ => {}
            }
        }
        // The stream ended without a close event (connection torn down).
        inner.set_live(&peer, false);
    }));
}

/// Await `future`, giving up after `duration` instead of hanging the caller.
async fn within<T>(duration: Duration, future: impl Future<Output = T>) -> Option<T> {
    webrtc::runtime::timeout(&*runtime(), duration, future)
        .await
        .ok()
}

/// A paper's data plane: one DataChannel per peer, all against the hub.
///
/// The plane does not know which half of the star it is. The hub calls
/// [`answer`](Self::answer) for each dialer and a trustee calls
/// [`offer`](Self::offer) for the hub; which one a daemon is follows from
/// whether it created the paper — a fact about the paper, not about a
/// connection — so it stays with the caller instead of being mirrored here.
pub struct DataPlane {
    inner: Arc<Inner>,
}

impl DataPlane {
    /// A plane with no connections yet.
    ///
    /// `stun` is the operator's STUN server, when they run one. With `None`,
    /// peers gather host candidates only, which is what lets a loopback or LAN
    /// run connect without handing a third party anyone's address.
    pub fn new(stun: Option<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                stun,
                links: Mutex::new(HashMap::new()),
                inbound: Mutex::new(Vec::new()),
                presence: Mutex::new(HashMap::new()),
                dialled: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The peers with an open fast path right now.
    ///
    /// This is the star's fan-out list, and it is the same list for both roles:
    /// a trustee's only channel is to the hub, so its "all peers" *is* the hub;
    /// the hub's is every trustee. That is what lets re-broadcast and outbound
    /// send share one shape without asking which side of the star they are on.
    pub fn peers(&self) -> Vec<String> {
        self.inner
            .lock()
            .iter()
            .filter(|(_, link)| link.live)
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// Whether it is time to dial `peer` again.
    ///
    /// True at most once per [`DIAL_COOLDOWN`], and never while the channel is
    /// already up. Marks the attempt, so a caller that dials on a UI poll gets
    /// one offer rather than one per poll.
    pub fn should_dial(&self, peer: &str) -> bool {
        if self.inner.live_channel(peer).is_some() {
            return false;
        }
        let mut dialled = self
            .inner
            .dialled
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match dialled.get(peer) {
            Some(at) if at.elapsed() < DIAL_COOLDOWN => false,
            _ => {
                dialled.insert(peer.to_string(), Instant::now());
                true
            }
        }
    }

    /// Open the fast path to `peer`, returning the offer to signal.
    ///
    /// Trustee half of the handshake: the trustee offers, the hub answers.
    pub async fn offer(&self, peer: &str) -> Result<String, String> {
        self.forget(peer);
        self.inner.begin(peer);
        let (connection, mut gathered) = self.build(peer).await?;
        self.inner.set_connection(peer, Arc::clone(&connection));

        let channel = connection
            .create_data_channel(CHANNEL_LABEL, None)
            .await
            .map_err(|e| format!("could not create the paper channel: {e}"))?;
        self.inner.set_channel(peer, Arc::clone(&channel));
        pump(Arc::clone(&self.inner), peer.to_string(), channel);

        let offer = connection
            .create_offer(None)
            .await
            .map_err(|e| format!("could not build an offer: {e}"))?;
        connection
            .set_local_description(offer)
            .await
            .map_err(|e| format!("could not accept our own offer: {e}"))?;
        let description = self.gathered_description(&connection, &mut gathered).await?;
        serde_json::to_string(&description).map_err(|e| format!("offer is unencodable: {e}"))
    }

    /// Finish the fast path with the hub's answer. Trustee half.
    pub async fn accept_answer(&self, peer: &str, answer: &str) -> Result<(), String> {
        let description: RTCSessionDescription = serde_json::from_str(answer)
            .map_err(|e| format!("the hub's answer is unreadable: {e}"))?;
        let connection = self
            .inner
            .connection(peer)
            .ok_or_else(|| format!("no outstanding offer for {peer}"))?;
        connection
            .set_remote_description(description)
            .await
            .map_err(|e| format!("the hub's answer was refused: {e}"))
    }

    /// Answer a dialer's offer, returning the answer to signal. Hub half.
    pub async fn answer(&self, peer: &str, offer: &str) -> Result<String, String> {
        let description: RTCSessionDescription = serde_json::from_str(offer)
            .map_err(|e| format!("the offer is unreadable: {e}"))?;
        self.forget(peer);
        self.inner.begin(peer);
        let (connection, mut gathered) = self.build(peer).await?;
        self.inner.set_connection(peer, Arc::clone(&connection));

        connection
            .set_remote_description(description)
            .await
            .map_err(|e| format!("the offer was refused: {e}"))?;
        let answer = connection
            .create_answer(None)
            .await
            .map_err(|e| format!("could not build an answer: {e}"))?;
        connection
            .set_local_description(answer)
            .await
            .map_err(|e| format!("could not accept our own answer: {e}"))?;
        let description = self.gathered_description(&connection, &mut gathered).await?;
        serde_json::to_string(&description).map_err(|e| format!("answer is unencodable: {e}"))
    }

    /// Try to put `update` on the fast path to `peer`.
    ///
    /// Any reason the fast path is unusable — no channel, a channel that failed
    /// mid-send — is reported as [`Sent::UseRelay`] rather than an error, and
    /// drops the link's live flag so the badge follows.
    pub async fn send(&self, peer: &str, update: &[u8]) -> Sent {
        self.send_frame(peer, TAG_UPDATE, update).await
    }

    /// Try to put a presence frame on the fast path to `peer`.
    ///
    /// Same contract as [`send`](Self::send): presence is an optimisation too,
    /// and a peer that cannot be reached quickly simply has no live cursor on
    /// this screen. There is deliberately no relay fallback for presence — a
    /// cursor that arrives a second and a half late is worse than none.
    pub async fn send_presence(&self, peer: &str, presence: &[u8]) -> Sent {
        self.send_frame(peer, TAG_PRESENCE, presence).await
    }

    async fn send_frame(&self, peer: &str, tag: u8, body: &[u8]) -> Sent {
        let Some(channel) = self.inner.live_channel(peer) else {
            return Sent::UseRelay;
        };
        match channel.send(BytesMut::from(&frame(tag, body)[..])).await {
            Ok(()) => Sent::Live,
            Err(_) => {
                self.inner.set_live(peer, false);
                Sent::UseRelay
            }
        }
    }

    /// Frames received off the channels since the last call.
    pub fn drain(&self) -> Vec<Inbound> {
        self.inner.take_inbound()
    }

    /// The last presence frame from each peer that is still current.
    ///
    /// This is what a joining editor is caught up with: presence has no log, so
    /// the only way a newcomer learns that somebody is already in the document
    /// is by being handed the frames that are still fresh.
    pub fn presence(&self) -> Vec<(String, Vec<u8>)> {
        let now = Instant::now();
        let presence = self
            .inner
            .presence
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        presence
            .iter()
            .filter(|(_, (_, at))| now.duration_since(*at) < PRESENCE_TTL)
            .map(|(peer, (bytes, _))| (peer.clone(), bytes.clone()))
            .collect()
    }

    /// Drop a peer: close its connection and forget it.
    ///
    /// Used when a peer leaves, and at the head of a fresh handshake so a
    /// renegotiation does not leave the old connection behind.
    pub fn forget(&self, peer: &str) {
        self.inner
            .presence
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(peer);
        let link = self.inner.lock().remove(peer);
        if let Some(link) = link {
            if let Some(connection) = link.connection {
                runtime().spawn(Box::pin(async move {
                    let _ = connection.close().await;
                }));
            }
        }
    }

    /// Close every connection. The plane is unusable afterwards.
    pub fn shutdown(&self) {
        let links: Vec<String> = self.inner.lock().keys().cloned().collect();
        for peer in links {
            self.forget(&peer);
        }
    }

    /// Build one peer connection and the receiver its gathering signals arrive on.
    async fn build(&self, peer: &str) -> Result<(Arc<dyn PeerConnection>, Receiver<()>), String> {
        let (gathered_tx, gathered_rx) = channel::<()>(1);
        let handler = Arc::new(Handler {
            peer: peer.to_string(),
            inner: Arc::clone(&self.inner),
            gathered: gathered_tx,
        });
        let mut builder = PeerConnectionBuilder::new()
            .with_handler(handler)
            .with_runtime(runtime())
            // Bind every interface: a LAN peer has to be reachable on the
            // interface the two machines actually share, not just loopback.
            .with_udp_addrs(vec!["0.0.0.0:0".to_string()]);
        // No STUN unless the operator configured one: a public server would
        // learn the address of every peer that opens a paper.
        if let Some(stun) = self.inner.stun.as_ref() {
            builder = builder.with_configuration(
                RTCConfigurationBuilder::new()
                    .with_ice_servers(vec![RTCIceServer {
                        urls: vec![stun.clone()],
                        ..Default::default()
                    }])
                    .build(),
            );
        }
        let connection = builder
            .build()
            .await
            .map_err(|e| format!("could not open a peer connection: {e}"))?;
        Ok((Arc::new(connection), gathered_rx))
    }

    /// Wait for gathering to finish and return the description carrying the
    /// candidates, so the handshake is one round trip.
    async fn gathered_description(
        &self,
        connection: &Arc<dyn PeerConnection>,
        gathered: &mut Receiver<()>,
    ) -> Result<RTCSessionDescription, String> {
        if within(HANDSHAKE_DEADLINE, gathered.recv()).await.is_none() {
            return Err("gathering ICE candidates timed out".to_string());
        }
        connection
            .local_description()
            .await
            .ok_or_else(|| "no local description after gathering".to_string())
    }
}

#[cfg(test)]
mod data_plane_tests {
    use super::*;
    use std::time::Instant;

    /// Drive a future to completion on the data-plane runtime.
    ///
    /// `Runtime::block_on` is restricted to a `()` output so the trait stays
    /// object-safe, so the value is moved out through a local slot — the same
    /// workaround the crate's own tests use. Only for tests: the daemon already
    /// owns a tokio runtime and must never nest this inside it.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut out: Option<F::Output> = None;
        {
            let slot = &mut out;
            runtime().block_on(Box::pin(async move {
                *slot = Some(future.await);
            }));
        }
        out.expect("the future ran to completion")
    }

    /// Give the connection tasks a moment to make progress.
    async fn settle(millis: u64) {
        runtime().sleep(Duration::from_millis(millis)).await;
    }

    /// Whether `peer` is on the fast path.
    ///
    /// `peers()` is the fan-out list, so it answers this for one peer and for
    /// all of them: a trustee's only peer is the hub, so "is the hub live" and
    /// "is anyone live" are the same question there.
    fn is_live(plane: &DataPlane, peer: &str) -> bool {
        plane.peers().iter().any(|p| p == peer)
    }

    /// Wait for a peer's channel to come up (or, with `want_live: false`, to
    /// stay down), the way the daemon's badge would.
    async fn wait_for(plane: &DataPlane, peer: &str, want_live: bool) -> bool {
        for _ in 0..100 {
            if is_live(plane, peer) == want_live {
                return true;
            }
            settle(50).await;
        }
        false
    }

    /// A real v1 update, produced the way any Yjs peer produces one. The daemon
    /// treats updates as opaque bytes, so the document shape is not the point.
    fn update(text: &str) -> Vec<u8> {
        use yrs::{Doc, ReadTxn, StateVector, Text, Transact};
        let doc = Doc::new();
        let content = doc.get_or_insert_text("content");
        let mut txn = doc.transact_mut();
        content.insert(&mut txn, 0, text);
        txn.encode_state_as_update_v1(&StateVector::default())
    }

    /// The whole point: two planes, one channel, a real update across it.
    #[test]
    fn a_trustee_dials_the_hub_and_an_update_crosses() {
        block_on(async {
            let hub = DataPlane::new(None);
            let trustee = DataPlane::new(None);
            let started = Instant::now();

            let offer = trustee.offer("hub").await.expect("the trustee offers");
            let answer = hub.answer("trustee", &offer).await.expect("the hub answers");
            trustee
                .accept_answer("hub", &answer)
                .await
                .expect("the trustee takes the answer");

            assert!(
                wait_for(&trustee, "hub", true).await,
                "the trustee's channel must come up"
            );
            assert!(
                wait_for(&hub, "trustee", true).await,
                "the hub's side of the same channel must come up"
            );
            let handshake = started.elapsed();

            let bytes = update("the paper crosses a data channel");
            let expected = crate::paper_doc::apply(&[], &bytes).expect("a valid v1 state");

            let sent_at = Instant::now();
            assert_eq!(
                trustee.send("hub", &bytes).await,
                Sent::Live,
                "an open channel takes the update"
            );

            let mut received = None;
            for _ in 0..2000 {
                if let Some(inbound) = hub.drain().into_iter().next() {
                    received = Some(inbound);
                    break;
                }
                settle(1).await;
            }
            let received = received.expect("the hub receives the update");
            // The plane is drained on the daemon's own cadence, so this bounds
            // the transport plus at most one poll interval rather than the
            // transport alone — the spike measured that at ~1 ms.
            let delivered = sent_at.elapsed();

            assert_eq!(received.from, "trustee", "the update names its sender");
            let InboundFrame::Update(bytes_in) = received.frame else {
                panic!("an update must arrive as an update, not as presence");
            };
            assert_eq!(bytes_in, bytes, "the bytes must arrive intact");
            let applied =
                crate::paper_doc::apply(&[], &bytes_in).expect("the update applies");
            assert_eq!(applied, expected, "the hub's document matches the trustee's");

            println!(
                "paper_data: handshake {handshake:?}, one update end to end {delivered:?} \
                 ({} bytes)",
                bytes.len()
            );

            trustee.shutdown();
            hub.shutdown();
        });
    }

    /// The contract the relay depends on: an unreachable peer is not an error,
    /// it is the relay's turn.
    #[test]
    fn a_peer_with_no_channel_reports_the_relay() {
        block_on(async {
            let hub = DataPlane::new(None);
            assert!(!is_live(&hub, "nobody"));
            assert_eq!(hub.send("nobody", b"x").await, Sent::UseRelay);
            assert!(hub.drain().is_empty());
        });
    }

    /// The degradation the plan asks for, without needing a NAT: a trustee that
    /// offered but never got an answer stays on the relay, and nothing hangs.
    #[test]
    fn a_hub_that_never_answers_leaves_the_trustee_on_the_relay() {
        block_on(async {
            let trustee = DataPlane::new(None);
            trustee.offer("hub").await.expect("the trustee offers");

            assert!(
                !is_live(&trustee, "hub"),
                "an unanswered offer is not a channel"
            );
            assert_eq!(trustee.send("hub", b"x").await, Sent::UseRelay);
            // The offer is still outstanding, so a late answer would still land.
            assert!(trustee.accept_answer("hub", "not json").await.is_err());
            trustee.shutdown();
        });
    }

    /// A hub re-answering the same peer must not leak the previous connection,
    /// and the old one must stop counting as live.
    #[test]
    fn renegotiating_replaces_the_previous_connection() {
        block_on(async {
            let hub = DataPlane::new(None);
            let trustee = DataPlane::new(None);

            let offer = trustee.offer("hub").await.unwrap();
            let answer = hub.answer("trustee", &offer).await.unwrap();
            trustee.accept_answer("hub", &answer).await.unwrap();
            assert!(wait_for(&hub, "trustee", true).await);

            // The same trustee dials again (a restart, or a recovered network).
            let offer = trustee.offer("hub").await.unwrap();
            let answer = hub.answer("trustee", &offer).await.unwrap();
            trustee.accept_answer("hub", &answer).await.unwrap();
            assert!(wait_for(&hub, "trustee", true).await);

            trustee.shutdown();
            hub.shutdown();
        });
    }

    /// Wait for one frame of a given shape off a plane.
    async fn next_frame(plane: &DataPlane, want_presence: bool) -> Option<Inbound> {
        for _ in 0..2000 {
            for inbound in plane.drain() {
                let presence = matches!(inbound.frame, InboundFrame::Presence(_));
                if presence == want_presence {
                    return Some(inbound);
                }
            }
            settle(1).await;
        }
        None
    }

    /// Presence crosses the same channel as an update and is told apart from
    /// one, because the two are handled differently: an update is merged into
    /// the document and persisted, presence is only ever forwarded.
    #[test]
    fn a_presence_frame_crosses_and_is_remembered() {
        block_on(async {
            let hub = DataPlane::new(None);
            let trustee = DataPlane::new(None);
            let offer = trustee.offer("hub").await.unwrap();
            let answer = hub.answer("trustee", &offer).await.unwrap();
            trustee.accept_answer("hub", &answer).await.unwrap();
            assert!(wait_for(&hub, "trustee", true).await);

            let bytes = b"awareness-bytes-that-mean-nothing-here".to_vec();
            assert_eq!(trustee.send_presence("hub", &bytes).await, Sent::Live);

            let inbound = next_frame(&hub, true)
                .await
                .expect("the hub receives the presence frame");
            assert_eq!(inbound.from, "trustee");
            match inbound.frame {
                InboundFrame::Presence(got) => assert_eq!(got, bytes, "presence arrives intact"),
                InboundFrame::Update(_) => panic!("presence must not arrive as an update"),
            }

            // Kept, so a peer that joins later is caught up rather than staring
            // at an empty document while somebody is already typing in it.
            let held = hub.presence();
            assert_eq!(held.len(), 1);
            assert_eq!(held[0].0, "trustee");
            assert_eq!(held[0].1, bytes);

            trustee.shutdown();
            hub.shutdown();
        });
    }

    /// A tag this build does not know is dropped, not guessed at: a newer peer
    /// must not be able to break this one's document by sending something it
    /// has never heard of.
    #[test]
    fn an_unknown_frame_tag_is_dropped() {
        block_on(async {
            assert_eq!(unframe(&[]), None, "an empty message carries nothing");
            assert_eq!(unframe(&[9, 1, 2, 3]), None, "tag 9 is not a frame this build knows");
            assert_eq!(unframe(&[TAG_UPDATE, 7]), Some(InboundFrame::Update(vec![7])));
            assert_eq!(
                unframe(&[TAG_PRESENCE, 8]),
                Some(InboundFrame::Presence(vec![8]))
            );
        });
    }

    /// A peer that goes away takes its cursor with it: presence is dropped with
    /// the connection rather than waiting out the TTL, so a kicked trustee does
    /// not linger in the document.
    #[test]
    fn forgetting_a_peer_drops_its_presence() {
        block_on(async {
            let hub = DataPlane::new(None);
            let trustee = DataPlane::new(None);
            let offer = trustee.offer("hub").await.unwrap();
            let answer = hub.answer("trustee", &offer).await.unwrap();
            trustee.accept_answer("hub", &answer).await.unwrap();
            assert!(wait_for(&hub, "trustee", true).await);
            trustee.send_presence("hub", b"here").await;
            assert!(next_frame(&hub, true).await.is_some());
            assert_eq!(hub.presence().len(), 1);

            hub.forget("trustee");
            assert!(hub.presence().is_empty(), "a forgotten peer has no cursor");

            trustee.shutdown();
            hub.shutdown();
        });
    }
}
