use anyhow::{anyhow, Context, Result};
use aptos_sdk::{
    move_types::identifier::Identifier,
    rest_client::{Client as AptosClient, PendingTransaction}, // Renamed Client to AptosClient
    transaction_builder::TransactionFactory,
    types::{
        account_address::AccountAddress,
        chain_id::ChainId,
        transaction::{authenticator::AuthenticationKey, EntryFunction, TransactionPayload}, // Import ModuleId
        LocalAccount, // Use LocalAccount for signing
    },
};
use async_trait::async_trait;
use price_pusher_core::{
    chain::TargetChain,
    types::{HexString, PriceInfo, UnixTimestamp},
};
use serde::Deserialize; // Add Serialize for Resource
 // Use serde_json::Value
use std::{
    collections::HashMap,
    fmt::{Debug, Formatter},
    str::FromStr,
    sync::Arc,
};
use aptos_sdk::move_types::language_storage::ModuleId;
use aptos_sdk::rest_client::AptosBaseUrl;
use tokio::sync::Mutex; // For sequence number management
use tracing::{debug, error, info, instrument, warn};
use price_pusher_core::types::RpcPrice;

// Constants
const APTOS_ACCOUNT_HD_PATH: &str = "m/44'/637'/0'/0'/0'"; // Standard Aptos derivation path

// --- Aptos Specific Data Structures ---

// Structure matching the data field within the Aptos Resource JSON
#[derive(Deserialize, Debug)]
struct AptosLatestPriceInfoData {
    info: AptosPriceInfoTable,
}

#[derive(Deserialize, Debug)]
struct AptosPriceInfoTable {
    handle: String, // Table handle as a string (hex)
}

// Structure matching the table item value in the PriceInfo table
#[derive(Deserialize, Debug)]
struct AptosPriceInfo {
    attestation_time: u64,
    arrival_time: u64,
    price_feed: AptosPriceFeed,
}

#[derive(Deserialize, Debug)]
struct AptosPriceFeed {
    /// The price identifier
    price_identifier: AptosPriceIdentifier,
    /// The current aggregate price
    price: AptosPrice,
    /// The current exponentially moving average aggregate price
    ema_price: AptosPrice,
}

#[derive(Deserialize, Debug)]
struct I64 {
    negative: bool,
    magnitude: u64,
}
#[derive(Deserialize, Debug)]
struct AptosPrice {
    price: I64, // Price as a struct
    conf: u64,
    expo: I64, // Not directly used in PriceInfo, but part of the struct
    timestamp: u64,
}

impl Into<RpcPrice> for AptosPrice {
    fn into(self) -> RpcPrice {
        RpcPrice {
            price: if self.price.negative {
                -(self.price.magnitude as i64)
            } else {
                self.price.magnitude as i64
            },
            conf: self.conf,
            expo: if self.expo.negative {
                -(self.expo.magnitude as i32)
            } else {
                self.expo.magnitude as i32
            },
            publish_time: i64::try_from(self.timestamp).unwrap(),
        }
    }
}

// Structure for PriceIdentifier key used in the table
#[derive(Deserialize, Debug, serde::Serialize)] // Need Serialize for table item query
struct AptosPriceIdentifier {
    bytes: Vec<u8>,
}

#[derive(Deserialize, Debug, serde::Serialize)] // Need for table item query key
struct AptosPriceIdentifierKey {
    bytes: String, // The price feed ID (hex string without 0x)
}

// --- AptosChain Implementation ---

/// Aptos implementation of the TargetChain trait.
pub struct AptosChain {
    client: Arc<AptosClient>, // Use Arc for potential sharing
    pyth_contract_address: AccountAddress,
    pusher_account: LocalAccount, // Holds private key for signing
    transaction_factory: TransactionFactory,
    // Sequence number management state (wrapped in Arc for cloning into tasks)
    sequence_number_manager: Arc<Mutex<SequenceNumberManager>>,
}

// Separate struct to manage sequence number state
struct SequenceNumberManager {
    last_known_sequence_number: Option<u64>,
    is_fetching: bool, // Lock to prevent concurrent fetches
}

impl Debug for AptosChain {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AptosChain")
            .field("client", &self.client.path_prefix_string()) // Show URL prefix
            .field("pyth_contract_address", &self.pyth_contract_address)
            .field("pusher_account", &self.pusher_account.address()) // Show only address
            .field("transaction_factory", &self.transaction_factory)
            .field("sequence_number_manager", &"Mutex<SequenceNumberManager>") // Avoid printing mutex details
            .finish()
    }
}

impl AptosChain {
    /// Creates a new AptosChain instance.
    pub async fn new(
        endpoint_url: String,
        api_key: Option<String>,
        pyth_contract_address_str: String,
        mnemonic_phrase: String,
    ) -> Result<Self> {
        let mut client = AptosClient::builder(AptosBaseUrl::Custom(endpoint_url.parse()?));
        if let Some(key) = api_key {
            client = client.api_key(key.as_str())?;
        }
        let client_arc = Arc::new(client.build());

        let pyth_contract_address = AccountAddress::from_str(&pyth_contract_address_str)?;

        // Derive the pusher account from mnemonic
        // Note: aptos-sdk might require specific features or handling for mnemonic derivation.
        // Using LocalAccount::from_derive_path assumes it exists and works similarly to TS.
        // If not, we might need `aptos_key::key_derive` or similar.
        // For now, let's assume LocalAccount handles it.
        let pusher_account = LocalAccount::from_derive_path(APTOS_ACCOUNT_HD_PATH, &mnemonic_phrase, 0)
            .map_err(|e| anyhow!("Failed to derive Aptos account from mnemonic: {:?}", e))?;

        info!(
            pusher_address = %pusher_account.address(),
            auth_key = %hex::encode(AuthenticationKey::ed25519(&pusher_account.public_key()).to_vec()), // Use to_vec() and hex::encode
            "Initialized Aptos pusher account"
        );

        // Get chain ID for transaction factory
        let chain_id = client_arc
            .get_ledger_information()
            .await
            .context("Failed to get Aptos ledger information")?
            .inner()
            .chain_id;

        let transaction_factory = TransactionFactory::new(ChainId::new(chain_id));

        Ok(Self {
            client: client_arc,
            pyth_contract_address,
            pusher_account,
            transaction_factory,
            sequence_number_manager: Arc::new(Mutex::new(SequenceNumberManager { // Wrap in Arc
                last_known_sequence_number: None,
                is_fetching: false,
            })),
        })
    }

    /// Fetches the next sequence number, using cache or querying the chain.
    /// Mimics the logic from the TypeScript implementation.
    async fn get_next_sequence_number(&self) -> Result<u64> {
        let mut guard = self.sequence_number_manager.lock().await;

        if let Some(seq_num) = guard.last_known_sequence_number {
            let next_seq_num = seq_num + 1;
            guard.last_known_sequence_number = Some(next_seq_num);
            debug!(next_seq_num, "Using cached sequence number + 1");
            return Ok(next_seq_num);
        }

        // Prevent concurrent fetches if multiple pushes happen while fetching
        if guard.is_fetching {
            return Err(anyhow!(
                "Sequence number fetch already in progress by another task"
            ));
        }

        guard.is_fetching = true;
        // Drop the guard before the await point to allow other tasks to proceed if needed
        // (though in this specific logic, it might not matter much as we return immediately after)
        drop(guard);

        debug!(address = %self.pusher_account.address(), "Fetching sequence number from chain");
        match self
            .client
            .get_account(self.pusher_account.address())
            .await
        {
            Ok(account_data) => {
                let fetched_seq_num = account_data.inner().sequence_number;
                debug!(fetched_seq_num, "Fetched sequence number successfully");
                let mut guard = self.sequence_number_manager.lock().await;
                guard.last_known_sequence_number = Some(fetched_seq_num);
                guard.is_fetching = false;
                Ok(fetched_seq_num)
            }
            Err(e) => {
                // Ensure lock is released even on error
                let mut guard = self.sequence_number_manager.lock().await;
                guard.is_fetching = false;
                Err(anyhow!(
                    "Failed to fetch account sequence number: {}",
                    e.to_string()
                ))
            }
        }
    }

    /// Resets the cached sequence number, forcing a refetch on the next call.
    /// Used when a transaction might have failed or timed out.
    async fn reset_sequence_number(&self) {
        let mut guard = self.sequence_number_manager.lock().await;
        warn!("Resetting cached sequence number.");
        guard.last_known_sequence_number = None;
        // is_fetching should already be false if we are here, but reset just in case
        guard.is_fetching = false;
    }

    /// Spawns a background task to wait for transaction confirmation.
    /// If confirmation fails or times out, resets the sequence number cache.
    fn spawn_wait_for_confirmation(&self, pending_tx: PendingTransaction) {
        let client = Arc::clone(&self.client);
        // Clone the Arc<Mutex<...>> to move into the task
        let sequence_manager_mutex = Arc::clone(&self.sequence_number_manager);

        tokio::spawn(async move {
            let tx_hash = pending_tx.hash; // Access hash field directly
            debug!(%tx_hash, "Waiting for transaction confirmation...");
            // Use a reasonable timeout, e.g., 10-20 seconds
            // Removed duration argument from wait_for_transaction
            // TODO: Add manual timeout using tokio::time::timeout if needed
            match client.wait_for_transaction(&pending_tx).await {
                Ok(response) => {
                    if response.inner().success() {
                        info!(%tx_hash, vm_status = ?response.inner().vm_status(), "Transaction confirmed successfully.");
                    } else {
                        error!(
                            %tx_hash,
                            vm_status = ?response.inner().vm_status(),
                            "Transaction failed on-chain."
                        );
                        // Reset sequence number on failure
                        let mut guard = sequence_manager_mutex.lock().await;
                        warn!(%tx_hash, "Resetting sequence number due to transaction failure.");
                        guard.last_known_sequence_number = None;
                        guard.is_fetching = false;
                    }
                }
                Err(e) => {
                    error!(%tx_hash, error = %e, "Failed waiting for transaction confirmation (timeout or other error).");
                     // Reset sequence number on timeout/error
                    let mut guard = sequence_manager_mutex.lock().await;
                    warn!(%tx_hash, "Resetting sequence number due to confirmation failure/timeout.");
                    guard.last_known_sequence_number = None;
                    guard.is_fetching = false;
                }
            }
        });
    }
}

// --- TargetChain Trait Implementation ---

#[async_trait]
impl TargetChain for AptosChain {
    #[instrument(skip(self), level = "debug")]
    async fn get_latest_price_infos(
        &self,
        price_ids: &[HexString],
    ) -> Result<HashMap<HexString, PriceInfo>> {
        if price_ids.is_empty() {
            return Ok(HashMap::new());
        }

        debug!(count = price_ids.len(), "Fetching latest price infos from Aptos");

        // 1. Get the LatestPriceInfo resource
        let resource_type = format!("{}::state::LatestPriceInfo", self.pyth_contract_address);
        let resource_response = self
            .client
            .get_account_resource(self.pyth_contract_address, &resource_type)
            .await
            .with_context(|| format!("Failed to get resource {}", resource_type))?;

        // Check if the resource exists
        let resource = match resource_response.into_inner() {
            Some(res) => res,
            None => {
                warn!(%resource_type, "LatestPriceInfo resource not found on chain.");
                // Return empty map if the core resource is missing
                return Ok(HashMap::new());
            }
        };
        let table_handle = {
            // Deserialize the JSON data within the resource
            let latest_price_info_data: AptosLatestPriceInfoData = serde_json::from_value(resource.data)
                .with_context(|| format!("Failed to deserialize resource data for {}", resource_type))?;

            AccountAddress::from_str(&latest_price_info_data.info.handle)?
        };

        // 2. Fetch each price info from the table concurrently
        let mut results = HashMap::new();
        let mut tasks = Vec::new();

        let key_type = format!("{}::price_identifier::PriceIdentifier", self.pyth_contract_address);
        let value_type = format!("{}::price_info::PriceInfo", self.pyth_contract_address);


        for price_id in price_ids {
             // Clone necessary data for the async block
            let client = Arc::clone(&self.client);
            let table_handle = table_handle; // Copy handle
            let key_type = key_type.clone();
            let value_type = value_type.clone();
            let price_id = price_id.clone(); // Clone price_id

            tasks.push(tokio::spawn(async move {
                let key = AptosPriceIdentifierKey {
                    // Ensure the key bytes are the hex string *without* 0x prefix
                    bytes: price_id.trim_start_matches("0x").to_string(),
                };

                match client
                    .get_table_item_bcs::<AptosPriceIdentifierKey, AptosPriceInfo>(
                        table_handle,
                        &key_type,
                        &value_type,
                        key, // Pass the key struct directly if get_table_item_bcs supports it
                           // Or serialize if needed: serde_json::to_value(key).unwrap() ? Check aptos-sdk docs
                    )
                    .await
                {
                    Ok(response) => {
                        let item = response.into_inner();
                        // Convert AptosPrice to PriceInfo
                        let price_info = PriceInfo {

                            price: item.price_feed.price.into(),
                            ema_price: item.price_feed.ema_price.into(),
                        };
                         // Wrap the successful result in Ok(Some(...)) to match the Err arm's Option type
                         Ok(Some((price_id, price_info)))
                    }
                    Err(rest_error) => {
                        // For other errors, log and return Err
                        warn!(%price_id, error=?rest_error, "Failed to get table item for price feed.");
                        // Return Err to propagate the error, matching the Ok arm's Result type
                        return Err(anyhow!("Failed to get table item for {}: {}", price_id, rest_error)); // Use return
                    }
                }
            }));
        }

        // Collect results
        for task in tasks {
            match task.await {
                Ok(Ok(Some((id, info)))) => { // Successfully fetched
                    results.insert(id, info);
                }
                 Ok(Ok(None)) => { // Successfully determined not found
                    // Do nothing, just don't insert
                }
                Ok(Err(e)) => { // Error during fetch for a specific ID
                    error!(error = %e, "Error fetching price info for one feed.");
                    // Decide whether to return partial results or fail the whole operation
                    // For now, log and continue, returning partial results.
                }
                Err(e) => { // Task join error (panic, etc.)
                    error!(error = %e, "Task join error while fetching price infos.");
                     // Decide whether to return partial results or fail the whole operation
                }
            }
        }

        debug!(found_count = results.len(), "Finished fetching Aptos price infos.");
        Ok(results)
    }

    #[instrument(skip(self, price_update_data), level = "info")]
    async fn update_price_feed(
        &self, // Needs &mut self because get_next_sequence_number modifies internal state
        price_ids: &[HexString],
        price_update_data: &[Vec<u8>],
        _min_publish_times: &[UnixTimestamp], // Ignored for Aptos update_price_feeds_with_funder
    ) -> Result<()> {
        if price_ids.is_empty() {
            info!("No price IDs provided for update, skipping.");
            return Ok(());
        }
        // if price_ids.len() != price_update_data.len() {
        //     return Err(anyhow!(
        //         "Mismatched lengths for price_ids ({}) and price_update_data ({})",
        //         price_ids.len(),
        //         price_update_data.len()
        //     ));
        // }

        info!(count = price_ids.len(), "Attempting to update Aptos price feeds");

        let sequence_number = match self.get_next_sequence_number().await {
             Ok(seq) => seq,
             Err(e) => {
                 error!(error = %e, "Failed to get sequence number, cannot submit transaction.");
                 // Don't reset here, as the fetch might be in progress elsewhere.
                 // The error from get_next_sequence_number indicates the issue.
                 return Err(e).context("Failed to obtain sequence number for transaction");
             }
        };


        // Construct the payload for `update_price_feeds_with_funder`
        // The argument needs to be Vec<Vec<u8>> serialized appropriately.
        // BCS serialization is typically used for transaction arguments.
        let serialized_args = vec![bcs::to_bytes(price_update_data)?];

        // Identifier::new doesn't return Result, remove ?
        let module_id = ModuleId::new(self.pyth_contract_address, Identifier::new("pyth").unwrap()); // Assuming "pyth" is valid
        let function_id = Identifier::new("update_price_feeds_with_funder").unwrap(); // Assuming valid

        // EntryFunction::new takes 4 args: module_id, function_id, ty_args, args
        let payload = TransactionPayload::EntryFunction(EntryFunction::new(
            module_id,
            function_id,
            vec![], // type_arguments
            serialized_args,                    // arguments: Vec<Vec<u8>> serialized with BCS
        ));

        // Build the transaction
        // Use reasonable defaults for gas, adjust if needed
        let raw_tx = self.transaction_factory.payload(payload)
            .sender(self.pusher_account.address())
            .sequence_number(sequence_number)
            .build();


        // Sign the transaction (remove ?)
        let signed_tx = self.pusher_account.sign_transaction(raw_tx);

        // Submit the transaction using submit_bcs for SignedTransaction
        debug!(%sequence_number, "Submitting signed transaction to Aptos node");
        match self.client.submit(&signed_tx).await {
            Ok(response) => {
                // Successfully submitted, now get the PendingTransaction
                let pending_tx = response.into_inner();
                info!(hash = %pending_tx.hash, "Transaction submitted successfully."); // Access hash field
                // Spawn background task to wait for confirmation
                self.spawn_wait_for_confirmation(pending_tx);
                Ok(())
            }
            Err(e) => {
                error!(error = %e, "Failed to submit transaction.");
                // Reset sequence number immediately on submission failure (e.g., connection error, bad format)
                // Don't reset if the error suggests a sequence number mismatch that might resolve.
                // Aptos SDK error types might help distinguish here. For now, reset cautiously.
                // if !e.to_string().contains("sequence number") { // Basic check, improve if possible
                     self.reset_sequence_number().await;

                Err(anyhow!("Failed to submit Aptos transaction: {}", e))
            }
        }
    }
}