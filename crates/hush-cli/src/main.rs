use std::path::PathBuf;

use clap::{Parser, Subcommand};
use std::path::PathBuf as StdPathBuf;

use hush_core::{Account, MessagePayload};

const DEFAULT_RELAY: &str = "http://127.0.0.1:8787";

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
        #[arg(long, default_value = DEFAULT_RELAY)]
        relay: String,
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let dir = cli.dir.clone();
    let passphrase = cli.passphrase.clone();
    match cli.command {
        Commands::Init { passphrase, relay } => {
            Account::create(&dir, &passphrase, &relay).await?;
            println!("vault ready at {}", dir.display());
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
    }
    Ok(())
}

async fn open(dir: &PathBuf, passphrase: &Option<String>) -> Result<Account, hush_core::Error> {
    let passphrase = passphrase
        .as_deref()
        .ok_or(hush_core::Error::PassphraseRequired)?;
    Account::open(dir, passphrase).await
}
