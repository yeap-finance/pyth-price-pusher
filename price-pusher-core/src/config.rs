use crate::types::PriceConfig;
use anyhow::{Context, Result};
use std::{collections::HashSet, fs, path::Path};
use tracing::debug;

/// Loads and parses the price configuration YAML file.
///
/// Reads the file at the given path, parses the YAML content into a vector
/// of `PriceConfig` structs, validates uniqueness of IDs and aliases,
/// and normalizes the price feed IDs (removes leading "0x").
pub fn load_price_config(path: impl AsRef<Path>) -> Result<Vec<PriceConfig>> {
    let path_ref = path.as_ref();
    debug!(path = %path_ref.display(), "Loading price config file");

    let yaml_content = fs::read_to_string(path_ref)
        .with_context(|| format!("Failed to read price config file: {}", path_ref.display()))?;

    let mut price_configs: Vec<PriceConfig> = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML from file: {}", path_ref.display()))?;

    // --- Validation ---
    let mut unique_ids = HashSet::new();
    let mut unique_aliases = HashSet::new();

    for config in &mut price_configs {
        // Normalize ID: remove leading "0x" if present
        if config.id.starts_with("0x") || config.id.starts_with("0X") {
            config.id = config.id[2..].to_string();
        }
        // Ensure ID is lowercase hex (optional, but good practice)
        config.id = config.id.to_lowercase();

        // Validate ID format (basic hex check)
        if config.id.len() != 64 || !config.id.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!(
                "Invalid price feed ID format in config file {}: {} (alias: {})",
                path_ref.display(),
                config.id,
                config.alias
            );
        }

        // Check uniqueness
        if !unique_ids.insert(config.id.clone()) {
            anyhow::bail!(
                "Duplicate price feed ID found in config file {}: {} (alias: {})",
                path_ref.display(),
                config.id,
                config.alias
            );
        }
        if !unique_aliases.insert(config.alias.clone()) {
            anyhow::bail!(
                "Duplicate price feed alias found in config file {}: {} (id: {})",
                path_ref.display(),
                config.alias,
                config.id
            );
        }
    }

    debug!(
        count = price_configs.len(),
        "Successfully loaded and validated price configs"
    );
    Ok(price_configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EarlyUpdateConfig;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tracing::info;
    use log::log;

    #[test]
    fn test_load_valid_config() {
        let yaml_content = r#"
- alias: BTC/USD
  id: 0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  time_difference: 60
  price_deviation: 0.5
  confidence_ratio: 1.0
- alias: ETH/USD
  id: fedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321
  time_difference: 120
  price_deviation: 1.0
  confidence_ratio: 1.5
  early_update:
    time_difference: 30
    price_deviation: 0.1
"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", yaml_content).unwrap();

        let configs = load_price_config(file.path()).unwrap();

        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].alias, "BTC/USD");
        assert_eq!(
            configs[0].id,
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
        );
        assert_eq!(configs[0].time_difference_threshold, 60);
        assert_eq!(configs[0].price_deviation_threshold_pct, 0.5);
        assert_eq!(configs[0].confidence_ratio_threshold_pct, 1.0);
        assert_eq!(configs[0].early_update, None);

        assert_eq!(configs[1].alias, "ETH/USD");
        assert_eq!(
            configs[1].id,
            "fedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321"
        );
        assert_eq!(configs[1].time_difference_threshold, 120);
        assert_eq!(configs[1].price_deviation_threshold_pct, 1.0);
        assert_eq!(configs[1].confidence_ratio_threshold_pct, 1.5);
        assert_eq!(
            configs[1].early_update,
            Some(EarlyUpdateConfig {
                time_difference_threshold: Some(30),
                price_deviation_threshold_pct: Some(0.1),
                confidence_ratio_threshold_pct: None, // Not specified in YAML
            })
        );
    }

    #[test]
    fn test_load_duplicate_id() {
        let yaml_content = r#"
- alias: BTC/USD
  id: 0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  time_difference: 60
  price_deviation: 0.5
  confidence_ratio: 1.0
- alias: BTC2/USD
  id: 0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  time_difference: 120
  price_deviation: 1.0
  confidence_ratio: 1.5
"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", yaml_content).unwrap();

        let result = load_price_config(file.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Duplicate price feed ID"));
    }

    #[test]
    fn test_load_duplicate_alias() {
        let yaml_content = r#"
- alias: BTC/USD
  id: 0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  time_difference: 60
  price_deviation: 0.5
  confidence_ratio: 1.0
- alias: BTC/USD
  id: 0xfedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321
  time_difference: 120
  price_deviation: 1.0
  confidence_ratio: 1.5
"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", yaml_content).unwrap();

        let result = load_price_config(file.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Duplicate price feed alias"));
    }

    #[test]
    fn test_load_invalid_id_format() {
        let yaml_content = r#"
- alias: BTC/USD
  id: 0xinvalidhex
  time_difference: 60
  price_deviation: 0.5
  confidence_ratio: 1.0
"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", yaml_content).unwrap();

        let result = load_price_config(file.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid price feed ID format"));
    }

    #[test]
    fn test_load_missing_field() {
                let yaml_content = r#"
- alias: BTC/USD
  id: 0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  # time_difference: 60 # Missing
  price_deviation: 0.5
  confidence_ratio: 1.0
"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", yaml_content).unwrap();

        let result = load_price_config(file.path());
        assert!(result.is_err());
        let error_str = format!("{:?}", result.unwrap_err());
        // Serde error message might vary slightly
        assert!(error_str
            .contains("missing field `time_difference`"));
    }
}
