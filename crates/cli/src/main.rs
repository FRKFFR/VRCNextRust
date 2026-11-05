//! `vrcnext-cli` binary: headless clap CLI driving `vrc-core`.
//!
//! Auth cookies persist only in the OS credential store via
//! `vrc_api::KeyringVault` (RULES.md §4). Passwords and 2FA codes are read
//! with `rpassword` and held in memory as `SecretString`s for the duration
//! of the login call only.

use std::io::Write;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use secrecy::SecretString;
use tokio::sync::broadcast;

use vrc_core::{
    sections, App, AppConfig, AppEvent, AppHandle, AuthState, Client, Command, KeyringVault,
    TwoFactorMethod, USER_AGENT,
};

#[derive(Parser)]
#[command(name = "vrcnext-cli", version, about = "VRCNextRust headless CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the HTTP User-Agent used for all API requests
    Version,
    /// Log in with username/e-mail + password (and 2FA when required)
    Login,
    /// Show the currently logged-in user
    Me,
    /// List friends grouped by section (favorites, in-game, active, offline)
    Friends,
    /// Stream live pipeline events until Ctrl-C
    Watch,
    /// Log out and clear stored session cookies
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Version => {
            println!("{USER_AGENT}");
            Ok(())
        }
        Commands::Login => login().await,
        Commands::Me => me().await,
        Commands::Friends => friends().await,
        Commands::Watch => watch().await,
        Commands::Logout => logout().await,
    }
}

/// REST client backed by the OS credential store.
fn build_client() -> Result<Client> {
    let vault = KeyringVault::new().context("opening the OS credential store")?;
    Client::builder()
        .vault(vault)
        .build()
        .context("building the REST client")
}

/// Wait for the next event matching `wanted`; errors abort the command.
async fn wait_for(handle: &mut AppHandle, wanted: impl Fn(&AppEvent) -> bool) -> Result<AppEvent> {
    loop {
        match handle.events.recv().await {
            Ok(event) if wanted(&event) => return Ok(event),
            Ok(AppEvent::Error(message)) => bail!(message),
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(e) => bail!("event stream closed: {e}"),
        }
    }
}

/// Send `Command::ResumeSession` and wait for the auth outcome.
async fn ensure_session(handle: &mut AppHandle) -> Result<()> {
    handle
        .commands
        .send(Command::ResumeSession)
        .await
        .context("sending ResumeSession")?;
    match wait_for(handle, |e| matches!(e, AppEvent::AuthChanged(_))).await? {
        AppEvent::AuthChanged(AuthState::Authenticated) => Ok(()),
        _ => bail!("not logged in — run `vrcnext-cli login` first"),
    }
}

async fn login() -> Result<()> {
    let mut handle = App::spawn(AppConfig::new(build_client()?));

    print!("Username or e-mail: ");
    std::io::stdout().flush()?;
    let mut username = String::new();
    std::io::stdin()
        .read_line(&mut username)
        .context("reading username")?;
    let password =
        SecretString::from(rpassword::prompt_password("Password: ").context("reading password")?);

    handle
        .commands
        .send(Command::Login {
            username: username.trim().to_string(),
            password,
        })
        .await?;

    match wait_for(&mut handle, |e| matches!(e, AppEvent::AuthChanged(_))).await? {
        AppEvent::AuthChanged(AuthState::Authenticated) => {
            println!("Logged in.");
            Ok(())
        }
        AppEvent::AuthChanged(AuthState::Needs2fa(methods)) => {
            let method = *methods.first().context("empty 2FA method list")?;
            let label = match method {
                TwoFactorMethod::Totp => "authenticator app (totp)",
                TwoFactorMethod::EmailOtp => "e-mail (emailOtp)",
                TwoFactorMethod::RecoveryOtp => "recovery code (otp)",
            };
            eprintln!("Two-factor required; using {label} of {methods:?}");
            let code = SecretString::from(
                rpassword::prompt_password("2FA code: ").context("reading 2FA code")?,
            );
            handle
                .commands
                .send(Command::Submit2fa { method, code })
                .await?;
            match wait_for(&mut handle, |e| {
                matches!(e, AppEvent::AuthChanged(AuthState::Authenticated))
            })
            .await?
            {
                AppEvent::AuthChanged(AuthState::Authenticated) => {
                    println!("Logged in.");
                    Ok(())
                }
                other => bail!("unexpected event: {other:?}"),
            }
        }
        other => bail!("unexpected event: {other:?}"),
    }
}

async fn me() -> Result<()> {
    let mut handle = App::spawn(AppConfig::new(build_client()?));
    ensure_session(&mut handle).await?;
    let state = handle.state.snapshot();
    let user = state.current_user.context("no user despite session")?;
    println!("{}  <{}>", user.display_name, user.id);
    println!("  status:  {} — {}", user.status, user.status_description);
    println!("  trust:   {:?}", user.trust_rank());
    println!("  friends: {}", user.friends.len());
    Ok(())
}

async fn friends() -> Result<()> {
    let mut handle = App::spawn(AppConfig::new(build_client()?));
    ensure_session(&mut handle).await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::FriendsRefreshed)).await?;
    let state = handle.state.snapshot();
    let s = sections(&state);
    for (title, list) in [
        ("Favorites", &s.favorites),
        ("In-Game", &s.in_game),
        ("Web/Active", &s.active),
        ("Offline", &s.offline),
    ] {
        println!("== {title} ({}) ==", list.len());
        for f in list {
            let where_ = if f.location.is_empty() {
                f.status.clone()
            } else {
                f.location.clone()
            };
            println!("  {:<32} {:?}  {}", f.display_name, f.trust_rank, where_);
        }
    }
    Ok(())
}

async fn watch() -> Result<()> {
    let mut handle = App::spawn(AppConfig::new(build_client()?));
    ensure_session(&mut handle).await?;
    println!("Watching pipeline events (Ctrl-C to stop)…");
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Ok(AppEvent::Pipeline(e)) => println!("{e:?}"),
                Ok(AppEvent::AuthChanged(a)) => println!("auth: {a:?}"),
                Ok(AppEvent::Error(m)) => println!("error: {m}"),
                Ok(AppEvent::FriendsRefreshed) => println!("friends refreshed"),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    println!("(missed {n} events)");
                }
                Err(_) => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

async fn logout() -> Result<()> {
    let mut handle = App::spawn(AppConfig::new(build_client()?));
    handle.commands.send(Command::Logout).await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::LoggedOut))
    })
    .await?;
    println!("Logged out.");
    Ok(())
}
