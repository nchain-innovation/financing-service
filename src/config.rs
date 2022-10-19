use serde::Deserialize;
use std::env;
use std::net::Ipv4Addr;
use sv::network::Network;

/// Blockchain Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct BlockchainInterfaceConfig {
    pub interface_type: String,
    pub network_type: String,
}

/// Client Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub wif_key: String,
}

/// Web Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WebInterfaceConfig {
    pub address: Ipv4Addr,
    pub port: u16,
}

/// Service Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub blockchain_interface: BlockchainInterfaceConfig,
    pub web_interface: WebInterfaceConfig,
    pub client: Vec<ClientConfig>,
}

impl Config {
    /// Return the configured network as Network type
    pub fn get_network(&self) -> Result<Network, &str> {
        match self.blockchain_interface.network_type.as_str() {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::Testnet),
            "stn" => Ok(Network::STN),
            _ => Err("unable to decode network"),
        }
    }
}

/// Read the config from the provided file
fn read_config(filename: &str) -> std::io::Result<Config> {
    dbg!(filename);
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