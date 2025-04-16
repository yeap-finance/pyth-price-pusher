use anyhow::{Context, Result};
use clap::Parser;
use price_pusher_aptos::AptosChain;
use price_pusher_core::{config::load_price_config, types::DurationInSeconds, Controller, HermesClient};
use std::{fs, path::PathBuf};
use tracing::{error, info, debug, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to the price configuration YAML file.
    #[clap(short='c', long, value_parser, default_value = "price-config.yaml")]
    price_config: PathBuf,

    /// Path to the file containing the Aptos account mnemonic phrase.
    #[clap(short, long, value_parser, default_value = "mnemonic.txt")]
    mnemonic_file: PathBuf,

    /// Aptos FullNode RPC endpoint URL.
    #[clap(short, long, value_parser)]
    endpoint: String,
    #[clap(long, value_parser)]
    api_key: Option<String>,
    /// Pyth contract address on Aptos.
    #[clap(short, long, value_parser)]
    pyth_contract_address: String,

    /// Hermes Price Service endpoint URL.
    #[clap(short = 's', long, value_parser, default_value = "https://hermes.pyth.network")]
    price_service_endpoint: String,

    /// Frequency (in seconds) for checking and pushing price updates.
    #[clap(long, value_parser, default_value_t = 10)]
    pushing_frequency: DurationInSeconds,

    /// Log level (e.g., trace, debug, info, warn, error).
    #[clap(long, value_parser, default_value = "info")]
    log_level: String,

    // TODO: Add other Aptos-specific options if needed (e.g., gas overrides)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // --- Setup Logging ---
    let log_level = args
        .log_level
        .parse::<Level>()
        .context("Invalid log level specified")?;
    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Setting default tracing subscriber failed");

    info!("Starting Rust Price Pusher for Aptos...");
    debug!(?args, "Parsed command line arguments");

    // --- Load Configurations ---
    let price_configs = load_price_config(&args.price_config)
        .with_context(|| format!("Failed to load price config from {:?}", args.price_config))?;
    info!(count = price_configs.len(), path = %args.price_config.display(), "Loaded price configs");

    let mnemonic = fs::read_to_string(&args.mnemonic_file)
        .with_context(|| format!("Failed to read mnemonic file: {:?}", args.mnemonic_file))?
        .trim() // Remove potential trailing newline
        .to_string();
    if mnemonic.is_empty() {
        anyhow::bail!("Mnemonic file is empty: {:?}", args.mnemonic_file);
    }
    info!(path = %args.mnemonic_file.display(), "Loaded mnemonic");

    // --- Initialize Components ---
    info!(url = %args.price_service_endpoint, "Initializing Hermes client");
    let hermes_client = HermesClient::new(args.price_service_endpoint.parse()?);


    info!(url = %args.endpoint, contract = %args.pyth_contract_address, "Initializing Aptos chain interface");
    let aptos_chain = AptosChain::new(
        args.endpoint.clone(), // Clone endpoint URL
        args.api_key.clone(),
        args.pyth_contract_address.clone(), // Clone contract address
        mnemonic, // Move mnemonic
    )
    .await // AptosChain::new is async
    .context("Failed to initialize Aptos chain interface")?;

    info!(frequency = args.pushing_frequency, "Initializing Controller");
    let controller = Controller::new(
        price_configs, // Move price_configs
        hermes_client, // Move hermes_client
        aptos_chain,   // Move aptos_chain
        args.pushing_frequency,
    );

    // --- Start Controller ---
    if let Err(e) = controller.start().await {
        error!(error = %e, "Controller loop exited with error");
        // Depending on the error, might want specific exit codes
        return Err(e).context("Controller failed");
    }

    // Controller::start should ideally loop forever. If it exits without error,
    // it might indicate an unexpected shutdown.
    info!("Controller loop finished unexpectedly without error.");
    Ok(())
}