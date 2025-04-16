use crate::types::{HexString, PriceInfo, UnixTimestamp};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt::Debug;

/// Trait representing the target blockchain interaction layer.
///
/// Implementors of this trait provide the chain-specific logic for
/// fetching on-chain price information and submitting price updates.
#[async_trait]
pub trait TargetChain: Send + Sync + Debug {
    /// Fetches the latest price information for multiple price feeds directly from the target chain.
    ///
    /// This method should perform polling or querying of the chain's state.
    ///
    /// # Arguments
    /// * `price_ids` - A slice of hex-encoded price feed IDs (without "0x" prefix) to query.
    ///
    /// # Returns
    /// A `Result` containing a `HashMap` mapping price feed IDs to their latest `PriceInfo`
    /// found on-chain. If a price feed is not found or an error occurs for a specific feed,
    /// it might be omitted from the map or an error returned, depending on implementation strategy.
    async fn get_latest_price_infos(
        &self,
        price_ids: &[HexString],
    ) -> Result<HashMap<HexString, PriceInfo>>;

    /// Submits price updates to the target chain.
    ///
    /// This method constructs and sends a transaction to the chain's Pyth contract
    /// to update the specified price feeds.
    ///
    /// # Arguments
    /// * `price_ids` - A slice of hex-encoded price feed IDs (without "0x" prefix) being updated.
    /// * `price_update_data` - A slice of byte vectors, where each vector contains the VAA or
    ///   price update data obtained from Hermes for the corresponding price ID.
    /// * `min_publish_times` - A slice of Unix timestamps representing the minimum publish time
    ///   required for each update (used in `updatePriceFeedsIfNecessary` style calls).
    ///   *Note: Aptos uses `update_price_feeds_with_funder` which doesn't take this directly,
    ///   so the Aptos implementation might ignore this or use it differently.*
    ///
    /// # Returns
    /// A `Result` indicating success or failure of the submission attempt. Note that success
    /// here might only mean successful broadcast, not necessarily on-chain confirmation,
    /// depending on the implementation (similar to the Aptos TS version).
    async fn update_price_feed(
        &self, // Use &mut self if sequence number management needs mutable access
        price_ids: &[HexString],
        price_update_data: &[Vec<u8>],
        min_publish_times: &[UnixTimestamp], // Keep for potential future compatibility/use
    ) -> Result<()>;
}