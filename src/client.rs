use bitcoin::{secp256k1::Secp256k1, util::key::PrivateKey, Address, PublicKey};

use chain_gang::{
    address::addr_decode,
    interface::{Balance, BlockchainInterface, Utxo, UtxoEntry},
    messages::{OutPoint, Tx, TxIn, TxOut},
    network::Network as SvNetwork,
    script::Script,
    transaction::{
        generate_signature,
        p2pkh::{create_lock_script, create_unlock_script},
        sighash::{sighash, SigHashCache, SIGHASH_ALL, SIGHASH_FORKID},
    },
    util::{Hash160, Hash256},
};

use crate::config::ClientConfig;

/// Convert network to bitcoin network type
pub fn as_bitcoin_network(network: &SvNetwork) -> bitcoin::Network {
    match network {
        SvNetwork::BSV_Mainnet => bitcoin::Network::Bitcoin,
        SvNetwork::BSV_Testnet => bitcoin::Network::Testnet,
        SvNetwork::BSV_STN => bitcoin::Network::Signet,
        _ => panic!("unknown network {}", &network),
    }
}

pub struct FundRequest {
    pub client_id: String,
    pub satoshi: u64,
    pub no_of_outpoints: u32,
    pub multiple_tx: bool,
    pub locking_script: Vec<u8>,
}

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
    balance: Balance,
    /// Current funding UTXO
    unspent: Utxo,
}

impl Client {
    /// Create a new
    pub fn new(config: &ClientConfig, network: SvNetwork) -> Self {
        let private_key = PrivateKey::from_wif(&config.wif_key).unwrap_or_else(|_| {
            panic!(
                r#"wif_key = "{}" is not a valid WIF key (client_id = "{}")."#,
                config.wif_key, config.client_id
            )
        });
        let secp = Secp256k1::new();
        let public_key: PublicKey = private_key.public_key(&secp);

        let address: Address = Address::p2pkh(&public_key, as_bitcoin_network(&network));
        let (funding_address, _addr_type) = addr_decode(&address.to_string(), network).unwrap();

        Client {
            client_id: config.client_id.clone(),
            private_key,
            public_key,
            address,
            funding_address,
            balance: Balance::default(),
            unspent: Vec::new(),
        }
    }

    /// Given an interface query it for the latest balance
    pub async fn update_balance(
        &mut self,
        blockchain_interface: &dyn BlockchainInterface,
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
    pub fn get_balance(&self) -> Balance {
        self.balance
    }

    pub fn get_address(&self) -> String {
        self.address.to_string()
    }

    /// Return the value of the largest unspent UTXO
    fn get_largest_unspent(&self) -> Option<i64> {
        self.unspent.iter().max_by_key(|x| x.value).map(|x| x.value)
    }

    /// Return the smallest unspent that is greater than given satoshi
    fn get_smallest_unspent(&self, satoshi: u64) -> Option<&UtxoEntry> {
        // Note unspent is already sorted by value
        self.unspent
            .iter()
            .find(|x| x.value > satoshi.try_into().unwrap())
    }

    /// Given the tx inputs, determine if there is a suitable Utxo for a funding tx
    pub fn has_sufficent_balance(&self, fund_request: &FundRequest) -> Option<bool> {
        let largest_unspent = self.get_largest_unspent()?;
        let locking_script_len: u64 = fund_request.locking_script.len() as u64;

        let total_cost: u64 = if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            let fee_estimate: u64 = ((locking_script_len / 1000) * 500) + 750;
            (fund_request.satoshi * fund_request.no_of_outpoints as u64)
                + (fee_estimate * fund_request.no_of_outpoints as u64)
        } else {
            // One tx
            let fee_estimate =
                (((locking_script_len * fund_request.no_of_outpoints as u64) / 1000) * 500) + 750;
            (fund_request.satoshi * fund_request.no_of_outpoints as u64) + fee_estimate
        };

        Some(total_cost < largest_unspent.try_into().unwrap())
    }

    /// Create one funding transaction
    pub fn create_funding_tx(&mut self, fund_request: &FundRequest) -> Option<Tx> {
        // Calculate fee...
        let locking_script_len: u64 = fund_request.locking_script.len() as u64;
        let fee_estimate: u64 =
            (((locking_script_len * fund_request.no_of_outpoints as u64) / 1000) * 500) + 750;
        let total_cost: u64 =
            (fund_request.satoshi * fund_request.no_of_outpoints as u64) + fee_estimate;
        // Create a locking script for change
        let change_script = create_lock_script(&self.funding_address);
        // Find smallest funding unspent that is big enough for tx
        let unspent = self.get_smallest_unspent(total_cost)?;
        // Create vin
        let vins: Vec<TxIn> = vec![TxIn {
            prev_output: OutPoint {
                hash: Hash256::decode(&unspent.tx_hash).unwrap(),
                index: unspent.tx_pos,
            },
            unlock_script: Script::new(),
            sequence: 0xffffffff,
        }];
        // Create the vout
        // create vout for change
        let change = unspent.value - total_cost as i64;
        assert!(change > 0);
        let mut vouts: Vec<TxOut> = vec![TxOut {
            satoshis: change,
            lock_script: change_script.clone(),
        }];

        // Append the provided script
        let mut script_pubkey: Script = Script::new();
        script_pubkey.append_slice(&fund_request.locking_script);

        let txout = TxOut {
            satoshis: fund_request.satoshi as i64,
            lock_script: script_pubkey,
        };
        for _ in 0..fund_request.no_of_outpoints {
            vouts.push(txout.clone());
        }

        let mut tx = Tx {
            version: 1,
            inputs: vins,
            outputs: vouts,
            lock_time: 0,
        };
        // Sign transaction
        let mut cache = SigHashCache::new();
        let sighash_type = SIGHASH_ALL | SIGHASH_FORKID;

        let sighash = sighash(
            &tx,
            0,
            &change_script.0,
            unspent.value,
            sighash_type,
            &mut cache,
        )
        .unwrap();
        //let sighash = sighash(&tx, 0, &change_script.0, total_cost.try_into().unwrap(), sighash_type, &mut cache).unwrap();
        let signature = generate_signature(
            &self.private_key.to_bytes().try_into().unwrap(),
            &sighash,
            sighash_type,
        )
        .unwrap();
        // insert the ScriptSig (unlock_script)
        tx.inputs[0].unlock_script =
            create_unlock_script(&signature, &self.public_key.to_bytes().try_into().unwrap());

        // find unspent index
        let index = self.unspent.iter().position(|x| x == unspent).unwrap();

        // Remove input from unspent
        self.unspent.remove(index);
        // Add output to unspent
        let entry = UtxoEntry {
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
    pub fn create_multiple_funding_txs(&mut self, fund_request: &FundRequest) -> Vec<Tx> {
        let mut txs: Vec<Tx> = Vec::new();
        for _i in 0..fund_request.no_of_outpoints {
            match self.create_funding_tx(fund_request) {
                Some(tx) => txs.push(tx),
                None => {
                    print!("create_funding_tx failed");
                    break;
                }
            }
        }
        // Return the list of created txs
        txs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{BlockchainInterfaceConfig, ClientConfig, Config},
        util::tx_as_hexstr,
    };
    use chain_gang::interface::{BlockchainInterface, TestInterface, UtxoEntry};
    use log::debug;

    async fn setup_blockchain(config: &Config) -> Box<dyn BlockchainInterface + Send + Sync> {
        let mut blockchain_interface = TestInterface::new();
        blockchain_interface.set_network(&config.get_network().unwrap());

        let utxo = vec![
            UtxoEntry {
                height: 1514933,
                tx_pos: 0,
                tx_hash: "f67272e5c1408ecbeb8da543437c125ee1a17110317d44d13eafe31b771b795e"
                    .to_string(),
                value: 240,
            },
            UtxoEntry {
                height: 1514939,
                tx_pos: 1,
                tx_hash: "b3ec9a52a1fe1689a998c869c2ae38d64d08ece8aaf218286461f330f6fd2ca8"
                    .to_string(),
                value: 100,
            },
            UtxoEntry {
                height: 1514939,
                tx_pos: 1,
                tx_hash: "76f9302ab84fc5da40de02617c10f1a26dff7007c2bfffa9d0845e57d47fa82f"
                    .to_string(),
                value: 100,
            },
            UtxoEntry {
                height: 1514939,
                tx_pos: 1,
                tx_hash: "447ee285748e88d8b875ce09026815578f0474ec3f1babcd5ba917ecb9f1dd7a"
                    .to_string(),
                value: 100,
            },
            UtxoEntry {
                height: 1514939,
                tx_pos: 2,
                tx_hash: "447ee285748e88d8b875ce09026815578f0474ec3f1babcd5ba917ecb9f1dd7a"
                    .to_string(),
                value: 100,
            },
            UtxoEntry {
                height: 1516841,
                tx_pos: 0,
                tx_hash: "70d0365df8062e5af41f8e8f2e42bafde3cabbaebd1fc94e94fa5559e87777b2"
                    .to_string(),
                value: 39080962,
            },
            UtxoEntry {
                height: 1517272,
                tx_pos: 0,
                tx_hash: "51b349bda57674a02ea5b90b43f2204dd6df330d751d646d74d76b19348bf5be"
                    .to_string(),
                value: 39327675,
            },
            UtxoEntry {
                height: 1517429,
                tx_pos: 0,
                tx_hash: "e533109de9df0184299e2199fa8f74baae7d99e4b39d3dea1e957e2f26636578"
                    .to_string(),
                value: 9564208,
            },
            UtxoEntry {
                height: 1517429,
                tx_pos: 1,
                tx_hash: "e533109de9df0184299e2199fa8f74baae7d99e4b39d3dea1e957e2f26636578"
                    .to_string(),
                value: 123,
            },
        ];

        blockchain_interface
            .set_utxo("mwxrVFsJps3sxz5A38Mbrze8kPKq7D5NxF", &utxo)
            .await;

        blockchain_interface.set_height(1517571).await;
        Box::new(blockchain_interface)
    }

    #[tokio::test]
    async fn test_create_tx() {
        // Create a test config
        let config = Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: "test".to_string(),
                network_type: "testnet".to_string(),
                url: None,
            },

            client: vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "cW1ciwAgTLs2EGa6cZHpfLZmUzXbkvq72s15rbiUonkrQAhDU4FG".to_string(),
            }]
            .into(),
            ..Default::default()
        };

        // Set up test blockchain
        let blockchain_interface = setup_blockchain(&config).await;

        let network = config.get_network().unwrap();
        let client_config = config.client.unwrap();
        let mut client = Client::new(&client_config[0], network);

        let result = client.update_balance(&*blockchain_interface).await;
        assert!(&result.is_ok());

        let locking_script =
            hex::decode("76a914b467faf0ef536db106d67f872c448bcaccb878c988ac").unwrap();

        let fund_request = FundRequest {
            client_id: "client1".to_string(),
            satoshi: 123,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script: locking_script,
        };
        let tx = client.create_funding_tx(&fund_request).unwrap();

        debug!("tx = {:?}", &tx);
        assert_eq!(tx_as_hexstr(&tx), "0100000001786563262f7e951eea3d9db3e4997daeba748ffa99219e298401dfe99d1033e5000000006b483045022100c59cb6d235e26d32d2efde738aaa2d18c12c7c75a026731ff3c01448162da85c02203e73cdbba595148e7e64b0379bc6fa4205a7978c592bb1f70440a0e312c5f7594121021abeddfe1373942015c1ef7168dc841d86753431932babdeb2f6e2fccdef882fffffffff02c7ec9100000000001976a914b467faf0ef536db106d67f872c448bcaccb878c988ac7b000000000000001976a914b467faf0ef536db106d67f872c448bcaccb878c988ac00000000");
    }

    #[tokio::test]
    #[should_panic]
    async fn test_invalid_wif_key() {
        let client_config: ClientConfig = ClientConfig {
            client_id: "id1".to_string(),
            wif_key: "EGa6cZHpfLZmUzXbkvq72s15rbiUonkrQAhDU4FG".to_string(),
        };
        Client::new(&client_config, SvNetwork::BSV_Testnet);
    }
}
