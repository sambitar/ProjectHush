use std::path::PathBuf;

use clap::{Parser, Subcommand};
use std::path::PathBuf as StdPathBuf;

use hush_core::{
    Account, MessagePayload, config, resolve_data_dir,
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
                }
            }
        }
        Commands::Contacts => {
            let account = open(&dir, &passphrase).await?;
            for c in account.contacts() {
                println!("{} ({})", c.nickname, c.protocol_name);
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
