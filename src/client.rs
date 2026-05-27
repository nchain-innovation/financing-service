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
    api_key: Option<String>,
    /// Funding Wallet
    wallet: Wallet,
    address: String,
    /// Current funding balance
    balance: Balance,
    /// Current funding UTXO
    unspent: Utxo,
}

impl Client {
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
            api_key: config.api_key.clone().filter(|key| !key.is_empty()),
            wallet,
            address,
            balance: Balance::default(),
            unspent: Vec::new(),
        })
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
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

    pub fn apply_chain_state(&mut self, balance: Balance, unspent: Utxo) {
        self.balance = balance;
        self.unspent = unspent;
        self.unspent.sort_by_key(|x| x.value);
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
        self.unspent.iter().find(|utxo| utxo.value > satoshi as i64)
    }

    fn total_unspent(&self) -> i64 {
        self.unspent.iter().map(|utxo| utxo.value).sum()
    }

    fn estimate_fee(locking_script_len: u64, no_of_outpoints: u32, no_of_inputs: u32) -> u64 {
        const INPUT_BYTES: u64 = 148;
        const CHANGE_OUTPUT_BYTES: u64 = 34;
        const TX_OVERHEAD_BYTES: u64 = 10;
        let output_bytes = locking_script_len * no_of_outpoints as u64 + CHANGE_OUTPUT_BYTES;
        let tx_bytes = TX_OVERHEAD_BYTES + INPUT_BYTES * no_of_inputs.max(1) as u64 + output_bytes;
        ((tx_bytes / 1000) * 500) + 750
    }

    fn estimate_total_cost(fund_request: &FundRequest, no_of_inputs: u32) -> u64 {
        let locking_script_len = fund_request.locking_script.len() as u64;
        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            let fee_estimate = Self::estimate_fee(locking_script_len, 1, 1);
            fund_request.no_of_outpoints as u64 * (fund_request.satoshi + fee_estimate)
        } else {
            fund_request.satoshi * fund_request.no_of_outpoints as u64
                + Self::estimate_fee(
                    locking_script_len,
                    fund_request.no_of_outpoints,
                    no_of_inputs,
                )
        }
    }

    fn count_utxos_above(&self, amount: u64) -> usize {
        self.unspent
            .iter()
            .filter(|utxo| utxo.value > amount as i64)
            .count()
    }

    /// Select UTXO indices whose combined value covers the estimated cost, preferring fewer inputs.
    fn select_utxo_indices(&self, fund_request: &FundRequest) -> Option<Vec<usize>> {
        let mut sorted: Vec<usize> = (0..self.unspent.len()).collect();
        sorted.sort_by_key(|&index| std::cmp::Reverse(self.unspent[index].value));

        let mut selected = Vec::new();
        for index in sorted {
            selected.push(index);
            let input_sum: i64 = selected.iter().map(|&i| self.unspent[i].value).sum();
            let total_cost = Self::estimate_total_cost(fund_request, selected.len() as u32) as i64;
            if input_sum >= total_cost {
                return Some(selected);
            }
        }
        None
    }

    fn outpoint_from_utxo(unspent: &UtxoEntry) -> Result<OutPoint, String> {
        Ok(OutPoint {
            hash: Hash256::decode(&unspent.tx_hash)
                .map_err(|e| format!("Invalid UTXO tx hash: {e}"))?,
            index: unspent.tx_pos,
        })
    }

    fn funding_outputs(
        &self,
        fund_request: &FundRequest,
        change: i64,
        change_script: &Script,
    ) -> Vec<TxOut> {
        let mut vouts = vec![TxOut {
            satoshis: change,
            lock_script: change_script.clone(),
        }];

        let mut script_pubkey = Script::new();
        script_pubkey.append_slice(&fund_request.locking_script);
        let txout = TxOut {
            satoshis: fund_request.satoshi as i64,
            lock_script: script_pubkey,
        };
        for _ in 0..fund_request.no_of_outpoints {
            vouts.push(txout.clone());
        }
        vouts
    }

    fn sign_funding_tx_inputs(
        &self,
        tx: &mut Tx,
        input_amounts: &[i64],
        change_script: &Script,
        sighash_flags: u8,
    ) -> Result<(), String> {
        for (index, amount) in input_amounts.iter().enumerate() {
            let sighash = create_sighash(tx, index, change_script, *amount, sighash_flags)
                .map_err(|e| format!("Failed to create sighash: {e}"))?;
            let signature = self
                .wallet
                .sign_sighash(sighash, sighash_flags)
                .map_err(|e| format!("Failed to sign transaction: {e}"))?;
            tx.inputs[index].unlock_script = self.wallet.create_unlock_script(&signature);
        }
        Ok(())
    }

    fn spend_utxos(&mut self, spent_indices: &[usize], change_entry: UtxoEntry) {
        let spent: std::collections::HashSet<usize> = spent_indices.iter().copied().collect();
        self.unspent = self
            .unspent
            .iter()
            .enumerate()
            .filter_map(|(index, utxo)| {
                if spent.contains(&index) {
                    None
                } else {
                    Some(utxo.clone())
                }
            })
            .collect();
        self.unspent.push(change_entry);
        self.unspent.sort_by_key(|utxo| utxo.value);
    }

    /// Return a description when the client cannot fund the request.
    pub fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<String> {
        if self.unspent.is_empty() {
            return Some("No UTXOs available for funding.".to_string());
        }

        let total_cost = Self::estimate_total_cost(fund_request, 1);
        let total_available = self.total_unspent();

        if total_available <= total_cost as i64 {
            return Some(format!(
                "Insufficent client balance: {total_available} satoshi available, {total_cost} required."
            ));
        }

        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            let per_tx_cost = fund_request.satoshi
                + Self::estimate_fee(fund_request.locking_script.len() as u64, 1, 1);
            let suitable_utxos = self.count_utxos_above(per_tx_cost);
            if suitable_utxos < fund_request.no_of_outpoints as usize {
                return Some(format!(
                    "Not enough UTXOs for {} separate funding transactions: {suitable_utxos} suitable UTXOs, {} required.",
                    fund_request.no_of_outpoints, fund_request.no_of_outpoints
                ));
            }
            return None;
        }

        if self.select_utxo_indices(fund_request).is_none() {
            let largest = self.get_largest_unspent().unwrap_or(0);
            let max_inputs = self.unspent.len() as u32;
            let required_with_all_inputs = Self::estimate_total_cost(fund_request, max_inputs);
            return Some(format!(
                "Unable to select UTXOs for funding transaction: largest UTXO is {largest} satoshi, {required_with_all_inputs} required including fees."
            ));
        }

        None
    }

    fn create_funding_tx_single_input(
        &mut self,
        fund_request: &FundRequest,
        unspent: &UtxoEntry,
        total_cost: u64,
    ) -> Result<Tx, String> {
        let change_script = self.wallet.get_locking_script();
        let change = unspent.value - total_cost as i64;
        if change <= 0 {
            return Err("Insufficient UTXO value for funding transaction.".to_string());
        }

        let mut tx = Tx {
            version: 1,
            inputs: vec![TxIn {
                prev_output: Self::outpoint_from_utxo(unspent)?,
                unlock_script: Script::new(),
                sequence: 0xffffffff,
            }],
            outputs: self.funding_outputs(fund_request, change, &change_script),
            lock_time: 0,
        };

        let sighash_flags = SIGHASH_ALL | SIGHASH_FORKID;
        self.sign_funding_tx_inputs(&mut tx, &[unspent.value], &change_script, sighash_flags)?;

        let index = self
            .unspent
            .iter()
            .position(|x| x == unspent)
            .ok_or_else(|| "UTXO not found in local cache.".to_string())?;
        let change_entry = UtxoEntry {
            height: 0,
            tx_pos: 0,
            tx_hash: tx.hash().encode(),
            value: change,
        };
        self.spend_utxos(&[index], change_entry);

        Ok(tx)
    }

    fn create_funding_tx_multi_input(
        &mut self,
        fund_request: &FundRequest,
        selected_indices: &[usize],
    ) -> Result<Tx, String> {
        let change_script = self.wallet.get_locking_script();
        let input_amounts: Vec<i64> = selected_indices
            .iter()
            .map(|&index| self.unspent[index].value)
            .collect();
        let input_sum: i64 = input_amounts.iter().sum();
        let total_cost =
            Self::estimate_total_cost(fund_request, selected_indices.len() as u32) as i64;
        let change = input_sum - total_cost;
        if change <= 0 {
            return Err("Insufficient UTXO value for funding transaction.".to_string());
        }

        let inputs = selected_indices
            .iter()
            .map(|&index| {
                Ok(TxIn {
                    prev_output: Self::outpoint_from_utxo(&self.unspent[index])?,
                    unlock_script: Script::new(),
                    sequence: 0xffffffff,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let mut tx = Tx {
            version: 1,
            inputs,
            outputs: self.funding_outputs(fund_request, change, &change_script),
            lock_time: 0,
        };

        let sighash_flags = SIGHASH_ALL | SIGHASH_FORKID;
        self.sign_funding_tx_inputs(&mut tx, &input_amounts, &change_script, sighash_flags)?;

        let change_entry = UtxoEntry {
            height: 0,
            tx_pos: 0,
            tx_hash: tx.hash().encode(),
            value: change,
        };
        self.spend_utxos(selected_indices, change_entry);

        Ok(tx)
    }

    /// Create one funding transaction
    pub fn create_funding_tx(&mut self, fund_request: &FundRequest) -> Result<Tx, String> {
        let total_cost_single = Self::estimate_total_cost(fund_request, 1);
        if let Some(unspent) = self.get_smallest_unspent(total_cost_single) {
            let unspent = unspent.clone();
            return self.create_funding_tx_single_input(fund_request, &unspent, total_cost_single);
        }

        let selected_indices = self
            .select_utxo_indices(fund_request)
            .ok_or_else(|| "No suitable UTXO set available for funding transaction.".to_string())?;
        self.create_funding_tx_multi_input(fund_request, &selected_indices)
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
        test_support::{
            test_blockchain_interface, test_config, LOCKING_SCRIPT_HEX, TEST_CLIENT_ID,
        },
        util::tx_as_hexstr,
    };

    #[tokio::test]
    async fn test_create_tx() {
        let config = test_config("/tmp/financing-service-client-test-dynamic.toml");

        let blockchain_interface = test_blockchain_interface(&config).await;

        let client_config = config.client.unwrap();
        let mut client = Client::try_new(&client_config[0]).unwrap();

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
            api_key: None,
        };
        assert!(Client::try_new(&client_config).is_err());
    }

    #[tokio::test]
    async fn test_create_funding_tx_no_utxo() {
        let client_config = ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: crate::test_support::TEST_WIF.to_string(),
            api_key: None,
        };
        let mut client = Client::try_new(&client_config).unwrap();
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

    fn test_client_with_utxos(values: &[i64]) -> Client {
        use chain_gang::interface::UtxoEntry;

        let client_config = ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: crate::test_support::TEST_WIF.to_string(),
            api_key: None,
        };
        let mut client = Client::try_new(&client_config).unwrap();
        client.unspent = values
            .iter()
            .enumerate()
            .map(|(index, value)| UtxoEntry {
                height: 1,
                tx_pos: 0,
                tx_hash: format!("{:064x}", index + 1),
                value: *value,
            })
            .collect();
        client.unspent.sort_by_key(|utxo| utxo.value);
        client
    }

    fn sample_fund_request(satoshi: u64) -> FundRequest {
        FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script: hex::decode(LOCKING_SCRIPT_HEX).unwrap(),
        }
    }

    #[test]
    fn test_funding_balance_error_insufficient_total() {
        let client = test_client_with_utxos(&[100]);
        let error = client
            .funding_balance_error(&sample_fund_request(123))
            .unwrap();
        assert!(error.contains("Insufficent client balance"));
        assert!(error.contains("100 satoshi available"));
    }

    #[test]
    fn test_funding_balance_error_consolidates_multiple_utxos() {
        let client = test_client_with_utxos(&[300, 300, 300]);
        assert!(client
            .funding_balance_error(&sample_fund_request(123))
            .is_none());
    }

    #[test]
    fn test_create_funding_tx_consolidates_multiple_utxos() {
        let mut client = test_client_with_utxos(&[300, 300, 300]);
        let tx = client
            .create_funding_tx(&sample_fund_request(123))
            .expect("expected multi-input funding transaction");
        assert_eq!(tx.inputs.len(), 3);
        assert_eq!(tx.outputs.len(), 2);
    }

    #[test]
    fn test_funding_balance_error_sufficient() {
        let client = test_client_with_utxos(&[100, 10_000]);
        assert!(client
            .funding_balance_error(&sample_fund_request(123))
            .is_none());
    }

    #[test]
    fn test_funding_balance_error_multiple_tx_not_enough_utxos() {
        let client = test_client_with_utxos(&[10_000, 10_000]);
        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 3,
            multiple_tx: true,
            locking_script: hex::decode(LOCKING_SCRIPT_HEX).unwrap(),
        };
        let error = client.funding_balance_error(&fund_request).unwrap();
        assert!(error.contains("Not enough UTXOs"));
        assert!(error.contains("2 suitable UTXOs, 3 required"));
    }
}
