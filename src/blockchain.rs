use serde::{Deserialize, Serialize};

use crate::block::Block;
use crate::transaction::Transaction;

#[derive(Debug, Serialize, Deserialize)]
pub struct Blockchain {
    pub chain: Vec<Block>,
}

impl Blockchain {
    pub fn new() -> Self {
        let mut blockchain = Blockchain { chain: Vec::new() };

        println!("Creating Genesis Block...");
        let genesis = Block::genesis();
        blockchain.chain.push(genesis);

        blockchain
    }

    pub fn add_block(&mut self, transactions: Vec<Transaction>) {
        let index = self.chain.len() as u32;
        let previous_hash = self.chain.last().unwrap().hash.clone();

        println!("Mining Block {}...", index);
        let block = Block::new(index, transactions, previous_hash);
        self.chain.push(block);
    }

    pub fn validate_chain(&self) -> bool {
        Self::is_chain_valid(&self.chain)
    }

    fn is_chain_valid(chain: &[Block]) -> bool {
        for i in 1..chain.len() {
            let current = &chain[i];
            let previous = &chain[i - 1];

            // Check that the stored hash is still valid
            if current.hash != current.calculate_hash() {
                println!("Block {} has been tampered with!", current.index);
                return false;
            }

            // Check that the chain links are intact
            if current.previous_hash != previous.hash {
                println!("Block {} is not linked to the previous block!", current.index);
                return false;
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
        true
    }

    /// Try to add a pre-mined block received from the network
    pub fn try_add_block(&mut self, block: Block) -> bool {
        let last = self.chain.last().unwrap();

        // Verify the block links to our chain
        if block.previous_hash != last.hash {
            println!("Rejected block: previous_hash doesn't match our chain tip.");
            return false;
        }

        // Verify the block's hash is valid
        if block.hash != block.calculate_hash() {
            println!("Rejected block: hash is invalid.");
            return false;
        }

        self.chain.push(block);
        true
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
