use std::sync::Arc;

use chain_gang::interface::{BlockchainInterface, TestInterface, UaaSInterface, WocInterface};

use crate::config::Config;

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface
pub fn blockchain_factory(
    config: &Config,
) -> Result<Arc<dyn BlockchainInterface + Send + Sync>, String> {
    let network = config
        .get_network()
        .map_err(|_| "Unable to decode network from config".to_string())?;

    match config.blockchain_interface.interface_type.as_str() {
        "woc" => {
            let mut interface = WocInterface::new();
            interface.set_network(&network);
            Ok(Arc::new(interface))
        }
        "test" => {
            let mut interface = TestInterface::new();
            interface.set_network(&network);
            Ok(Arc::new(interface))
        }
        "uaas" => {
            let uaas_url = config
                .blockchain_interface
                .url
                .as_ref()
                .ok_or_else(|| "Config blockchain interface url not found.".to_string())?;
            let mut interface = UaaSInterface::new(uaas_url)
                .map_err(|e| format!("Unable to create UaaS interface: {e:?}"))?;
            interface.set_network(&network);
            Ok(Arc::new(interface))
        }
        other => Err(format!("Unknown interface type '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BlockchainInterfaceConfig;

    fn base_config(interface_type: &str, url: Option<&str>) -> Config {
        Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: interface_type.to_string(),
                network_type: "testnet".to_string(),
                url: url.map(str::to_string),
            },
            ..Default::default()
        }
    }

    #[test]
    fn sr_bchn_001_blockchain_factory_supports_woc_test_and_uaas() {
        assert!(blockchain_factory(&base_config("woc", None)).is_ok());
        assert!(blockchain_factory(&base_config("test", None)).is_ok());
        assert!(blockchain_factory(&base_config("uaas", Some("http://localhost:5010"))).is_ok());
    }

    #[test]
    fn sr_bchn_001_blockchain_factory_rejects_unknown_interface_type() {
        let result = blockchain_factory(&base_config("unknown", None));
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Unknown interface type"));
    }
}
