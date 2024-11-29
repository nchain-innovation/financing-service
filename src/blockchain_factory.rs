use chain_gang::interface::{BlockchainInterface, TestInterface, UaaSInterface, WocInterface};

use crate::config::Config;

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface
pub fn blockchain_factory(config: &Config) -> Box<dyn BlockchainInterface + Send + Sync> {
    match config.blockchain_interface.interface_type.as_str() {
        "woc" => {
            let mut interface = WocInterface::new();
            interface.set_network(&config.get_network().unwrap());
            Box::new(interface) as Box<dyn BlockchainInterface + Send + Sync>
        }
        "test" => {
            let mut interface = TestInterface::new();
            interface.set_network(&config.get_network().unwrap());
            Box::new(interface) as Box<dyn BlockchainInterface + Send + Sync>
        }
        "uaas" => {
            if let Some(uaas_url) = &config.blockchain_interface.url {
                let mut interface = UaaSInterface::new(uaas_url).unwrap();
                interface.set_network(&config.get_network().unwrap());
                Box::new(interface) as Box<dyn BlockchainInterface + Send + Sync>
            } else {
                panic!("Config blockchain interface url not found.");
            }
        }

        _ => {
            panic!(
                "Unknown interface type '{}'",
                config.blockchain_interface.interface_type
            );
        }
    }
}
