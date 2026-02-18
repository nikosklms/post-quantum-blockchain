use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use num_bigint::BigUint;
use num_traits::Num;

use crate::block::Block;
use crate::transaction::{Transaction, TxOutput, block_reward};

const TARGET_BLOCK_TIME: u64 = 10;
const DIFFICULTY_ADJUSTMENT_INTERVAL: u64 = 10;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum AddBlockResult {
    Added,
    Buffered,
    Exists,
    Orphan(u32), // Returns index of the missing parent
    Fork,        // Block is at existing height but different hash
    Invalid,
}

// ─── UTXO Key: (transaction_id, output_index) ────────────────────────────────

/// An unspent output is uniquely identified by the transaction that created it
/// and which output index within that transaction.
pub type UtxoKey = (String, u32);

#[derive(Serialize, Deserialize)]
pub struct Blockchain {
    pub chain: Vec<Block>,
    pub mempool: Vec<Transaction>,
    #[serde(skip)]
    pub pending_blocks: HashMap<u32, Block>,
    #[serde(skip)]
    pub utxo_set: HashMap<UtxoKey, TxOutput>,
    #[serde(skip)]
    pub file_path: String,
}

impl Blockchain {
    pub fn new(file_path: &str) -> Self {
        if let Ok(loaded) = Self::load_chain(file_path) {
            println!("📂 Blockchain loaded from {} (Height: {})", file_path, loaded.chain.len() - 1);
            return loaded;
        }

        println!("⚠️  No blockchain found at {}. Creating Genesis...", file_path);
        let mut blockchain = Blockchain {
            chain: Vec::new(),
            mempool: Vec::new(),
            pending_blocks: HashMap::new(),
            utxo_set: HashMap::new(),
            file_path: file_path.to_string(),
        };

        let genesis = Block::genesis();
        blockchain.chain.push(genesis);
        // Save genesis immediately
        blockchain.save_chain();
        blockchain
    }

    pub fn save_chain(&self) {
        let file = std::fs::File::create(&self.file_path).expect("Could not create chain file");
        serde_json::to_writer(file, &self).expect("Could not save chain");
    }

    pub fn load_chain(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let mut blockchain: Blockchain = serde_json::from_reader(file)?;
        blockchain.file_path = path.to_string();
        blockchain.pending_blocks = HashMap::new();
        blockchain.utxo_set = HashMap::new();
        // Rebuild UTXO set from scratch to ensure consistency
        blockchain.rebuild_utxo_set();
        Ok(blockchain)
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

        // Sanity Check: All outputs must be positive and finite
        for output in &tx.outputs {
            if !output.amount.is_finite() || output.amount <= 0.0 {
                println!("Transaction rejected: invalid output amount ({})", output.amount);
                return false;
            }
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
        self.save_chain();
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

        // Check for Double Spend in Mempool
        // We must ensure that none of the inputs in `transaction` are already spent 
        // by any other transaction currently in the mempool.
        for mem_tx in &self.mempool {
            for mem_input in &mem_tx.inputs {
                for new_input in &transaction.inputs {
                    if mem_input.txid == new_input.txid && mem_input.output_index == new_input.output_index {
                        println!("Transaction rejected: Input ({}, {}) already spent in mempool by {}",
                             new_input.txid, new_input.output_index, mem_tx.id);
                        return false;
                    }
                }
            }
        }

        self.mempool.push(transaction);
        true
    }

    pub fn get_difficulty(&self) -> String {
        let chain = &self.chain;
        let last_block = chain.last().unwrap();
        
        // Initial target (match genesis)
        let initial_target = "0000ffff00000000000000000000000000000000000000000000000000000000".to_string();

        if chain.len() < DIFFICULTY_ADJUSTMENT_INTERVAL as usize {
             return initial_target;
        }
        
        if chain.len() % DIFFICULTY_ADJUSTMENT_INTERVAL as usize != 0 {
            return last_block.difficulty.clone();
        }

        // Retargeting
        let interval = DIFFICULTY_ADJUSTMENT_INTERVAL as usize;
        let first_block = chain.iter().rev().nth(interval - 1).unwrap();
        
        let actual_time_ms = last_block.timestamp - first_block.timestamp;
        let actual_seconds = actual_time_ms / 1000;
        let expected_seconds = (DIFFICULTY_ADJUSTMENT_INTERVAL * TARGET_BLOCK_TIME) as u128;
        
        // Damping: Limit adjustment factor to 4x or 0.25x
        let adjusted_actual = if actual_seconds < expected_seconds / 4 {
             expected_seconds / 4
        } else if actual_seconds > expected_seconds * 4 {
             expected_seconds * 4
        } else {
             actual_seconds
        };

        let current_target = BigUint::from_str_radix(&last_block.difficulty, 16)
            .unwrap_or_else(|_| BigUint::from_str_radix(&initial_target, 16).unwrap());
            
        // Algorithm: New = Old * (Actual / Expected)
        // BigUint integer division implies floor, which is fine.
        let new_target = current_target * BigUint::from(adjusted_actual) / BigUint::from(expected_seconds);

        // Ensure we don't exceed max target (or some reasonable upper bound) or go to zero
        let max_target = BigUint::from_str_radix(&initial_target, 16).unwrap();
        
        let final_target = if new_target > max_target {
            max_target
        } else if new_target == BigUint::from(0u32) {
             BigUint::from(1u32) // Avoid zero target (impossible to mine)
        } else {
             new_target
        };

        // Format as 32-byte hex (64 chars), padded
        let mut hex = final_target.to_str_radix(16);
        while hex.len() < 64 {
            hex.insert(0, '0');
        }
        hex
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
        self.save_chain();
        true
    }

    /// Create locator hashes: a list of block hashes from tip backwards.
    /// Used to find the common ancestor with a peer.
    pub fn create_locator_hashes(&self) -> Vec<String> {
        self.chain.iter().rev().take(50).map(|b| b.hash.clone()).collect()
    }

    /// Find the highest block index that we share with the locator list.
    pub fn find_common_ancestor(&self, locators: &[String]) -> Option<u32> {
        // Iterate through locators (which are from tip backwards)
        // The first one we recognize is the common ancestor (latest shared block)
        for hash in locators {
            if let Some(block) = self.chain.iter().find(|b| &b.hash == hash) {
                return Some(block.index);
            }
        }
        // If no match, maybe genesis? (Genesis should be in locators if chain is short)
        // Or if locators didn't reach genesis, we might default to 0 if we assume same genesis.
        if let Some(genesis) = self.chain.first() {
             if locators.contains(&genesis.hash) {
                 return Some(0);
             }
        }
        None
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
         // 2. Proof of Work check
        // Check against the difficulty expected for THIS block
        // (which is calculated from the chain state *before* this block is added)
        if !block.is_valid(&self.get_difficulty()) {
             println!("Block {} has invalid PoW", block.index);
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
            self.save_chain();
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

        // Old/duplicate block or Fork
        if block.index <= tip.index {
            if let Some(existing) = self.chain.get(block.index as usize) {
                if existing.hash == block.hash {
                    return AddBlockResult::Exists;
                }
                // Different hash at existing height => FORK
                return AddBlockResult::Fork;
            }
        }
        
        AddBlockResult::Exists
    }

    /// Add a batch of blocks. Returns (count_added, failure_reason)
    pub fn try_add_block_batch(&mut self, blocks: Vec<Block>) -> (u32, Option<AddBlockResult>) {
        let mut added = 0u32;
        for (i, block) in blocks.iter().enumerate() {
            // Check if we can append normally
            let res = self.try_add_block(block.clone());
            match res {
                AddBlockResult::Added => added += 1,
                AddBlockResult::Buffered => {},
                AddBlockResult::Exists => {},
                // If we hit an Orphan or Fork, check if the BATCH itself contains the solution (Chain Reorg)
                AddBlockResult::Orphan(_) | AddBlockResult::Fork => {
                    println!("DEBUG: Fork/Orphan at block {}. Checking if batch allows reorg...", block.index);
                    // Does this block connect to a known ancestor?
                    if let Some(ancestor_idx) = self.chain.iter().position(|b| b.hash == block.previous_hash) {
                         println!("🔱 Potential Fork detected at height {}. Attempting reorg with batch...", ancestor_idx + 1);
                         
                         // Construct candidate chain: Genesis...Ancestor + Rest of Batch
                         // We use blocks[i..] to get the current block and all subsequent ones
                         let mut candidate = self.chain[0..=ancestor_idx].to_vec();
                         candidate.extend(blocks[i..].to_vec());
                         
                         if self.replace_chain(candidate) {
                             println!("DEBUG: Reorg successful via batch!");
                             // If reorg succeeds, we effectively "added" the whole batch (or the relevant part)
                             // and established a new main chain.
                             // We return success (None error).
                             return (blocks.len() as u32, None);
                         } else {
                             println!("DEBUG: Reorg failed verification.");
                         }
                    } else {
                        println!("DEBUG: Ancestor not found for reorg.");
                    }
                    
                    // If reorg failed or wasn't possible, return the error
                    return (added, Some(res));
                }
                AddBlockResult::Invalid => {
                    return (added, Some(res));
                }
            }
        }
        
        (added, None)
    }

    /// Drain pending_blocks that can now link to the chain tip
    fn drain_pending(&mut self) {
        let mut added = false;
        loop {
            let next_idx = self.chain.last().unwrap().index + 1;
            if let Some(block) = self.pending_blocks.remove(&next_idx) {
                let tip = self.chain.last().unwrap();
                if block.previous_hash == tip.hash && block.hash == block.calculate_hash() {
                    println!("🔗 Linking buffered block {}", next_idx);
                    self.apply_block(&block);
                    self.chain.push(block.clone());
                    self.remove_mined_transactions(&block.transactions);
                    added = true;
                } else {
                    println!("Buffered block {} invalid, discarding.", next_idx);
                    break;
                }
            } else {
                break;
            }
        }
        
        if added {
            self.save_chain();
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
    fn build_valid_chain(n: u32, filename: &str) -> Vec<Block> {
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        for _ in 0..n {
            bc.add_block(vec![]);
        }
        bc.chain.into_iter().skip(1).collect()
    }

    // ── Parallel Sync Tests ──────────────────────────────────────────────

    #[test]
    fn test_out_of_order_blocks_get_buffered_then_linked() {
        let filename = "test_chain_ooo.json";
        let _ = std::fs::remove_file(filename);
        let blocks = build_valid_chain(5, "test_chain_helper_ooo.json");
        let mut bc = Blockchain::new(filename);

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
        
        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file("test_chain_helper_ooo.json");
    }

    #[test]
    fn test_in_order_batch_adds_immediately() {
        let filename = "test_chain_batch.json";
        let _ = std::fs::remove_file(filename);
        let blocks = build_valid_chain(5, "test_chain_helper_batch.json");
        let mut bc = Blockchain::new(filename);

        let (added, _) = bc.try_add_block_batch(blocks);
        // Should add all 5 immediately
        assert_eq!(added, 5);
        assert_eq!(bc.chain.len(), 6);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());

        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file("test_chain_helper_batch.json");
    }

    #[test]
    fn test_parallel_download_two_peers() {
        let filename = "test_chain_parallel.json";
        let _ = std::fs::remove_file(filename);
        let blocks = build_valid_chain(6, "test_chain_helper_parallel.json");
        let mut bc = Blockchain::new(filename);

        let batch_b = blocks[3..6].to_vec(); // 3,4,5 (future -> buffered)
        let (added_b, _) = bc.try_add_block_batch(batch_b);
        assert_eq!(added_b, 0); // 0 added to chain, 3 buffered
        assert_eq!(bc.pending_blocks.len(), 3);

        let b0 = blocks[0].clone();
        let b1 = blocks[1].clone();
        let b2 = blocks[2].clone();
        // Now process a batch from Peer 1 (suppose they send 0,1,2)
        let peer1_batch = vec![b0.clone(), b1.clone(), b2.clone()];
        let (added, _) = bc.try_add_block_batch(peer1_batch);
        assert_eq!(added, 3);
        assert_eq!(bc.chain.len(), 7);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());

        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file("test_chain_helper_parallel.json");
    }

    #[test]
    fn test_duplicate_blocks_rejected() {
        let filename = "test_chain_dup.json";
        let _ = std::fs::remove_file(filename);
        let blocks = build_valid_chain(3, "test_chain_helper_dup.json");
        let mut bc = Blockchain::new(filename);

        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Exists);
        assert_eq!(bc.chain.len(), 2);

        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file("test_chain_helper_dup.json");
    }

    #[test]
    fn test_three_peer_parallel_interleaved() {
        let filename = "test_chain_interleaved.json";
        let _ = std::fs::remove_file(filename);
        let blocks = build_valid_chain(9, "test_chain_helper_interleaved.json");
        let mut bc = Blockchain::new(filename);

        bc.try_add_block_batch(blocks[6..9].to_vec());
        assert_eq!(bc.chain.len(), 1);

        bc.try_add_block_batch(blocks[3..6].to_vec());
        assert_eq!(bc.chain.len(), 1);

        let (added, _) = bc.try_add_block_batch(blocks[0..3].to_vec());
        assert_eq!(added, 3);
        assert_eq!(bc.chain.len(), 10);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());

        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file("test_chain_helper_interleaved.json");
    }

    // ── UTXO Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_coinbase_creates_utxo() {
        let filename = "test_chain_utxo.json";
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let miner = Wallet::new();

        let coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        assert_eq!(bc.get_balance(&miner.get_public_key()), 50.0);
        assert_eq!(bc.utxo_set.len(), 1);

        let _ = std::fs::remove_file(filename);
    }

    #[test]
    fn test_spend_and_change() {
        let filename = "test_chain_spend.json";
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
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

        let _ = std::fs::remove_file(filename);
    }

    #[test]
    fn test_double_spend_rejected() {
        let filename = "test_chain_ds.json";
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
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

        let _ = std::fs::remove_file(filename);
    }

    #[test]
    fn test_insufficient_balance_rejected() {
        let filename = "test_chain_balance.json";
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();

        // Alice has 0 balance — no coinbase
        let (inputs, total) = bc.find_spendable_outputs(&alice.get_public_key(), 100.0);
        assert_eq!(total, 0.0);
        assert!(inputs.is_empty(), "Should find no UTXOs");

        let _ = std::fs::remove_file(filename);
    }
    #[test]
    fn test_mempool_double_spend_rejected() {
        let filename = "test_chain_mempool_ds.json";
        let _ = std::fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();
        let charlie = Wallet::new();

        // 1. Mine coins for Alice
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        // 2. Create TX1: Alice -> Bob
        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        let tx1 = Transaction::new(inputs.clone(), &bob.get_public_key(), 50.0, &alice);
        
        // 3. Create TX2: Alice -> Charlie (USING SAME INPUTS)
        let tx2 = Transaction::new(inputs, &charlie.get_public_key(), 50.0, &alice);

        // 4. Add to mempool
        assert!(bc.add_to_mempool(tx1), "First TX should be accepted");
        
        // 5. THIS SHOULD FAIL (currently passes = valid bug)
        // If it returns TRUE, then assert!(!...) fails, meaning "Double spend NOT rejected".
        // If it returns FALSE, then assert!(!...) passes, meaning "Double spend rejected".
        assert!(!bc.add_to_mempool(tx2), "Second TX (double spend) should be REJECTED");

        let _ = std::fs::remove_file(filename);
    }

    #[test]
    fn test_chain_reorg() {
        let filename = "test_chain_reorg.json";
        let _ = std::fs::remove_file(filename);

        // 1. Common chain (Genesis + 2 blocks)
        let common = build_valid_chain(2, "common.json");
        
        // 2. Chain A: Extend common by 1 block
        let mut chain_a = Blockchain::new(filename);
        // Add common blocks
        for b in &common { chain_a.try_add_block(b.clone()); }
        // Mine block 3A
        chain_a.add_block(vec![]); 
        let _block_3a = chain_a.chain.last().unwrap().clone();

        // 3. Chain B: Extend common by 3 blocks (longer)
        let filename_b = "test_chain_reorg_b.json";
        let _ = std::fs::remove_file(filename_b);
        let mut chain_b = Blockchain::new(filename_b);
        for b in &common { chain_b.try_add_block(b.clone()); }
        
        // Mine block 3B (make it different so hash differs)
        let alice = Wallet::new();
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 3);
        chain_b.add_block(vec![coinbase]); 
        // Mine 4B, 5B
        chain_b.add_block(vec![]);
        chain_b.add_block(vec![]);
        
        // Get the new blocks from B (excluding common 0,1,2)
        // Chain B: 0, 1, 2, 3, 4, 5
        let new_blocks_b: Vec<Block> = chain_b.chain.iter().skip(3).cloned().collect(); 
        
        // 4. Try to add Chain B blocks to Chain A
        // `try_add_block_batch` should trigger reorg because 3,4,5 is longer than 3A
        let (result, _) = chain_a.try_add_block_batch(new_blocks_b);
        
        // Verified: It should switch to Chain B
        assert_eq!(result, 3); // 3 blocks added (reorg success)
        assert_eq!(chain_a.chain.len(), 6); // 0,1,2,3,4,5
        assert_eq!(chain_a.chain.last().unwrap().hash, chain_b.chain.last().unwrap().hash);
        
        // Cleanup
        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file(filename_b);
        let _ = std::fs::remove_file("common.json");
    }
}
