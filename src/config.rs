use std::{env, net::Ipv4Addr};

use chain_gang::network::Network;
use log::debug;
use serde::{Deserialize, Serialize};

/// Blockchain Interface Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct BlockchainInterfaceConfig {
    pub interface_type: String,
    pub network_type: String,
    pub url: Option<String>,
}

/// Client Configuration
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub wif_key: String,
    /// When set, required for this client's API endpoints.
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ServiceConfig {
    pub utxo_refresh_period: u64,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct DynamicConfigConfig {
    pub filename: String,
}

/// Web Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WebInterfaceConfig {
    pub address: Ipv4Addr,
    pub port: u16,
    /// When set, required for `POST /client`.
    #[serde(default)]
    pub admin_api_key: Option<String>,
}

impl Default for WebInterfaceConfig {
    fn default() -> Self {
        WebInterfaceConfig {
            address: Ipv4Addr::new(0, 0, 0, 0),
            port: 0,
            admin_api_key: None,
        }
    }
}

/// Service Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    pub blockchain_interface: BlockchainInterfaceConfig,
    pub web_interface: WebInterfaceConfig,
    pub logging: LoggingConfig,
    pub service: ServiceConfig,
    pub client: Option<Vec<ClientConfig>>,
    pub dynamic_config: DynamicConfigConfig,
}

impl Config {
    /// Return the configured network as Network type
    pub fn get_network(&self) -> Result<Network, &str> {
        match self.blockchain_interface.network_type.as_str() {
            "mainnet" => Ok(Network::BSV_Mainnet),
            "testnet" => Ok(Network::BSV_Testnet),
            "stn" => Ok(Network::BSV_STN),
            _ => Err("unable to decode network"),
        }
    }

    // Return the log level (as a log::Level type) from the config
    pub fn get_log_level(&self) -> Result<log::Level, String> {
        match self.logging.level.as_str() {
            "error" => Ok(log::Level::Error),
            "warn" | "warning" => Ok(log::Level::Warn),
            "info" | "information" => Ok(log::Level::Info),
            "debug" => Ok(log::Level::Debug),
            "trace" => Ok(log::Level::Trace),
            other => Err(format!("Unknown log level '{other}'")),
        }
    }
}

/// Read the config from the provided file
fn read_config(filename: &str) -> Result<Config, String> {
    debug!("read_config = {}", &filename);
    // Given filename read the config
    let content = std::fs::read_to_string(filename).map_err(|e| e.to_string())?;
    let config = toml::from_str(&content).map_err(|e| e.to_string())?;
    Ok(config)
}

/// Read the config from environment variable, if not read from filename
pub fn get_config(env_var: &str, filename: &str) -> Result<Config, String> {
    match env::var_os(env_var) {
        Some(content) => {
            let val = content
                .into_string()
                .map_err(|_| format!("Environment variable {env_var} is not valid UTF-8"))?;
            serde_json::from_str(&val)
                .map_err(|e| format!("Error parsing JSON environment variable {env_var}: {e}"))
        }
        None => {
            read_config(filename).map_err(|e| format!("Error reading config file {filename}: {e}"))
        }
    }
}
