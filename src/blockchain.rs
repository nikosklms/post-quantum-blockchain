use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::block::Block;
use crate::transaction::{Transaction, TxOutput, block_reward};

const TARGET_BLOCK_TIME: u64 = 10;
const DIFFICULTY_ADJUSTMENT_INTERVAL: u64 = 10;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum AddBlockResult {
    Added,
    Buffered,
    Exists,
    Invalid,
    Orphan(u32), // Returns index of the missing parent
}

// ─── UTXO Key: (transaction_id, output_index) ────────────────────────────────

/// An unspent output is uniquely identified by the transaction that created it
/// and which output index within that transaction.
pub type UtxoKey = (String, u32);

pub struct Blockchain {
    pub chain: Vec<Block>,
    pub mempool: Vec<Transaction>,
    pub pending_blocks: HashMap<u32, Block>,
    pub utxo_set: HashMap<UtxoKey, TxOutput>,
}

impl Blockchain {
    pub fn new() -> Self {
        let mut blockchain = Blockchain {
            chain: Vec::new(),
            mempool: Vec::new(),
            pending_blocks: HashMap::new(),
            utxo_set: HashMap::new(),
        };

        println!("Creating Genesis Block...");
        let genesis = Block::genesis();
        blockchain.chain.push(genesis);
        // Genesis has no transactions → nothing to add to UTXO set

        blockchain
    }

    // ─── UTXO Methods ─────────────────────────────────────────────────────

    /// Apply a block's transactions to the UTXO set.
    /// For each transaction: remove spent outputs, add new outputs.
    pub fn apply_block(&mut self, block: &Block) {
        for tx in &block.transactions {
            // Remove spent UTXOs (skip for coinbase — no inputs)
            for input in &tx.inputs {
                self.utxo_set.remove(&(input.txid.clone(), input.output_index));
            }
            // Add new UTXOs
            for (i, output) in tx.outputs.iter().enumerate() {
                self.utxo_set.insert(
                    (tx.id.clone(), i as u32),
                    output.clone(),
                );
            }
        }
    }

    /// Get the balance of a public key by summing all UTXOs they own.
    pub fn get_balance(&self, pub_key: &str) -> f64 {
        self.utxo_set
            .values()
            .filter(|output| output.recipient == pub_key)
            .map(|output| output.amount)
            .sum()
    }

    /// Find spendable UTXOs for a given public key, up to the requested amount.
    /// Returns (Vec of (txid, output_index, amount), total accumulated).
    pub fn find_spendable_outputs(&self, pub_key: &str, amount: f64) -> (Vec<(String, u32, f64)>, f64) {
        let mut accumulated = 0.0;
        let mut unspent: Vec<(String, u32, f64)> = Vec::new();

        for ((txid, idx), output) in &self.utxo_set {
            if output.recipient == pub_key {
                accumulated += output.amount;
                unspent.push((txid.clone(), *idx, output.amount));
                if accumulated >= amount {
                    break;
                }
            }
        }

        (unspent, accumulated)
    }

    /// Validate a transaction against the current UTXO set.
    /// - Coinbase: check reward ≤ block_reward(height)
    /// - Regular: check inputs exist, signatures valid, inputs ≥ outputs
    pub fn validate_transaction(&self, tx: &Transaction, block_height: u32) -> bool {
        use crate::transaction::{MAX_INPUTS_PER_TX, MAX_OUTPUTS_PER_TX};

        // Size limits
        if tx.inputs.len() > MAX_INPUTS_PER_TX {
            println!("Transaction rejected: {} inputs exceeds max {}", tx.inputs.len(), MAX_INPUTS_PER_TX);
            return false;
        }
        if tx.outputs.len() > MAX_OUTPUTS_PER_TX {
            println!("Transaction rejected: {} outputs exceeds max {}", tx.outputs.len(), MAX_OUTPUTS_PER_TX);
            return false;
        }

        // Coinbase validation
        if tx.is_coinbase() {
            let max_reward = block_reward(block_height);
            let total_output: f64 = tx.outputs.iter().map(|o| o.amount).sum();
            if total_output > max_reward {
                println!("Coinbase rejected: output {} exceeds reward {}", total_output, max_reward);
                return false;
            }
            return true;
        }

        // Regular transaction validation
        // 1. Verify signatures
        if !tx.verify_signatures() {
            println!("Transaction rejected: invalid signature");
            return false;
        }

        // 2. Check all inputs exist in UTXO set and are owned by the spender
        let mut input_total = 0.0;
        for input in &tx.inputs {
            let key = (input.txid.clone(), input.output_index);
            match self.utxo_set.get(&key) {
                Some(utxo) => {
                    // The input's pub_key must match the UTXO's recipient
                    if utxo.recipient != input.pub_key {
                        println!("Transaction rejected: input pubkey doesn't match UTXO owner");
                        return false;
                    }
                    input_total += utxo.amount;
                }
                None => {
                    println!("Transaction rejected: UTXO ({}, {}) not found", input.txid, input.output_index);
                    return false;
                }
            }
        }

        // 3. Check inputs ≥ outputs (can't create money)
        let output_total: f64 = tx.outputs.iter().map(|o| o.amount).sum();
        if output_total > input_total {
            println!("Transaction rejected: outputs ({}) exceed inputs ({})", output_total, input_total);
            return false;
        }

        true
    }

    // ─── Block Operations ─────────────────────────────────────────────────

    pub fn add_block(&mut self, transactions: Vec<Transaction>) {
        let index = self.chain.len() as u32;
        let previous_hash = self.chain.last().unwrap().hash.clone();
        let difficulty = self.get_difficulty();

        println!("Mining Block {} with difficulty {}...", index, difficulty);
        let mut block = Block::new(index, transactions.clone(), previous_hash, difficulty);
        let stop_signal = std::sync::atomic::AtomicBool::new(false);
        block.mine_block(&stop_signal);
        // Apply UTXO changes
        self.apply_block(&block);
        self.chain.push(block);
        self.remove_mined_transactions(&transactions);
    }

    pub fn add_to_mempool(&mut self, transaction: Transaction) -> bool {
        // Verify signatures
        if !transaction.verify_signatures() {
            println!("Transaction rejected: Invalid signature");
            return false;
        }

        // Validate against UTXO set (skip for coinbase)
        if !transaction.is_coinbase() {
            if !self.validate_transaction(&transaction, self.chain.len() as u32) {
                return false;
            }
        }

        // Check Mempool Capacity (DoS Protection)
        if self.mempool.len() >= 5000 {
            println!("Transaction rejected: Mempool full (max 5000)");
            return false;
        }

        // Check if already in mempool
        if self.mempool.iter().any(|tx| tx.id == transaction.id) {
            println!("Transaction already in mempool.");
            return false;
        }

        self.mempool.push(transaction);
        true
    }

    pub fn get_difficulty(&self) -> usize {
        let chain = &self.chain;
        let last_block = chain.last().unwrap();
        if chain.len() % DIFFICULTY_ADJUSTMENT_INTERVAL as usize != 0 {
            return last_block.difficulty;
        }
        let first_block = chain.iter()
                                .rev()
                                .nth(DIFFICULTY_ADJUSTMENT_INTERVAL as usize - 1)
                                .unwrap();
        let ts_new = last_block.timestamp;
        let ts_first = first_block.timestamp;
        let mut new_difficulty = last_block.difficulty;
        if (ts_new - ts_first) / 1000 > 
            ((DIFFICULTY_ADJUSTMENT_INTERVAL * TARGET_BLOCK_TIME) * 2).into() {
            
            if new_difficulty > 1 {
                new_difficulty -= 1;
            }
                    
        } else if (ts_new - ts_first) / 1000 < 
            ((DIFFICULTY_ADJUSTMENT_INTERVAL * TARGET_BLOCK_TIME) / 2).into() {
            
            new_difficulty += 1;
                    
        }
        new_difficulty
    }

    fn remove_mined_transactions(&mut self, mined_txs: &[Transaction]) {
        self.mempool.retain(|tx| {
            !mined_txs.iter().any(|mined| mined.id == tx.id)
        });
    }

    pub fn validate_chain(&self) -> bool {
        Self::is_chain_valid(&self.chain)
    }

    fn is_chain_valid(chain: &[Block]) -> bool {
        for i in 1..chain.len() {
            let current = &chain[i];
            let previous = &chain[i - 1];

            if current.hash != current.calculate_hash() {
                println!("Block {} has been tampered with!", current.index);
                return false;
            }

            // Verify Merkle Root
            let calculated_root = Block::calculate_merkle_root(&current.transactions);
            if current.merkle_root != calculated_root {
                println!("Block {} has invalid Merkle Root!", current.index);
                return false;
            }

            if current.previous_hash != previous.hash {
                println!("Block {} is not linked to the previous block!", current.index);
                return false;
            }

            // Verify all transaction signatures
            for (t_idx, tx) in current.transactions.iter().enumerate() {
                if !tx.verify_signatures() {
                    println!("Block {} contains invalid transaction (index {})", current.index, t_idx);
                    return false;
                }
            }
        }
        true
    }

    pub fn replace_chain(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.len() <= self.chain.len() {
            println!("Received chain is not longer than current chain.");
            return false;
        }

        if !Self::is_chain_valid(&new_chain) {
            println!("Received chain is invalid.");
            return false;
        }

        println!("Replacing current chain with new longer chain (len {})", new_chain.len());
        self.chain = new_chain;
        // Rebuild UTXO set from scratch after chain replacement
        self.rebuild_utxo_set();
        true
    }

    /// Rebuild the entire UTXO set by replaying all blocks.
    /// Used after chain replacement or initial sync.
    pub fn rebuild_utxo_set(&mut self) {
        self.utxo_set.clear();
        for block in &self.chain {
            for tx in &block.transactions {
                for input in &tx.inputs {
                    self.utxo_set.remove(&(input.txid.clone(), input.output_index));
                }
                for (i, output) in tx.outputs.iter().enumerate() {
                    self.utxo_set.insert(
                        (tx.id.clone(), i as u32),
                        output.clone(),
                    );
                }
            }
        }
    }



    /// Try to add a single block received from the network
    pub fn try_add_block(&mut self, block: Block) -> AddBlockResult {
        let tip = self.chain.last().unwrap();

        // Already have this exact block?
        if tip.hash == block.hash || self.chain.iter().any(|b| b.hash == block.hash) {
            return AddBlockResult::Exists;
        }

        // Direct link: block is the next in sequence
        if block.index == tip.index + 1 && block.previous_hash == tip.hash {
            if block.hash != block.calculate_hash() {
                println!("Rejected block {}: invalid hash.", block.index);
                return AddBlockResult::Invalid;
            }
            // Check block size
            let block_bytes = serde_json::to_vec(&block).unwrap_or_default();
            if block_bytes.len() > crate::block::MAX_BLOCK_SIZE {
                println!("Rejected block {}: size {} exceeds max {}",
                    block.index, block_bytes.len(), crate::block::MAX_BLOCK_SIZE);
                return AddBlockResult::Invalid;
            }
            self.apply_block(&block);
            self.chain.push(block.clone());
            self.remove_mined_transactions(&block.transactions);
            self.drain_pending();
            return AddBlockResult::Added;
        }

        // Future block (gap) -> buffer it
        if block.index > tip.index + 1 {
            if !self.pending_blocks.contains_key(&block.index) {
                println!("📦 Buffering future block {} (tip is {})", block.index, tip.index);
                self.pending_blocks.insert(block.index, block);
                return AddBlockResult::Buffered;
            }
            return AddBlockResult::Exists;
        }

        // Fork or Orphan: block.index == tip.index + 1 BUT wrong parent
        if block.index == tip.index + 1 && block.previous_hash != tip.hash {
            println!("⚠️  Orphan Block {} detected (prev: {}). Requesting parent...", 
                block.index, 
                &block.previous_hash[..8]
            );
            // Buffet it so when parent arrives, we can process it
            self.pending_blocks.insert(block.index, block);
            // Return request for the parent (index - 1)
            return AddBlockResult::Orphan(tip.index); 
        }

        // Old/duplicate block
        AddBlockResult::Exists
    }

    /// Add a batch of blocks (from direct sync response), in order
    pub fn try_add_block_batch(&mut self, blocks: Vec<Block>) -> u32 {
        let mut added = 0u32;
        for block in blocks {
            match self.try_add_block(block) {
                AddBlockResult::Added => added += 1,
                AddBlockResult::Buffered => {},
                _ => {}
            }
        }
        added
    }

    /// Drain pending_blocks that can now link to the chain tip
    fn drain_pending(&mut self) {
        loop {
            let next_idx = self.chain.last().unwrap().index + 1;
            if let Some(block) = self.pending_blocks.remove(&next_idx) {
                let tip = self.chain.last().unwrap();
                if block.previous_hash == tip.hash && block.hash == block.calculate_hash() {
                    println!("🔗 Linking buffered block {}", next_idx);
                    self.apply_block(&block);
                    self.chain.push(block.clone());
                    self.remove_mined_transactions(&block.transactions);
                } else {
                    println!("Buffered block {} invalid, discarding.", next_idx);
                    break;
                }
            } else {
                break;
            }
        }
    }
}

impl std::fmt::Display for Blockchain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n⛓️  Blockchain:")?;
        for block in &self.chain {
            writeln!(f, "{:?}", block)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;

    /// Helper: build a valid chain of `n` blocks on top of genesis.
    fn build_valid_chain(n: u32) -> Vec<Block> {
        let mut bc = Blockchain::new();
        for _ in 0..n {
            bc.add_block(vec![]);
        }
        bc.chain.into_iter().skip(1).collect()
    }

    // ── Parallel Sync Tests ──────────────────────────────────────────────

    #[test]
    fn test_out_of_order_blocks_get_buffered_then_linked() {
        let blocks = build_valid_chain(5);
        let mut bc = Blockchain::new();

        assert_eq!(bc.try_add_block(blocks[2].clone()), AddBlockResult::Buffered);
        assert_eq!(bc.try_add_block(blocks[3].clone()), AddBlockResult::Buffered);
        assert_eq!(bc.try_add_block(blocks[4].clone()), AddBlockResult::Buffered);
        assert_eq!(bc.chain.len(), 1);
        assert_eq!(bc.pending_blocks.len(), 3);

        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        assert_eq!(bc.chain.len(), 2);

        assert_eq!(bc.try_add_block(blocks[1].clone()), AddBlockResult::Added);
        assert_eq!(bc.chain.len(), 6);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }

    #[test]
    fn test_in_order_batch_adds_immediately() {
        let blocks = build_valid_chain(5);
        let mut bc = Blockchain::new();

        let added = bc.try_add_block_batch(blocks);
        assert_eq!(added, 5);
        assert_eq!(bc.chain.len(), 6);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }

    #[test]
    fn test_parallel_download_two_peers() {
        let blocks = build_valid_chain(6);
        let mut bc = Blockchain::new();

        let added_b = bc.try_add_block_batch(blocks[3..6].to_vec());
        assert_eq!(added_b, 0);
        assert_eq!(bc.pending_blocks.len(), 3);

        let added_a = bc.try_add_block_batch(blocks[0..3].to_vec());
        assert_eq!(added_a, 3);
        assert_eq!(bc.chain.len(), 7);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }

    #[test]
    fn test_duplicate_blocks_rejected() {
        let blocks = build_valid_chain(3);
        let mut bc = Blockchain::new();

        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Exists);
        assert_eq!(bc.chain.len(), 2);
    }

    #[test]
    fn test_three_peer_parallel_interleaved() {
        let blocks = build_valid_chain(9);
        let mut bc = Blockchain::new();

        bc.try_add_block_batch(blocks[6..9].to_vec());
        assert_eq!(bc.chain.len(), 1);

        bc.try_add_block_batch(blocks[3..6].to_vec());
        assert_eq!(bc.chain.len(), 1);

        let added = bc.try_add_block_batch(blocks[0..3].to_vec());
        assert_eq!(added, 3);
        assert_eq!(bc.chain.len(), 10);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }

    // ── UTXO Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_coinbase_creates_utxo() {
        let mut bc = Blockchain::new();
        let miner = Wallet::new();

        let coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        assert_eq!(bc.get_balance(&miner.get_public_key()), 50.0);
        assert_eq!(bc.utxo_set.len(), 1);
    }

    #[test]
    fn test_spend_and_change() {
        let mut bc = Blockchain::new();
        let alice = Wallet::new();
        let bob = Wallet::new();

        // Mine a block to give Alice 50 coins
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);
        assert_eq!(bc.get_balance(&alice.get_public_key()), 50.0);

        // Alice sends 30 to Bob
        let (inputs, _total) = bc.find_spendable_outputs(&alice.get_public_key(), 30.0);
        let tx = Transaction::new(inputs, &bob.get_public_key(), 30.0, &alice);
        bc.add_block(vec![tx]);

        assert_eq!(bc.get_balance(&alice.get_public_key()), 20.0);
        assert_eq!(bc.get_balance(&bob.get_public_key()), 30.0);
    }

    #[test]
    fn test_double_spend_rejected() {
        let mut bc = Blockchain::new();
        let alice = Wallet::new();
        let bob = Wallet::new();

        // Mine a block to give Alice 50 coins
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        // Alice creates a valid transaction
        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        let tx = Transaction::new(inputs.clone(), &bob.get_public_key(), 50.0, &alice);
        bc.add_block(vec![tx]);

        // Alice tries to spend the SAME UTXO again (double spend!)
        let tx2 = Transaction::new(inputs, &bob.get_public_key(), 50.0, &alice);
        assert!(!bc.validate_transaction(&tx2, 3), "Double spend should be rejected");
    }

    #[test]
    fn test_insufficient_balance_rejected() {
        let mut bc = Blockchain::new();
        let alice = Wallet::new();
        let bob = Wallet::new();

        // Alice has 0 balance — no coinbase
        let (inputs, total) = bc.find_spendable_outputs(&alice.get_public_key(), 100.0);
        assert_eq!(total, 0.0);
        assert!(inputs.is_empty(), "Should find no UTXOs");
    }
}
