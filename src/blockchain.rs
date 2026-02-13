use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::block::Block;
use crate::transaction::Transaction;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum AddBlockResult {
    Added,
    Buffered,
    Exists,
    Invalid,
}

pub struct Blockchain {
    pub chain: Vec<Block>,
    pub mempool: Vec<Transaction>,
    pub pending_blocks: HashMap<u32, Block>,
}

impl Blockchain {
    pub fn new() -> Self {
        let mut blockchain = Blockchain {
            chain: Vec::new(),
            mempool: Vec::new(),
            pending_blocks: HashMap::new(),
        };

        println!("Creating Genesis Block...");
        let genesis = Block::genesis();
        blockchain.chain.push(genesis);

        blockchain
    }

    pub fn add_block(&mut self, transactions: Vec<Transaction>) {
        let index = self.chain.len() as u32;
        let previous_hash = self.chain.last().unwrap().hash.clone();

        println!("Mining Block {}...", index);
        let mut block = Block::new(index, transactions.clone(), previous_hash);
        // Synchronous mining for now (dummy signal)
        let stop_signal = std::sync::atomic::AtomicBool::new(false);
        block.mine_block(&stop_signal);
        self.chain.push(block);
        
        // Remove mined transactions from mempool
        self.remove_mined_transactions(&transactions);
    }
    
    pub fn add_to_mempool(&mut self, transaction: Transaction) -> bool {
        // Verify signature
        if !transaction.verify() {
            println!("Transaction rejected: Invalid signature");
            return false;
        }
        
        // Check Mempool Capacity (DoS Protection)
        if self.mempool.len() >= 5000 {
            println!("Transaction rejected: Mempool full (max 5000)");
            return false;
        }
        
        // Check if already in mempool
        if self.mempool.iter().any(|tx| tx.calculate_hash() == transaction.calculate_hash()) {
            println!("Transaction already in mempool.");
            return false;
        }
        
        self.mempool.push(transaction);
        true
    }
    
    fn remove_mined_transactions(&mut self, mined_txs: &[Transaction]) {
        self.mempool.retain(|tx| {
            !mined_txs.iter().any(|mined| mined.calculate_hash() == tx.calculate_hash())
        });
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

            // Verify all transactions in the block
            for (t_idx, tx) in current.transactions.iter().enumerate() {
                if !tx.verify() {
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
        true
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
            self.chain.push(block.clone());
            self.remove_mined_transactions(&block.transactions);
            // Drain any buffered blocks that can now link
            self.drain_pending();
            return AddBlockResult::Added;
        }

        // Future block (gap) -> buffer it
        if block.index > tip.index + 1 {
            if !self.pending_blocks.contains_key(&block.index) {
                println!("📦 Buffering block {} (tip is {})", block.index, tip.index);
                self.pending_blocks.insert(block.index, block);
                return AddBlockResult::Buffered;
            }
            return AddBlockResult::Exists;
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
    /// Returns the blocks (excluding genesis) in order.
    fn build_valid_chain(n: u32) -> Vec<Block> {
        let mut bc = Blockchain::new(); // has genesis
        for _ in 0..n {
            bc.add_block(vec![]); // mine empty blocks
        }
        // Return blocks 1..n (skip genesis at index 0)
        bc.chain.into_iter().skip(1).collect()
    }

    #[test]
    fn test_duplicate_transaction() {
        let mut blockchain = Blockchain::new();
        let wallet = Wallet::new();
        let tx = Transaction::new(
            wallet.get_public_key(),
            "receiver".to_string(),
            10.0,
            &wallet
        );

        assert!(blockchain.add_to_mempool(tx.clone()), "First add should succeed");
        assert!(!blockchain.add_to_mempool(tx.clone()), "Duplicate should be rejected");
        assert_eq!(blockchain.mempool.len(), 1);
    }

    // ── Parallel Sync Tests ──────────────────────────────────────────────

    #[test]
    fn test_out_of_order_blocks_get_buffered_then_linked() {
        // Simulate: Peer B's response (blocks 3,4,5) arrives BEFORE Peer A's (blocks 1,2)
        let blocks = build_valid_chain(5); // blocks at index 1,2,3,4,5
        let mut bc = Blockchain::new(); // fresh node, only genesis

        // Feed blocks 3,4,5 first (out of order!) → should be buffered
        assert_eq!(bc.try_add_block(blocks[2].clone()), AddBlockResult::Buffered); // block 3
        assert_eq!(bc.try_add_block(blocks[3].clone()), AddBlockResult::Buffered); // block 4
        assert_eq!(bc.try_add_block(blocks[4].clone()), AddBlockResult::Buffered); // block 5
        assert_eq!(bc.chain.len(), 1, "Chain should still be just genesis");
        assert_eq!(bc.pending_blocks.len(), 3, "3 blocks should be buffered");

        // Now feed block 1 → added directly
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        // Block 1 added, but blocks 3-5 still can't link (missing block 2)
        assert_eq!(bc.chain.len(), 2); // genesis + block 1

        // Feed block 2 → added, AND triggers drain of 3,4,5
        assert_eq!(bc.try_add_block(blocks[1].clone()), AddBlockResult::Added);
        assert_eq!(bc.chain.len(), 6, "All 5 blocks + genesis should be linked");
        assert_eq!(bc.pending_blocks.len(), 0, "Buffer should be empty");

        // Verify chain integrity
        assert!(bc.validate_chain(), "Chain should be valid after assembly");
    }

    #[test]
    fn test_in_order_batch_adds_immediately() {
        // Simulate: blocks arrive in perfect order (single peer response)
        let blocks = build_valid_chain(5);
        let mut bc = Blockchain::new();

        let added = bc.try_add_block_batch(blocks);
        assert_eq!(added, 5);
        assert_eq!(bc.chain.len(), 6); // genesis + 5
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }

    #[test]
    fn test_parallel_download_two_peers() {
        // Simulate parallel download from 2 peers:
        //   Peer A serves blocks 1-3
        //   Peer B serves blocks 4-6
        //   Peer B's response arrives FIRST
        let blocks = build_valid_chain(6);
        let mut bc = Blockchain::new();

        // Peer B's batch arrives first (blocks 4,5,6)
        let peer_b_batch: Vec<Block> = blocks[3..6].to_vec();
        let added_b = bc.try_add_block_batch(peer_b_batch);
        assert_eq!(added_b, 0, "None should be added yet (all buffered)");
        assert_eq!(bc.pending_blocks.len(), 3);
        assert_eq!(bc.chain.len(), 1, "Still just genesis");

        // Peer A's batch arrives (blocks 1,2,3)
        let peer_a_batch: Vec<Block> = blocks[0..3].to_vec();
        let added_a = bc.try_add_block_batch(peer_a_batch);
        assert_eq!(added_a, 3, "3 blocks added directly");
        // drain_pending should have linked blocks 4,5,6 automatically
        assert_eq!(bc.chain.len(), 7, "genesis + 6 blocks");
        assert_eq!(bc.pending_blocks.len(), 0, "All buffered blocks drained");
        assert!(bc.validate_chain(), "Final chain must be valid");
    }

    #[test]
    fn test_duplicate_blocks_rejected() {
        let blocks = build_valid_chain(3);
        let mut bc = Blockchain::new();

        // Add block 1
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        // Try adding block 1 again → Exists
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Exists);
        // Chain should still be length 2
        assert_eq!(bc.chain.len(), 2);
    }

    #[test]
    fn test_three_peer_parallel_interleaved() {
        // 3 peers, blocks arrive interleaved:
        //   Peer C: blocks 7,8,9 (arrives first)
        //   Peer B: blocks 4,5,6 (arrives second) 
        //   Peer A: blocks 1,2,3 (arrives last → triggers full drain)
        let blocks = build_valid_chain(9);
        let mut bc = Blockchain::new();

        // Peer C (blocks 7-9)
        bc.try_add_block_batch(blocks[6..9].to_vec());
        assert_eq!(bc.chain.len(), 1);
        assert_eq!(bc.pending_blocks.len(), 3);

        // Peer B (blocks 4-6)
        bc.try_add_block_batch(blocks[3..6].to_vec());
        assert_eq!(bc.chain.len(), 1);
        assert_eq!(bc.pending_blocks.len(), 6);

        // Peer A (blocks 1-3) → triggers cascade
        let added = bc.try_add_block_batch(blocks[0..3].to_vec());
        assert_eq!(added, 3);
        assert_eq!(bc.chain.len(), 10, "genesis + 9 blocks fully assembled");
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());
    }
}
