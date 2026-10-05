use std::path::PathBuf;

use clap::{Parser, Subcommand};
use hush_core::Account;

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
    Recv,
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
        Commands::Recv => {
            let mut account = open(&dir, &passphrase).await?;
            for (from, text) in account.recv().await? {
                println!("{}: {}", from, text);
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
