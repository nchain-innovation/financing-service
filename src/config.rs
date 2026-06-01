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

/// OpenTelemetry trace export configuration.
#[derive(Debug, Default, Deserialize, Clone)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub service_name: Option<String>,
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ServiceConfig {
    pub utxo_refresh_period: u64,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct DynamicConfigConfig {
    pub filename: String,
}

/// Per-IP HTTP rate limiting configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_requests_per_second")]
    pub requests_per_second: u64,
    #[serde(default)]
    pub burst_size: Option<u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            enabled: false,
            requests_per_second: default_requests_per_second(),
            burst_size: None,
        }
    }
}

fn default_requests_per_second() -> u64 {
    10
}

impl RateLimitConfig {
    pub fn effective_burst_size(&self) -> u32 {
        self.burst_size
            .unwrap_or(self.requests_per_second.min(u32::MAX as u64) as u32)
            .max(1)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.requests_per_second == 0 {
            return Err(
                "web_interface.rate_limit.requests_per_second must be greater than 0 when rate limiting is enabled".to_string(),
            );
        }
        Ok(())
    }
}

/// Web Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WebInterfaceConfig {
    pub address: Ipv4Addr,
    pub port: u16,
    /// When set, required for `POST /client`.
    #[serde(default)]
    pub admin_api_key: Option<String>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

impl Default for WebInterfaceConfig {
    fn default() -> Self {
        WebInterfaceConfig {
            address: Ipv4Addr::new(0, 0, 0, 0),
            port: 0,
            admin_api_key: None,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

/// Service Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    pub blockchain_interface: BlockchainInterfaceConfig,
    pub web_interface: WebInterfaceConfig,
    pub logging: LoggingConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
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

/// Return the HTTP bind address and port, honouring `APP_ENV=docker`.
pub fn web_bind_address(config: &Config) -> (Ipv4Addr, u16) {
    let port = config.web_interface.port;
    match env::var_os("APP_ENV") {
        Some(content) if content == "docker" => (Ipv4Addr::new(0, 0, 0, 0), port),
        Some(_) | None => (config.web_interface.address, port),
    }
}

pub fn load_config(env_var: &str, filename: &str) -> Result<Config, String> {
    let config = get_config(env_var, filename)?;
    config.web_interface.rate_limit.validate()?;
    config.telemetry.validate()?;
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
    use chain_gang::network::Network;

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
                rate_limit: RateLimitConfig::default(),
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

    #[test]
    fn sr_sec_008_resolve_secrets_applies_client_api_key_env_override() {
        unsafe { env::set_var("FS_CLIENT_ID1_API_KEY", "override-api-key") };
        let config = Config {
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:FS_TEST_CONFIG_WIF".to_string(),
                api_key: Some("env:SHOULD_NOT_BE_USED".to_string()),
            }]),
            ..Default::default()
        };
        unsafe { env::set_var("FS_TEST_CONFIG_WIF", "test-wif-value") };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.client.as_ref().unwrap()[0].api_key.as_deref(),
            Some("override-api-key")
        );
        unsafe {
            env::remove_var("FS_CLIENT_ID1_API_KEY");
            env::remove_var("FS_TEST_CONFIG_WIF");
        }
    }

    #[test]
    fn rate_limit_config_rejects_zero_requests_per_second_when_enabled() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_second: 0,
            burst_size: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn sr_cfg_001_load_config_reads_toml_file() {
        let dir = std::env::temp_dir().join(format!(
            "financing-service-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[blockchain_interface]
interface_type = "test"
network_type = "testnet"

[web_interface]
address = "127.0.0.1"
port = 9091

[logging]
level = "info"

[service]
utxo_refresh_period = 45

[dynamic_config]
filename = "./data/dynamic.toml"
"#,
        )
        .unwrap();

        unsafe { env::remove_var("FS_CONFIG") };
        let config = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap();
        assert_eq!(config.web_interface.port, 9091);
        assert_eq!(config.service.utxo_refresh_period, 45);
    }

    #[test]
    fn sr_cfg_002_get_config_reads_fs_config_json() {
        unsafe {
            env::set_var(
                "FS_CONFIG",
                r#"{"blockchain_interface":{"interface_type":"test","network_type":"testnet"},"web_interface":{"address":"127.0.0.1","port":9092},"logging":{"level":"info"},"service":{"utxo_refresh_period":30},"dynamic_config":{"filename":"./data/dynamic.toml"}}"#,
            );
        }
        let config = get_config("FS_CONFIG", "missing-file.toml").unwrap();
        assert_eq!(config.web_interface.port, 9092);
        unsafe { env::remove_var("FS_CONFIG") };
    }

    #[test]
    fn sr_cfg_003_web_bind_address_uses_all_interfaces_in_docker() {
        unsafe { env::set_var("APP_ENV", "docker") };
        let config = Config {
            web_interface: WebInterfaceConfig {
                address: Ipv4Addr::new(127, 0, 0, 1),
                port: 8080,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(web_bind_address(&config), (Ipv4Addr::new(0, 0, 0, 0), 8080));
        unsafe { env::remove_var("APP_ENV") };
    }

    #[test]
    fn sr_cfg_006_get_log_level_accepts_configured_levels() {
        for (level, expected) in [
            ("error", log::Level::Error),
            ("warn", log::Level::Warn),
            ("info", log::Level::Info),
            ("debug", log::Level::Debug),
            ("trace", log::Level::Trace),
        ] {
            let config = Config {
                logging: LoggingConfig {
                    level: level.to_string(),
                },
                ..Default::default()
            };
            assert_eq!(config.get_log_level().unwrap(), expected);
        }
    }

    #[test]
    fn sr_bchn_002_get_network_supports_mainnet_testnet_and_stn() {
        for (network_type, expected) in [
            ("mainnet", Network::BSV_Mainnet),
            ("testnet", Network::BSV_Testnet),
            ("stn", Network::BSV_STN),
        ] {
            let config = Config {
                blockchain_interface: BlockchainInterfaceConfig {
                    network_type: network_type.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(config.get_network().unwrap(), expected);
        }
    }

    #[test]
    fn sr_clnt_001_config_supports_multiple_static_clients() {
        let config = Config {
            client: Some(vec![
                ClientConfig {
                    client_id: "id1".to_string(),
                    wif_key: "wif1".to_string(),
                    api_key: None,
                },
                ClientConfig {
                    client_id: "id2".to_string(),
                    wif_key: "wif2".to_string(),
                    api_key: None,
                },
            ]),
            ..Default::default()
        };
        assert_eq!(config.client.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn sr_nfr_007_load_config_returns_error_for_invalid_file() {
        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", "definitely-missing-config.toml").unwrap_err();
        assert!(err.contains("Error reading config file"));
    }

    #[test]
    fn sr_bchn_003_sample_config_sets_utxo_refresh_period() {
        let content = std::fs::read_to_string("data/financing-service.toml").unwrap();
        let config: Config = toml::from_str(&content).unwrap();
        assert_eq!(config.service.utxo_refresh_period, 60);
    }

    #[test]
    fn sr_cfg_007_load_config_validates_telemetry_config() {
        let dir = std::env::temp_dir().join(format!(
            "financing-service-telemetry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[blockchain_interface]
interface_type = "test"
network_type = "testnet"

[web_interface]
address = "127.0.0.1"
port = 8080

[logging]
level = "info"

[telemetry]
enabled = true
otlp_endpoint = ""

[service]
utxo_refresh_period = 60

[dynamic_config]
filename = "./data/dynamic.toml"
"#,
        )
        .unwrap();

        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("telemetry.otlp_endpoint"));
    }
}
