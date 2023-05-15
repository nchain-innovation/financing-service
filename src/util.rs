use chain_gang::{
    messages::{Payload, Tx},
    util::Serializable,
};

/// Convert a transaction into a hexstring
pub fn tx_as_hexstr(tx: &Tx) -> String {
    let mut b = Vec::with_capacity(tx.size());
    tx.write(&mut b).unwrap();
    hex::encode(&b)
}
