use std::path::PathBuf;

use clap::{Parser, Subcommand};
use std::path::PathBuf as StdPathBuf;

use hush_core::{config, resolve_data_dir, Account, MessagePayload, TrusteeReach};

#[derive(Parser)]
#[command(name = "hush-cli", about = "Dogfood client for Hush")]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    dir: PathBuf,
    /// Unlock passphrase (or set env HUSH_PASSPHRASE).
    #[arg(long, global = true, env = "HUSH_PASSPHRASE")]
    passphrase: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init {
        #[arg(long)]
        passphrase: String,
        /// Relay URL for this vault. Defaults to HUSH_RELAY_URL, then client-config.json.
        #[arg(long)]
        relay: Option<String>,
        /// The relay's v3 onion address. Recorded alongside the clearnet base
        /// so the Tor switch can move between them; with `--tor`, the vault
        /// starts on the onion.
        #[arg(long)]
        onion: Option<String>,
        /// Start this vault with the relay path through Tor. Needs `--onion`.
        #[arg(long)]
        tor: bool,
        /// The vault's name. It becomes the suffix of this member's mail
        /// address (`name@vaultname`) and travels in the invite.
        #[arg(long)]
        name: Option<String>,
    },
    Invite,
    AddContact {
        nickname: String,
        invite: String,
        /// Reach this contact by mail only: mail flows, but there is no live
        /// chat and no conclave. The choice is announced to them, and either
        /// side narrowing makes the relationship mail-only.
        #[arg(long)]
        mail_only: bool,
    },
    /// Change how this vault reaches an existing contact (Task 24).
    SetReach {
        nickname: String,
        /// `full` or `mail-only`.
        #[arg(long, default_value = "full")]
        reach: String,
    },
    Send {
        to: String,
        #[arg(long)]
        text: String,
    },
    SendFile {
        to: String,
        #[arg(long)]
        path: StdPathBuf,
    },
    Recv {
        #[arg(long)]
        save_dir: Option<StdPathBuf>,
    },
    Contacts,
    /// The Tor relay switch: `on` (with `--onion` to attach first), `off`, `show`.
    /// `set --url <url>` moves the vault's clearnet relay (contacts on the old
    /// relay must move too — the mailbox lives on the relay).
    Relay {
        #[arg(default_value = "show")]
        action: String,
        /// With `on`: the relay's v3 onion address, if none is on file yet.
        #[arg(long)]
        onion: Option<String>,
        /// With `set`: the new clearnet relay base.
        #[arg(long)]
        url: Option<String>,
    },
    /// Read or change the central client config (DR-14).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Print the published drop key and the drop page's URL.
    DropKeyShow,
    /// Generate the drop keypair. Fails when one already exists.
    DropKeygen,
    /// Rotate the drop keypair; the retired key keeps draining until drops
    /// still in flight on it expire at the relay.
    DropRotate,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show effective relay URL and daemon address with their source.
    Show,
    /// Write values into `client-config.json` under the data root (empty string clears).
    Set {
        #[arg(long)]
        relay: Option<String>,
        /// TCP bind address; empty string disables the TCP listener (socket only).
        #[arg(long)]
        daemon_addr: Option<String>,
        /// Unix socket path; empty string restores the runtime-dir default.
        #[arg(long)]
        daemon_socket: Option<String>,
    },
    /// Print the config file path.
    Path,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let dir = resolve_data_dir(&cli.dir);
    let passphrase = cli.passphrase.clone();
    match cli.command {
        Commands::Init {
            passphrase,
            relay,
            onion,
            tor,
            name,
        } => {
            if tor && onion.is_none() {
                return Err("--tor needs --onion: Tor relay mode moves the relay path to the \
                            onion address, so there must be one on file."
                    .into());
            }
            let relay = config::resolve_relay_url(&dir, relay.as_deref());
            let mut account = Account::create(&dir, &passphrase, &relay).await?;
            if let Some(onion) = onion.as_deref() {
                account.set_relay_tor(tor, Some(onion)).await?;
            }
            if let Some(name) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                account.set_vault_name(name).await?;
            }
            println!("vault ready at {}", dir.display());
            println!("relay: {}", account.relay());
            if let Some(onion) = account.onion_relay() {
                println!("onion relay: {}", onion);
            }
            println!("vault name: {}", account.vault_name());
            if let Some(code) = account.take_pending_recovery_code() {
                println!("SAVE THIS RECOVERY CODE (shown once): {}", code);
            }
        }
        Commands::Invite => {
            let mut account = open(&dir, &passphrase).await?;
            println!("{}", account.invite().await?);
        }
        Commands::Relay { action, onion, url } => {
            let mut account = open(&dir, &passphrase).await?;
            match action.as_str() {
                "on" => {
                    account.set_relay_tor(true, onion.as_deref()).await?;
                    println!("relay: {} (through tor)", account.relay());
                }
                "off" => {
                    account.set_relay_tor(false, None).await?;
                    println!("relay: {} (direct)", account.relay());
                }
                "show" => {
                    println!("switch: {}", if account.relay_tor_mode() { "tor" } else { "direct" });
                    println!("relay: {}", account.relay());
                    match account.onion_relay() {
                        Some(onion) => println!("onion: {}", onion),
                        None => println!("onion: (none on file)"),
                    }
                }
                "set" => {
                    let url = url.as_deref().ok_or(
                        "relay set needs --url <http(s)://host:port>",
                    )?;
                    account.set_relay_clearnet(url).await?;
                    println!("relay: {} (direct base updated)", account.relay());
                }
                other => {
                    return Err(format!(
                        "unknown relay action {:?}; use on, off, show, or set",
                        other
                    )
                    .into());
                }
            }
        }
        Commands::AddContact {
            nickname,
            invite,
            mail_only,
        } => {
            let mut account = open(&dir, &passphrase).await?;
            account.add_contact(&nickname, &invite).await?;
            if mail_only {
                account
                    .set_contact_reach(&nickname, TrusteeReach::MailOnly)
                    .await?;
            }
            println!(
                "added contact {nickname}{}",
                if mail_only { " (mail only)" } else { "" }
            );
        }
        Commands::SetReach { nickname, reach } => {
            let reach = parse_reach(&reach)?;
            let mut account = open(&dir, &passphrase).await?;
            account.set_contact_reach(&nickname, reach).await?;
            println!(
                "{nickname} is now {}",
                match reach {
                    TrusteeReach::MailOnly => "mail only",
                    TrusteeReach::Full => "full: mail, chat and conclaves",
                }
            );
        }
        Commands::Send { to, text } => {
            let mut account = open(&dir, &passphrase).await?;
            account.send_text(&to, &text).await?;
            println!("sent");
        }
        Commands::SendFile { to, path } => {
            let mut account = open(&dir, &passphrase).await?;
            account.send_file(&to, &path).await?;
            println!("sent file");
        }
        Commands::Recv { save_dir } => {
            let mut account = open(&dir, &passphrase).await?;
            let inbox = account.recv().await?;
            for w in &inbox.warnings {
                eprintln!("warning: {}", w);
            }
            for msg in inbox.messages {
                match msg.payload {
                    MessagePayload::Text { body } => {
                        println!("{}: {}", msg.from_nick, body);
                    }
                    MessagePayload::File { name, data } => {
                        let out = if let Some(d) = &save_dir {
                            std::fs::create_dir_all(d)?;
                            d.join(&name)
                        } else {
                            StdPathBuf::from(format!("received-{}-{}", msg.from_nick, name))
                        };
                        std::fs::write(&out, &data)?;
                        println!("{}: file -> {}", msg.from_nick, out.display());
                    }
                    MessagePayload::Mail {
                        thread_id,
                        subject,
                        body,
                        ..
                    } => {
                        let subject = subject.as_deref().unwrap_or("(no subject)");
                        println!(
                            "{}: mail [{}] {} — {}",
                            msg.from_nick, thread_id, subject, body
                        );
                        for att in &msg.mail_attachments {
                            let out = if let Some(d) = &save_dir {
                                std::fs::create_dir_all(d)?;
                                d.join(&att.name)
                            } else {
                                StdPathBuf::from(format!(
                                    "received-{}-{}",
                                    msg.from_nick, att.name
                                ))
                            };
                            let bytes = account.read_blob(&att.blob_id)?;
                            std::fs::write(&out, &bytes)?;
                            println!("  attachment -> {}", out.display());
                        }
                    }
                    // A conclave message. The conclave itself is only ever
                    // materialised by an accepted invite; a message for an
                    // unknown conclave is dropped during `recv`.
                    MessagePayload::Conclave {
                        conclave_id,
                        name,
                        body,
                        ..
                    } => {
                        println!(
                            "{}: conclave [{}] {} — {}",
                            msg.from_nick, conclave_id, name, body
                        );
                    }
                    // Control payloads are handled inside `recv` and never
                    // surface as inbox items.
                    MessagePayload::QueueGrant { .. }
                    | MessagePayload::ZineGrant { .. }
                    | MessagePayload::Reach { .. }
                    | MessagePayload::ConclaveInvite { .. }
                    | MessagePayload::ConclaveAccept { .. }
                    | MessagePayload::ConclaveDecline { .. }
                    | MessagePayload::ConclaveLeave { .. }
                    | MessagePayload::ConclaveUpdate { .. }
                    | MessagePayload::ConclaveEnd { .. }
                    | MessagePayload::ConclaveIntroReq { .. }
                    | MessagePayload::ConclaveIntro { .. } => {}
                    // Paper payloads are routed to the paper store, and an
                    // unknown tag has nothing to print, so neither is an inbox
                    // item.
                    MessagePayload::PaperState { .. }
                    | MessagePayload::PaperDelta { .. }
                    | MessagePayload::PaperSnapshot { .. }
                    | MessagePayload::PaperPresence { .. }
                    | MessagePayload::PaperHub { .. }
                    | MessagePayload::PaperOffer { .. }
                    | MessagePayload::PaperAnswer { .. }
                    // Voice (Task 23) has no CLI surface: it is daemon- and
                    // UI-only, and a CLI recv has nothing to print for it.
                    | MessagePayload::VoiceOpen { .. }
                    | MessagePayload::VoiceOffer { .. }
                    | MessagePayload::VoiceAnswer { .. }
                    | MessagePayload::VoiceMute { .. }
                    | MessagePayload::VoiceLeave { .. }
                    | MessagePayload::Unknown => {}
                }
            }
        }
        Commands::DropKeyShow => {
            let printed = drop_key_show(&dir, &passphrase).await?;
            println!("{printed}");
        }
        Commands::DropKeygen => {
            let printed = drop_keygen(&dir, &passphrase).await?;
            println!("{printed}");
        }
        Commands::DropRotate => {
            let printed = drop_rotate(&dir, &passphrase).await?;
            println!("{printed}");
        }
        Commands::Contacts => {
            let account = open(&dir, &passphrase).await?;
            for c in account.contacts() {
                println!("{} ({})", c.nickname, c.account_id);
            }
        }
        Commands::Config { cmd } => match cmd {
            ConfigCmd::Path => {
                println!("{}", config::config_path(&dir).display());
            }
            ConfigCmd::Show => {
                let (relay, relay_src) = config::resolve_relay_url_with_source(&dir, None);
                let (addr, addr_src) = config::resolve_daemon_addr_with_source(&dir, None);
                let (sock, sock_src) = config::resolve_daemon_socket_with_source(&dir, None);
                println!("config file: {}", config::config_path(&dir).display());
                println!("relay_url:   {}  ({})", relay, relay_src.as_str());
                match addr {
                    Some(a) => println!("daemon_addr: {}  ({})", a, addr_src.as_str()),
                    None => println!("daemon_addr: (TCP off — Unix socket only)"),
                }
                println!("socket:      {}  ({})", sock.display(), sock_src.as_str());
                println!(
                    "env overrides: {} / {} / {}",
                    config::ENV_RELAY_URL,
                    config::ENV_DAEMON_ADDR,
                    config::ENV_DAEMON_SOCKET
                );
            }
            ConfigCmd::Set {
                relay,
                daemon_addr,
                daemon_socket,
            } => {
                let mut cfg = config::load(&dir);
                if let Some(v) = relay {
                    cfg.relay_url = normalize_optional(&v);
                }
                if let Some(v) = daemon_addr {
                    cfg.daemon_addr = normalize_optional(&v);
                }
                if let Some(v) = daemon_socket {
                    cfg.daemon_socket = normalize_optional(&v);
                }
                config::save(&dir, &cfg).map_err(std::io::Error::other)?;
                println!("wrote {}", config::config_path(&dir).display());
                println!(
                    "note: an existing vault keeps the relay it was created with ({} for new vaults).",
                    config::resolve_relay_url(&dir, None)
                );
            }
        },
    }
    Ok(())
}

fn normalize_optional(v: &str) -> Option<String> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Parse the `--reach` spelling, accepting both the CLI's hyphenated form and
/// the wire's snake_case one, because the wire name is what a user copying from
/// the API docs will have in hand.
fn parse_reach(v: &str) -> Result<TrusteeReach, hush_core::Error> {
    match v.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "full" => Ok(TrusteeReach::Full),
        "mail-only" => Ok(TrusteeReach::MailOnly),
        other => Err(hush_core::Error::InvalidReach(other.to_string())),
    }
}

// ------------------------------------------------------------- drop keys

/// The two strings a member publishes together: the drop key and the page URL
/// that posts to it.
async fn drop_key_show(
    dir: &PathBuf,
    passphrase: &Option<String>,
) -> Result<String, String> {
    let account = open(dir, passphrase).await.map_err(|e| e.to_string())?;
    match account.drop_public_key() {
        Some(pk) => {
            let base = account
                .onion_relay()
                .map(str::to_string)
                .unwrap_or_else(|| account.relay().to_string());
            Ok(format!("{pk}\ndrop page: {base}/drop"))
        }
        None => Err("no drop key yet — run: hush-cli drop-keygen".into()),
    }
}

/// Generate the drop keypair, strictly: a second call is an error, because
/// quietly returning the same key means someone thinks they rotated when they
/// did not.
async fn drop_keygen(
    dir: &PathBuf,
    passphrase: &Option<String>,
) -> Result<String, String> {
    let mut account = open(dir, passphrase).await.map_err(|e| e.to_string())?;
    match account.create_drop_keys() {
        Ok(pk) => Ok(format!(
            "drop key (publish this next to the drop page URL):\n{pk}"
        )),
        Err(hush_core::Error::DropKeyExists) => Err(
            "a drop key already exists — show it with: hush-cli drop-key-show\n\
             (to replace it, rotate with: hush-cli drop-rotate)"
                .into(),
        ),
        Err(e) => Err(e.to_string()),
    }
}

/// Rotate: the old key is retired, not deleted, so drops sealed to it before
/// the rotation still drain until the relay's TTL expires them.
async fn drop_rotate(
    dir: &PathBuf,
    passphrase: &Option<String>,
) -> Result<String, String> {
    let mut account = open(dir, passphrase).await.map_err(|e| e.to_string())?;
    let pk = account.rotate_drop_keys().map_err(|e| e.to_string())?;
    Ok(format!(
        "new drop key (publish this):\n{pk}\n\
         the retired key keeps draining until drops in flight on it expire"
    ))
}

async fn open(dir: &PathBuf, passphrase: &Option<String>) -> Result<Account, hush_core::Error> {
    let passphrase = passphrase
        .as_deref()
        .ok_or(hush_core::Error::PassphraseRequired)?;
    Account::open(dir, passphrase).await
}

#[cfg(test)]
mod drop_command_tests {
    use super::*;
    use tempfile::TempDir;

    /// A fresh vault on disk, plus its passphrase. The TempDir is leaked on
    /// purpose: the vault has to outlive the test's calls, and each test
    /// creates exactly one.
    async fn temp_vault() -> (PathBuf, Option<String>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        // Never dialed: the key commands are purely local vault work.
        Account::create(&path, "test-pass", "http://127.0.0.1:1")
            .await
            .unwrap();
        (path, Some("test-pass".to_string()))
    }

    #[tokio::test]
    async fn keygen_is_strict_and_rotate_replaces() {
        let (dir, pass) = temp_vault().await;

        // No key yet: show says so rather than printing an empty line.
        assert!(drop_key_show(&dir, &pass).await.is_err());

        let first = drop_keygen(&dir, &pass).await.unwrap();
        assert!(first.contains("hush-drop-pk-v1:"), "{first}");

        // A second keygen is the mistake it should look like.
        let err = drop_keygen(&dir, &pass).await.unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        // Rotate replaces, and show reports the new one.
        let second = drop_rotate(&dir, &pass).await.unwrap();
        assert!(second.contains("hush-drop-pk-v1:"), "{second}");
        let shown = drop_key_show(&dir, &pass).await.unwrap();
        assert!(shown.contains("hush-drop-pk-v1:"));
    }
}
