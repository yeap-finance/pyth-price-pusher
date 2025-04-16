pub mod chain;
pub mod config;
pub mod controller;
// pub mod hermes; // Removed custom hermes module
pub mod types;
pub mod utils; // Add a utils module for common helpers

// Re-export key types/functions for easier use by other crates
pub use chain::TargetChain;
pub use config::load_price_config;
pub use controller::Controller;
pub use pyth_hermes_client::PythClient as HermesClient; // Removed re-export of custom client
pub use types::{PriceConfig, PriceInfo, UpdateCondition};
// pub use utils::*;
