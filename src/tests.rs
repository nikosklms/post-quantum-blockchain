#[cfg(test)]
mod tests {
    use crate::blockchain::{Blockchain, AddBlockResult};
    use crate::block::Block;
    use crate::transaction::{Transaction, TxOutput, block_reward};
    use crate::wallet::Wallet;
    use crate::network::{SyncManager, MAX_IN_FLIGHT_PER_PEER};
    use libp2p::PeerId;
    use std::collections::HashMap;
    use std::fs;

    // ═══════════════════════════════════════════════════════════════════════
    //  Helpers
    // ═══════════════════════════════════════════════════════════════════════

    /// Build a valid chain of `n` blocks on top of genesis.
    fn build_valid_chain(n: u32, filename: &str) -> Vec<Block> {
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        for _ in 0..n {
            bc.add_block(vec![]);
        }
        bc.chain.into_iter().skip(1).collect()
    }

    /// Create N distinct fake PeerIds
    fn fake_peers(n: usize) -> Vec<PeerId> {
        (0..n).map(|_| PeerId::random()).collect()
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Transaction Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_coinbase_transaction() {
        let wallet = Wallet::new();
        let coinbase = Transaction::new_coinbase(&wallet.get_public_key(), 0);

        assert!(coinbase.is_coinbase());
        assert_eq!(coinbase.outputs.len(), 1);
        assert_eq!(coinbase.outputs[0].amount, 50.0);
        assert!(coinbase.verify_signatures());
    }

    #[test]
    fn test_block_reward_halving() {
        assert_eq!(block_reward(0), 50.0);
        assert_eq!(block_reward(999), 50.0);
        assert_eq!(block_reward(1000), 25.0);
        assert_eq!(block_reward(2000), 12.5);
        assert_eq!(block_reward(3000), 6.25);
    }

    #[test]
    fn test_regular_transaction_with_change() {
        let alice = Wallet::new();
        let bob = Wallet::new();

        // Simulate Alice having a 50-coin UTXO with txid "aaa"
        let utxo_inputs = vec![("aaa".to_string(), 0, 50.0)];
        let tx = Transaction::new(utxo_inputs, &bob.get_public_key(), 30.0, &alice);

        // Should have 2 outputs: 30 to Bob, 20 change to Alice
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(tx.outputs[0].amount, 30.0);
        assert_eq!(tx.outputs[0].recipient, bob.get_public_key());
        assert_eq!(tx.outputs[1].amount, 20.0);
        assert_eq!(tx.outputs[1].recipient, alice.get_public_key());

        // Signatures should verify
        assert!(tx.verify_signatures());
    }

    #[test]
    fn test_tampered_transaction_fails() {
        let alice = Wallet::new();
        let bob = Wallet::new();

        let utxo_inputs = vec![("aaa".to_string(), 0, 50.0)];
        let mut tx = Transaction::new(utxo_inputs, &bob.get_public_key(), 30.0, &alice);

        // Tamper: change the amount after signing
        tx.outputs[0].amount = 1000.0;

        // Re-compute the id (attacker would need to)
        tx.id = tx.calculate_hash();

        // Signatures should now fail (signed with original id)
        assert!(!tx.verify_signatures());
    }

    #[test]
    fn test_forged_signature_fails() {
        let alice = Wallet::new();
        let eve = Wallet::new();
        let bob = Wallet::new();

        // Eve tries to spend Alice's UTXO
        let utxo_inputs = vec![("aaa".to_string(), 0, 50.0)];
        let mut tx = Transaction::new(utxo_inputs, &bob.get_public_key(), 50.0, &eve);

        // Eve signs with her key, but claims Alice's pubkey
        tx.inputs[0].pub_key = alice.get_public_key();

        // Should fail: eve's signature doesn't match alice's pubkey
        assert!(!tx.verify_signatures());
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Exploit / Security Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_negative_amount_exploit() {
        let filename = "test_exploit_neg.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();

        // Fund Alice
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        // Alice tries to print money: Input 50 -> Output 100 to Bob, -50 to Alice
        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        let mut tx = Transaction::new(inputs, &bob.get_public_key(), 100.0, &alice);
        
        // Manual tamper to add negative output
        tx.outputs.push(TxOutput {
            amount: -50.0,
            recipient: alice.get_public_key(),
        });
        
        // Re-sign because we tampered
        tx.id = tx.calculate_hash();
        for i in 0..tx.inputs.len() {
            tx.inputs[i].signature = alice.sign(tx.id.as_bytes());
        }

        let accepted = bc.add_to_mempool(tx);
        assert!(!accepted, "VULNERABILITY: Negative amount transaction was accepted!");
        
        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_nan_exploit() {
        let filename = "test_exploit_nan.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();

        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        
        // Create TX with NaN amount
        let mut tx = Transaction::new(inputs, &bob.get_public_key(), 50.0, &alice);
        tx.outputs[0].amount = f64::NAN;
        
        tx.id = tx.calculate_hash();
        for i in 0..tx.inputs.len() {
            tx.inputs[i].signature = alice.sign(tx.id.as_bytes());
        }

        let accepted = bc.add_to_mempool(tx);
        assert!(!accepted, "VULNERABILITY: NaN transaction was accepted!");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Blockchain / Block Sync Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_out_of_order_blocks_get_buffered_then_linked() {
        let filename = "test_chain_ooo.json";
        let _ = fs::remove_file(filename);
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
        
        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_chain_helper_ooo.json");
    }

    #[test]
    fn test_in_order_batch_adds_immediately() {
        let filename = "test_chain_batch.json";
        let _ = fs::remove_file(filename);
        let blocks = build_valid_chain(5, "test_chain_helper_batch.json");
        let mut bc = Blockchain::new(filename);

        let (added, _) = bc.try_add_block_batch(blocks);
        assert_eq!(added, 5);
        assert_eq!(bc.chain.len(), 6);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());

        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_chain_helper_batch.json");
    }

    #[test]
    fn test_parallel_download_two_peers() {
        let filename = "test_chain_parallel.json";
        let _ = fs::remove_file(filename);
        let blocks = build_valid_chain(6, "test_chain_helper_parallel.json");
        let mut bc = Blockchain::new(filename);

        let batch_b = blocks[3..6].to_vec();
        let (added_b, _) = bc.try_add_block_batch(batch_b);
        assert_eq!(added_b, 0);
        assert_eq!(bc.pending_blocks.len(), 3);

        let peer1_batch = vec![blocks[0].clone(), blocks[1].clone(), blocks[2].clone()];
        let (added, _) = bc.try_add_block_batch(peer1_batch);
        assert_eq!(added, 3);
        assert_eq!(bc.chain.len(), 7);
        assert_eq!(bc.pending_blocks.len(), 0);
        assert!(bc.validate_chain());

        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_chain_helper_parallel.json");
    }

    #[test]
    fn test_duplicate_blocks_rejected() {
        let filename = "test_chain_dup.json";
        let _ = fs::remove_file(filename);
        let blocks = build_valid_chain(3, "test_chain_helper_dup.json");
        let mut bc = Blockchain::new(filename);

        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Added);
        assert_eq!(bc.try_add_block(blocks[0].clone()), AddBlockResult::Exists);
        assert_eq!(bc.chain.len(), 2);

        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_chain_helper_dup.json");
    }

    #[test]
    fn test_three_peer_parallel_interleaved() {
        let filename = "test_chain_interleaved.json";
        let _ = fs::remove_file(filename);
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

        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_chain_helper_interleaved.json");
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  UTXO Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_coinbase_creates_utxo() {
        let filename = "test_chain_utxo.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let miner = Wallet::new();

        let coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        assert_eq!(bc.get_balance(&miner.get_public_key()), 50.0);
        assert_eq!(bc.utxo_set.len(), 1);

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_spend_and_change() {
        let filename = "test_chain_spend.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();

        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);
        assert_eq!(bc.get_balance(&alice.get_public_key()), 50.0);

        let (inputs, _total) = bc.find_spendable_outputs(&alice.get_public_key(), 30.0);
        let tx = Transaction::new(inputs, &bob.get_public_key(), 30.0, &alice);
        bc.add_block(vec![tx]);

        assert_eq!(bc.get_balance(&alice.get_public_key()), 20.0);
        assert_eq!(bc.get_balance(&bob.get_public_key()), 30.0);

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_double_spend_rejected() {
        let filename = "test_chain_ds.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();

        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        let tx = Transaction::new(inputs.clone(), &bob.get_public_key(), 50.0, &alice);
        bc.add_block(vec![tx]);

        let tx2 = Transaction::new(inputs, &bob.get_public_key(), 50.0, &alice);
        assert!(!bc.validate_transaction(&tx2, 3), "Double spend should be rejected");

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_insufficient_balance_rejected() {
        let filename = "test_chain_balance.json";
        let _ = fs::remove_file(filename);
        let bc = Blockchain::new(filename);
        let alice = Wallet::new();

        let (inputs, total) = bc.find_spendable_outputs(&alice.get_public_key(), 100.0);
        assert_eq!(total, 0.0);
        assert!(inputs.is_empty(), "Should find no UTXOs");

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_mempool_double_spend_rejected() {
        let filename = "test_chain_mempool_ds.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let bob = Wallet::new();
        let charlie = Wallet::new();

        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);

        let (inputs, _) = bc.find_spendable_outputs(&alice.get_public_key(), 50.0);
        let tx1 = Transaction::new(inputs.clone(), &bob.get_public_key(), 50.0, &alice);
        let tx2 = Transaction::new(inputs, &charlie.get_public_key(), 50.0, &alice);

        assert!(bc.add_to_mempool(tx1), "First TX should be accepted");
        assert!(!bc.add_to_mempool(tx2), "Second TX (double spend) should be REJECTED");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Chain Reorg Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_chain_reorg() {
        let filename = "test_chain_reorg.json";
        let _ = fs::remove_file(filename);

        let common = build_valid_chain(2, "common.json");
        
        let mut chain_a = Blockchain::new(filename);
        for b in &common { chain_a.try_add_block(b.clone()); }
        chain_a.add_block(vec![]); 

        let filename_b = "test_chain_reorg_b.json";
        let _ = fs::remove_file(filename_b);
        let mut chain_b = Blockchain::new(filename_b);
        for b in &common { chain_b.try_add_block(b.clone()); }
        
        let alice = Wallet::new();
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 3);
        chain_b.add_block(vec![coinbase]); 
        chain_b.add_block(vec![]);
        chain_b.add_block(vec![]);
        
        let new_blocks_b: Vec<Block> = chain_b.chain.iter().skip(3).cloned().collect(); 
        let (result, _) = chain_a.try_add_block_batch(new_blocks_b);
        
        assert_eq!(result, 3);
        assert_eq!(chain_a.chain.len(), 6);
        assert_eq!(chain_a.chain.last().unwrap().hash, chain_b.chain.last().unwrap().hash);
        
        let _ = fs::remove_file(filename);
        let _ = fs::remove_file(filename_b);
        let _ = fs::remove_file("common.json");
    }

    #[test]
    fn test_full_chain_divergence_reorg() {
        // Two chains from same genesis but completely different blocks from height 1.
        // Chain A: genesis + 3 blocks (short)
        // Chain B: genesis + 6 blocks (long, different)

        let filename_a = "test_diverge_a.json";
        let filename_b = "test_diverge_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        let mut chain_a = Blockchain::new(filename_a);
        for _ in 0..3 {
            chain_a.add_block(vec![]);
        }
        assert_eq!(chain_a.chain.len(), 4);

        let mut chain_b = Blockchain::new(filename_b);
        let alice = Wallet::new();
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        chain_b.add_block(vec![coinbase]);
        for _ in 0..5 {
            chain_b.add_block(vec![]);
        }
        assert_eq!(chain_b.chain.len(), 7);
        assert_ne!(chain_a.chain[1].hash, chain_b.chain[1].hash);

        // Step 1: A receives blocks above its height → NeedsReorg
        let high_blocks: Vec<Block> = chain_b.chain[4..7].to_vec();
        let (added, failure) = chain_a.try_add_block_batch(high_blocks);
        assert_eq!(added, 0);
        assert!(matches!(failure, Some(AddBlockResult::NeedsReorg)));

        // Step 2: A receives full chain from peer → successful reorg
        let full_blocks: Vec<Block> = chain_b.chain[1..7].to_vec();
        let (added, failure) = chain_a.try_add_block_batch(full_blocks);
        assert!(failure.is_none(), "Reorg should succeed, got: {:?}", failure);
        assert_eq!(added, 6);
        assert_eq!(chain_a.chain.len(), 7);
        assert_eq!(chain_a.chain.last().unwrap().hash, chain_b.chain.last().unwrap().hash);
        assert!(chain_a.validate_chain());

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
    }

    #[test]
    fn test_different_genesis_rejected() {
        // Two nodes with completely different genesis blocks (different networks).
        // The node should reject the longer chain because even genesis doesn't match.

        let filename_a = "test_diff_genesis_a.json";
        let filename_b = "test_diff_genesis_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        // Chain A: normal genesis + 2 blocks
        let mut chain_a = Blockchain::new(filename_a);
        chain_a.add_block(vec![]);
        chain_a.add_block(vec![]);
        assert_eq!(chain_a.chain.len(), 3);

        // Chain B: build a completely different chain by mining a coinbase 
        // into genesis-level (this creates a different block 1)
        let mut chain_b = Blockchain::new(filename_b);
        let alice = Wallet::new();
        for i in 0..6 {
            let coinbase = Transaction::new_coinbase(&alice.get_public_key(), i + 1);
            chain_b.add_block(vec![coinbase]);
        }
        assert_eq!(chain_b.chain.len(), 7);

        // Both chains share the same genesis (Blockchain::new creates identical genesis)
        // So find_common_ancestor WILL find genesis. This is correct behavior.
        // The reorg will succeed because genesis is shared — this is the expected design.
        //
        // If genesis were truly different (different networks), the locator-based
        // find_common_ancestor would return None, and the NeedsReorg handler would
        // keep returning NeedsReorg, effectively rejecting the foreign chain.
        //
        // We can verify this by checking that replace_chain rejects a chain
        // whose genesis doesn't match ours:
        let mut foreign_chain = chain_b.chain.clone();
        // Tamper with genesis hash to simulate a different network
        foreign_chain[0].hash = "FOREIGN_GENESIS_HASH".to_string();
        // This chain won't pass is_chain_valid because block 1's previous_hash 
        // won't match our genesis
        let replaced = chain_a.replace_chain(foreign_chain);
        assert!(!replaced, "Chain with different genesis should be rejected by is_chain_valid");

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Sync Manager Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_single_peer_gets_limited_work() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(1);

        sm.record_peer_height(peers[0], 1000, 0);
        let assignments = sm.plan_assignments(0);

        assert_eq!(assignments.len(), MAX_IN_FLIGHT_PER_PEER,
            "Single peer should only get {} requests per tick", MAX_IN_FLIGHT_PER_PEER);
        assert!(sm.work_queue_len() > 0,
            "Remaining work should stay in queue for next tick");
    }

    #[test]
    fn test_work_distributed_across_three_peers() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(3);

        for &p in &peers {
            sm.record_peer_height(p, 1000, 0);
        }

        let assignments = sm.plan_assignments(0);
        assert_eq!(assignments.len(), 3 * MAX_IN_FLIGHT_PER_PEER);

        for &p in &peers {
            let peer_assignments: Vec<_> = assignments.iter().filter(|(pid, _, _)| *pid == p).collect();
            assert_eq!(peer_assignments.len(), MAX_IN_FLIGHT_PER_PEER,
                "Each peer should get exactly {} chunks", MAX_IN_FLIGHT_PER_PEER);
        }
    }

    #[test]
    fn test_late_peer_gets_work_on_next_tick() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(2);

        sm.record_peer_height(peers[0], 1000, 0);
        let tick1 = sm.plan_assignments(0);
        assert_eq!(tick1.len(), MAX_IN_FLIGHT_PER_PEER);
        assert!(tick1.iter().all(|(p, _, _)| *p == peers[0]), "Tick 1: only Peer A");

        sm.record_peer_height(peers[1], 1000, 0);

        for (_, s, e) in &tick1 {
            sm.on_response(&peers[0], *s, *e);
        }

        let tick2 = sm.plan_assignments(0);

        let a_work: Vec<_> = tick2.iter().filter(|(p, _, _)| *p == peers[0]).collect();
        let b_work: Vec<_> = tick2.iter().filter(|(p, _, _)| *p == peers[1]).collect();
        assert!(!a_work.is_empty(), "Peer A should get more work");
        assert!(!b_work.is_empty(), "Late Peer B should also get work");
    }

    #[test]
    fn test_peer_disconnect_requeues_work() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(2);

        sm.record_peer_height(peers[0], 500, 0);
        sm.record_peer_height(peers[1], 500, 0);

        let _tick1 = sm.plan_assignments(0);
        let initial_queue = sm.work_queue_len();

        let a_in_flight = sm.in_flight_count(&peers[0]);
        assert!(a_in_flight > 0, "Peer A should have in-flight requests");

        sm.remove_peer(&peers[0]);

        let queue_after = sm.work_queue_len();
        assert_eq!(queue_after, initial_queue + a_in_flight,
            "Disconnected peer's in-flight work should be re-queued");

        let tick2 = sm.plan_assignments(0);
        assert!(tick2.iter().all(|(p, _, _)| *p == peers[1]),
            "After disconnect, only Peer B should receive work");
    }

    #[test]
    fn test_full_sync_1000_blocks_parallel_drain() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(4);

        for &p in &peers {
            sm.record_peer_height(p, 1000, 0);
        }

        let mut total_assigned: Vec<(PeerId, u32, u32)> = Vec::new();
        let mut tick_count = 0;

        loop {
            let assignments = sm.plan_assignments(0);
            if assignments.is_empty() && sm.is_idle() {
                break;
            }

            for &(p, s, e) in &assignments {
                total_assigned.push((p, s, e));
                sm.on_response(&p, s, e);
            }

            tick_count += 1;
            assert!(tick_count < 100, "Sync should complete in reasonable ticks");
        }

        let mut covered: Vec<bool> = vec![false; 1001];
        for (_, s, e) in &total_assigned {
            for h in *s..=*e {
                assert!(!covered[h as usize], "Block {} assigned twice!", h);
                covered[h as usize] = true;
            }
        }
        for h in 1..=1000 {
            assert!(covered[h], "Block {} was never assigned!", h);
        }

        let mut per_peer_blocks: HashMap<PeerId, u32> = HashMap::new();
        for (p, s, e) in &total_assigned {
            *per_peer_blocks.entry(*p).or_insert(0) += e - s + 1;
        }

        assert_eq!(per_peer_blocks.len(), 4, "All 4 peers should have participated");

        let total: u32 = per_peer_blocks.values().sum();
        assert_eq!(total, 1000, "All 1000 blocks should be assigned");
        for (&_p, &count) in &per_peer_blocks {
            assert!(count >= 100, "Each peer should have at least 100 blocks (got {})", count);
            assert!(count <= 400, "No peer should have more than 400 blocks (got {})", count);
        }
    }

    #[test]
    fn test_partial_sync_skips_known_blocks() {
        let mut sm = SyncManager::new();
        let peers = fake_peers(1);
        let peer = peers[0];

        sm.record_peer_height(peer, 105, 100);
        let assignments = sm.plan_assignments(100);

        assert_eq!(assignments.len(), 1, "Should generate exactly 1 request chunk");
        let (p, start, end) = assignments[0];
        assert_eq!(p, peer);
        assert_eq!(start, 101, "Should start requesting from 101 (my_height + 1)");
        assert_eq!(end, 105, "Should end at peer height");
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Difficulty Adjustment Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_initial_difficulty_before_first_adjustment() {
        let filename = "test_diff_initial.json";
        let _ = fs::remove_file(filename);
        let bc = Blockchain::new(filename);

        // Before DIFFICULTY_ADJUSTMENT_INTERVAL (10) blocks, should return initial target
        let initial = "0000ffff00000000000000000000000000000000000000000000000000000000";
        assert_eq!(bc.get_difficulty(), initial);

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_difficulty_stays_same_between_adjustments() {
        // Between adjustment intervals, difficulty should stay the same as the last block's
        let filename = "test_diff_between.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);

        // Mine 5 blocks (below the 10-block adjustment interval)
        for _ in 0..5 {
            bc.add_block(vec![]);
        }
        assert_eq!(bc.chain.len(), 6); // genesis + 5

        // Difficulty should still be the initial target since we haven't reached 10 blocks
        let initial = "0000ffff00000000000000000000000000000000000000000000000000000000";
        assert_eq!(bc.get_difficulty(), initial);

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_difficulty_adjusts_at_interval() {
        // Retargeting happens when chain.len() % 10 == 0.
        // The first window (blocks 0-9) may have a time gap between the fixed genesis
        // timestamp and block 1 (mined now), capping at max difficulty.
        // The second window (blocks 10-19) has all blocks mined milliseconds apart,
        // so actual << expected → damping caps at 4x → new target = old / 4.
        let filename = "test_diff_adjust.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);

        for _ in 0..20 {
            bc.add_block(vec![]);
        }
        assert_eq!(bc.chain.len(), 21); // genesis + 20

        // Block 20 should have the retargeted difficulty from the second window
        let block_20_diff = &bc.chain[20].difficulty;
        let initial = "0000ffff00000000000000000000000000000000000000000000000000000000";
        
        // Block 20 should have a harder (smaller) difficulty target
        use num_bigint::BigUint;
        use num_traits::Num;
        let diff_val = BigUint::from_str_radix(block_20_diff, 16).unwrap();
        let initial_val = BigUint::from_str_radix(initial, 16).unwrap();
        assert!(diff_val < initial_val, 
            "Block 20 difficulty {} should be harder (smaller) than initial {}", block_20_diff, initial);
        assert_eq!(block_20_diff.len(), 64, "Difficulty should be 64 hex chars");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Block PoW Validation Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_mined_block_passes_pow_check() {
        let filename = "test_pow_valid.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        bc.add_block(vec![]);
        
        let block = &bc.chain[1]; // first mined block
        let expected_diff = "0000ffff00000000000000000000000000000000000000000000000000000000";
        assert!(block.is_valid(expected_diff), "Mined block should pass PoW check");

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_wrong_difficulty_fails_pow_check() {
        let filename = "test_pow_wrong_diff.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        bc.add_block(vec![]);
        
        let block = &bc.chain[1];
        // Pass a different difficulty than what's in the block
        let wrong_diff = "0000000f00000000000000000000000000000000000000000000000000000000";
        assert!(!block.is_valid(wrong_diff), "Block with wrong difficulty should fail");

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_tampered_block_fails_pow_check() {
        let filename = "test_pow_tampered.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        bc.add_block(vec![]);
        
        let mut block = bc.chain[1].clone();
        let expected_diff = block.difficulty.clone();
        
        // Tamper with the nonce — hash will no longer satisfy PoW
        block.nonce += 1;
        block.hash = block.calculate_hash();
        
        // The recalculated hash is very unlikely to still be under the target
        // (astronomically unlikely, so this is effectively deterministic)
        assert!(!block.is_valid(&expected_diff), "Tampered block should fail PoW check");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Persistence Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_chain_save_and_load_roundtrip() {
        let filename = "test_persist_chain.json";
        let _ = fs::remove_file(filename);

        // Build a chain with transactions
        let mut bc = Blockchain::new(filename);
        let alice = Wallet::new();
        let coinbase = Transaction::new_coinbase(&alice.get_public_key(), 1);
        bc.add_block(vec![coinbase]);
        bc.add_block(vec![]);
        
        let original_len = bc.chain.len();
        let original_tip_hash = bc.chain.last().unwrap().hash.clone();

        // Save is automatic, but let's force it
        bc.save_chain();

        // Load from file
        let loaded = Blockchain::load_chain(filename).expect("Should load chain");
        assert_eq!(loaded.chain.len(), original_len);
        assert_eq!(loaded.chain.last().unwrap().hash, original_tip_hash);
        assert!(loaded.validate_chain());
        
        // UTXO set should be rebuilt from the loaded chain
        assert_eq!(loaded.get_balance(&alice.get_public_key()), bc.get_balance(&alice.get_public_key()));

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_wallet_save_and_load_roundtrip() {
        let wallet_path = "test_wallet_persist.bin";
        let _ = fs::remove_file(wallet_path);

        let wallet = Wallet::new();
        let original_pubkey = wallet.get_public_key();
        
        // Sign a test message
        let msg = b"test message";
        let signature = wallet.sign(msg);

        // Save
        wallet.save_to_file(wallet_path).expect("Should save wallet");

        // Load
        let loaded = Wallet::load_from_file(wallet_path).expect("Should load wallet");
        assert_eq!(loaded.get_public_key(), original_pubkey, "Public key should survive round-trip");
        
        // Loaded wallet should produce valid signatures
        let new_sig = loaded.sign(b"another message");
        assert!(Wallet::verify(b"another message", &new_sig, &original_pubkey), 
            "Loaded wallet's signature should be valid");

        // Original signature should still verify
        assert!(Wallet::verify(msg, &signature, &original_pubkey));

        let _ = fs::remove_file(wallet_path);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Block Size Limit Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_normal_block_within_size_limit() {
        let filename = "test_blocksize_ok.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        let miner = Wallet::new();
        
        let coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        bc.add_block(vec![coinbase]);
        
        // Verify the block was actually added (not rejected for size)
        assert_eq!(bc.chain.len(), 2);
        
        // Verify block is under size limit
        let block_bytes = serde_json::to_vec(&bc.chain[1]).unwrap();
        assert!(block_bytes.len() < crate::block::MAX_BLOCK_SIZE, 
            "Normal block should be well under 4MB limit");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Mempool Capacity Tests
    // ═══════════════════════════════════════════════════════════════════════

    // #[test]
    // fn test_mempool_capacity_limit() {
    //     let filename = "test_mempool_cap.json";
    //     let _ = fs::remove_file(filename);
    //     let mut bc = Blockchain::new(filename);
    //     let alice = Wallet::new();

    //     // Fund Alice with many UTXOs by mining many blocks
    //     for i in 1..=50 {
    //         let coinbase = Transaction::new_coinbase(&alice.get_public_key(), i);
    //         bc.add_block(vec![coinbase]);
    //     }

    //     // Manually fill the mempool to capacity (5000)
    //     // We'll create minimal fake coinbase-like transactions 
    //     // (they pass mempool validation since they're coinbase)
    //     for i in 0..5000 {
    //         let tx = Transaction::new_coinbase(&format!("addr_{}", i), 999);
    //         bc.mempool.push(tx);
    //     }
    //     assert_eq!(bc.mempool.len(), 5000);

    //     // The 5001th transaction should be rejected
    //     let extra_tx = Transaction::new_coinbase(&alice.get_public_key(), 999);
    //     let accepted = bc.add_to_mempool(extra_tx);
    //     assert!(!accepted, "Transaction should be rejected when mempool is full (5000 limit)");
    //     assert_eq!(bc.mempool.len(), 5000, "Mempool count should not increase");

    //     let _ = fs::remove_file(filename);
    // }

    // ═══════════════════════════════════════════════════════════════════════
    //  replace_chain Edge Case Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_replace_chain_rejects_equal_length() {
        let filename_a = "test_replace_eq_a.json";
        let filename_b = "test_replace_eq_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        let mut chain_a = Blockchain::new(filename_a);
        chain_a.add_block(vec![]);
        chain_a.add_block(vec![]);

        let mut chain_b = Blockchain::new(filename_b);
        let alice = Wallet::new();
        let cb = Transaction::new_coinbase(&alice.get_public_key(), 1);
        chain_b.add_block(vec![cb]);
        chain_b.add_block(vec![]);

        // Both have 3 blocks (genesis + 2). replace_chain should reject equal length.
        assert_eq!(chain_a.chain.len(), chain_b.chain.len());
        let replaced = chain_a.replace_chain(chain_b.chain.clone());
        assert!(!replaced, "replace_chain should reject chain of equal length");

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
    }

    #[test]
    fn test_replace_chain_rejects_shorter() {
        let filename_a = "test_replace_short_a.json";
        let filename_b = "test_replace_short_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        let mut chain_a = Blockchain::new(filename_a);
        for _ in 0..5 { chain_a.add_block(vec![]); }

        let mut chain_b = Blockchain::new(filename_b);
        chain_b.add_block(vec![]);

        // A has 6 blocks, B has 2. Should reject.
        let replaced = chain_a.replace_chain(chain_b.chain.clone());
        assert!(!replaced, "replace_chain should reject shorter chain");

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
    }

    #[test]
    fn test_replace_chain_rejects_invalid_chain() {
        let filename = "test_replace_invalid.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);
        bc.add_block(vec![]);

        // Create a longer but invalid chain
        let mut fake_chain = bc.chain.clone();
        // Add fake blocks that don't have valid hashes
        for i in 2..5 {
            let mut block = Block::new(
                i, vec![], fake_chain.last().unwrap().hash.clone(),
                "0000ffff00000000000000000000000000000000000000000000000000000000".to_string()
            );
            block.hash = format!("fake_hash_{}", i); // invalid PoW
            fake_chain.push(block);
        }

        let replaced = bc.replace_chain(fake_chain);
        assert!(!replaced, "replace_chain should reject chain with invalid blocks");
        assert_eq!(bc.chain.len(), 2, "Original chain should be untouched");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Multi-output Transaction (sendmany) Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_sendmany_multiple_recipients() {
        let alice = Wallet::new();
        let bob = Wallet::new();
        let charlie = Wallet::new();

        let bob_pk = bob.get_public_key();
        let charlie_pk = charlie.get_public_key();

        let utxo_inputs = vec![("aaa".to_string(), 0, 100.0)];
        let recipients = vec![
            (bob_pk.as_str(), 30.0),
            (charlie_pk.as_str(), 40.0),
        ];

        let tx = Transaction::new_multi(utxo_inputs, recipients, &alice);

        // Should have 3 outputs: 30 to Bob, 40 to Charlie, 30 change to Alice
        assert_eq!(tx.outputs.len(), 3);
        assert_eq!(tx.outputs[0].amount, 30.0);
        assert_eq!(tx.outputs[0].recipient, bob_pk);
        assert_eq!(tx.outputs[1].amount, 40.0);
        assert_eq!(tx.outputs[1].recipient, charlie_pk);
        assert_eq!(tx.outputs[2].amount, 30.0);
        assert_eq!(tx.outputs[2].recipient, alice.get_public_key());

        assert!(tx.verify_signatures());
    }

    #[test]
    fn test_sendmany_exact_amount_no_change() {
        let alice = Wallet::new();
        let bob = Wallet::new();
        let charlie = Wallet::new();

        let bob_pk = bob.get_public_key();
        let charlie_pk = charlie.get_public_key();

        let utxo_inputs = vec![("aaa".to_string(), 0, 50.0)];
        let recipients = vec![
            (bob_pk.as_str(), 30.0),
            (charlie_pk.as_str(), 20.0),
        ];

        let tx = Transaction::new_multi(utxo_inputs, recipients, &alice);

        // Exact amount: 30 + 20 = 50, no change (within 0.0001 tolerance)
        assert_eq!(tx.outputs.len(), 2);
        assert!(tx.verify_signatures());
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Locator / Common Ancestor Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_create_locator_hashes() {
        let filename = "test_locators.json";
        let _ = fs::remove_file(filename);
        let mut bc = Blockchain::new(filename);

        for _ in 0..5 {
            bc.add_block(vec![]);
        }

        let locators = bc.create_locator_hashes();

        // Should contain hashes from tip backwards
        assert_eq!(locators.len(), 6); // genesis + 5 blocks
        // First locator should be the tip hash
        assert_eq!(locators[0], bc.chain.last().unwrap().hash);
        // Last locator should be genesis hash
        assert_eq!(*locators.last().unwrap(), bc.chain[0].hash);

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_find_common_ancestor_with_shared_blocks() {
        let filename_a = "test_ancestor_a.json";
        let filename_b = "test_ancestor_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        // Build two chains from same genesis + 3 common blocks, then diverge
        let common = build_valid_chain(3, "test_ancestor_common.json");

        let mut chain_a = Blockchain::new(filename_a);
        for b in &common { chain_a.try_add_block(b.clone()); }
        chain_a.add_block(vec![]); // diverge

        let mut chain_b = Blockchain::new(filename_b);
        for b in &common { chain_b.try_add_block(b.clone()); }
        let alice = Wallet::new();
        let cb = Transaction::new_coinbase(&alice.get_public_key(), 4);
        chain_b.add_block(vec![cb]); // diverge differently

        // A creates locators, B finds common ancestor
        let locators_a = chain_a.create_locator_hashes();
        let ancestor = chain_b.find_common_ancestor(&locators_a);

        // Common ancestor should be block 3 (the last shared block)
        assert_eq!(ancestor, Some(3), "Common ancestor should be block 3");

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
        let _ = fs::remove_file("test_ancestor_common.json");
    }

    #[test]
    fn test_find_common_ancestor_none_when_completely_foreign() {
        let filename = "test_ancestor_none.json";
        let _ = fs::remove_file(filename);
        let bc = Blockchain::new(filename);

        // Fake locators that don't match anything in our chain
        let fake_locators = vec![
            "aaaa".to_string(),
            "bbbb".to_string(),
            "cccc".to_string(),
        ];

        let ancestor = bc.find_common_ancestor(&fake_locators);
        assert_eq!(ancestor, None, "Should find no common ancestor with foreign locators");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  drain_pending Edge Cases
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_drain_pending_discards_invalid_buffered_block() {
        let filename = "test_drain_invalid.json";
        let _ = fs::remove_file(filename);
        let blocks = build_valid_chain(3, "test_drain_helper.json");
        let mut bc = Blockchain::new(filename);

        // Add block 0 normally
        bc.try_add_block(blocks[0].clone());
        assert_eq!(bc.chain.len(), 2);

        // Insert an invalid buffered block at index 2
        // with wrong previous_hash so drain_pending rejects it
        let mut bad_block = blocks[1].clone();
        bad_block.previous_hash = "INVALID_PARENT_HASH".to_string();
        bad_block.hash = bad_block.calculate_hash();
        bc.pending_blocks.insert(2, bad_block);

        // Trigger drain_pending directly — the invalid block should be discarded
        bc.drain_pending();

        // Chain should still be length 2 (genesis + block 0), invalid block discarded
        assert_eq!(bc.chain.len(), 2);
        assert!(bc.pending_blocks.is_empty(), "Invalid buffered block should be discarded");

        let _ = fs::remove_file(filename);
        let _ = fs::remove_file("test_drain_helper.json");
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  UTXO Rebuild After Reorg Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_utxo_set_correct_after_reorg_with_transactions() {
        // Verify that UTXO set is correctly rebuilt when reorg displaces
        // blocks containing real transactions.
        let filename_a = "test_utxo_reorg_a.json";
        let filename_b = "test_utxo_reorg_b.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);

        let alice = Wallet::new();
        let bob = Wallet::new();

        // Chain A: genesis + coinbase to Alice (block 1) + 1 empty block
        let mut chain_a = Blockchain::new(filename_a);
        let cb_a = Transaction::new_coinbase(&alice.get_public_key(), 1);
        chain_a.add_block(vec![cb_a]);
        chain_a.add_block(vec![]);
        assert_eq!(chain_a.chain.len(), 3);
        assert_eq!(chain_a.get_balance(&alice.get_public_key()), 50.0);

        // Chain B: genesis + coinbase to Bob (block 1) + 3 more blocks (longer)
        let mut chain_b = Blockchain::new(filename_b);
        let cb_b = Transaction::new_coinbase(&bob.get_public_key(), 1);
        chain_b.add_block(vec![cb_b]);
        for _ in 0..3 {
            chain_b.add_block(vec![]);
        }
        assert_eq!(chain_b.chain.len(), 5);

        // Reorg: feed chain B's blocks to chain A
        let b_blocks: Vec<Block> = chain_b.chain[1..5].to_vec();
        let (added, failure) = chain_a.try_add_block_batch(b_blocks);
        assert!(failure.is_none(), "Reorg should succeed");
        assert_eq!(added, 4);

        // After reorg, Alice should have 0 balance (her coinbase is no longer in the chain)
        // Bob should have 50.0 (his coinbase IS in the new chain)
        assert_eq!(chain_a.get_balance(&alice.get_public_key()), 0.0,
            "Alice's coinbase should be gone after reorg");
        assert_eq!(chain_a.get_balance(&bob.get_public_key()), 50.0,
            "Bob's coinbase should be present after reorg");

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Coinbase Reward Validation Tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_coinbase_overpaying_rejected() {
        let filename = "test_coinbase_overpay.json";
        let _ = fs::remove_file(filename);
        let bc = Blockchain::new(filename);
        let miner = Wallet::new();

        // Create a coinbase that pays more than block_reward
        let mut greedy_coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        greedy_coinbase.outputs[0].amount = 9999.0; // way more than 50.0
        greedy_coinbase.id = greedy_coinbase.calculate_hash();

        let valid = bc.validate_transaction(&greedy_coinbase, 1);
        assert!(!valid, "Coinbase paying more than block reward should be rejected");

        let _ = fs::remove_file(filename);
    }

    #[test]
    fn test_coinbase_at_exact_reward_accepted() {
        let filename = "test_coinbase_exact.json";
        let _ = fs::remove_file(filename);
        let bc = Blockchain::new(filename);
        let miner = Wallet::new();

        let coinbase = Transaction::new_coinbase(&miner.get_public_key(), 1);
        let valid = bc.validate_transaction(&coinbase, 1);
        assert!(valid, "Coinbase at exact block reward should be accepted");

        let _ = fs::remove_file(filename);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  Multiple Sequential Reorgs Test
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_multiple_sequential_reorgs() {
        // Chain flip-flops: A adopts B, then adopts C (even longer)
        let filename_a = "test_multi_reorg_a.json";
        let filename_b = "test_multi_reorg_b.json";
        let filename_c = "test_multi_reorg_c.json";
        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
        let _ = fs::remove_file(filename_c);

        let alice = Wallet::new();
        let bob = Wallet::new();

        // Chain A: genesis + 2 blocks
        let mut chain_a = Blockchain::new(filename_a);
        chain_a.add_block(vec![]);
        chain_a.add_block(vec![]);
        assert_eq!(chain_a.chain.len(), 3);

        // Chain B: genesis + 4 blocks (different, longer than A)
        let mut chain_b = Blockchain::new(filename_b);
        let cb_b = Transaction::new_coinbase(&alice.get_public_key(), 1);
        chain_b.add_block(vec![cb_b]);
        for _ in 0..3 { chain_b.add_block(vec![]); }
        assert_eq!(chain_b.chain.len(), 5);

        // Reorg 1: A adopts B
        let b_blocks: Vec<Block> = chain_b.chain[1..5].to_vec();
        let (_, failure) = chain_a.try_add_block_batch(b_blocks);
        assert!(failure.is_none(), "First reorg should succeed");
        assert_eq!(chain_a.chain.len(), 5);
        assert_eq!(chain_a.get_balance(&alice.get_public_key()), 50.0);

        // Chain C: genesis + 7 blocks (different, even longer)
        let mut chain_c = Blockchain::new(filename_c);
        let cb_c = Transaction::new_coinbase(&bob.get_public_key(), 1);
        chain_c.add_block(vec![cb_c]);
        for _ in 0..6 { chain_c.add_block(vec![]); }
        assert_eq!(chain_c.chain.len(), 8);

        // Reorg 2: A adopts C
        let c_blocks: Vec<Block> = chain_c.chain[1..8].to_vec();
        let (_, failure) = chain_a.try_add_block_batch(c_blocks);
        assert!(failure.is_none(), "Second reorg should succeed");
        assert_eq!(chain_a.chain.len(), 8);
        
        // After double reorg: Alice's coinbase (from chain B) is gone, Bob's (from chain C) is active
        assert_eq!(chain_a.get_balance(&alice.get_public_key()), 0.0,
            "Alice balance should be 0 after second reorg displaced chain B");
        assert_eq!(chain_a.get_balance(&bob.get_public_key()), 50.0,
            "Bob balance should be 50 from chain C coinbase");
        assert!(chain_a.validate_chain());

        let _ = fs::remove_file(filename_a);
        let _ = fs::remove_file(filename_b);
        let _ = fs::remove_file(filename_c);
    }
}
