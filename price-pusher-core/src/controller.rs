use crate::{
    chain::TargetChain,
    // config::PriceConfig, // Removed private import
    // hermes::HermesClient, // Removed custom client import
    types::{DurationInSeconds, HexString, PriceConfig, PriceInfo, UnixTimestamp, UpdateCondition},
};
use anyhow::Result; // Keep Result, Context
use pyth_hermes_client::{EncodingType, PythClient as HermesClient, RpcPrice};
use std::{collections::HashMap, sync::Arc, time::Duration};
use bigdecimal::{BigDecimal, ToPrimitive, Zero};
use tokio::sync::Mutex; // For shared state if using separate listener task
use tracing::{debug, error, info, instrument, warn};

// --- should_update function ---
/// Checks whether the on-chain price needs to be updated based on the latest source price.
///
/// # Arguments
/// * `price_config` - Configuration for the specific price feed.
/// * `source_price` - The latest price information from the source (Hermes).
/// * `target_price` - The latest price information from the target chain.
///
/// # Returns
/// An `UpdateCondition` indicating whether an update is needed (Yes, Early, No).
pub fn should_update(
    price_config: &PriceConfig,
    source_price_opt: Option<&RpcPrice>,
    target_price_opt: Option<&RpcPrice>,
) -> Result<UpdateCondition> {
    let price_id = &price_config.id;
    let alias = &price_config.alias;

    // --- Condition 1: No source price available ---
    let source_price = match source_price_opt {
        Some(p) => p,
        None => {
            // If source price is missing, we can't evaluate conditions meaningfully.
            // The TS version returns YES, but we can't push without data.
            // Log and return NO, as no actionable update can be derived.
            debug!(%price_id, %alias, "No source price available from Hermes. Cannot determine update condition.");
            return Ok(UpdateCondition::No); // Changed from TS logic: cannot push without source
        }
    };

    // --- Condition 2: No target price available ---
    if target_price_opt.is_none() {
        info!(%price_id, %alias, "Price feed not found on target chain. Update required.");
        return Ok(UpdateCondition::Yes);
    }
    let target_price = target_price_opt.unwrap(); // Safe due to the check above

    // --- Condition 3: Source price is not newer ---
    if source_price.publish_time <= target_price.publish_time {
        debug!(%price_id, %alias, source_time = source_price.publish_time, target_time = target_price.publish_time, "Source price is not newer than target price. No update needed.");
        return Ok(UpdateCondition::No);
    }

    // --- Condition 4: Calculate and check thresholds ---
    let time_difference = source_price.publish_time - target_price.publish_time;

    // Use f64 for calculations involving percentages to avoid precision loss with integers.
    // Convert price/conf strings to f64. Handle potential parsing errors.

    let source_price_bd = BigDecimal::from(source_price.price);
    let source_conf_bd:BigDecimal = source_price.conf.into();
    let target_price_bd = BigDecimal::from(target_price.price);

    // Avoid division by zero or near-zero prices
    let price_deviation_pct = if !target_price_bd.is_zero() {
        (((source_price_bd.clone() - target_price_bd.clone()).abs() / target_price_bd.abs()) * BigDecimal::from(100)).to_f64().unwrap()
    } else {
        // If target price is zero/negligible, deviation is infinite if source is non-zero
        if !source_price_bd.abs().is_zero() {
            f64::INFINITY
        } else {
            0.0
        }
    };

    let confidence_ratio_pct = if !source_price_bd.is_zero() {
        (source_conf_bd.abs() / source_price_bd.abs()).to_f64().unwrap() * 100.0
    } else {
        // If source price is zero/negligible, ratio is infinite if conf is non-zero
        if !source_conf_bd.abs().is_zero() {
            f64::INFINITY
        } else {
            0.0
        }
    };

    debug!(
        price_id,
        alias,
        source_time = source_price.publish_time,
        target_time = target_price.publish_time,
        time_diff = time_difference,
        time_diff_thresh = price_config.time_difference_threshold,
        price_dev_pct = format!("{:.5}", price_deviation_pct),
        price_dev_thresh = price_config.price_deviation_threshold_pct,
        conf_ratio_pct = format!("{:.5}", confidence_ratio_pct),
        conf_ratio_thresh = price_config.confidence_ratio_threshold_pct,
        "Calculated update check values"
    );

    // --- Check Main Thresholds ---
    if time_difference >= price_config.time_difference_threshold as i64
        || price_deviation_pct >= price_config.price_deviation_threshold_pct
        || confidence_ratio_pct >= price_config.confidence_ratio_threshold_pct
    {
        info!(%price_id, %alias, "Main update condition met.");
        return Ok(UpdateCondition::Yes);
    }

    // --- Check Early Update Thresholds ---
    match &price_config.early_update {
        Some(early_config) => {
            let early_time = early_config
                .time_difference_threshold
                .map_or(false, |t| time_difference >= t as i64);
            let early_price = early_config
                .price_deviation_threshold_pct
                .map_or(false, |p| price_deviation_pct >= p);
            let early_conf = early_config
                .confidence_ratio_threshold_pct
                .map_or(false, |c| confidence_ratio_pct >= c);

            if early_time || early_price || early_conf {
                debug!(%price_id, %alias, "Custom early update condition met.");
                Ok(UpdateCondition::Early)
            } else {
                debug!(%price_id, %alias, "No update condition met (including custom early).");
                Ok(UpdateCondition::No)
            }
        }
        None => {
            // Default behavior: Allow early update if main conditions not met
            debug!(%price_id, %alias, "Default early update condition met.");
            Ok(UpdateCondition::Early)
        }
    }
}

// --- Controller Implementation ---

/// Orchestrates the process of fetching prices, checking update conditions,
/// and pushing updates to the target chain.
#[derive(Debug)]
pub struct Controller<T: TargetChain> {
    price_configs: Arc<Vec<PriceConfig>>,
    hermes_client: HermesClient, // Type updated to the imported client
    target_chain: T,
    pushing_frequency: Duration,
    // Store price IDs for efficiency
    all_price_ids: Vec<HexString>,
    // Map ID to alias for logging
    price_id_to_alias: HashMap<HexString, String>,
    // Shared state for latest source prices (updated by listener task)
    latest_source_prices: Arc<Mutex<HashMap<HexString, PriceInfo>>>,
}

impl<T: TargetChain + 'static> Controller<T> {
    /// Creates a new Controller.
    pub fn new(
        price_configs: Vec<PriceConfig>,
        hermes_client: HermesClient,
        target_chain: T,
        pushing_frequency_secs: DurationInSeconds,
        // polling_frequency_secs: DurationInSeconds, // Polling is handled by TargetChain impl
    ) -> Self {
        let all_price_ids = price_configs.iter().map(|c| c.id.clone()).collect();
        let price_id_to_alias = price_configs
            .iter()
            .map(|c| (c.id.clone(), c.alias.clone()))
            .collect();

        Self {
            price_configs: Arc::new(price_configs),
            hermes_client,
            target_chain,
            pushing_frequency: Duration::from_secs(pushing_frequency_secs),
            all_price_ids,
            price_id_to_alias,
            latest_source_prices: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Starts the main control loop.
    ///
    /// This function runs indefinitely, periodically checking for price updates
    /// and pushing them to the target chain based on the configuration.
    #[instrument(skip_all, name = "controller_loop")]
    pub async fn start(&self) -> Result<()> {
        info!("Starting controller loop...");

        // --- Start Source Listener Task ---
        // Spawn a background task to continuously fetch source prices
        // NOTE: The current listener polls VAAs and doesn't parse them into PriceInfo yet.
        // This means the `latest_source_prices` map will remain empty for now.
        // The `should_update` logic will currently return NO due to missing source prices.
        // This needs to be fixed by implementing VAA parsing or using a different Hermes source.
        let _source_listener_handle = tokio::spawn(run_source_listener(
            self.hermes_client.clone(),
            self.all_price_ids.clone(),
            Arc::clone(&self.latest_source_prices),
            self.pushing_frequency, // Use pushing frequency for polling interval for now
        ));
        // TODO: Add proper error handling / restart for the listener task

        // Initial sleep like in TS version to allow listeners to potentially populate
        // and respect cooldowns if restarting.
        info!(duration=?self.pushing_frequency, "Initial sleep before starting main loop.");
        tokio::time::sleep(self.pushing_frequency).await;



        loop {
            info!("Starting new check cycle.");
            if let Err(e) = self.try_sync().await {
                error!(error = %e, "Error during sync cycle. Retrying in next cycle.");
            }
            // --- 4. Sleep ---
            debug!(duration=?self.pushing_frequency, "Sleeping until next cycle.");
            tokio::time::sleep(self.pushing_frequency).await;
        }
        // Loop should not exit under normal operation
    }

    async fn try_sync(&self,) -> Result<()> {

        // --- 1. Fetch Target Chain Prices ---
        let latest_target_prices = match self.target_chain
            .get_latest_price_infos(&self.all_price_ids)
            .await
        {
            Ok(prices) => {
                debug!(count = prices.len(), "Fetched latest target chain prices.");
                prices // Update local cache
            }
            Err(e) => {
                error!(error = %e, "Failed to fetch target chain prices. Skipping cycle.");
                // Continue to next iteration after delay
                tokio::time::sleep(self.pushing_frequency).await;
                return Err(e);
            }
        };

        // --- 2. Check Update Conditions ---
        let mut push_threshold_met = false;
        let mut prices_to_push_ids: Vec<HexString> = Vec::new();
        // Store the publish times that triggered the update (or 0 if target price missing)
        let mut min_publish_times: Vec<UnixTimestamp> = Vec::new();

        // Get read lock on source prices
        let source_prices_guard = self.latest_source_prices.lock().await;

        for price_config in self.price_configs.iter() {
            let price_id = &price_config.id;
            let source_price = source_prices_guard.get(price_id); // Will be None currently
            let target_price = latest_target_prices.get(price_id);

            match should_update(price_config, source_price.map(|p| &p.price), target_price.map(|p| &p.price)) {
                Ok(UpdateCondition::Yes) => {
                    debug!(price_id, alias = %self.price_id_to_alias[price_id], "Update condition: YES");
                    push_threshold_met = true;
                    prices_to_push_ids.push(price_id.clone());
                    min_publish_times.push(target_price.map_or(0, |p| p.price.publish_time + 1));
                }
                Ok(UpdateCondition::Early) => {
                    debug!(price_id, alias = %self.price_id_to_alias[price_id], "Update condition: EARLY");
                    // Only add if we are already pushing something else
                    prices_to_push_ids.push(price_id.clone());
                    min_publish_times.push(target_price.map_or(0, |p| p.price.publish_time + 1));
                }
                Ok(UpdateCondition::No) => {
                    debug!(price_id, alias = %self.price_id_to_alias[price_id], "Update condition: NO");
                    // Do nothing
                }
                Err(e) => {
                    error!(price_id, alias = %self.price_id_to_alias[price_id], error = %e, "Error checking update condition. Skipping feed for this cycle.");
                    // Continue to next price feed
                }
            }
        }
        drop(source_prices_guard); // Release lock

        // Filter prices_to_push based on push_threshold_met
        // If only EARLY conditions were met, we still add them to prices_to_push_ids,
        // but push_threshold_met remains false. We only push if at least one YES was found.
        if !push_threshold_met {
            // If no YES condition was met, clear the list even if it contains EARLY items.
            prices_to_push_ids.clear();
            min_publish_times.clear();
        }
        // If push_threshold_met is true, we push everything currently in prices_to_push_ids (YES and EARLY items).

        // --- 3. Push Updates (if needed) ---
        if !prices_to_push_ids.is_empty() {
            info!(
                    count = prices_to_push_ids.len(),
                    ids = ?prices_to_push_ids.iter().map(|id| self.price_id_to_alias.get(id).unwrap_or(id)).collect::<Vec<_>>(),
                    "Push threshold met. Attempting to push updates."
                );

            // Fetch the actual update data from Hermes using the new client
            // Assuming a method like `get_latest_vaas` exists and takes IDs without "0x"
            // Also assuming it returns Result<Vec<Vec<u8>>>
            match self
                .hermes_client
                .latest_price_update(prices_to_push_ids.clone(), Some(EncodingType::Hex), Some(true))
                .await
            {
                Ok(update_data) => {
                    let updated_data_len = update_data.parsed.map(|d| d.len()).unwrap_or_default();
                    if updated_data_len == prices_to_push_ids.len()
                    {

                        if let Err(e) = self.target_chain
                            .update_price_feed(
                                &prices_to_push_ids,
                                &update_data
                                    .binary.decode()?,

                                &min_publish_times,
                            )
                            .await
                        {
                            error!(error = %e, "Failed to push price updates to target chain.");
                        } else {
                            info!("Successfully submitted price update transaction.");
                        }
                    } else {
                        warn!(
                                expected = prices_to_push_ids.len(),
                                received = updated_data_len,
                                "Mismatch fetching update data count from Hermes for push. Skipping push cycle."
                            );
                    }
                }
                Err(e) => {
                    error!(error = %e, "Failed to fetch price update data from Hermes for push. Skipping push cycle.");
                }
            }
        } else {
            info!("No update conditions met. No push needed.");
        }

        Ok(())
    }
}


/// Background task to poll Hermes for source price updates and parse them.
#[instrument(skip_all, name = "source_listener")]
async fn run_source_listener(
    hermes_client: HermesClient,
    price_ids: Vec<HexString>,
    latest_prices: Arc<Mutex<HashMap<HexString, PriceInfo>>>,
    polling_interval: Duration,
) {
    info!("Starting source price listener task (polling Hermes)...");
    loop {
        debug!("Polling Hermes for latest source prices...");
        // Use the assumed method name from the new client
        match hermes_client.latest_price_update(price_ids.clone(), Some(EncodingType::Hex), Some(true)).await {
            Ok(update_data_list) => {
                let updated_data_len = update_data_list.parsed.as_ref().map(|d| d.len()).unwrap_or_default();
                if updated_data_len == price_ids.len() {
                    debug!(
                        count = updated_data_len,
                        "Successfully polled Hermes update data (VAAs). Processing payloads..."
                    );

                    let mut prices_guard = latest_prices.lock().await;
                    let updated_count = 0;
                    let error_count = 0;

                    // Process each VAA payload
                    // Note: get_latest_price_updates returns VAAs in the same order as requested IDs.
                    for (_i, updated_data) in update_data_list.parsed.as_ref().unwrap().iter().enumerate() {
                        prices_guard.insert(updated_data.id.clone(), PriceInfo {
                            price: updated_data.price,
                            ema_price: updated_data.ema_price,
                        });
                    }
                    debug!(
                        processed = updated_count,
                        errors = error_count,
                        "Finished processing VAA payloads."
                    );
                    // Drop the lock explicitly after processing all items
                    drop(prices_guard);
                } else {
                    // This case might occur if ignore_invalid_price_ids=true was used and some IDs were invalid.
                    warn!(
                        expected = price_ids.len(),
                        received = updated_data_len,
                        "Source listener polling mismatch count from Hermes. Check requested IDs."
                    );
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to poll Hermes for source prices in listener task.");
            }
        }

        tokio::time::sleep(polling_interval).await;
    }
}

// --- Tests for should_update ---
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EarlyUpdateConfig, PriceConfig};
    use std::str::FromStr;
    fn create_config(
        id: &str,
        time_diff: u64,
        price_dev: f64,
        conf_ratio: f64,
        early: Option<EarlyUpdateConfig>,
    ) -> PriceConfig {
        PriceConfig {
            alias: format!("{}/USD", id),
            id: id.to_string(),
            time_difference_threshold: time_diff,
            price_deviation_threshold_pct: price_dev,
            confidence_ratio_threshold_pct: conf_ratio,
            early_update: early,
        }
    }

    fn create_price(price: &str, conf: &str, time: i64) -> RpcPrice {
        RpcPrice {
            price: i64::from_str(price).unwrap(),
            conf: conf.parse().unwrap(),
            publish_time: time,
            expo: 0
        }
    }

    #[test]
    fn test_should_update_no_target() {
        let config = create_config("btc", 60, 1.0, 1.0, None);
        let source = create_price("50000", "50", 1000);
        assert_eq!(
            should_update(&config, Some(&source), None).unwrap(),
            UpdateCondition::Yes
        );
    }

    #[test]
    fn test_should_update_no_source() {
        let config = create_config("btc", 60, 1.0, 1.0, None);
        let target = create_price("50000", "50", 1000);
        // Changed expectation: returns NO because source is missing
        assert_eq!(
            should_update(&config, None, Some(&target)).unwrap(),
            UpdateCondition::No
        );
    }

    #[test]
    fn test_should_update_source_not_newer() {
        let config = create_config("btc", 60, 1.0, 1.0, None);
        let source = create_price("50000", "50", 1000);
        let target = create_price("50000", "50", 1001); // Target is newer
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::No
        );
    }

    #[test]
    fn test_should_update_source_same_time() {
        let config = create_config("btc", 60, 1.0, 1.0, None);
        let source = create_price("50000", "50", 1000);
        let target = create_price("50000", "50", 1000); // Same time
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::No
        );
    }

    #[test]
    fn test_should_update_time_diff_met() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 60s threshold
        let source = create_price("50000", "50", 1060);
        let target = create_price("50000", "50", 1000); // 60s difference
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Yes
        );
    }

    #[test]
    fn test_should_update_price_dev_met() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 1% threshold
        let source = create_price("50501", "50", 1010); // > 1% deviation from 50000
        let target = create_price("50000", "50", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Yes
        );
    }

    #[test]
    fn test_should_update_price_dev_met_zero_target() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 1% threshold
        let source = create_price("1", "1", 1010); // Non-zero source
        let target = create_price("0", "0", 1000); // Zero target
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Yes // Infinite deviation
        );
    }

    #[test]
    fn test_should_update_price_dev_not_met_both_zero() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 1% threshold
        let source = create_price("0", "0", 1010); // Zero source
        let target = create_price("0", "0", 1000); // Zero target
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Early // 0% deviation, check early
        );
    }

    #[test]
    fn test_should_update_conf_ratio_met() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 1% threshold
        let source = create_price("50000", "501", 1010); // conf/price > 1%
        let target = create_price("50000", "50", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Yes
        );
    }

    #[test]
    fn test_should_update_conf_ratio_met_zero_source() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // 1% threshold
        let source = create_price("0", "1", 1010); // Zero price, non-zero conf
        let target = create_price("0", "0", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Yes // Infinite ratio
        );
    }

    #[test]
    fn test_should_update_default_early() {
        let config = create_config("btc", 60, 1.0, 1.0, None); // No thresholds met
        let source = create_price("50001", "5", 1010);
        let target = create_price("50000", "5", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Early // Default early update
        );
    }

    #[test]
    fn test_should_update_custom_early_met() {
        let early_config = EarlyUpdateConfig {
            time_difference_threshold: Some(5), // 5s threshold
            price_deviation_threshold_pct: Some(0.01),
            confidence_ratio_threshold_pct: Some(0.01),
        };
        let config = create_config("btc", 60, 1.0, 1.0, Some(early_config));
        let source = create_price("50001", "5", 1006); // 6s diff > 5s early threshold
        let target = create_price("50000", "5", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::Early
        );
    }

    #[test]
    fn test_should_update_custom_early_not_met() {
        let early_config = EarlyUpdateConfig {
            time_difference_threshold: Some(10), // 10s threshold
            price_deviation_threshold_pct: Some(0.01),
            confidence_ratio_threshold_pct: Some(0.01),
        };
        let config = create_config("btc", 60, 1.0, 1.0, Some(early_config));
        let source = create_price("50001", "5", 1006); // 6s diff < 10s early threshold, others also not met
        let target = create_price("50000", "5", 1000);
        assert_eq!(
            should_update(&config, Some(&source), Some(&target)).unwrap(),
            UpdateCondition::No
        );
    }

}
