use std::{env, net::Ipv4Addr};

use chain_gang::network::Network;
use log::debug;
use serde::{Deserialize, Serialize};

/// Blockchain Interface Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct BlockchainInterfaceConfig {
    pub interface_type: String,
    pub network_type: String,
}

/// Client Configuration
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub wif_key: String,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
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
}

impl Default for WebInterfaceConfig {
    fn default() -> Self {
        WebInterfaceConfig {
            address: Ipv4Addr::new(0, 0, 0, 0),
            port: 0,
        }
    }
}

/// Service Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    pub blockchain_interface: BlockchainInterfaceConfig,
    pub web_interface: WebInterfaceConfig,
    pub logging: LoggingConfig,
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

    // Return the log level
    // Return the log level (as a log::Level type) from the config
    pub fn get_log_level(&self) -> log::Level {
        match self.logging.level.as_str() {
            "error" => log::Level::Error,
            "warn" | "warning" => log::Level::Warn,
            "info" | "information" => log::Level::Info,
            "debug" => log::Level::Debug,
            "trace" => log::Level::Trace,
            _ => panic!("Unknown log level {}", self.logging.level),
        }
    }
}

/// Read the config from the provided file
fn read_config(filename: &str) -> std::io::Result<Config> {
    debug!("read_config = {}", &filename);
    // Given filename read the config
    let content = std::fs::read_to_string(filename)?;
    Ok(toml::from_str(&content)?)
}

/// Read the config from environment variable, if not read from filename
pub fn get_config(env_var: &str, filename: &str) -> Option<Config> {
    // read config try env var, then filename, panic if fails

    match env::var_os(env_var) {
        Some(content) => {
            let val = content.into_string().unwrap();
            // Parse to Config
            match serde_json::from_str(&val) {
                Ok(config) => Some(config),
                Err(e) => panic!("Error parsing JSON environment var {:?}", e),
            }
        }
        None => {
            // Read config from file
            let config = match read_config(filename) {
                Ok(config) => config,
                Err(error) => panic!("Error reading config file {:?}", error),
            };
            Some(config)
        }
    }
}
