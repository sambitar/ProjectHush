use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine};
use clap::{Parser, Subcommand};
use std::path::PathBuf as StdPathBuf;

use hush_core::{
    config, decode_drop_private_key, encode_drop_private_key, encode_drop_public_key,
    generate_drop_keypair, open_drop_with_key, resolve_data_dir, Account, MessagePayload,
    RelayClient,
};

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
    },
    Invite,
    AddContact {
        nickname: String,
        invite: String,
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
    /// Make a keypair for receiving outsider drops sealed to a public key.
    ///
    /// Prints the public key to hand out; the private key goes only to a file
    /// the user controls, created 0600 and never overwritten.
    DropKeygen {
        /// Where to write the private key. Refuses to overwrite an existing file.
        #[arg(long, default_value = "hush-drop-private-key.txt")]
        out: StdPathBuf,
    },
    /// Open an outsider drop sealed to a public key, from a file or a relay.
    DropOpen {
        /// The container file. Omit this when using --delivery.
        file: Option<StdPathBuf>,
        /// Fetch the container from a relay by its pickup (capability) id.
        #[arg(long, requires = "relay")]
        delivery: Option<String>,
        /// Relay base URL. Required with --delivery.
        #[arg(long)]
        relay: Option<String>,
        /// Private key file written by `drop-keygen`.
        #[arg(long, default_value = "hush-drop-private-key.txt")]
        key: StdPathBuf,
        /// Directory to write attachments into. Defaults to the current directory.
        #[arg(long)]
        save_dir: Option<StdPathBuf>,
    },
    Contacts,
    /// Read or change the central client config (DR-14).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
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
        Commands::Init { passphrase, relay } => {
            let relay = config::resolve_relay_url(&dir, relay.as_deref());
            let mut account = Account::create(&dir, &passphrase, &relay).await?;
            println!("vault ready at {}", dir.display());
            println!("relay: {}", relay);
            if let Some(code) = account.take_pending_recovery_code() {
                println!("SAVE THIS RECOVERY CODE (shown once): {}", code);
            }
        }
        Commands::Invite => {
            let mut account = open(&dir, &passphrase).await?;
            println!("{}", account.invite().await?);
        }
        Commands::AddContact { nickname, invite } => {
            let mut account = open(&dir, &passphrase).await?;
            account.add_contact(&nickname, &invite).await?;
            println!("added contact {}", nickname);
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
                    // Queue grants are handled inside `recv` and never surface as
                    // inbox items.
                    MessagePayload::QueueGrant { .. } => {}
                }
            }
        }
        Commands::DropKeygen { out } => {
            let keys = generate_drop_keypair();
            write_private_key(&out, &encode_drop_private_key(&keys.private_key))?;
            // The public half is the only thing printed: it is meant to be shared.
            println!("{}", encode_drop_public_key(&keys.public_key));
            eprintln!(
                "private key written to {} (mode 0600) — keep it secret; it is never printed",
                out.display()
            );
        }
        Commands::DropOpen {
            file,
            delivery,
            relay,
            key,
            save_dir,
        } => {
            let container = read_container(file, delivery, relay).await?;
            let private = decode_drop_private_key(&std::fs::read_to_string(&key)?)?;
            let package = open_drop_with_key(&container, &private)?;
            println!("subject: {}", package.subject);
            println!("{}", package.body);
            let dir = save_dir.unwrap_or_else(|| StdPathBuf::from("."));
            std::fs::create_dir_all(&dir)?;
            for att in &package.attachments {
                let bytes = STANDARD
                    .decode(&att.data)
                    .map_err(|_| "an attachment in the package is not valid base64")?;
                let path = dir.join(&att.name);
                std::fs::write(&path, &bytes)?;
                println!("attachment -> {}", path.display());
            }
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

async fn open(dir: &PathBuf, passphrase: &Option<String>) -> Result<Account, hush_core::Error> {
    let passphrase = passphrase
        .as_deref()
        .ok_or(hush_core::Error::PassphraseRequired)?;
    Account::open(dir, passphrase).await
}

/// Write a drop private key where the user asked, creating it 0600 and refusing
/// to overwrite an existing file. The value is never printed.
fn write_private_key(path: &StdPathBuf, encoded: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| {
        format!(
            "cannot create {} ({e}); an existing key is never overwritten",
            path.display()
        )
    })?;
    use std::io::Write;
    writeln!(file, "{encoded}")?;
    Ok(())
}

/// Get the container bytes from a file or from a relay by pickup id.
async fn read_container(
    file: Option<StdPathBuf>,
    delivery: Option<String>,
    relay: Option<String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match (file, delivery) {
        (Some(path), None) => Ok(std::fs::read(path)?),
        (None, Some(id)) => {
            let relay = relay.ok_or("--relay is required with --delivery")?;
            let client = RelayClient::try_new(&relay)?;
            Ok(client.get_drop(&id).await?)
        }
        (Some(_), Some(_)) => Err("give either a container file or --delivery, not both".into()),
        (None, None) => Err("give a container file or --delivery".into()),
    }
}
