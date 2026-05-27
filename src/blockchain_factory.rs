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
