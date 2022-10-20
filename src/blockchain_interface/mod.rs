pub mod blockchain_factory;
pub mod blockchain_if;

#[cfg(test)]
pub mod blockchain_interface_test;
#[cfg(test)]
pub use blockchain_interface_test::BlockchainInterfaceTest;

pub mod blockchain_interface_woc;

pub use blockchain_interface_woc::BlockchainInterfaceWoc;

pub use blockchain_factory::blockchain_factory;
pub use blockchain_if::{BlockchainInterface, WocBalance, WocUtxo};
