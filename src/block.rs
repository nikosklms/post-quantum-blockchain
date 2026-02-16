use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transaction::Transaction;

pub const MAX_BLOCK_SIZE: usize = 1_000_000; // 1 MB max block size (serialized bytes)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub index: u32,
    pub timestamp: u128,
    pub nonce: u64,
    pub previous_hash: String,
    pub transactions: Vec<Transaction>,
    pub merkle_root: String,
    pub hash: String,
    pub difficulty: usize,
}

impl Block {
    pub fn new(index: u32, transactions: Vec<Transaction>, previous_hash: String, 
            difficulty: usize) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let merkle_root = Self::calculate_merkle_root(&transactions);
        
        let mut block = Block {
            index,
            timestamp,
            nonce: 0,
            previous_hash,
            transactions,
            merkle_root,
            hash: String::new(),
            difficulty,
        };

        // Initialize hash (it's invalid until mined)
        block.hash = block.calculate_hash();
        block
    }

    pub fn genesis() -> Self {
        let mut block = Block {
            index: 0,
            timestamp: 0,
            nonce: 0,
            previous_hash: String::from("0"),
            transactions: vec![],
            merkle_root: String::new(),
            hash: String::new(),
            difficulty: 3,
        };
        block.merkle_root = Self::calculate_merkle_root(&block.transactions);
        // Create a dummy "false" signal for genesis (never cancel)
        let stop_signal = std::sync::atomic::AtomicBool::new(false);
        block.mine_block(&stop_signal);
        block
    }

    pub fn calculate_hash(&self) -> String {
        // Now valid: Hash(Header Only)
        // Header = index + timestamp + prev_hash + merkle_root + nonce + difficulty
        let input = format!(
            "{}{}{}{}{}{}",
            self.index, self.timestamp, self.previous_hash, self.merkle_root, self.nonce, self.difficulty
        );

        let mut hasher = Sha256::new();
        hasher.update(input);
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    pub fn calculate_merkle_root(transactions: &[Transaction]) -> String {
        if transactions.is_empty() {
            return String::from("0000000000000000000000000000000000000000000000000000000000000000"); // Empty root
        }

        let mut hashes: Vec<String> = transactions.iter()
            .map(|tx| {
                let mut hasher = Sha256::new();
                hasher.update(serde_json::to_string(tx).unwrap());
                format!("{:x}", hasher.finalize())
            })
            .collect();

        while hashes.len() > 1 {
            let mut new_level = Vec::new();
            
            for i in (0..hashes.len()).step_by(2) {
                let left = &hashes[i];
                // Duplicate last element if odd number
                let right = if i + 1 < hashes.len() {
                    &hashes[i + 1]
                } else {
                    &hashes[i]
                };

                let mut hasher = Sha256::new();
                hasher.update(left);
                hasher.update(right);
                new_level.push(format!("{:x}", hasher.finalize()));
            }
            hashes = new_level;
        }

        hashes[0].clone()
    }

    pub fn mine_block(&mut self, stop_signal: &std::sync::atomic::AtomicBool) {
        let target = "0".repeat(self.difficulty);
        use std::sync::atomic::Ordering;

        while !self.hash.starts_with(&target) {
            // Check if we should stop
            if stop_signal.load(Ordering::Relaxed) {
                println!("🛑 Mining cancelled!");
                return;
            }

            self.nonce += 1;
            self.hash = self.calculate_hash();
        }
        println!("⛏️  Block Mined: {}", self.hash);
    }
}
