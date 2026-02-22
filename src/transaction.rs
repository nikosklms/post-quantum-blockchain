use serde::{Deserialize, Serialize};
use crate::wallet::Wallet;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

// ─── UTXO Transaction Model ──────────────────────────────────────────────────

/// A reference to a specific output from a previous transaction being spent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxInput {
    pub txid: String,        // Hash of the transaction that created the UTXO
    pub output_index: u32,   // Which output of that transaction (0, 1, 2...)
    pub signature: String,   // Proof that the spender owns this UTXO
    pub pub_key: String,     // Spender's public key (for verification)
}

/// A new coin created by a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxOutput {
    pub amount: f64,         // Value of this coin
    pub recipient: String,   // Owner's public key (who can spend this)
}

/// A UTXO-based transaction: destroys old coins (inputs) and creates new ones (outputs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,                // SHA-256 hash of (inputs + outputs + timestamp)
    pub inputs: Vec<TxInput>,      // Coins being spent (destroyed)
    pub outputs: Vec<TxOutput>,    // Coins being created
    pub timestamp: u128,
}

// ─── Block Reward with Halving ────────────────────────────────────────────────

const INITIAL_REWARD: f64 = 50.0;
const HALVING_INTERVAL: u32 = 1000;  // halves every 1000 blocks (fast for testing)
pub const MAX_OUTPUTS_PER_TX: usize = 256;
pub const MAX_INPUTS_PER_TX: usize = 256;

/// Calculate the mining reward for a given block height.
pub fn block_reward(height: u32) -> f64 {
    let halvings = height / HALVING_INTERVAL;
    if halvings >= 64 {
        return 0.0; // reward vanishes after 64 halvings
    }
    INITIAL_REWARD / (2_u64.pow(halvings) as f64)
}

// ─── Transaction Implementation ──────────────────────────────────────────────

impl Transaction {
    /// Create a coinbase transaction (mining reward — coins from nothing).
    /// Has no inputs; a single output to the miner.
    pub fn new_coinbase(to: &str, block_height: u32) -> Self {
        let reward = block_reward(block_height);
        let outputs = vec![TxOutput {
            amount: reward,
            recipient: to.to_string(),
        }];

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let mut tx = Transaction {
            id: String::new(),
            inputs: vec![],  // no inputs — coinbase
            outputs,
            timestamp,
        };
        tx.id = tx.calculate_hash();
        tx
    }

    /// Create a regular transaction that spends existing UTXOs.
    ///
    /// `utxo_inputs`: the UTXOs being spent  — Vec<(txid, output_index, amount)>
    /// `to`: recipient public key
    /// `amount`: how much to send
    /// `wallet`: sender's wallet (for signing)
    ///
    /// Automatically creates a change output if inputs > amount.
    pub fn new(
        utxo_inputs: Vec<(String, u32, f64)>,
        to: &str,
        amount: f64,
        wallet: &Wallet,
    ) -> Self {
        let sender_key = wallet.get_public_key();
        let input_total: f64 = utxo_inputs.iter().map(|(_, _, a)| a).sum();

        // Build outputs
        let mut outputs = vec![TxOutput {
            amount,
            recipient: to.to_string(),
        }];

        // Change back to sender if inputs > amount
        let change = input_total - amount;
        if change > 0.0001 {
            outputs.push(TxOutput {
                amount: change,
                recipient: sender_key.clone(),
            });
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        // Build inputs (unsigned first, then sign)
        let inputs: Vec<TxInput> = utxo_inputs
            .iter()
            .map(|(txid, idx, _)| TxInput {
                txid: txid.clone(),
                output_index: *idx,
                signature: String::new(), // placeholder
                pub_key: sender_key.clone(),
            })
            .collect();

        let mut tx = Transaction {
            id: String::new(),
            inputs,
            outputs,
            timestamp,
        };
        tx.id = tx.calculate_hash();

        // Sign each input
        for i in 0..tx.inputs.len() {
            tx.inputs[i].signature = wallet.sign(tx.id.as_bytes());
        }

        tx
    }

    /// Create a transaction with multiple recipients (like Bitcoin's sendmany).
    ///
    /// `utxo_inputs`: UTXOs being spent — Vec<(txid, output_index, amount)>
    /// `recipients`: list of (recipient_pubkey, amount)
    /// `wallet`: sender's wallet (for signing)
    pub fn new_multi(
        utxo_inputs: Vec<(String, u32, f64)>,
        recipients: Vec<(&str, f64)>,
        wallet: &Wallet,
    ) -> Self {
        let sender_key = wallet.get_public_key();
        let input_total: f64 = utxo_inputs.iter().map(|(_, _, a)| a).sum();
        let output_total: f64 = recipients.iter().map(|(_, a)| a).sum();

        // Build recipient outputs
        let mut outputs: Vec<TxOutput> = recipients
            .iter()
            .map(|(to, amt)| TxOutput {
                amount: *amt,
                recipient: to.to_string(),
            })
            .collect();

        // Change back to sender
        let change = input_total - output_total;
        if change > 0.0001 {
            outputs.push(TxOutput {
                amount: change,
                recipient: sender_key.clone(),
            });
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let inputs: Vec<TxInput> = utxo_inputs
            .iter()
            .map(|(txid, idx, _)| TxInput {
                txid: txid.clone(),
                output_index: *idx,
                signature: String::new(),
                pub_key: sender_key.clone(),
            })
            .collect();

        let mut tx = Transaction {
            id: String::new(),
            inputs,
            outputs,
            timestamp,
        };
        tx.id = tx.calculate_hash();

        for i in 0..tx.inputs.len() {
            tx.inputs[i].signature = wallet.sign(tx.id.as_bytes());
        }

        tx
    }

    /// Compute the transaction ID (hash of inputs + outputs + timestamp).
    pub fn calculate_hash(&self) -> String {
        let mut hasher = Sha256::new();

        // Hash inputs (txid + index)
        for inp in &self.inputs {
            hasher.update(inp.txid.as_bytes());
            hasher.update(inp.output_index.to_le_bytes());
        }

        // Hash outputs (amount + recipient)
        for out in &self.outputs {
            hasher.update(out.amount.to_le_bytes());
            hasher.update(out.recipient.as_bytes());
        }

        hasher.update(self.timestamp.to_le_bytes());

        format!("{:x}", hasher.finalize())
    }

    /// Check if this is a coinbase transaction (no inputs = mining reward).
    pub fn is_coinbase(&self) -> bool {
        self.inputs.is_empty()
    }

    /// Verify all input signatures.
    /// For coinbase transactions, always returns true.
    pub fn verify_signatures(&self) -> bool {
        if self.is_coinbase() {
            return true;
        }

        for input in &self.inputs {
            if !Wallet::verify(self.id.as_bytes(), &input.signature, &input.pub_key) {
                return false;
            }
        }
        true
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

