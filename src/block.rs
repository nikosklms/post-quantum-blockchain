use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transaction::Transaction;

pub const DIFFICULTY: usize = 2; /* how many 0s the beggining of the hash must have */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub index: u32,
    pub timestamp: u128,
    pub nonce: u64,
    pub previous_hash: String,
    pub transactions: Vec<Transaction>,
    pub hash: String,
}

impl Block {
    pub fn new(index: u32, transactions: Vec<Transaction>, previous_hash: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let mut block = Block {
            index,
            timestamp,
            nonce: 0,
            previous_hash,
            transactions,
            hash: String::new(),
        };

        block.mine_block();
        block
    }

    pub fn genesis() -> Self {
        let mut block = Block {
            index: 0,
            timestamp: 0,
            nonce: 0,
            previous_hash: String::from("0"),
            transactions: vec![],
            hash: String::new(),
        };
        block.mine_block();
        block
    }

    pub fn calculate_hash(&self) -> String {
        let transactions_data = serde_json::to_string(&self.transactions).unwrap();
        let input = format!(
            "{}{}{}{}{}",
            self.index, self.timestamp, self.previous_hash, transactions_data, self.nonce
        );

        let mut hasher = Sha256::new();
        hasher.update(input);
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    fn mine_block(&mut self) {
        let target = "0".repeat(DIFFICULTY);

        loop {
            self.hash = self.calculate_hash();
            if self.hash.starts_with(&target) {
                println!("⛏️  Block Mined: {}", self.hash);
                break;
            }
            self.nonce += 1;
        }
    }
}
