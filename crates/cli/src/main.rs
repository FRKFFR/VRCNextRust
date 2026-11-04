use clap::{Parser, Subcommand};

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
}

fn main() {
    match Cli::parse().command {
        Commands::Version => println!("{}", vrc_core::USER_AGENT),
    }
}
