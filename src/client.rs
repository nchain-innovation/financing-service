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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::config::ClientConfig;
use crate::responses::{CodedError, ErrorCode};

/// How long an outpoint stays reserved after a funding transaction whose
/// outcome is unknown.
///
/// The reservation exists to stop the next funding request spending an input
/// that may already be spent on chain, and it has to outlive the gap between a
/// transaction reaching the network and the read interface admitting it --
/// mempool visibility, not confirmation, so seconds to a minute in practice.
/// Ten minutes is far longer than that, and the cost of overshooting is only
/// that funds sit idle: if the transaction never landed, the reservation
/// expires and the outpoint comes back on the next refresh. The cost of
/// undershooting is a conflicting transaction, so the bound leans long.
const UNCERTAIN_SPEND_RESERVATION: Duration = Duration::from_secs(600);

/// An outpoint, as the chain names it.
type OutPointKey = (String, u32);

fn outpoint_key(entry: &UtxoEntry) -> OutPointKey {
    (entry.tx_hash.clone(), entry.tx_pos)
}

/// An input handed to the network in a transaction whose fate is unknown.
///
/// The entry is kept whole rather than just its key so the withheld value can
/// be taken off the reported balance too, leaving the balance and the UTXO set
/// telling the same story.
#[derive(Clone, Debug)]
struct ReservedOutpoint {
    entry: UtxoEntry,
    since: Instant,
}

/// A change output this service created and broadcast, which the chain has not
/// reported back yet.
///
/// Held whole, because keeping it in the cache means putting it back after a
/// refresh has replaced the cache with what the chain says.
#[derive(Clone, Debug)]
struct PendingChange {
    entry: UtxoEntry,
    since: Instant,
}

#[derive(Clone, Debug)]
pub struct FundingSpendPlan {
    spent_indices: Vec<usize>,
    change_entry: UtxoEntry,
    /// The inputs this plan spends, by outpoint, so they can be reserved when
    /// the broadcast outcome is unknown. Indices address the UTXO list this
    /// plan was built against and do not survive a refresh; outpoints do.
    spent_outpoints: Vec<UtxoEntry>,
}

#[derive(Clone)]
pub struct FundRequest {
    pub client_id: String,
    pub satoshi: u64,
    pub no_of_outpoints: u32,
    pub multiple_tx: bool,
    /// One locking script per requested outpoint.
    ///
    /// The REST layer normalises both request forms into this: a single
    /// `locking_script` is expanded to `no_of_outpoints` copies, and a
    /// `locking_scripts` list is used as given. Invariant: the length always
    /// equals `no_of_outpoints`.
    pub locking_scripts: Vec<Vec<u8>>,
}

impl FundRequest {
    /// Total bytes of all output locking scripts, for fee estimation.
    fn output_script_bytes(&self) -> u64 {
        self.locking_scripts
            .iter()
            .map(|script| script.len() as u64)
            .sum()
    }

    /// Script for the `index`-th outpoint, falling back to the last one if the
    /// list is somehow short (the invariant should prevent this).
    fn script_at(&self, index: usize) -> &[u8] {
        self.locking_scripts
            .get(index)
            .or_else(|| self.locking_scripts.last())
            .map(|script| script.as_slice())
            .unwrap_or(&[])
    }
}

/// Represents a Client of the service
#[derive(Debug, Clone)]
pub struct Client {
    api_key: Option<String>,
    /// Funding Wallet
    wallet: Wallet,
    address: String,
    /// Current funding balance
    balance: Balance,
    /// Current funding UTXO
    unspent: Utxo,
    /// Outpoints spent by a funding transaction whose outcome is unknown.
    ///
    /// Held out of `unspent` -- and out of every refresh that would otherwise
    /// resurrect them -- until the chain agrees they are spent or the
    /// reservation expires. See [`UNCERTAIN_SPEND_RESERVATION`].
    reserved: HashMap<OutPointKey, ReservedOutpoint>,
    /// Change this service created and broadcast, which the chain has not
    /// caught up with. Kept in the cache across refreshes so a client can
    /// spend its own change without waiting for the chain to confirm what the
    /// service already knows it sent.
    pending_change: HashMap<OutPointKey, PendingChange>,
}

impl Client {
    pub fn try_new(config: &ClientConfig) -> Result<Self, String> {
        let wallet = Wallet::from_wif(&config.wif_key).map_err(|_| {
            format!(
                r#"wif_key is not a valid WIF key (client_id = "{}")."#,
                config.client_id
            )
        })?;
        let address = wallet.get_address().map_err(|_| {
            format!(
                r#"wif_key is not a valid WIF key - issues with address (client_id = "{}")."#,
                config.client_id
            )
        })?;
        Ok(Client {
            api_key: config.api_key.clone().filter(|key| !key.is_empty()),
            wallet,
            address,
            balance: Balance::default(),
            unspent: Vec::new(),
            reserved: HashMap::new(),
            pending_change: HashMap::new(),
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

    /// Replace the cached balance and UTXO set with what the chain reports,
    /// less anything still reserved by an uncertain funding transaction.
    ///
    /// Without that subtraction a refresh would undo a reservation as fast as
    /// it was made: a transaction that has reached the network but is not yet
    /// visible to the read interface still reads as unspent, and the service
    /// would offer the same input to the next funding request.
    pub fn apply_chain_state(&mut self, balance: Balance, unspent: Utxo) {
        self.release_expired_reservations();
        self.expire_pending_change();

        // An outpoint the chain no longer reports as unspent has been spent,
        // so the reservation has done its job and can go.
        //
        // Except for one this service created itself and the chain has not
        // reported yet -- change from a transaction still in the mempool.
        // "The chain stopped reporting it" and "the chain never reported it"
        // look identical from here, so applying the rule to those would
        // release the reservation the moment it was made. Those are held
        // until they expire instead.
        let ours: std::collections::HashSet<OutPointKey> =
            self.pending_change.keys().cloned().collect();
        self.reserved.retain(|key, _| {
            ours.contains(key) || unspent.iter().any(|entry| &outpoint_key(entry) == key)
        });

        self.balance = balance;
        self.unspent = unspent;
        if !self.reserved.is_empty() {
            self.withhold_reserved();
        }
        self.restore_pending_change();
        self.unspent.sort_by_key(|x| x.value);
    }

    /// Forget change that has been pending too long to still be believed.
    fn expire_pending_change(&mut self) {
        let now = Instant::now();
        self.pending_change
            .retain(|_, pending| now.duration_since(pending.since) < UNCERTAIN_SPEND_RESERVATION);
    }

    /// Put back the change the service has broadcast but the chain has not
    /// reported yet, and count it towards the balance so the two agree.
    ///
    /// A refresh replaces the cache with what the chain says, and the chain
    /// does not yet say anything about a transaction still in the mempool. So
    /// the inputs it spent are held out (`withhold_reserved`) and the change
    /// it created is put back here -- the two halves of the same gap. An entry
    /// goes once the chain reports it, which is the chain catching up, or once
    /// the service has spent it in turn.
    fn restore_pending_change(&mut self) {
        let reported: std::collections::HashSet<OutPointKey> =
            self.unspent.iter().map(outpoint_key).collect();
        self.pending_change.retain(|key, _| !reported.contains(key));
        self.pending_change
            .retain(|key, _| !self.reserved.contains_key(key));

        for pending in self.pending_change.values() {
            let value = pending.entry.value;
            if pending.entry.height < 0 {
                self.balance.unconfirmed += value;
            } else {
                self.balance.confirmed += value;
            }
            self.unspent.push(pending.entry.clone());
        }
    }

    /// Drop the cached copy of every reserved outpoint, and take its value off
    /// the reported balance so the two agree.
    fn withhold_reserved(&mut self) {
        self.unspent
            .retain(|entry| !self.reserved.contains_key(&outpoint_key(entry)));
        for reserved in self.reserved.values() {
            let value = reserved.entry.value;
            if reserved.entry.height < 0 {
                self.balance.unconfirmed -= value;
            } else {
                self.balance.confirmed -= value;
            }
        }
    }

    fn release_expired_reservations(&mut self) {
        let now = Instant::now();
        self.reserved.retain(|key, reserved| {
            let held = now.duration_since(reserved.since) < UNCERTAIN_SPEND_RESERVATION;
            if !held {
                log::info!(
                    "releasing reserved outpoint {}:{} -- no longer spent on chain after {}s, so \
                     the uncertain funding transaction never landed",
                    key.0,
                    key.1,
                    UNCERTAIN_SPEND_RESERVATION.as_secs()
                );
            }
            held
        });
    }

    /// Number of outpoints currently withheld from funding.
    #[cfg(test)]
    pub fn reserved_outpoint_count(&self) -> usize {
        self.reserved.len()
    }

    /// Age every reservation by `by`, so a test can reach the expiry without
    /// waiting for it.
    #[cfg(test)]
    fn backdate_reservations(&mut self, by: Duration) {
        for reserved in self.reserved.values_mut() {
            reserved.since -= by;
        }
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

    /// Estimate the fee for a transaction whose output locking scripts total
    /// `output_script_bytes`.
    ///
    /// Taking a total rather than a length and a count means scripts of
    /// differing sizes are costed correctly, instead of assuming they are all
    /// the same size as the first.
    fn estimate_fee(output_script_bytes: u64, no_of_inputs: u32) -> u64 {
        const INPUT_BYTES: u64 = 148;
        const CHANGE_OUTPUT_BYTES: u64 = 34;
        const TX_OVERHEAD_BYTES: u64 = 10;
        let output_bytes = output_script_bytes + CHANGE_OUTPUT_BYTES;
        let tx_bytes = TX_OVERHEAD_BYTES + INPUT_BYTES * no_of_inputs.max(1) as u64 + output_bytes;
        ((tx_bytes / 1000) * 500) + 750
    }

    fn estimate_total_cost(fund_request: &FundRequest, no_of_inputs: u32) -> u64 {
        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            // One transaction per outpoint, each paying its own script, so
            // cost each separately rather than multiplying one estimate.
            (0..fund_request.no_of_outpoints as usize)
                .map(|index| {
                    let bytes = fund_request.script_at(index).len() as u64;
                    fund_request.satoshi + Self::estimate_fee(bytes, 1)
                })
                .sum()
        } else {
            fund_request.satoshi * fund_request.no_of_outpoints as u64
                + Self::estimate_fee(fund_request.output_script_bytes(), no_of_inputs)
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

        // One output per requested outpoint, each with its own script, so N
        // outpoints from one request can be independently spendable.
        for index in 0..fund_request.no_of_outpoints as usize {
            let mut script_pubkey = Script::new();
            script_pubkey.append_slice(fund_request.script_at(index));
            vouts.push(TxOut {
                satoshis: fund_request.satoshi as i64,
                lock_script: script_pubkey,
            });
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

    /// Return a coded error when the client cannot fund the request.
    pub fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<CodedError> {
        if self.unspent.is_empty() {
            return Some(CodedError::new(
                ErrorCode::NoSuitableUtxo,
                "No UTXOs available for funding.",
            ));
        }

        let total_cost = Self::estimate_total_cost(fund_request, 1);
        let total_available = self.total_unspent();

        if total_available <= total_cost as i64 {
            return Some(CodedError::new(
                ErrorCode::InsufficientBalance,
                format!(
                    "Insufficient client balance: {total_available} satoshi available, {total_cost} required."
                ),
            ));
        }

        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            // Scripts may differ in size, so require a UTXO large enough for
            // the most expensive of the transactions rather than assuming all
            // are the size of the first.
            let per_tx_cost = (0..fund_request.no_of_outpoints as usize)
                .map(|index| {
                    fund_request.satoshi
                        + Self::estimate_fee(fund_request.script_at(index).len() as u64, 1)
                })
                .max()
                .unwrap_or(fund_request.satoshi);
            let suitable_utxos = self.count_utxos_above(per_tx_cost);
            if suitable_utxos < fund_request.no_of_outpoints as usize {
                return Some(CodedError::new(
                    ErrorCode::NoSuitableUtxo,
                    format!(
                        "Not enough UTXOs for {} separate funding transactions: {suitable_utxos} suitable UTXOs, {} required.",
                        fund_request.no_of_outpoints, fund_request.no_of_outpoints
                    ),
                ));
            }
            return None;
        }

        if self.select_utxo_indices(fund_request).is_none() {
            let largest = self.get_largest_unspent().unwrap_or(0);
            let max_inputs = self.unspent.len() as u32;
            let required_with_all_inputs = Self::estimate_total_cost(fund_request, max_inputs);
            return Some(CodedError::new(
                ErrorCode::NoSuitableUtxo,
                format!(
                    "Unable to select UTXOs for funding transaction: largest UTXO is {largest} satoshi, {required_with_all_inputs} required including fees."
                ),
            ));
        }

        None
    }

    fn create_funding_tx_single_input(
        &self,
        fund_request: &FundRequest,
        unspent: &UtxoEntry,
        total_cost: u64,
    ) -> Result<(Tx, FundingSpendPlan), String> {
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

        Ok((
            tx,
            FundingSpendPlan {
                spent_indices: vec![index],
                change_entry,
                spent_outpoints: vec![unspent.clone()],
            },
        ))
    }

    fn create_funding_tx_multi_input(
        &self,
        fund_request: &FundRequest,
        selected_indices: &[usize],
    ) -> Result<(Tx, FundingSpendPlan), String> {
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

        Ok((
            tx,
            FundingSpendPlan {
                spent_indices: selected_indices.to_vec(),
                change_entry,
                spent_outpoints: selected_indices
                    .iter()
                    .filter_map(|index| self.unspent.get(*index).cloned())
                    .collect(),
            },
        ))
    }

    /// Plan a funding transaction without updating the local UTXO cache.
    pub fn plan_funding_tx(
        &self,
        fund_request: &FundRequest,
    ) -> Result<(Tx, FundingSpendPlan), String> {
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

    /// Commit a spend whose transaction was broadcast successfully.
    ///
    /// The inputs are reserved, not merely dropped from the cache. Dropping
    /// them is undone by the very next refresh: the transaction is in the
    /// mempool and the read interface still reports its inputs as unspent, so
    /// the cache takes them back and the next funding request selects the same
    /// one -- building the identical transaction and handing a second caller
    /// an outpoint that already belongs to the first. Three requests a second
    /// apart were enough to see it.
    ///
    /// The change output is pinned for the same reason in reverse. It is real
    /// -- the transaction carrying it was broadcast -- but it is not on chain
    /// yet either, so a refresh would drop it and leave the client unable to
    /// spend its own change until the chain caught up.
    pub fn commit_funding_spend(&mut self, plan: FundingSpendPlan) {
        let change_entry = plan.change_entry.clone();
        self.spend_utxos(&plan.spent_indices, plan.change_entry);
        let now = Instant::now();
        for entry in plan.spent_outpoints {
            self.reserved
                .insert(outpoint_key(&entry), ReservedOutpoint { entry, since: now });
        }
        self.pending_change.insert(
            outpoint_key(&change_entry),
            PendingChange {
                entry: change_entry,
                since: now,
            },
        );
    }

    /// Commit a spend whose transaction may or may not have reached the
    /// network, reserving its inputs so no later refresh offers them again.
    ///
    /// Pessimistic on purpose. The two ways of being wrong are not equal: if
    /// the transaction landed and this service kept treating the inputs as
    /// spendable, the next funding request would build a conflicting
    /// transaction and be refused by the network -- a failure the service
    /// creates for itself and cannot detect. If it never landed, the inputs
    /// sit idle until the reservation expires and the next refresh restores
    /// them. Idle funds heal; a double spend does not.
    ///
    /// The change output is deliberately *not* added to the cache: it exists
    /// only if the transaction landed, and unlike the inputs, wrongly counting
    /// it would have the service try to spend an output that may not exist.
    pub fn commit_uncertain_funding_spend(&mut self, plan: FundingSpendPlan) {
        let spent: std::collections::HashSet<usize> = plan.spent_indices.iter().copied().collect();
        self.unspent = self
            .unspent
            .iter()
            .enumerate()
            .filter(|(index, _)| !spent.contains(index))
            .map(|(_, utxo)| utxo.clone())
            .collect();

        let now = Instant::now();
        for entry in plan.spent_outpoints {
            let value = entry.value;
            if entry.height < 0 {
                self.balance.unconfirmed -= value;
            } else {
                self.balance.confirmed -= value;
            }
            log::warn!(
                "reserving outpoint {}:{} for up to {}s: its funding transaction was handed to \
                 the broadcaster and the outcome is unknown",
                entry.tx_hash,
                entry.tx_pos,
                UNCERTAIN_SPEND_RESERVATION.as_secs()
            );
            self.reserved
                .insert(outpoint_key(&entry), ReservedOutpoint { entry, since: now });
        }
    }

    /// Create one funding transaction and update the local UTXO cache.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn create_funding_tx(&mut self, fund_request: &FundRequest) -> Result<Tx, String> {
        let (tx, plan) = self.plan_funding_tx(fund_request)?;
        self.commit_funding_spend(plan);
        Ok(tx)
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
            locking_scripts: vec![locking_script],
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
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
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
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
        }
    }

    #[test]
    fn test_funding_balance_error_insufficient_total() {
        let client = test_client_with_utxos(&[100]);
        let error = client
            .funding_balance_error(&sample_fund_request(123))
            .unwrap();
        assert_eq!(error.code, ErrorCode::InsufficientBalance);
        assert!(error.description.contains("Insufficient client balance"));
        assert!(error.description.contains("100 satoshi available"));
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
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
        };
        let error = client.funding_balance_error(&fund_request).unwrap();
        assert_eq!(error.code, ErrorCode::NoSuitableUtxo);
        assert!(error.description.contains("Not enough UTXOs"));
        assert!(error.description.contains("2 suitable UTXOs, 3 required"));
    }

    #[test]
    fn sr_fund_001_funding_tx_uses_supplied_locking_script() {
        let client = test_client_with_utxos(&[50_000]);
        let fund_request = sample_fund_request(1_000);
        let (tx, _) = client.plan_funding_tx(&fund_request).unwrap();
        let tx_hex = tx_as_hexstr(&tx).unwrap();
        assert!(tx_hex.contains(LOCKING_SCRIPT_HEX));
    }

    #[test]
    fn sr_fund_010_plan_funding_tx_leaves_utxo_cache_unchanged_until_commit() {
        let mut client = test_client_with_utxos(&[50_000, 40_000]);
        let fund_request = sample_fund_request(1_000);
        let (_, plan_a) = client.plan_funding_tx(&fund_request).unwrap();
        let (_, plan_b) = client.plan_funding_tx(&fund_request).unwrap();
        assert_eq!(plan_a.spent_indices, plan_b.spent_indices);
        client.commit_funding_spend(plan_a);
        assert!(client.plan_funding_tx(&fund_request).is_ok());
    }

    // ---- Reserved outpoints after an uncertain broadcast (issue #66) ----

    fn utxo(tx_hash: &str, tx_pos: u32, value: i64, height: i32) -> UtxoEntry {
        UtxoEntry {
            height,
            tx_pos,
            tx_hash: tx_hash.to_string(),
            value,
        }
    }

    /// A client holding exactly `unspent`, with a matching balance.
    fn client_holding(unspent: Utxo) -> Client {
        let client_config = ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: crate::test_support::TEST_WIF.to_string(),
            api_key: None,
        };
        let mut client = Client::try_new(&client_config).unwrap();
        let balance = Balance {
            confirmed: unspent.iter().map(|u| u.value).sum(),
            unconfirmed: 0,
        };
        client.apply_chain_state(balance, unspent);
        client
    }

    /// Reserve the first cached outpoint, as an uncertain broadcast does.
    fn reserve_first(client: &mut Client) -> UtxoEntry {
        let entry = client.unspent[0].clone();
        client.commit_uncertain_funding_spend(FundingSpendPlan {
            spent_indices: vec![0],
            change_entry: utxo("change", 0, 1, 0),
            spent_outpoints: vec![entry.clone()],
        });
        entry
    }

    /// The defect in #66. A funding transaction reaches the network, the
    /// deadline fires before the answer does, and the refresh that follows
    /// still sees the input as unspent because the transaction is not
    /// confirmed yet. Without the reservation the cache takes that at face
    /// value and offers the same input to the next funding request, which
    /// builds a transaction the network can only refuse.
    #[test]
    fn sr_fund_012_a_refresh_does_not_resurrect_a_reserved_outpoint() {
        let chain = vec![utxo("aa", 0, 5_000, 100), utxo("bb", 1, 7_000, 100)];
        let mut client = client_holding(chain.clone());
        let reserved = reserve_first(&mut client);

        // the read interface has not seen the spend yet, so it reports both
        client.apply_chain_state(
            Balance {
                confirmed: 12_000,
                unconfirmed: 0,
            },
            chain,
        );

        assert_eq!(client.reserved_outpoint_count(), 1);
        assert!(
            !client
                .unspent
                .iter()
                .any(|u| u.tx_hash == reserved.tx_hash && u.tx_pos == reserved.tx_pos),
            "the reserved outpoint came back and could be spent again"
        );
        assert_eq!(client.unspent.len(), 1);
    }

    /// The reservation is not a leak: once the chain stops reporting the
    /// outpoint as unspent, the uncertain transaction has landed and there is
    /// nothing left to protect against.
    #[test]
    fn sr_fund_012_a_reservation_is_released_once_the_chain_agrees_it_is_spent() {
        let mut client = client_holding(vec![utxo("aa", 0, 5_000, 100), utxo("bb", 1, 7_000, 100)]);
        reserve_first(&mut client);
        assert_eq!(client.reserved_outpoint_count(), 1);

        // the spend is visible now: "aa:0" is gone and the change has arrived
        client.apply_chain_state(
            Balance {
                confirmed: 11_800,
                unconfirmed: 0,
            },
            vec![utxo("bb", 1, 7_000, 100), utxo("cc", 0, 4_800, -1)],
        );

        assert_eq!(client.reserved_outpoint_count(), 0);
        assert_eq!(client.unspent.len(), 2);
    }

    /// The other way the uncertainty resolves: the transaction never landed.
    /// The outpoint is still spendable, so the reservation must let go of it
    /// rather than stranding the funds for the life of the process.
    #[test]
    fn sr_fund_012_a_reservation_expires_so_funds_return_if_nothing_landed() {
        let chain = vec![utxo("aa", 0, 5_000, 100), utxo("bb", 1, 7_000, 100)];
        let mut client = client_holding(chain.clone());
        reserve_first(&mut client);

        client.backdate_reservations(UNCERTAIN_SPEND_RESERVATION + Duration::from_secs(1));
        client.apply_chain_state(
            Balance {
                confirmed: 12_000,
                unconfirmed: 0,
            },
            chain,
        );

        assert_eq!(client.reserved_outpoint_count(), 0);
        assert_eq!(client.unspent.len(), 2, "the funds came back");
        assert_eq!(client.get_balance().confirmed, 12_000);
    }

    /// Balance and UTXO set have to tell the same story, or `/balance` invites
    /// a funding request that `/fund` then refuses for want of a UTXO.
    #[test]
    fn sr_fund_012_a_reserved_outpoint_is_off_the_balance_too() {
        let chain = vec![utxo("aa", 0, 5_000, 100), utxo("bb", 1, 7_000, 100)];
        let mut client = client_holding(chain.clone());
        reserve_first(&mut client);
        assert_eq!(client.get_balance().confirmed, 7_000);

        // and it stays off across a refresh that still reports it
        client.apply_chain_state(
            Balance {
                confirmed: 12_000,
                unconfirmed: 0,
            },
            chain,
        );
        assert_eq!(client.get_balance().confirmed, 7_000);
    }

    /// The change output of an uncertain transaction exists only if that
    /// transaction landed. Counting it would have the service try to spend an
    /// output that may never have been created -- the same defect as #66, in
    /// the other direction.
    #[test]
    fn sr_fund_012_an_uncertain_commit_does_not_add_the_change_output() {
        let mut client = client_holding(vec![utxo("aa", 0, 5_000, 100)]);
        client.commit_uncertain_funding_spend(FundingSpendPlan {
            spent_indices: vec![0],
            change_entry: utxo("change", 0, 4_800, 0),
            spent_outpoints: vec![utxo("aa", 0, 5_000, 100)],
        });
        assert!(client.unspent.is_empty(), "{:?}", client.unspent);
    }

    /// A certain broadcast commits the spend outright, change and all -- and
    /// reserves its inputs too.
    ///
    /// This test used to assert the opposite, that a successful broadcast
    /// reserved nothing. That was wrong, and CS-426 is what it cost: the
    /// inputs were dropped from the cache but not held, so the next refresh
    /// took them straight back from a chain that had not seen the transaction
    /// yet, and the request after that spent them again.
    #[test]
    fn sr_fund_012_a_successful_broadcast_reserves_its_inputs() {
        let mut client = client_holding(vec![utxo("aa", 0, 5_000, 100)]);
        client.commit_funding_spend(FundingSpendPlan {
            spent_indices: vec![0],
            change_entry: utxo("change", 0, 4_800, 0),
            spent_outpoints: vec![utxo("aa", 0, 5_000, 100)],
        });
        assert_eq!(
            client.reserved_outpoint_count(),
            1,
            "the spent input must be held, not merely dropped"
        );
        assert_eq!(client.unspent.len(), 1);
        assert_eq!(client.unspent[0].value, 4_800, "change is spendable");
    }

    // ---- CS-426: the same outpoint handed out more than once ----

    /// A real 32-byte txid, because these tests build transactions rather than
    /// only inspecting the cache.
    fn cs_426_utxo(seed: u8, value: i64) -> UtxoEntry {
        utxo(&format!("{:064x}", seed), 0, value, 1_758_719)
    }

    fn cs_426_request(satoshi: u64) -> FundRequest {
        FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
        }
    }

    /// The bug as reported. Three identical `POST /fund` calls a second apart
    /// came back with the same outpoint and the same raw transaction, so three
    /// callers were each told they owned what was in fact one output.
    ///
    /// Each request refreshes, and the chain has not caught up: the first
    /// transaction is in the mempool and its input still reads as unspent. The
    /// cache took it back, and the next request spent it again.
    #[test]
    fn cs_426_repeated_requests_do_not_hand_out_the_same_outpoint() {
        let chain = vec![
            cs_426_utxo(1, 5_000),
            cs_426_utxo(2, 6_000),
            cs_426_utxo(3, 7_000),
        ];
        let balance = Balance {
            confirmed: 18_000,
            unconfirmed: 0,
        };
        let mut client = client_holding(chain.clone());

        let mut hashes = Vec::new();
        for _ in 0..3 {
            let tx = client
                .create_funding_tx(&cs_426_request(1_000))
                .expect("funds");
            hashes.push(tx.hash().encode());
            // the refresh each request makes, against a chain that still
            // reports every input as unspent
            client.apply_chain_state(balance, chain.clone());
        }

        hashes.sort();
        hashes.dedup();
        assert_eq!(
            hashes.len(),
            3,
            "three requests must produce three different transactions"
        );
    }

    /// The mechanism behind it. A refresh replaces the cache with what the
    /// chain says, and the chain says nothing about a transaction it has not
    /// seen, so an input the service has just spent must be held out of the
    /// cache rather than merely removed from it.
    #[test]
    fn cs_426_a_refresh_does_not_return_an_input_already_spent() {
        let chain = vec![cs_426_utxo(1, 5_000), cs_426_utxo(2, 6_000)];
        let balance = Balance {
            confirmed: 11_000,
            unconfirmed: 0,
        };
        let mut client = client_holding(chain.clone());

        let tx = client
            .create_funding_tx(&cs_426_request(1_000))
            .expect("funds");
        let spent: Vec<String> = tx
            .inputs
            .iter()
            .map(|input| input.prev_output.hash.encode())
            .collect();

        client.apply_chain_state(balance, chain);

        for hash in &spent {
            assert!(
                !client.unspent.iter().any(|utxo| &utxo.tx_hash == hash),
                "the refresh handed back an input the service had spent: {hash}"
            );
        }
    }

    /// The other half. The change is real -- its transaction was broadcast --
    /// but the chain has not reported it either, so a refresh would drop it
    /// and leave the client unable to spend its own change.
    #[test]
    fn cs_426_change_survives_a_refresh_that_has_not_seen_it() {
        let chain = vec![cs_426_utxo(1, 5_000)];
        let balance = Balance {
            confirmed: 5_000,
            unconfirmed: 0,
        };
        let mut client = client_holding(chain.clone());

        client
            .create_funding_tx(&cs_426_request(1_000))
            .expect("funds");
        let before: i64 = client.unspent.iter().map(|u| u.value).sum();
        assert!(before > 0, "there is change to keep");

        client.apply_chain_state(balance, chain);

        assert_eq!(
            client.unspent.iter().map(|u| u.value).sum::<i64>(),
            before,
            "the refresh dropped change the service had already broadcast"
        );
    }

    /// Once the chain catches up it is authoritative again: the spent input is
    /// gone from what it reports, the change is in it, and the service stops
    /// holding either.
    #[test]
    fn cs_426_the_service_defers_to_the_chain_once_it_catches_up() {
        let mut client = client_holding(vec![cs_426_utxo(1, 5_000)]);
        client
            .create_funding_tx(&cs_426_request(1_000))
            .expect("funds");
        let change = client.unspent[0].clone();

        // the chain now reports the change and no longer reports the input
        client.apply_chain_state(
            Balance {
                confirmed: change.value,
                unconfirmed: 0,
            },
            vec![UtxoEntry {
                height: 900,
                ..change.clone()
            }],
        );

        assert_eq!(client.reserved_outpoint_count(), 0, "reservation released");
        assert_eq!(client.unspent.len(), 1);
        assert_eq!(
            client.get_balance().confirmed,
            change.value,
            "the chain's confirmed copy is what is reported now"
        );
    }
}
