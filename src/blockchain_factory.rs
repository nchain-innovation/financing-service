use chain_gang::interface::{BlockchainInterface, TestInterface, WocInterface};

use crate::config::Config;

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface

pub fn blockchain_factory(config: &Config) -> Box<dyn BlockchainInterface + Send + Sync> {
    match config.blockchain_interface.interface_type.as_str() {
        "woc" => {
            let mut interface = WocInterface::new();
            interface.set_network(&config.get_network().unwrap());
            Box::new(interface) as Box<dyn BlockchainInterface + Send + Sync>
        },
        "test" => {
            let mut interface = TestInterface::new();
            interface.set_network(&config.get_network().unwrap());
            Box::new(interface) as Box<dyn BlockchainInterface + Send + Sync>
        },
        _ => {
            panic!(
                "Unknown interface type '{}'",
                config.blockchain_interface.interface_type
            );
        }
    }
}
