use std::{env, net::Ipv4Addr};

use chain_gang::network::Network;
use log::debug;
use serde::{Deserialize, Serialize};

use crate::secrets::{resolve_secret, warn_plaintext_secrets};

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

impl ClientConfig {
    /// Resolve `env:VAR` references and client-specific environment overrides.
    pub fn resolve_secrets(self) -> Result<Self, String> {
        resolve_client_config(self)
    }
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

    /// Resolve secret references and apply environment overrides.
    pub fn resolve_secrets(mut self) -> Result<Self, String> {
        if let Ok(admin_api_key) = env::var("FS_ADMIN_API_KEY") {
            if !admin_api_key.is_empty() {
                self.web_interface.admin_api_key = Some(admin_api_key);
            }
        } else if let Some(admin_api_key) = self.web_interface.admin_api_key.take() {
            self.web_interface.admin_api_key = Some(resolve_secret(&admin_api_key)?);
        }

        if let Some(clients) = self.client.as_mut() {
            let resolved = std::mem::take(clients)
                .into_iter()
                .map(resolve_client_config)
                .collect::<Result<Vec<_>, _>>()?;
            *clients = resolved;
        }

        Ok(self)
    }
}

fn resolve_client_config(mut client: ClientConfig) -> Result<ClientConfig, String> {
    if let Ok(wif_key) = env::var(client_wif_env_var(&client.client_id)) {
        if !wif_key.is_empty() {
            client.wif_key = wif_key;
        } else {
            client.wif_key = resolve_secret(&client.wif_key)?;
        }
    } else {
        client.wif_key = resolve_secret(&client.wif_key)?;
    }

    if let Ok(api_key) = env::var(client_api_key_env_var(&client.client_id)) {
        if !api_key.is_empty() {
            client.api_key = Some(api_key);
        } else if let Some(key) = client.api_key.take() {
            client.api_key = Some(resolve_secret(&key)?);
        }
    } else if let Some(api_key) = client.api_key.take() {
        client.api_key = Some(resolve_secret(&api_key)?);
    }

    Ok(client)
}

fn client_wif_env_var(client_id: &str) -> String {
    format!("FS_CLIENT_{}_WIF", env_key_suffix(client_id))
}

fn client_api_key_env_var(client_id: &str) -> String {
    format!("FS_CLIENT_{}_API_KEY", env_key_suffix(client_id))
}

fn env_key_suffix(client_id: &str) -> String {
    client_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub fn load_config(env_var: &str, filename: &str) -> Result<Config, String> {
    let config = get_config(env_var, filename)?;
    warn_plaintext_secrets(&config);
    config.resolve_secrets()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_secrets_replaces_env_references() {
        unsafe { env::set_var("FS_TEST_CONFIG_WIF", "test-wif-value") };
        unsafe { env::set_var("FS_TEST_CONFIG_API_KEY", "test-api-key") };
        unsafe { env::set_var("FS_TEST_CONFIG_ADMIN", "admin-from-env") };
        unsafe { env::remove_var("FS_CLIENT_ID1_WIF") };

        let config = Config {
            web_interface: WebInterfaceConfig {
                address: Ipv4Addr::new(127, 0, 0, 1),
                port: 8080,
                admin_api_key: Some("env:FS_TEST_CONFIG_ADMIN".to_string()),
            },
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:FS_TEST_CONFIG_WIF".to_string(),
                api_key: Some("env:FS_TEST_CONFIG_API_KEY".to_string()),
            }]),
            ..Default::default()
        };

        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.web_interface.admin_api_key.as_deref(),
            Some("admin-from-env")
        );
        let client = resolved.client.as_ref().unwrap().first().unwrap();
        assert_eq!(client.wif_key, "test-wif-value");
        assert_eq!(client.api_key.as_deref(), Some("test-api-key"));
    }

    #[test]
    fn resolve_secrets_applies_client_env_overrides() {
        unsafe { env::set_var("FS_CLIENT_ID1_WIF", "override-wif") };
        let config = Config {
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:SHOULD_NOT_BE_USED".to_string(),
                api_key: None,
            }]),
            ..Default::default()
        };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(resolved.client.as_ref().unwrap()[0].wif_key, "override-wif");
        unsafe { env::remove_var("FS_CLIENT_ID1_WIF") };
    }
}
