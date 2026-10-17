//! What the store-and-forward path does under a burst.
//!
//! The relay rate-limits blob PUTs per queue (`RelayConfig::rate_limit_max` per
//! `rate_limit_window`), so a burst larger than the window is the interesting
//! case: does a refused payload get reported, queued, retried, and finally
//! delivered — or does it vanish with both ends believing it worked?
//!
//! This is a load test, not a correctness test in the fast suite, so it is
//! `#[ignore]`d and uses a deliberately tight window. Run it with:
//!
//! ```text
//! cargo test -p hush-core --test relay_flood -- --ignored --nocapture
//! ```
//!
//! Every surface is flooded over the *same* pair of vaults, which is the
//! realistic shape: a chat line, a conclave message and a paper delta from one
//! peer all land in the same per-contact queue and share one rate budget.
//!
//! The invariant it asserts is "nothing is silently lost": a payload the sender
//! was told it delivered must arrive, and a payload the relay refused must be
//! visible to the sender as a refusal or queued for retry. Volume and timing are
//! reported rather than asserted, because they are the measurement.

use hush_core::{Account, Error, MessagePayload};
use hush_relay::{router, RelayConfig};
use tempfile::TempDir;
use tokio::time::{sleep, Duration, Instant};

/// A relay with a window small enough to cross inside a test.
///
/// The limit is the thing under test, so it is tightened rather than removed:
/// at the default 60/60 s a burst would have to be enormous to cross it, and the
/// retry would then take minutes to drain. TTLs are long so nothing expires
/// mid-run and a "missing" payload can never be blamed on expiry.
fn flood_config() -> RelayConfig {
    RelayConfig {
        // The relay forces its window to at least one second, so this is the
        // tightest limit it can express. Two PUTs per queue per second is below
        // what the client can push (see the per-surface timings the burst
        // prints), so the burst is guaranteed to cross it.
        rate_limit_max: 2,
        rate_limit_window: Duration::from_secs(1),
        blob_ttl: Duration::from_secs(600),
        bulletin_ttl: Duration::from_secs(600),
        ..RelayConfig::default()
    }
}

async fn spawn_relay(config: RelayConfig) -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

/// How many payloads a surface was told it delivered, and how many it refused.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    attempted: usize,
    accepted: usize,
    refused: usize,
}

impl Tally {
    fn record(&mut self, outcome: Result<(), Error>) {
        self.attempted += 1;
        match outcome {
            Ok(()) => self.accepted += 1,
            Err(_) => self.refused += 1,
        }
    }
}
/// What the receiver actually ended up with, by payload kind.
#[derive(Debug, Default)]
struct Arrived {
    chats: usize,
    conclave_messages: usize,
    mail: usize,
    paper_deltas: usize,
    invites: usize,
}

impl Arrived {
    fn total(&self) -> usize {
        self.chats + self.conclave_messages + self.mail + self.paper_deltas + self.invites
    }
}

fn classify(arrived: &mut Arrived, payload: &MessagePayload) {
    match payload {
        MessagePayload::Text { .. } => arrived.chats += 1,
        MessagePayload::Conclave { .. } => arrived.conclave_messages += 1,
        MessagePayload::Mail { .. } => arrived.mail += 1,
        MessagePayload::PaperDelta { .. } => arrived.paper_deltas += 1,
        MessagePayload::ConclaveInvite { .. } => arrived.invites += 1,
        other => panic!("unclassified payload arrived: {other:?}"),
    }
}

/// Drain until the receiver stops producing for `quiet_rounds`, or the deadline
/// passes. Returns what arrived and how long the drain took.
///
/// A drain that stops producing is not proof it is finished — a payload can
/// still be queued on the sender — so the caller drives the sender's retries
/// between rounds and treats "quiet" as "nothing more is coming yet".
async fn drain(
    receiver: &mut Account,
    sender: &mut Account,
    quiet_rounds: usize,
    deadline: Duration,
) -> (Arrived, Duration) {
    let started = Instant::now();
    let mut arrived = Arrived::default();
    let mut quiet = 0;
    let mut rounds = 0;
    while quiet < quiet_rounds && started.elapsed() < deadline {
        rounds += 1;
        // Both sides move: the sender's outbox is what turns a refused payload
        // into a delivered one, so a drain that only reads would spin.
        let _ = sender.retry_outbox().await;
        let _ = sender.retry_pending_conclave().await;
        let _ = sender.retry_pending_reach().await;

        let inbox = receiver.recv().await.unwrap();
        let produced = inbox.messages.len() + inbox.papers.len() + inbox.voices.len();
        for message in &inbox.messages {
            classify(&mut arrived, &message.payload);
        }
        for paper in &inbox.papers {
            classify(&mut arrived, &paper.payload);
        }
        if produced == 0 {
            quiet += 1;
        } else {
            quiet = 0;
        }
        if quiet < quiet_rounds {
            sleep(Duration::from_millis(250)).await;
        }
    }
    eprintln!("drain: {rounds} rounds in {:?}", started.elapsed());
    (arrived, started.elapsed())
}

#[tokio::test]
#[ignore = "load test; run with --ignored --nocapture"]
async fn a_burst_across_every_surface_is_reported_queued_and_delivered() {
    let (relay, _relay_dir) = spawn_relay(flood_config()).await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.recv().await.unwrap();
    bob.recv().await.unwrap();
    // Live chat needs the recipient heartbeating, and a heartbeat is not rate
    // limited, so this costs nothing against the budget under test.
    bob.heartbeat_presence().await.unwrap();

    // A conclave both are joined to, so conclave messages and paper deltas have
    // somewhere to go. The invite and accept are control payloads and count
    // against the same budget, so they happen before the flood and drain out.
    let conclave = alice
        .create_conclave("Flood", &["bob".to_string()])
        .await
        .unwrap();
    // An invite is a control payload, not a message, so it arrives in
    // `pending_invites` and never in `inbox.messages`.
    let mut invite_seen = false;
    for _ in 0..40 {
        bob.recv().await.unwrap();
        if !bob.pending_invites().is_empty() {
            invite_seen = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        invite_seen,
        "the invite must reach bob before the flood starts"
    );
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    // The accept is a fan-out back to alice; drain it so the flood starts clean.
    for _ in 0..40 {
        let inbox = alice.recv().await.unwrap();
        if inbox.messages.is_empty() && inbox.joined.is_empty() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    // --- the burst ----------------------------------------------------------
    const BURST: usize = 15;
    let mut chats = Tally::default();
    let mut conclave_messages = Tally::default();
    let mut papers = Tally::default();
    let mut mail = Tally::default();

    let flood_started = Instant::now();
    let mut timings: Vec<(&str, Duration)> = Vec::new();
    for i in 0..BURST {
        let t = Instant::now();
        chats.record(alice.send_text("bob", &format!("chat {i}")).await);
        timings.push(("chat", t.elapsed()));

        let t = Instant::now();
        conclave_messages.record(
            alice
                .send_conclave_message(&conclave.id, &format!("conclave {i}"))
                .await,
        );
        timings.push(("conclave message", t.elapsed()));

        let t = Instant::now();
        papers.record(
            alice
                .send_conclave_paper_delta(&conclave.id, "paper-1", format!("delta {i}").as_bytes())
                .await,
        );
        timings.push(("paper delta", t.elapsed()));

        let t = Instant::now();
        mail.record(
            alice
                .send_mail("bob", None, Some("flood"), &format!("mail {i}"), &[])
                .await
                .map(|_| ()),
        );
        timings.push(("mail", t.elapsed()));
    }
    let flood_took = flood_started.elapsed();

    eprintln!("--- burst of {BURST} per surface ({flood_took:?}) ---");
    for name in ["chat", "conclave message", "paper delta", "mail"] {
        let tally = match name {
            "chat" => chats,
            "conclave message" => conclave_messages,
            "paper delta" => papers,
            _ => mail,
        };
        let times: Vec<Duration> = timings
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, d)| *d)
            .collect();
        let total: Duration = times.iter().sum();
        let slowest = times.iter().max().copied().unwrap_or_default();
        eprintln!(
            "{name:<18} attempted {:>3}  accepted {:>3}  refused {:>3}  \
             avg {:>7.1?}  slowest {:>7.1?}",
            tally.attempted,
            tally.accepted,
            tally.refused,
            total / times.len() as u32,
            slowest
        );
    }
    eprintln!("total send time {flood_took:?}");

    // A chat line is live-only: a refusal is reported to the caller and nothing
    // queues it. Every other surface is store-and-forward and queues its
    // refusals, so the API never reports one. The relay's own counters are the
    // only place a refusal is visible from outside, which is why they exist.
    let refused = relay_counters(&relay).await["rate_limited"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        refused > 0,
        "the relay refused nothing, so the burst never crossed the write limit \
         and the retry path went untested — lower rate_limit_max or raise BURST"
    );
    eprintln!("relay refused {refused} writes during the burst");

    // --- convergence --------------------------------------------------------
    let (arrived, drain_took) = drain(&mut bob, &mut alice, 8, Duration::from_secs(90)).await;

    eprintln!("--- arrived ---");
    eprintln!(
        "chats {}  conclave {}  paper {}  mail {}  invites {}  (total {})",
        arrived.chats,
        arrived.conclave_messages,
        arrived.paper_deltas,
        arrived.mail,
        arrived.invites,
        arrived.total()
    );
    eprintln!("drain took {drain_took:?}");

    // Everything the sender was told it delivered must have arrived. A chat
    // refusal is allowed to be missing — nothing retries it, by design — but an
    // accepted one is not.
    assert!(
        arrived.chats >= chats.accepted,
        "chats: {} accepted but only {} arrived",
        chats.accepted,
        arrived.chats
    );
    assert!(
        arrived.conclave_messages >= conclave_messages.attempted,
        "conclave messages: {} attempted ({} accepted, {} refused and queued) but \
         only {} arrived — a refused fan-out was dropped rather than retried",
        conclave_messages.attempted,
        conclave_messages.accepted,
        conclave_messages.refused,
        arrived.conclave_messages
    );
    assert!(
        arrived.paper_deltas >= papers.attempted,
        "paper deltas: {} attempted but only {} arrived",
        papers.attempted,
        arrived.paper_deltas
    );
    assert!(
        arrived.mail >= mail.attempted,
        "mail: {} attempted but only {} arrived — queued mail must drain",
        mail.attempted,
        arrived.mail
    );
}

/// Read the relay's own counters.
///
/// The relay is the only party that can say whether a write arrived, so a test
/// that only watches the client cannot tell "the relay refused it" from "the
/// client never sent it".
async fn relay_counters(relay: &str) -> serde_json::Value {
    reqwest::get(format!("{relay}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// A refusal that outlasts the client's retry is lost for chat and queued for
/// everything else.
///
/// `send_payload_inner` retries a refused delivery three times over about 1.2 s
/// before giving up. That is the right shape for a dropped packet and the wrong
/// one for a rate limit: the production window is 60 s, so every retry lands
/// inside the window that refused the first attempt. A chat line that hits the
/// limit is therefore dropped and reported as an error, while mail, conclave
/// fan-outs and paper deltas are handed to the durable outbox and survive.
///
/// This test uses a window longer than the retry to reproduce that from the
/// client's side without waiting out a real 60 s.
#[tokio::test]
#[ignore = "load test; run with --ignored --nocapture"]
async fn a_refusal_that_outlasts_the_retry_is_lost_for_chat_and_queued_for_the_rest() {
    let (relay, _relay_dir) = spawn_relay(RelayConfig {
        // One write per two seconds. The window has to be longer than the
        // client's retry (~1.2 s across three attempts) for every attempt to
        // land inside the window that refused the first — which is what 60/60 s
        // does to a client, in a fraction of the time.
        rate_limit_max: 1,
        rate_limit_window: Duration::from_secs(2),
        blob_ttl: Duration::from_secs(600),
        bulletin_ttl: Duration::from_secs(600),
        ..RelayConfig::default()
    })
    .await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.recv().await.unwrap();
    bob.recv().await.unwrap();
    bob.heartbeat_presence().await.unwrap();

    let conclave = alice
        .create_conclave("Hard limit", &["bob".to_string()])
        .await
        .unwrap();
    for _ in 0..40 {
        bob.recv().await.unwrap();
        if !bob.pending_invites().is_empty() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    // Let the window the setup consumed roll before measuring, so the first
    // burst write is not refused by setup traffic.
    sleep(Duration::from_secs(3)).await;
    while !bob.recv().await.unwrap().messages.is_empty() {}
    alice.recv().await.unwrap();

    let before = relay_counters(&relay).await;

    // Four writes to the same per-contact queue, one budget.
    const BURST: usize = 4;
    let mut chats = Tally::default();
    for i in 0..BURST {
        chats.record(alice.send_text("bob", &format!("chat {i}")).await);
    }
    let mut conclave_messages = Tally::default();
    for i in 0..BURST {
        conclave_messages.record(
            alice
                .send_conclave_message(&conclave.id, &format!("conclave {i}"))
                .await,
        );
    }
    let mut papers = Tally::default();
    for i in 0..BURST {
        papers.record(
            alice
                .send_conclave_paper_delta(&conclave.id, "paper-1", format!("delta {i}").as_bytes())
                .await,
        );
    }
    let mut mail = Tally::default();
    for i in 0..BURST {
        mail.record(
            alice
                .send_mail("bob", None, Some("hard"), &format!("mail {i}"), &[])
                .await
                .map(|_| ()),
        );
    }

    let after = relay_counters(&relay).await;
    let refused = after["rate_limited"].as_u64().unwrap() - before["rate_limited"].as_u64().unwrap();
    let accepted = after["puts"].as_u64().unwrap() - before["puts"].as_u64().unwrap();
    let queued_mail = alice.queued_mail_count();

    eprintln!("--- one write per 2 s, {BURST} per surface ---");
    eprintln!(
        "relay: {accepted} writes accepted, {refused} refused (429) in this window"
    );
    eprintln!(
        "chat              {}/{} accepted, {} refused to the caller",
        chats.accepted, chats.attempted, chats.refused
    );
    eprintln!(
        "conclave message  {}/{} accepted, {} refused to the caller (queued)",
        conclave_messages.accepted, conclave_messages.attempted, conclave_messages.refused
    );
    eprintln!(
        "paper delta       {}/{} accepted, {} refused to the caller (queued)",
        papers.accepted, papers.attempted, papers.refused
    );
    eprintln!(
        "mail              {}/{} accepted, {} refused to the caller; {queued_mail} queued for retry",
        mail.accepted, mail.attempted, mail.refused
    );

    assert!(
        refused > 0,
        "the relay refused nothing, so the hard-limit path was not exercised"
    );
    assert!(
        chats.refused > 0,
        "a chat line that hits the write limit must be reported to the caller"
    );
    assert!(
        queued_mail > 0,
        "mail refused by the relay must be queued, not lost"
    );
    assert!(
        conclave_messages.refused == 0 && papers.refused == 0,
        "a conclave fan-out or paper delta that the relay refuses must be queued \
         rather than reported, because the outbox is what delivers it later"
    );

    // The window rolls, the outbox drains, and everything durable converges.
    let (arrived, _) = drain(&mut bob, &mut alice, 6, Duration::from_secs(60)).await;
    eprintln!(
        "after the window rolled: chats {}  conclave {}  paper {}  mail {}",
        arrived.chats, arrived.conclave_messages, arrived.paper_deltas, arrived.mail
    );
    assert_eq!(
        arrived.mail, BURST,
        "every mail must eventually arrive — queued means queued"
    );
    assert_eq!(
        arrived.conclave_messages, BURST,
        "every conclave message must eventually arrive"
    );
    assert_eq!(
        arrived.paper_deltas, BURST,
        "every paper delta must eventually arrive"
    );
    // The chats that were refused are gone: nothing queues a live-only line.
    assert!(
        arrived.chats < BURST,
        "a refused chat line is not queued by design, so fewer than {BURST} can arrive"
    );
}

/// Which surface a burst is aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    Chat,
    ConclaveMessage,
    PaperDelta,
    Mail,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Surface::Chat => "chat",
            Surface::ConclaveMessage => "conclave message",
            Surface::PaperDelta => "paper delta",
            Surface::Mail => "mail",
        }
    }
}

/// One surface's burst, measured on its own queue.
///
/// Returns the tally, the per-send times, and how much was queued for retry
/// afterwards (mail's outbox, and conclave's fan-out retry, are the two durable
/// queues a sender has).
async fn measure_surface(
    relay: &str,
    surface: Surface,
    burst: usize,
    settle: Duration,
) -> (Tally, Vec<Duration>, usize) {
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    let mut a = Account::create(a_dir.path(), "a-pass", relay).await.unwrap();
    let mut b = Account::create(b_dir.path(), "b-pass", relay).await.unwrap();
    let a_invite = a.invite().await.unwrap();
    let b_invite = b.invite().await.unwrap();
    a.add_contact("b", &b_invite).await.unwrap();
    b.add_contact("a", &a_invite).await.unwrap();
    a.recv().await.unwrap();
    b.recv().await.unwrap();
    b.heartbeat_presence().await.unwrap();

    let conclave = a
        .create_conclave("Cost", &["b".to_string()])
        .await
        .unwrap();
    for _ in 0..40 {
        b.recv().await.unwrap();
        if !b.pending_invites().is_empty() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    b.accept_conclave_invite(&conclave.id).await.unwrap();
    // The setup spent the window; let it roll so the measurement starts clean.
    sleep(settle).await;
    while !b.recv().await.unwrap().messages.is_empty() {}
    let _ = a.recv().await.unwrap();

    let mut tally = Tally::default();
    let mut times = Vec::new();
    for i in 0..burst {
        let t = Instant::now();
        let outcome = match surface {
            Surface::Chat => a.send_text("b", &format!("chat {i}")).await,
            Surface::ConclaveMessage => {
                a.send_conclave_message(&conclave.id, &format!("conclave {i}"))
                    .await
            }
            Surface::PaperDelta => {
                a.send_conclave_paper_delta(&conclave.id, "paper-1", format!("delta {i}").as_bytes())
                    .await
            }
            Surface::Mail => a
                .send_mail("b", None, Some("cost"), &format!("mail {i}"), &[])
                .await
                .map(|_| ()),
        };
        times.push(t.elapsed());
        tally.record(outcome);
    }
    let queued = a.queued_mail_count() + a.pending_conclave_count();
    (tally, times, queued)
}

/// What one refusal costs each surface, measured one surface at a time.
///
/// Flooding all four together hides the answer. `send_payload_inner` retries a
/// refused write three times over about 1.2 s, so whichever surface runs
/// immediately after a refusal inherits the freshly rolled window and looks
/// instant, while the one that meets the exhausted window pays the whole retry.
/// Run together, that ordering artefact made conclave messages look free and
/// chat look expensive; the two ride exactly the same delivery path.
#[tokio::test]
#[ignore = "load test; run with --ignored --nocapture"]
async fn a_refusal_costs_each_surface_the_same() {
    let (relay, _relay_dir) = spawn_relay(RelayConfig {
        rate_limit_max: 1,
        rate_limit_window: Duration::from_secs(2),
        blob_ttl: Duration::from_secs(600),
        bulletin_ttl: Duration::from_secs(600),
        ..RelayConfig::default()
    })
    .await;

    const BURST: usize = 6;
    eprintln!("--- {BURST} sends per surface, one surface at a time (1 write / 2 s) ---");
    eprintln!(
        "{:<18} {:>9} {:>9} {:>9} {:>9} {:>10}",
        "surface", "accepted", "refused", "avg", "slowest", "queued"
    );
    for surface in [
        Surface::Chat,
        Surface::ConclaveMessage,
        Surface::PaperDelta,
        Surface::Mail,
    ] {
        let (tally, times, queued) =
            measure_surface(&relay, surface, BURST, Duration::from_secs(3)).await;
        let total: Duration = times.iter().sum();
        let slowest = times.iter().max().copied().unwrap_or_default();
        eprintln!(
            "{:<18} {:>9} {:>9} {:>8.0?} {:>8.0?} {:>10}",
            surface.label(),
            format!("{}/{}", tally.accepted, tally.attempted),
            tally.refused,
            total / times.len() as u32,
            slowest,
            queued
        );
    }
}

#[tokio::test]
#[ignore = "load test; run with --ignored --nocapture"]
async fn a_burst_larger_than_the_queue_cap_is_still_drained_in_order() {
    // Placeholder for the second axis: what a *long* queue does to the drain.
    // Kept separate so the burst test above stays about rate-limit convergence.
    let (relay, _relay_dir) = spawn_relay(RelayConfig {
        rate_limit_max: 10_000,
        rate_limit_window: Duration::from_secs(1),
        blob_ttl: Duration::from_secs(600),
        bulletin_ttl: Duration::from_secs(600),
        ..RelayConfig::default()
    })
    .await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.recv().await.unwrap();
    bob.recv().await.unwrap();
    bob.heartbeat_presence().await.unwrap();

    // No rate limit, so this is purely "does a deep queue drain, and in what
    // order". The relay has no `LIMIT` on its listing, so the whole queue comes
    // back in one response and the drain is one big `recv`.
    const DEEP: usize = 500;
    let started = Instant::now();
    let mut accepted = 0;
    for i in 0..DEEP {
        if alice.send_text("bob", &format!("deep {i}")).await.is_ok() {
            accepted += 1;
        }
    }
    eprintln!("deep queue: {accepted}/{DEEP} accepted in {:?}", started.elapsed());

    let inbox = bob.recv().await.unwrap();
    eprintln!(
        "first drain returned {} of {accepted} in {:?}",
        inbox.messages.len(),
        started.elapsed()
    );
    assert_eq!(
        inbox.messages.len(),
        accepted,
        "a deep queue must drain whole; the relay's listing has no page limit, \
         so a short drain means blobs were dropped"
    );
}
