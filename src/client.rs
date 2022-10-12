use bitcoin::secp256k1::Secp256k1;
use bitcoin::util::key::PrivateKey;
use bitcoin::{Address, PublicKey};
use sv::address::addr_decode;
use sv::messages::{OutPoint, Tx, TxIn, TxOut};
use sv::network::Network as SvNetwork;
use sv::script::Script;
use sv::transaction::generate_signature;
use sv::transaction::p2pkh::{create_lock_script, create_unlock_script};
use sv::transaction::sighash::{sighash, SigHashCache, SIGHASH_FORKID, SIGHASH_NONE};
use sv::util::{Hash160, Hash256};

use crate::blockchain_interface::{
    as_bitcoin_network, BlockchainInterface, WocBalance, WocUtxo, WocUtxoEntry,
};
use crate::config::ClientConfig;

/// Represents a Client of the service
#[derive(Debug, Clone)]
pub struct Client {
    /// Used to identify the client
    pub client_id: String,
    /// Funding Private key
    private_key: PrivateKey,
    /// Funding Public key,
    public_key: PublicKey,
    /// Funding Address
    address: Address,
    // Funding Address as Hash
    funding_address: Hash160,
    /// Current funding balance
    balance: WocBalance,
    /// Current funding UTXO
    unspent: WocUtxo,
}

impl Client {
    /// Create a new
    pub fn new(config: &ClientConfig, network: SvNetwork) -> Self {
        let private_key = PrivateKey::from_wif(&config.wif_key).unwrap();
        let secp = Secp256k1::new();
        let public_key = private_key.public_key(&secp);

        let address = Address::p2pkh(&public_key, as_bitcoin_network(&network));
        let (funding_address, _addr_type) = addr_decode(&address.to_string(), network).unwrap();

        Client {
            client_id: config.client_id.clone(),
            private_key,
            public_key,
            address,
            funding_address,
            balance: WocBalance::new(),
            unspent: Vec::new(),
        }
    }

    /// Given an interface query it for the latest balance
    pub async fn update_balance(
        &mut self,
        blockchain_interface: &BlockchainInterface,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.balance = blockchain_interface
            .get_balance(&self.address.to_string())
            .await?;
        self.unspent = blockchain_interface
            .get_utxo(&self.address.to_string())
            .await?;
        // Sort unspent by value
        self.unspent.sort_by_key(|x| x.value);
        Ok(())
    }

    /// Return balance as JSON string
    pub fn get_balance(&self) -> String {
        let client_id = self.client_id.clone();
        let confirmed = self.balance.confirmed;
        let unconfirmed = self.balance.unconfirmed;
        format!("{{\"client_id\": \"{client_id}\", \"confirmed\":{confirmed}, \"unconfirmed\": {unconfirmed}}}")
    }

    /// Return the value of the largest unspent UTXO
    fn get_largest_unspent(&self) -> Option<u64> {
        self.unspent.iter().max_by_key(|x| x.value).map(|x| x.value)
    }

    /// Return the smallest unspent that is greater than given satoshi
    fn get_smallest_unspent(&self, satoshi: u64) -> Option<&WocUtxoEntry> {
        // Note unspent is already sorted by value
        self.unspent.iter().find(|x| x.value > satoshi)
    }

    /// Given the tx inputs, determine if there is a suitable Utxo for it
    pub fn has_sufficent_balance(
        &self,
        satoshi: u64,
        no_of_outpoints: u32,
        multiple_tx: bool,
        locking_script: &[u8],
    ) -> Option<bool> {
        let largest_unspent = self.get_largest_unspent()?;
        let locking_script_len: u64 = locking_script.len() as u64;
        let total_cost: u64 = if no_of_outpoints > 1 && multiple_tx {
            let fee_estimate: u64 = ((locking_script_len / 1000) * 500) + 750;
            (satoshi * no_of_outpoints as u64) + (fee_estimate * no_of_outpoints as u64)
        } else {
            // One tx
            let fee_estimate =
                (((locking_script_len as u64 * no_of_outpoints as u64) / 1000) * 500) + 750;
            (satoshi * no_of_outpoints as u64) + fee_estimate
        };

        Some(total_cost < largest_unspent)
    }

    /// Create one funding transaction
    pub fn create_funding_tx(
        &mut self,
        satoshi: u64,
        no_of_outpoints: u32,
        locking_script: &[u8],
    ) -> Option<Tx> {
        // Calculate fee...
        let locking_script_len: u64 = locking_script.len() as u64;
        let fee_estimate: u64 =
            (((locking_script_len as u64 * no_of_outpoints as u64) / 1000) * 500) + 750;
        let total_cost: u64 = (satoshi * no_of_outpoints as u64) + fee_estimate;

        // Chreat a locking script for change
        let change_script = create_lock_script(&self.funding_address);

        // Find smallest funding unspent that is big enough for tx
        let unspent = self.get_smallest_unspent(total_cost)?;

        // Create vin
        let vins: Vec<TxIn> = vec![TxIn {
            prev_output: OutPoint {
                hash: Hash256::decode(&unspent.tx_hash).unwrap(),
                index: unspent.tx_pos,
            },
            ..Default::default()
        }];
        // Create the vout
        // create vout for change
        let change = unspent.value - total_cost;
        assert!(change > 0);
        let mut vouts: Vec<TxOut> = vec![TxOut {
            satoshis: change as i64,
            lock_script: change_script.clone(),
        }];

        // Append the provided script
        let mut script_pubkey: Script = Script::new();
        script_pubkey.append_slice(locking_script);

        let txout = TxOut {
            satoshis: satoshi as i64,
            lock_script: script_pubkey,
        };
        for _ in 0..no_of_outpoints {
            vouts.push(txout.clone());
        }

        let mut tx = Tx {
            version: 1,
            inputs: vins,
            outputs: vouts,
            lock_time: 0,
        };
        // Sign tx
        let mut cache = SigHashCache::new();
        let sighash_type = SIGHASH_NONE | SIGHASH_FORKID;
        let sighash = sighash(&tx, 0, &change_script.0, 0, sighash_type, &mut cache).unwrap();
        let signature = generate_signature(
            &self.private_key.to_bytes().try_into().unwrap(),
            &sighash,
            sighash_type,
        )
        .unwrap();
        tx.inputs[0].unlock_script =
            create_unlock_script(&signature, &self.public_key.to_bytes().try_into().unwrap());

        // find unspent index
        let index = self.unspent.iter().position(|x| x == unspent).unwrap();

        // Remove input from unspent
        self.unspent.remove(index);
        // Add output to unspent
        let entry: WocUtxoEntry = WocUtxoEntry {
            height: 0,
            tx_pos: 0,
            tx_hash: tx.hash().encode(),
            value: change,
        };
        self.unspent.push(entry);
        // Sort unspent by value
        self.unspent.sort_by_key(|x| x.value);

        // Return the transaction
        Some(tx)
    }

    /// Create no_of_outpoints funding txs each with one outpoint
    pub fn create_multiple_funding_txs(
        &mut self,
        satoshi: u64,
        no_of_outpoints: u32,
        locking_script: &[u8],
    ) -> Vec<Tx> {
        let mut txs: Vec<Tx> = Vec::new();
        for _i in 0..no_of_outpoints {
            match self.create_funding_tx(satoshi, 1, locking_script) {
                Some(tx) => txs.push(tx),
                None => {
                    print!("create_funding_tx failed");
                    break;
                }
            }
        }

        txs
    }
}
