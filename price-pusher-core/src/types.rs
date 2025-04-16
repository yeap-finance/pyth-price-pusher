pub use pyth_hermes_client::RpcPrice;
use pythnet_sdk::messages::PublisherStakeCap;
use serde::Deserialize;


/// Represents the price information obtained from either Hermes or the target chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceInfo {
    pub price: RpcPrice,

    pub ema_price: RpcPrice,
}

/// Represents the configuration for a single price feed from the YAML file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PriceConfig {
    /// User-defined alias for the price feed (e.g., "BTC/USD"). Used for logging.
    pub alias: String,
    /// The unique identifier for the price feed (hex string without leading "0x").
    pub id: String, // HexString without 0x

    // --- Main Update Triggers ---
    /// Time difference threshold (in seconds) to trigger an update.
    #[serde(rename = "time_difference")]
    pub time_difference_threshold: u64, // DurationInSeconds
    /// Price deviation threshold (as a percentage) to trigger an update.
    #[serde(rename = "price_deviation")]
    pub price_deviation_threshold_pct: f64, // PctNumber
    /// Confidence/price ratio threshold (as a percentage) to trigger an update.
    #[serde(rename = "confidence_ratio")]
    pub confidence_ratio_threshold_pct: f64, // PctNumber

    // --- Early Update Triggers ---
    /// Optional configuration for early updates.
    #[serde(default)] // If 'early_update' is missing in YAML, this field will be None
    pub early_update: Option<EarlyUpdateConfig>,
}

/// Optional configuration specific to early updates.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EarlyUpdateConfig {
    /// Optional time difference threshold (in seconds) for early updates.
    #[serde(rename = "time_difference")]
    pub time_difference_threshold: Option<u64>,
    /// Optional price deviation threshold (as a percentage) for early updates.
    #[serde(rename = "price_deviation")]
    pub price_deviation_threshold_pct: Option<f64>,
    /// Optional confidence/price ratio threshold (as a percentage) for early updates.
    #[serde(rename = "confidence_ratio")]
    pub confidence_ratio_threshold_pct: Option<f64>,
}

/// Enum indicating whether a price feed should be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCondition {
    /// This price feed *must* be updated (main thresholds met).
    Yes,
    /// This price feed *may* be updated as part of a larger batch (early update conditions met).
    Early,
    /// This price feed *should not* be updated (no conditions met).
    No,
}

// Define type aliases for clarity, matching TS concepts where possible
pub type DurationInSeconds = u64;
pub type PctNumber = f64;
pub type HexString = String; // Representing hex strings without "0x" prefix internally
pub type UnixTimestamp = i64;
