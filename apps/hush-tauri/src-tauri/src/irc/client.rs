//! One IRC connection: dial (optionally through Tor), register, serve, and
//! reconnect with backoff.
//!
//! The task owns the socket and the per-server snapshot. Outgoing commands
//! arrive on a channel from the Tauri commands; incoming lines are parsed and
//! applied to the snapshot, which fans out as `irc://*` events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tauri::AppHandle;

use crate::irc::protocol::{parse_ctcp, ParsedLine};
use crate::irc::state::{mentions_me, Events, ServerSnapshot};
use crate::irc::{
    format, nicklist, socks, tls, OutCmd, ServerConfig, LIBERA_ONION_HOST, LOBBY_CHANNEL,
    STATUS_TARGET,
};
use crate::tor::{TorManager, DEFAULT_BOOTSTRAP_TIMEOUT};

/// A read/write byte stream, boxed so TCP and TLS share one code path.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

/// How the backoff grows between reconnect attempts.
fn backoff_secs(attempt: u32) -> u64 {
    (1u64 << attempt.min(6)).min(60)
}

/// Run the connection until the user disconnects or a fatal error occurs.
pub async fn run(
    app: AppHandle,
    snapshot: Arc<Mutex<ServerSnapshot>>,
    cfg: ServerConfig,
    tor: TorManager,
    mut rx: UnboundedReceiver<OutCmd>,
) {
    let events = Events::new(app, cfg.id.clone());
    let mut attempt: u32 = 0;

    loop {
        set_phase(&snapshot, &events, "Connecting");
        match serve(&snapshot, &events, &cfg, &tor, &mut rx).await {
            Outcome::UserQuit => {
                mark_disconnected(&snapshot, &events, "Disconnected");
                return;
            }
            Outcome::Fatal(reason) => {
                mark_disconnected(&snapshot, &events, "Disconnected");
                events.error(STATUS_TARGET, &reason);
                return;
            }
            Outcome::Dropped(reason) => {
                mark_disconnected(&snapshot, &events, "Disconnected");
                events.error(STATUS_TARGET, &reason);
                let wait = backoff_secs(attempt);
                attempt = attempt.saturating_add(1);
                events.status(
                    STATUS_TARGET,
                    &format!("Reconnecting in {wait}s…"),
                    "info",
                );
                if wait_or_disconnect(&mut rx, wait).await {
                    return;
                }
            }
        }
    }
}

/// The result of one connection attempt.
enum Outcome {
    /// The user asked to quit (or the command channel closed).
    UserQuit,
    /// A configuration/dial problem that retrying will not fix.
    Fatal(String),
    /// The connection dropped; reconnect with backoff.
    Dropped(String),
}

/// Wait `secs`, but return `true` early if the user disconnects meanwhile.
async fn wait_or_disconnect(rx: &mut UnboundedReceiver<OutCmd>, secs: u64) -> bool {
    let deadline = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return false,
            cmd = rx.recv() => match cmd {
                Some(OutCmd::Disconnect) | Some(OutCmd::Quit(_)) | None => return true,
                Some(_) => {} // drop other commands while reconnecting
            }
        }
    }
}

/// Open one socket, register, and pump lines until it drops.
async fn serve(
    snapshot: &Arc<Mutex<ServerSnapshot>>,
    events: &Events,
    cfg: &ServerConfig,
    tor: &TorManager,
    rx: &mut UnboundedReceiver<OutCmd>,
) -> Outcome {
    let stream = match dial(cfg, tor, events).await {
        Ok(s) => s,
        Err(e) => return Outcome::Fatal(e),
    };
    set_phase(snapshot, events, "Registering");

    let (read_half, write_half) = tokio::io::split(stream);
    let (wtx, wrx) = unbounded_channel::<String>();
    let writer = tokio::spawn(writer_task(write_half, wrx));

    // Registration. CAP first so a SASL exchange can happen before NICK/USER
    // is acknowledged; PASS must precede NICK/USER.
    send(&wtx, "CAP LS 302".into());
    if let Some(pw) = cfg.server_password.as_deref().filter(|p| !p.is_empty()) {
        send(&wtx, format!("PASS {pw}"));
    }
    send(&wtx, format!("NICK {}", cfg.nick));
    send(
        &wtx,
        format!("USER {} 0 * :{}", cfg.username, cfg.realname),
    );

    let mut session = Session {
        snapshot,
        events,
        cfg,
        wtx: wtx.clone(),
        cap_pending: true,
        sasl_requested: false,
        registered: false,
        nick_attempts: 0,
        names_accum: HashMap::new(),
    };

    let cap_deadline = tokio::time::sleep(Duration::from_secs(6));
    tokio::pin!(cap_deadline);

    let mut reader = BufReader::new(read_half).lines();
    let outcome = loop {
        tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(outcome) = session.handle(&line) {
                        break outcome;
                    }
                }
                Ok(None) => break Outcome::Dropped("the server closed the connection".into()),
                Err(e) => break Outcome::Dropped(format!("read error: {e}")),
            },
            cmd = rx.recv() => match cmd {
                Some(OutCmd::Disconnect) => break Outcome::UserQuit,
                Some(OutCmd::Quit(reason)) => {
                    session.quit(reason);
                    // Give the writer a moment to flush the QUIT before the
                    // socket is torn down.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    break Outcome::UserQuit;
                }
                Some(cmd) => session.dispatch(cmd),
                None => break Outcome::UserQuit,
            },
            _ = &mut cap_deadline, if session.cap_pending => session.end_cap(),
        }
    };

    drop(wtx);
    // On a user quit the writer may still be flushing; give it the same grace.
    if matches!(outcome, Outcome::UserQuit) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    writer.abort();
    outcome
}

/// Dial the server, through Tor when asked.
async fn dial(
    cfg: &ServerConfig,
    tor: &TorManager,
    events: &Events,
) -> Result<Box<dyn AsyncStream>, String> {
    let tcp = if cfg.use_tor {
        events.status(STATUS_TARGET, "Bringing up Tor…", "info");
        let tor = tor.clone();
        let status = tokio::task::spawn_blocking(move || {
            tor.ensure_blocking(DEFAULT_BOOTSTRAP_TIMEOUT)
        })
        .await
        .map_err(|e| format!("the Tor task failed: {e}"))?;
        let port = status.socks_port.ok_or_else(|| {
            format!(
                "Tor is not ready ({}).",
                status
                    .message
                    .unwrap_or_else(|| format!("{:?}", status.phase).to_lowercase())
            )
        })?;
        events.status(STATUS_TARGET, "Connecting through Tor…", "info");
        socks::connect(port, &cfg.host, cfg.port)
            .await
            .map_err(|e| format!("Tor could not reach {}:{} — {e}", cfg.host, cfg.port))?
    } else {
        TcpStream::connect((cfg.host.as_str(), cfg.port))
            .await
            .map_err(|e| format!("could not connect to {}:{} — {e}", cfg.host, cfg.port))?
    };

    if !cfg.tls {
        return Ok(Box::new(tcp));
    }

    let config = if tls::is_onion(&cfg.host) {
        tls::onion_config()
    } else {
        tls::client_config()
    };
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = rustls::pki_types::ServerName::try_from(cfg.host.clone())
        .map_err(|e| format!("invalid server name {}: {e}", cfg.host))?;
    let stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS handshake with {} failed: {e}", cfg.host))?;
    Ok(Box::new(stream))
}

/// The rate-limited writer. Bursts of a few lines go out at once, then the
/// pace settles to one line per 500 ms so a slow network does not flood us.
async fn writer_task<W: AsyncWrite + Unpin>(mut w: W, mut rx: UnboundedReceiver<String>) {
    const BURST: f64 = 4.0;
    const PER_LINE_SECS: f64 = 0.5;

    let mut tokens = BURST;
    let mut last = tokio::time::Instant::now();
    while let Some(line) = rx.recv().await {
        loop {
            let now = tokio::time::Instant::now();
            tokens = (tokens + now.duration_since(last).as_secs_f64() / PER_LINE_SECS).min(BURST);
            last = now;
            if tokens >= 1.0 {
                tokens -= 1.0;
                break;
            }
            tokio::time::sleep(Duration::from_secs_f64((1.0 - tokens) * PER_LINE_SECS)).await;
        }
        if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\r\n").await.is_err() {
            break;
        }
        let _ = w.flush().await;
    }
    let _ = w.shutdown().await;
}

fn send(tx: &UnboundedSender<String>, line: String) {
    let _ = tx.send(line);
}

fn set_phase(snapshot: &Arc<Mutex<ServerSnapshot>>, events: &Events, phase: &str) {
    let snap = {
        let mut s = snapshot.lock().expect("snapshot poisoned");
        s.phase = phase.to_string();
        s.clone()
    };
    events.server(&snap);
}

fn mark_disconnected(snapshot: &Arc<Mutex<ServerSnapshot>>, events: &Events, phase: &str) {
    let snap = {
        let mut s = snapshot.lock().expect("snapshot poisoned");
        s.connected = false;
        s.registered = false;
        s.phase = phase.to_string();
        s.clone()
    };
    events.server(&snap);
}

/// Whether a host is Libera (clearnet or its onion), which auto-joins the lobby.
fn is_libera(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h.contains("libera") || h == LIBERA_ONION_HOST
}

/// State for one connected session's line handling.
struct Session<'a> {
    snapshot: &'a Arc<Mutex<ServerSnapshot>>,
    events: &'a Events,
    cfg: &'a ServerConfig,
    wtx: UnboundedSender<String>,
    cap_pending: bool,
    sasl_requested: bool,
    registered: bool,
    nick_attempts: u32,
    names_accum: HashMap<String, Vec<nicklist::NickEntry>>,
}

impl Session<'_> {
    fn send(&self, line: String) {
        send(&self.wtx, line);
    }

    fn end_cap(&mut self) {
        if self.cap_pending {
            self.send("CAP END".into());
            self.cap_pending = false;
        }
    }

    fn quit(&mut self, reason: Option<String>) {
        match reason.filter(|r| !r.is_empty()) {
            Some(r) => self.send(format!("QUIT :{r}")),
            None => self.send("QUIT".into()),
        }
    }

    /// Translate a UI command into wire lines, with a local echo where the
    /// server will not send one back (private messages).
    fn dispatch(&mut self, cmd: OutCmd) {
        match cmd {
            OutCmd::Raw(line) => {
                if !line.trim().is_empty() {
                    self.send(line);
                }
            }
            OutCmd::Privmsg {
                target,
                text,
                action,
            } => {
                let is_channel = self
                    .snapshot
                    .lock()
                    .expect("snapshot poisoned")
                    .is_channel(&target);
                if action {
                    self.send(format!("PRIVMSG {target} :\u{1}ACTION {text}\u{1}"));
                } else {
                    self.send(format!("PRIVMSG {target} :{text}"));
                }
                if !is_channel {
                    self.local_echo(&target, &text, action);
                }
            }
            OutCmd::Join { channel, key } => {
                self.snapshot
                    .lock()
                    .expect("snapshot poisoned")
                    .ensure_channel(&channel);
                match key.filter(|k| !k.is_empty()) {
                    Some(k) => self.send(format!("JOIN {channel} {k}")),
                    None => self.send(format!("JOIN {channel}")),
                }
            }
            OutCmd::Part { channel, reason } => match reason.filter(|r| !r.is_empty()) {
                Some(r) => self.send(format!("PART {channel} :{r}")),
                None => self.send(format!("PART {channel}")),
            },
            OutCmd::Nick(nick) => self.send(format!("NICK {nick}")),
            OutCmd::Quit(reason) => self.quit(reason),
            OutCmd::Disconnect => {}
        }
    }

    /// Show our own message immediately for a target the server will not echo
    /// (a query); channel messages come back from the server instead.
    fn local_echo(&mut self, target: &str, text: &str, action: bool) {
        let (nick, opened) = {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            let opened = snap.ensure_query(target);
            (snap.nick.clone(), opened)
        };
        let _ = opened;
        let segments = format::parse(text);
        self.events
            .message(target, &nick, if action { "action" } else { "say" }, &segments, false);
    }

    /// Handle one server line. Returns `Some` only to end the session.
    fn handle(&mut self, line: &str) -> Option<Outcome> {
        let p = ParsedLine::parse(line);
        let cmd = p.command.as_str();
        match cmd {
            "PING" => {
                let token = p.last().unwrap_or("");
                self.send(format!("PONG :{token}"));
            }
            "ERROR" => {
                return Some(Outcome::Dropped(
                    p.last().unwrap_or("the server closed the connection").to_string(),
                ));
            }
            "CAP" => self.handle_cap(&p),
            "AUTHENTICATE" => {
                if p.param(0) == Some("+") {
                    self.send_sasl();
                }
            }
            "903" => self.end_cap(),
            "904" | "905" | "906" | "907" => {
                self.events
                    .error(STATUS_TARGET, "SASL authentication failed; continuing unauthenticated.");
                self.end_cap();
            }
            "001" => self.handle_welcome(&p),
            "433" | "432" => self.handle_nick_in_use(&p),
            "353" => self.handle_names(&p),
            "366" => self.handle_end_of_names(&p),
            "332" | "331" => self.handle_topic_reply(&p),
            "333" => self.handle_topic_who(&p),
            "322" => {
                let channel = p.param(1).unwrap_or("");
                let users = p.param(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
                let topic = p.last().unwrap_or("");
                self.events.list_entry(channel, users, topic);
            }
            "323" => self.events.list_end(),
            "375" | "372" | "376" => {
                let text = p.last().unwrap_or("");
                self.events.status(STATUS_TARGET, text, "motd");
            }
            "JOIN" => self.handle_join(&p),
            "PART" => self.handle_part(&p),
            "QUIT" => self.handle_quit(&p),
            "NICK" => self.handle_nick(&p),
            "TOPIC" => self.handle_topic(&p),
            "MODE" => self.handle_mode(&p),
            "KICK" => self.handle_kick(&p),
            "PRIVMSG" => self.handle_message(&p, false),
            "NOTICE" => self.handle_message(&p, true),
            other => {
                // Unhandled numerics and server text belong in the status
                // window, which is where mIRC puts them.
                if other.chars().all(|c| c.is_ascii_digit()) {
                    let text = p
                        .last()
                        .map(str::to_string)
                        .unwrap_or_else(|| line.to_string());
                    self.events.status(STATUS_TARGET, &text, "info");
                }
            }
        }
        None
    }

    fn handle_cap(&mut self, p: &ParsedLine) {
        let sub = p.param(1).unwrap_or("").to_ascii_uppercase();
        let caps = p.last().unwrap_or("");
        match sub.as_str() {
            "LS" => {
                let offered: Vec<&str> = caps.split_whitespace().collect();
                let want_sasl = self.cfg.sasl.is_some() && offered.contains(&"sasl");
                let mut req = Vec::new();
                if offered.contains(&"multi-prefix") {
                    req.push("multi-prefix");
                }
                if want_sasl {
                    req.push("sasl");
                }
                if req.is_empty() {
                    self.end_cap();
                } else {
                    self.sasl_requested = want_sasl;
                    self.send(format!("CAP REQ :{}", req.join(" ")));
                }
            }
            "ACK" => {
                if self.sasl_requested && caps.split_whitespace().any(|c| c == "sasl") {
                    self.send("AUTHENTICATE PLAIN".into());
                } else {
                    self.end_cap();
                }
            }
            "NAK" => {
                self.sasl_requested = false;
                self.end_cap();
            }
            "NEW" => {
                // A late capability change; nothing to negotiate now.
            }
            _ => {}
        }
    }

    fn send_sasl(&self) {
        let Some(sasl) = self.cfg.sasl.as_ref() else {
            self.send("AUTHENTICATE +".into());
            return;
        };
        let raw = format!("\0{}\0{}", sasl.account, sasl.password);
        let payload = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        let mut rest = payload.as_str();
        while rest.len() >= 400 {
            let (chunk, tail) = rest.split_at(400);
            self.send(format!("AUTHENTICATE {chunk}"));
            rest = tail;
        }
        if rest.is_empty() {
            // Exactly a multiple of 400: an empty chunk terminates the payload.
            self.send("AUTHENTICATE +".into());
        } else {
            self.send(format!("AUTHENTICATE {rest}"));
        }
    }

    fn handle_welcome(&mut self, p: &ParsedLine) {
        let nick = p.param(0).unwrap_or(&self.cfg.nick).to_string();
        let snap = {
            let mut s = self.snapshot.lock().expect("snapshot poisoned");
            s.nick = nick.clone();
            s.connected = true;
            s.registered = true;
            s.phase = "Connected".into();
            s.clone()
        };
        self.registered = true;
        self.events.server(&snap);
        self.events.status(
            STATUS_TARGET,
            &format!("Connected to {} as {nick}.", self.cfg.host),
            "info",
        );

        let mut channels: Vec<String> = self.cfg.auto_join.clone();
        if is_libera(&self.cfg.host)
            && !channels
                .iter()
                .any(|c| c.eq_ignore_ascii_case(LOBBY_CHANNEL))
        {
            channels.push(LOBBY_CHANNEL.to_string());
        }
        for channel in channels {
            self.snapshot
                .lock()
                .expect("snapshot poisoned")
                .ensure_channel(&channel);
            self.send(format!("JOIN {channel}"));
        }
    }

    fn handle_nick_in_use(&mut self, p: &ParsedLine) {
        if self.registered {
            return;
        }
        self.nick_attempts = self.nick_attempts.saturating_add(1);
        let candidate = if self.nick_attempts <= 5 {
            format!("{}{}", self.cfg.nick, "_".repeat(self.nick_attempts as usize))
        } else if self.nick_attempts == 6 && !self.cfg.alt_nick.is_empty() {
            self.cfg.alt_nick.clone()
        } else {
            format!("Guest{:04}", rand::random::<u16>() % 10_000)
        };
        let _ = p;
        self.events.status(
            STATUS_TARGET,
            &format!("Nickname taken; trying {candidate}."),
            "info",
        );
        self.send(format!("NICK {candidate}"));
    }

    fn handle_names(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(2) else { return };
        let body = p.last().unwrap_or("");
        let key = channel.to_ascii_lowercase();
        let entry = self.names_accum.entry(key).or_default();
        for token in body.split_whitespace() {
            let parsed = nicklist::parse_entry(token);
            if !parsed.nick.is_empty() {
                entry.push(parsed);
            }
        }
    }

    fn handle_end_of_names(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(1) else { return };
        let key = channel.to_ascii_lowercase();
        let mut names = self.names_accum.remove(&key).unwrap_or_default();
        nicklist::sort(&mut names);
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            let ch = snap.ensure_channel(channel);
            ch.name = channel.to_string();
            ch.names = names.clone();
        }
        self.events.names(channel, &names);
    }

    fn handle_topic_reply(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(1) else { return };
        let topic = if p.command == "331" {
            String::new()
        } else {
            p.last().unwrap_or("").to_string()
        };
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            let ch = snap.ensure_channel(channel);
            ch.topic = topic.clone();
            ch.topic_who = None;
        }
        self.events.topic(channel, &topic, None);
    }

    fn handle_topic_who(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(1) else { return };
        let who = p.param(2).map(str::to_string);
        let topic = {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            let ch = snap.ensure_channel(channel);
            ch.topic_who = who.clone();
            ch.topic.clone()
        };
        self.events.topic(channel, &topic, who.as_deref());
    }

    fn handle_join(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(0) else { return };
        let nick = p.nick().unwrap_or("").to_string();
        let is_me = nick.eq_ignore_ascii_case(&self.my_nick());
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            {
                let ch = snap.ensure_channel(channel);
                ch.name = channel.to_string();
                if is_me {
                    ch.joined = true;
                    ch.names.clear();
                }
            }
            if !is_me && !nick.is_empty() {
                snap.upsert_nick(
                    channel,
                    nicklist::NickEntry {
                        nick: nick.clone(),
                        prefixes: String::new(),
                    },
                );
            }
        }
        self.events.join(channel, &nick);
    }

    fn handle_part(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(0) else { return };
        let nick = p.nick().unwrap_or("").to_string();
        let reason = if p.params.len() > 1 {
            p.last().unwrap_or("").to_string()
        } else {
            String::new()
        };
        let is_me = nick.eq_ignore_ascii_case(&self.my_nick());
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            if is_me {
                snap.channels
                    .retain(|c| !c.name.eq_ignore_ascii_case(channel));
            } else if let Some(ch) = snap.channel_mut(channel) {
                ch.names.retain(|n| !n.nick.eq_ignore_ascii_case(&nick));
            }
        }
        self.events.part(channel, &nick, &reason);
    }

    fn handle_quit(&mut self, p: &ParsedLine) {
        let nick = p.nick().unwrap_or("").to_string();
        let reason = p.last().unwrap_or("").to_string();
        if !nick.is_empty() {
            self.snapshot
                .lock()
                .expect("snapshot poisoned")
                .remove_nick(&nick);
        }
        self.events.quit(&nick, &reason);
    }

    fn handle_nick(&mut self, p: &ParsedLine) {
        let old = p.nick().unwrap_or("").to_string();
        let new = p.last().unwrap_or("").to_string();
        if old.is_empty() || new.is_empty() {
            return;
        }
        let is_me = old.eq_ignore_ascii_case(&self.my_nick());
        let snap = {
            let mut s = self.snapshot.lock().expect("snapshot poisoned");
            s.rename_nick(&old, &new);
            if is_me {
                s.nick = new.clone();
            }
            if is_me {
                Some(s.clone())
            } else {
                None
            }
        };
        if let Some(snap) = snap {
            self.events.server(&snap);
        }
        self.events.nick(&old, &new);
    }

    fn handle_topic(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(0) else { return };
        let topic = p.last().unwrap_or("").to_string();
        let who = p.nick().map(str::to_string);
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            let ch = snap.ensure_channel(channel);
            ch.topic = topic.clone();
            ch.topic_who = who.clone();
        }
        self.events.topic(channel, &topic, who.as_deref());
    }

    fn handle_mode(&mut self, p: &ParsedLine) {
        let Some(target) = p.param(0) else { return };
        let modes = p.param(1).unwrap_or("").to_string();
        let args = p.params.get(2..).map(|a| a.join(" ")).unwrap_or_default();
        let by = p.nick().unwrap_or("").to_string();

        // Track +o/+v and their removals in the nick list so the list and its
        // context menu stay truthful without a fresh NAMES.
        let target_is_channel = self
            .snapshot
            .lock()
            .expect("snapshot poisoned")
            .is_channel(target);
        if target_is_channel {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            if let Some(ch) = snap.channel_mut(target) {
                apply_mode_changes(&mut ch.names, &modes, p.params.get(2..).unwrap_or(&[]));
                ch.modes = merge_modes(&ch.modes, &modes);
            }
            let names = snap
                .channel(target)
                .map(|c| c.names.clone())
                .unwrap_or_default();
            drop(snap);
            self.events.names(target, &names);
        }
        self.events.mode(target, &by, &modes, &args);
    }

    fn handle_kick(&mut self, p: &ParsedLine) {
        let Some(channel) = p.param(0) else { return };
        let who = p.param(1).unwrap_or("").to_string();
        let by = p.nick().unwrap_or("").to_string();
        let reason = p.last().unwrap_or("").to_string();
        let is_me = who.eq_ignore_ascii_case(&self.my_nick());
        {
            let mut snap = self.snapshot.lock().expect("snapshot poisoned");
            if is_me {
                snap.channels
                    .retain(|c| !c.name.eq_ignore_ascii_case(channel));
            } else if let Some(ch) = snap.channel_mut(channel) {
                ch.names.retain(|n| !n.nick.eq_ignore_ascii_case(&who));
            }
        }
        self.events.part(
            channel,
            &who,
            &format!("was kicked by {by}{}", if reason.is_empty() { String::new() } else { format!(" ({reason})") }),
        );
    }

    fn handle_message(&mut self, p: &ParsedLine, is_notice: bool) {
        let Some(raw_target) = p.param(0) else { return };
        let from = p.nick().unwrap_or("").to_string();
        let raw_text = p.last().unwrap_or("");
        let my_nick = self.my_nick();
        let from_is_me = from.eq_ignore_ascii_case(&my_nick);
        let from_is_user = p.prefix.as_deref().is_some_and(|s| s.contains('!'));

        // CTCP ACTION (`/me`), otherwise ordinary text.
        let (kind, body) = if !is_notice {
            match parse_ctcp(raw_text) {
                Some((cmd, arg)) if cmd.eq_ignore_ascii_case("ACTION") => ("action", arg.to_string()),
                Some(_) => return, // other CTCP queries are not chat
                None => ("say", raw_text.to_string()),
            }
        } else {
            match parse_ctcp(raw_text) {
                Some(_) => return,
                None => ("notice", raw_text.to_string()),
            }
        };

        let target_is_channel = self
            .snapshot
            .lock()
            .expect("snapshot poisoned")
            .is_channel(raw_target);

        // Where the line lands: a channel by name; a PM in the sender's query
        // window (opened on arrival); a server notice in the status window.
        let target = if target_is_channel {
            raw_target.to_string()
        } else if from_is_user && !from.is_empty() {
            self.snapshot
                .lock()
                .expect("snapshot poisoned")
                .ensure_query(&from);
            from.clone()
        } else {
            STATUS_TARGET.to_string()
        };

        let plain = format::plain(&body);
        let highlight = !from_is_me && mentions_me(&plain, &my_nick);
        let segments = format::parse(&body);
        self.events
            .message(&target, &from, kind, &segments, highlight);
    }

    fn my_nick(&self) -> String {
        self.snapshot
            .lock()
            .expect("snapshot poisoned")
            .nick
            .clone()
    }
}

/// Apply `+o/-o/+v/-v` style changes to a channel's nick list.
fn apply_mode_changes(
    names: &mut Vec<nicklist::NickEntry>,
    modes: &str,
    args: &[String],
) {
    let mut adding = true;
    let mut arg_iter = args.iter();
    for c in modes.chars() {
        match c {
            '+' => adding = true,
            '-' => adding = false,
            'o' | 'v' | 'h' | 'a' | 'q' => {
                let Some(nick) = arg_iter.next() else { break };
                let prefix = match c {
                    'q' => '~',
                    'a' => '&',
                    'o' => '@',
                    'h' => '%',
                    _ => '+',
                };
                if let Some(entry) = names
                    .iter_mut()
                    .find(|n| n.nick.eq_ignore_ascii_case(nick))
                {
                    if adding {
                        if !entry.prefixes.contains(prefix) {
                            entry.prefixes.push(prefix);
                        }
                    } else {
                        entry.prefixes.retain(|p| p != prefix);
                    }
                }
            }
            // Modes that take an argument but are not prefixes still consume one.
            'k' | 'l' | 'b' | 'e' | 'I' | 'O' => {
                let _ = arg_iter.next();
            }
            _ => {}
        }
    }
    nicklist::sort(names);
}

/// Fold a mode string into the channel's remembered modes, toggling letters.
fn merge_modes(current: &str, delta: &str) -> String {
    let mut set: Vec<char> = current
        .trim_start_matches('+')
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let mut adding = true;
    for c in delta.chars() {
        match c {
            '+' => adding = true,
            '-' => adding = false,
            c if c.is_ascii_alphabetic() => {
                if adding {
                    if !set.contains(&c) {
                        set.push(c);
                    }
                } else {
                    set.retain(|x| *x != c);
                }
            }
            _ => {}
        }
    }
    set.sort_unstable();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(3), 8);
        assert_eq!(backoff_secs(10), 60);
    }

    #[test]
    fn libera_detection_covers_clearnet_and_onion() {
        assert!(is_libera("irc.libera.chat"));
        assert!(is_libera(LIBERA_ONION_HOST));
        assert!(!is_libera("irc.oftc.net"));
    }

    #[test]
    fn mode_changes_update_prefixes() {
        let mut names = vec![
            nicklist::NickEntry { nick: "alice".into(), prefixes: String::new() },
            nicklist::NickEntry { nick: "bob".into(), prefixes: String::new() },
        ];
        apply_mode_changes(&mut names, "+ov", &["alice".into(), "bob".into()]);
        assert_eq!(prefixes_of(&names, "alice"), "@");
        assert_eq!(prefixes_of(&names, "bob"), "+");
        // `-o` demotes alice, and the list is re-sorted, so look her up by nick
        // rather than by index.
        apply_mode_changes(&mut names, "-o", &["alice".into()]);
        assert_eq!(prefixes_of(&names, "alice"), "");
        assert_eq!(prefixes_of(&names, "bob"), "+");
    }

    fn prefixes_of(names: &[nicklist::NickEntry], nick: &str) -> String {
        names
            .iter()
            .find(|n| n.nick == nick)
            .map(|n| n.prefixes.clone())
            .unwrap_or_default()
    }

    #[test]
    fn merge_modes_toggles_letters() {
        assert_eq!(merge_modes("", "+nt"), "nt");
        assert_eq!(merge_modes("nt", "+s"), "nst");
        assert_eq!(merge_modes("nst", "-n"), "st");
    }
}
