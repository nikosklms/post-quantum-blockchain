#[cfg(test)]
mod sync_tests {
    use crate::network::{SyncManager, MAX_IN_FLIGHT_PER_PEER};
    use libp2p::PeerId;
    use std::collections::HashMap;

    /// Helper: create N distinct fake PeerIds
    fn fake_peers(n: usize) -> Vec<PeerId> {
        (0..n).map(|_| PeerId::random()).collect()
    }

    #[test]
    fn test_single_peer_gets_limited_work() {
        // With 1 peer and 1000 blocks, only MAX_IN_FLIGHT_PER_PEER chunks assigned per tick
        let mut sm = SyncManager::new();
        let peers = fake_peers(1);

        sm.record_peer_height(peers[0], 1000, 0);
        let assignments = sm.plan_assignments(0);

        // Should only get MAX_IN_FLIGHT_PER_PEER (2) assignments, not all 20 chunks
        assert_eq!(assignments.len(), MAX_IN_FLIGHT_PER_PEER,
            "Single peer should only get {} requests per tick", MAX_IN_FLIGHT_PER_PEER);

        // Work queue should still have remaining chunks
        assert!(sm.work_queue_len() > 0,
            "Remaining work should stay in queue for next tick");
    }

    #[test]
    fn test_work_distributed_across_three_peers() {
        // With 3 peers and 1000 blocks, work should spread across all peers
        let mut sm = SyncManager::new();
        let peers = fake_peers(3);

        for &p in &peers {
            sm.record_peer_height(p, 1000, 0);
        }

        let assignments = sm.plan_assignments(0);

        // Each peer should get MAX_IN_FLIGHT_PER_PEER assignments
        // Total: 3 peers × 2 = 6 assignments per tick
        assert_eq!(assignments.len(), 3 * MAX_IN_FLIGHT_PER_PEER);

        // Verify EACH peer got work (not all to one peer)
        for &p in &peers {
            let peer_assignments: Vec<_> = assignments.iter().filter(|(pid, _, _)| *pid == p).collect();
            assert_eq!(peer_assignments.len(), MAX_IN_FLIGHT_PER_PEER,
                "Each peer should get exactly {} chunks", MAX_IN_FLIGHT_PER_PEER);
        }
    }

    #[test]
    fn test_late_peer_gets_work_on_next_tick() {
        // Tick 1: only Peer A known → gets 2 chunks
        // Peer B joins
        // Tick 2: both A and B get chunks → B was not left out
        let mut sm = SyncManager::new();
        let peers = fake_peers(2);

        // Only Peer A at first
        sm.record_peer_height(peers[0], 1000, 0);
        let tick1 = sm.plan_assignments(0);
        assert_eq!(tick1.len(), MAX_IN_FLIGHT_PER_PEER);
        assert!(tick1.iter().all(|(p, _, _)| *p == peers[0]), "Tick 1: only Peer A");

        // Peer B joins late
        sm.record_peer_height(peers[1], 1000, 0);

        // Simulate Peer A completing its requests
        for (_, s, e) in &tick1 {
            sm.on_response(&peers[0], *s, *e);
        }

        let tick2 = sm.plan_assignments(0);

        // Both peers should get work now
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

        // Tick: both get work
        let _tick1 = sm.plan_assignments(0);
        let initial_queue = sm.work_queue_len();

        // Peer 0 disconnects
        let a_in_flight = sm.in_flight_count(&peers[0]);
        assert!(a_in_flight > 0, "Peer A should have in-flight requests");

        sm.remove_peer(&peers[0]);

        // Peer A's work should be re-queued
        let queue_after = sm.work_queue_len();
        assert_eq!(queue_after, initial_queue + a_in_flight,
            "Disconnected peer's in-flight work should be re-queued");

        // Next tick: only Peer B gets work (including re-queued chunks)
        let tick2 = sm.plan_assignments(0);
        assert!(tick2.iter().all(|(p, _, _)| *p == peers[1]),
            "After disconnect, only Peer B should receive work");
    }

    #[test]
    fn test_full_sync_1000_blocks_parallel_drain() {
        // Simulate a complete sync of 1000 blocks across 4 peers
        // Prove all work gets assigned and drained across multiple ticks
        let mut sm = SyncManager::new();
        let peers = fake_peers(4);

        for &p in &peers {
            sm.record_peer_height(p, 1000, 0);
        }

        let mut total_assigned: Vec<(PeerId, u32, u32)> = Vec::new();
        let mut tick_count = 0;

        // Simulate tick loop: assign, "complete" all requests, repeat
        loop {
            let assignments = sm.plan_assignments(0);
            if assignments.is_empty() && sm.is_idle() {
                break;
            }

            // Record all assignments
            for &(p, s, e) in &assignments {
                total_assigned.push((p, s, e));
                // Simulate instant completion
                sm.on_response(&p, s, e);
            }

            tick_count += 1;
            assert!(tick_count < 100, "Sync should complete in reasonable ticks");
        }

        // Verify: all 1000 blocks were covered
        let mut covered: Vec<bool> = vec![false; 1001]; // index 0 unused
        for (_, s, e) in &total_assigned {
            for h in *s..=*e {
                assert!(!covered[h as usize], "Block {} assigned twice!", h);
                covered[h as usize] = true;
            }
        }
        for h in 1..=1000 {
            assert!(covered[h], "Block {} was never assigned!", h);
        }

        // Verify: work was distributed, not all to one peer
        let mut per_peer_blocks: HashMap<PeerId, u32> = HashMap::new();
        for (p, s, e) in &total_assigned {
            *per_peer_blocks.entry(*p).or_insert(0) += e - s + 1;
        }

        println!("\n📊 Sync distribution across {} ticks:", tick_count);
        for (p, count) in &per_peer_blocks {
            println!("  Peer {}: {} blocks", p, count);
        }
        assert_eq!(per_peer_blocks.len(), 4, "All 4 peers should have participated");

        // Each peer should have a fair share — not all work on one peer
        // With 4 peers, no peer should have more than 40% of total work
        // (In the OLD design, 1 peer would have 100%)
        let total: u32 = per_peer_blocks.values().sum();
        assert_eq!(total, 1000, "All 1000 blocks should be assigned");
        for (&p, &count) in &per_peer_blocks {
            let share = (count as f64 / total as f64) * 100.0;
            println!("  Peer {} share: {:.1}%", p, share);
            assert!(count >= 100, "Each peer should have at least 100 blocks (got {})", count);
            assert!(count <= 400, "No peer should have more than 400 blocks (got {})", count);
        }
    }
}
