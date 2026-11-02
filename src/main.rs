mod client;
mod config;
mod names;
mod ops;
mod output;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use client::IgClient;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ig",
    version,
    about = "IG spread betting CLI: log in, print balance/positions/orders, log out",
    after_help = "Commands: balance, positions, orders, search <term>,\n  open <buy|sell> <epic> <size> [level] [stop=<n>] [limit=<n>],\n  order <buy|sell> <epic> <size> <level> [limit|stop] [stop=<n>] [limit=<n>],\n  cancel <dealId>, transactions [days], modify <dealId> stop=<n|off> limit=<n|off>,\n  close <dealId> [size] [level], help, quit"
)]
struct Cli {
    /// Commands to run (space separated, e.g. `balance positions` or `search VOD*`)
    #[arg(value_name = "COMMANDS")]
    commands: Vec<String>,

    /// Interactive TUI with an in-session REPL
    #[arg(long)]
    tui: bool,

    /// Config file (default: ~/.config/ig/ig.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Use the live API host instead of the demo host
    #[arg(long)]
    live: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(config::default_path);
    let cfg = config::load(&config_path)?;

    let input = if cli.commands.is_empty() {
        "balance positions orders".to_string()
    } else {
        cli.commands.join(" ")
    };
    let commands = ops::parse_input(&input).map_err(|msg| anyhow::anyhow!("{msg}"))?;

    let mut name_cache = names::NameCache::load();

    let mut api = IgClient::new(&cfg.api_key, cli.live, cfg.base_url.as_deref())?;
    let mut session = api
        .login(&cfg.identifier, &cfg.password)
        .await
        .context("login failed")?;

    if cli.tui {
        client::pause().await;
        return tui::run(&mut api, &mut session, &mut name_cache, cli.live, &cfg.identifier).await;
    }

    print!(
        "Logged in to {} as {}",
        if cli.live { "LIVE" } else { "DEMO" },
        cfg.identifier
    );
    if let Some(expires) = expiry(&session) {
        print!(" (session expires {expires})");
    }
    println!();
    client::pause().await;

    let mut failed = false;
    for cmd in commands {
        match cmd {
            ops::Cmd::Quit => break,
            ops::Cmd::Clear => continue,
            _ => {}
        }
        match ops::exec(cmd, &api, &mut session, &mut name_cache, None).await {
            Ok(out) => print!("{out}"),
            Err(err) => {
                eprintln!("error: {err:#}");
                failed = true;
            }
        }
        client::pause().await;
    }

    if let Err(err) = api.logout().await {
        eprintln!("warning: logout failed: {err:#}");
    } else {
        println!("Logged out.");
    }

    if failed {
        anyhow::bail!("one or more commands failed");
    }
    Ok(())
}

fn expiry(session: &client::Session) -> Option<String> {
    session
        .expires_at
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
}
