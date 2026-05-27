// use bitcoin::{secp256k1::Secp256k1, util::key::PrivateKey, Address, PublicKey};
// use k256::ecdsa::{SigningKey, VerifyingKey};

use chain_gang::{
    interface::{Balance, BlockchainInterface, Utxo, UtxoEntry},
    messages::{OutPoint, Tx, TxIn, TxOut},
    script::Script,
    transaction::{
        //generate_signature,
        //p2pkh::{create_unlock_script},
        //sighash::{sighash, SigHashCache, SIGHASH_ALL, SIGHASH_FORKID},
        sighash::{SIGHASH_ALL, SIGHASH_FORKID},
    },
    util::Hash256,
    wallet::{create_sighash, Wallet},
};

use crate::config::ClientConfig;

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
    /// Funding Wallet
    wallet: Wallet,
    address: String,
    /// Current funding balance
    balance: Balance,
    /// Current funding UTXO
    unspent: Utxo,
}

impl Client {
    pub fn new(config: &ClientConfig) -> Self {
        Self::try_new(config).unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn try_new(config: &ClientConfig) -> Result<Self, String> {
        let wallet = Wallet::from_wif(&config.wif_key).map_err(|_| {
            format!(
                r#"wif_key = "{}" is not a valid WIF key (client_id = "{}")."#,
                config.wif_key, config.client_id
            )
        })?;
        let address = wallet.get_address().map_err(|_| {
            format!(
                r#"wif_key = "{}" is not a valid WIF key - issues with address (client_id = "{}")."#,
                config.wif_key, config.client_id
            )
        })?;
        Ok(Client {
            client_id: config.client_id.clone(),
            wallet,
            address,
            balance: Balance::default(),
            unspent: Vec::new(),
        })
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
            .find(|x| x.value > satoshi as i64)
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

        Some(total_cost < largest_unspent as u64)
    }

    /// Create one funding transaction
    pub fn create_funding_tx(&mut self, fund_request: &FundRequest) -> Result<Tx, String> {
        let locking_script_len: u64 = fund_request.locking_script.len() as u64;
        let fee_estimate: u64 =
            (((locking_script_len * fund_request.no_of_outpoints as u64) / 1000) * 500) + 750;
        let total_cost: u64 =
            (fund_request.satoshi * fund_request.no_of_outpoints as u64) + fee_estimate;
        let change_script = self.wallet.get_locking_script();
        let unspent = self
            .get_smallest_unspent(total_cost)
            .ok_or_else(|| "No suitable UTXO available for funding transaction.".to_string())?;
        let vins: Vec<TxIn> = vec![TxIn {
            prev_output: OutPoint {
                hash: Hash256::decode(&unspent.tx_hash)
                    .map_err(|e| format!("Invalid UTXO tx hash: {e}"))?,
                index: unspent.tx_pos,
            },
            unlock_script: Script::new(),
            sequence: 0xffffffff,
        }];
        let change = unspent.value - total_cost as i64;
        if change <= 0 {
            return Err("Insufficient UTXO value for funding transaction.".to_string());
        }
        let mut vouts: Vec<TxOut> = vec![TxOut {
            satoshis: change,
            lock_script: change_script.clone(),
        }];

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
        let sighash_flags = SIGHASH_ALL | SIGHASH_FORKID;

        let sighash = create_sighash(&tx, 0, &change_script, unspent.value, sighash_flags)
            .map_err(|e| format!("Failed to create sighash: {e}"))?;
        let signature = self
            .wallet
            .sign_sighash(sighash, sighash_flags)
            .map_err(|e| format!("Failed to sign transaction: {e}"))?;

        tx.inputs[0].unlock_script = self.wallet.create_unlock_script(&signature);

        let index = self
            .unspent
            .iter()
            .position(|x| x == unspent)
            .ok_or_else(|| "UTXO not found in local cache.".to_string())?;

        self.unspent.remove(index);
        let entry = UtxoEntry {
            height: 0,
            tx_pos: 0,
            tx_hash: tx.hash().encode(),
            value: change,
        };
        self.unspent.push(entry);
        self.unspent.sort_by_key(|x| x.value);

        Ok(tx)
    }

    /// Create no_of_outpoints funding txs each with one outpoint
    pub fn create_multiple_funding_txs(
        &mut self,
        fund_request: &FundRequest,
    ) -> Result<Vec<Tx>, String> {
        let mut txs: Vec<Tx> = Vec::new();
        for _ in 0..fund_request.no_of_outpoints {
            txs.push(self.create_funding_tx(fund_request)?);
        }
        Ok(txs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ClientConfig,
        test_support::{test_blockchain_interface, test_config, LOCKING_SCRIPT_HEX, TEST_CLIENT_ID},
        util::tx_as_hexstr,
    };

    #[tokio::test]
    async fn test_create_tx() {
        let config = test_config("/tmp/financing-service-client-test-dynamic.toml");

        let blockchain_interface = test_blockchain_interface(&config).await;

        let client_config = config.client.unwrap();
        let mut client = Client::new(&client_config[0]);

        let result = client.update_balance(&*blockchain_interface).await;
        assert!(result.is_ok());

        let locking_script = hex::decode(LOCKING_SCRIPT_HEX).unwrap();

        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script,
        };
        let tx = client.create_funding_tx(&fund_request).unwrap();

        assert_eq!(
            tx_as_hexstr(&tx).unwrap(),
            "0100000001786563262f7e951eea3d9db3e4997daeba748ffa99219e298401dfe99d1033e5000000006b483045022100c7a22fbf24470b2c96b82ce1bfd5896f515e3f0f509f307e94f699baefe0f8c3022044ddbb29952769c67ba117762ee628d299846039a6d90bc59618c770b226bfc2412103a8ae071ddd8690b94755c7112ca304bcac45c15904cc013f0ad6c2ea0b1019b2ffffffff02c7ec9100000000001976a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac7b000000000000001976a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac00000000"
        );
    }

    #[tokio::test]
    async fn test_invalid_wif_key() {
        let client_config = ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: "EGa6cZHpfLZmUzXbkvq72s15rbiUonkrQAhDU4FG".to_string(),
        };
        assert!(Client::try_new(&client_config).is_err());
    }

    #[tokio::test]
    async fn test_create_funding_tx_no_utxo() {
        let client_config = ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: crate::test_support::TEST_WIF.to_string(),
        };
        let mut client = Client::new(&client_config);
        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script: hex::decode(LOCKING_SCRIPT_HEX).unwrap(),
        };

        let result = client.create_funding_tx(&fund_request);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No suitable UTXO"));
    }
}
