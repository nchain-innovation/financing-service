use super::blockchain_if::BlockchainInterface;
#[cfg(test)]
use super::blockchain_interface_test::BlockchainInterfaceTest;
use super::blockchain_interface_woc::BlockchainInterfaceWoc;

use crate::config::Config;

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface
#[cfg(test)]
pub fn blockchain_factory(config: &Config) -> Box<dyn BlockchainInterface + Send + Sync> {
    match config.blockchain_interface.interface_type.as_str() {
        "WoC" => Box::new(BlockchainInterfaceWoc::new(config))
            as Box<dyn BlockchainInterface + Send + Sync>,

        "Test" => Box::new(BlockchainInterfaceTest::new(config))
            as Box<dyn BlockchainInterface + Send + Sync>,
        _ => {
            panic!(
                "Unknown interface type '{}'",
                config.blockchain_interface.interface_type
            );
        }
    }
}

#[cfg(not(test))]
pub fn blockchain_factory(config: &Config) -> Box<dyn BlockchainInterface + Send + Sync> {
    match config.blockchain_interface.interface_type.as_str() {
        "WoC" => Box::new(BlockchainInterfaceWoc::new(config))
            as Box<dyn BlockchainInterface + Send + Sync>,
        _ => {
            panic!(
                "Unknown interface type '{}'",
                config.blockchain_interface.interface_type
            );
        }
    }
}
