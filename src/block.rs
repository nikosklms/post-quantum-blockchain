use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use num_bigint::BigUint;
use num_traits::Num;

use crate::transaction::Transaction;

pub const MAX_BLOCK_SIZE: usize = 4_000_000; // 4 MB max block size (Falcon signatures are larger)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub index: u32,
    pub timestamp: u128,
    pub nonce: u64,
    pub previous_hash: String,
    pub transactions: Vec<Transaction>,
    pub merkle_root: String,
    pub hash: String,
    pub difficulty: String, // Target as Hex String
}

impl Block {
    pub fn new(index: u32, transactions: Vec<Transaction>, previous_hash: String, 
            difficulty: String) -> Self {
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
        // Initial Target: Start with 4 leading zeros (approx)
        // Max hash is 2^256. 
        // 0x0000FFFFFFFF... indicates we need ~16 bits of zeros (easy for testing)
        // A full "0000" prefix in hex is 16 bits.
        let initial_target = "0000ffff00000000000000000000000000000000000000000000000000000000".to_string();

        // Fixed genesis timestamp (Feb 10, 2026 00:00:00 UTC in milliseconds).
        // Using a fixed value ensures all nodes produce the same genesis hash.
        // Non-zero so difficulty adjustment works correctly from the first window.
        let timestamp: u128 = 1_770_681_600_000;

        let mut block = Block {
            index: 0,
            timestamp,
            nonce: 0,
            previous_hash: String::from("0"),
            transactions: vec![],
            merkle_root: String::new(),
            hash: String::new(),
            difficulty: initial_target,
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
        let target = BigUint::from_str_radix(&self.difficulty, 16)
            .expect("Invalid difficulty target");
        
        use std::sync::atomic::Ordering;

        loop {
            // Check if we should stop
            if stop_signal.load(Ordering::Relaxed) {
                println!("🛑 Mining cancelled!");
                return;
            }

            self.hash = self.calculate_hash();
            
            // Convert hash to BigUint for comparison
            // We need to parse it as hex
            if let Ok(hash_val) = BigUint::from_str_radix(&self.hash, 16) {
                if hash_val < target {
                    println!("⛏️  Block Mined: {}", self.hash);
                    return;
                }
            }
            
            self.nonce += 1;
        }
    }
    
    // Check if the block satisfies its own difficulty target
    pub fn is_valid(&self, expected_difficulty: &str) -> bool {
        // 1. Difficulty in block must match expected difficulty
        if self.difficulty != expected_difficulty {
            println!("Invalid difficulty target. Expected {}, got {}", expected_difficulty, self.difficulty);
            return false;
        }
        
        let target = BigUint::from_str_radix(&self.difficulty, 16).unwrap_or_default();
        let hash_val = BigUint::from_str_radix(&self.hash, 16).unwrap_or_default();
        
        // 2. Hash must be less than target
        if hash_val >= target {
             println!("Hash too high! {} >= {}", hash_val, target);
             return false;
        }
        
        true
    }
}
