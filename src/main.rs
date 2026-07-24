mod utils;
mod constants;
mod errors;
mod config;
mod configure;
mod daemon;
mod crypto;
mod scroll;
mod clipboard;
mod synq;
mod transport;
mod broadcast;

use clap::{Parser, Subcommand};
use crate::errors::Result;
use crate::config::Config;
use crate::utils::get_config_path;

#[derive(Parser, Debug)]
#[command(name = "synq")]
#[command(about = "Synq - Clipboard and scroll wheel sharing")]
struct Args {
    #[arg(long)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    Daemon,
    Configure {
        #[arg(long)]
        scroll: bool,
    },
    ListDevices,
    DetectDevices,
    GenerateKey,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let log_level = if args.debug {
        tracing::Level::TRACE
    } else {
        tracing::Level::INFO
    };
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(log_level)
        .compact()
        .init();

    if args.debug {
        errors::set_debug_output(true);
    }

    if let Some(command) = args.command {
        match command {
            Command::Daemon => {
                let config_path = get_config_path()?;
                let config = Config::load(&config_path).await?;
                if config.is_modified() {
                    config.save().await?;
                }

                daemon::run(config).await?;
            }
            Command::Configure { scroll } => {
                let config_path = get_config_path()?;
                let config = Config::load_or_create(&config_path).await?;
                if config.is_modified() {
                    config.save().await?;
                }

                configure::configure(config, scroll).await?;
            }
            Command::ListDevices => {
                let devices = scroll::list_devices()?;
                for device in devices {
                    println!("{}", device);
                }
            }
            Command::DetectDevices => {
                let config_path = get_config_path()?;
                let config = Config::load(&config_path).await?;
                if config.is_modified() {
                    config.save().await?;
                }

                scroll::detect_scroll_devices(config).await?;
            }
            Command::GenerateKey => {
                let config_path = get_config_path()?;
                let mut config = Config::load(&config_path).await?;

                let (private_key, public_key) = crypto::generate_keypair();
                config.set_keypair(private_key, public_key.clone());
                config.save().await?;

                println!("{}", public_key);
            }
        }
    }

    Ok(())
}
