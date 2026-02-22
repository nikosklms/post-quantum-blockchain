use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use libp2p::{
    gossipsub, mdns, noise, PeerId,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Swarm, ping,
    request_response,
};
use crate::block::Block;
use crate::blockchain::Blockchain;
use crate::transaction::Transaction;
use crate::sync::{SyncRequest, SyncResponse};

const TOPIC: &str = "blocks";
const PING_INTERVAL: Duration = Duration::from_secs(5);
const SYNC_CHUNK_SIZE: u32 = 50;           // blocks per request
pub const MAX_IN_FLIGHT_PER_PEER: usize = 2;   // concurrent requests per peer
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ─── Combined Behaviour ───────────────────────────────────────────────────────

#[derive(NetworkBehaviour)]
pub struct BlockchainBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub ping: ping::Behaviour,
    pub direct_sync: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
}

// ─── Peer Health Tracker ──────────────────────────────────────────────────────

pub struct PeerHealthTracker {
    last_pong: HashMap<PeerId, Instant>,
    timeout: Duration,
}

impl PeerHealthTracker {
    pub fn new(ping_interval: Duration, multiplier: u32) -> Self {
        Self {
            last_pong: HashMap::new(),
            timeout: ping_interval * multiplier,
        }
    }

    pub fn record_pong(&mut self, peer_id: PeerId) {
        self.last_pong.insert(peer_id, Instant::now());
    }

    pub fn add_peer(&mut self, peer_id: PeerId) {
        self.last_pong.insert(peer_id, Instant::now());
    }

    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.last_pong.remove(peer_id);
    }

    pub fn check_stale_peers(&self) -> Vec<PeerId> {
        let now = Instant::now();
        self.last_pong
            .iter()
            .filter(|(_, last_time)| now.duration_since(**last_time) > self.timeout)
            .map(|(peer_id, _)| *peer_id)
            .collect()
    }
}

// ─── Sync Manager (Work-Queue Scheduler) ──────────────────────────────────────

/// A request that is currently in-flight to a peer
struct InFlightRequest {
    start_height: u32,
    end_height: u32,
    sent_at: Instant,
}

pub struct SyncManager {
    /// Heights reported by peers via ChainParams
    pub peer_heights: HashMap<PeerId, u32>,
    /// Global work queue: chunks (start, end) we still need to fetch
    pub work_queue: VecDeque<(u32, u32)>,
    /// Per-peer in-flight requests
    in_flight: HashMap<PeerId, Vec<InFlightRequest>>,
    /// Highest height we've generated work for (avoids duplicate chunks)
    scheduled_up_to: u32,
}

impl SyncManager {
    pub fn new() -> Self {
        Self {
            peer_heights: HashMap::new(),
            work_queue: VecDeque::new(),
            in_flight: HashMap::new(),
            scheduled_up_to: 0,
        }
    }

    /// Record a peer's chain height. If new target is higher, generate new work.
    pub fn record_peer_height(&mut self, peer_id: PeerId, height: u32, my_height: u32) {
        self.peer_heights.insert(peer_id, height);
        self.ensure_work_generated(my_height);
    }

    /// Remove a peer and re-queue any of its in-flight work
    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.peer_heights.remove(peer_id);
        if let Some(requests) = self.in_flight.remove(peer_id) {
            for req in requests {
                println!("♻️  Re-queuing blocks {}-{} (peer disconnected)", req.start_height, req.end_height);
                self.work_queue.push_front((req.start_height, req.end_height));
            }
        }
    }

    /// Called when a response arrives from a peer — clears in-flight entry
    pub fn on_response(&mut self, peer: &PeerId, blocks_start: u32, blocks_end: u32) {
        if let Some(reqs) = self.in_flight.get_mut(peer) {
            reqs.retain(|r| !(r.start_height == blocks_start && r.end_height == blocks_end));
        }
    }

    /// Generate work chunks up to the max known peer height
    fn ensure_work_generated(&mut self, my_height: u32) {
        let target = self.peer_heights.values().max().copied().unwrap_or(0);
        // Start from whichever is higher: our chain tip or what we've already scheduled
        let start_from = std::cmp::max(my_height, self.scheduled_up_to) + 1;

        if target < start_from {
            return;
        }

        let mut s = start_from;
        while s <= target {
            let e = std::cmp::min(s + SYNC_CHUNK_SIZE - 1, target);
            self.work_queue.push_back((s, e));
            s = e + 1;
        }
        self.scheduled_up_to = target;
    }

    /// Periodic tick: assign work to peers with capacity, retry timed-out requests.
    /// Call this every ~1s from the main loop.
    pub fn tick(
        &mut self,
        swarm: &mut Swarm<BlockchainBehaviour>,
        my_height: u32,
    ) {
        let assignments = self.plan_assignments(my_height);
        for (peer, s, e) in assignments {
            println!("  📡 Requesting blocks {}-{} from {}", s, e, peer);
            let request = SyncRequest { start_height: s, end_height: e, locators: None };
            swarm.behaviour_mut().direct_sync.send_request(&peer, request);
        }
    }

    /// Pure scheduling logic: decides which chunks to assign to which peers.
    /// Returns Vec<(peer, start_height, end_height)> — the assignments for this tick.
    /// Separated from tick() so it can be unit-tested without a Swarm.
    pub fn plan_assignments(&mut self, my_height: u32) -> Vec<(PeerId, u32, u32)> {
        // 1. Re-generate work if our chain advanced or new peers appeared
        self.ensure_work_generated(my_height);

        // 2. Check for timed-out requests → re-queue
        let mut timed_out: Vec<(PeerId, u32, u32)> = Vec::new();
        for (&peer, reqs) in &self.in_flight {
            for req in reqs {
                if req.sent_at.elapsed() > REQUEST_TIMEOUT {
                    timed_out.push((peer, req.start_height, req.end_height));
                }
            }
        }
        for (peer, s, e) in timed_out {
            println!("⏰ Request {}-{} to {} timed out, re-queuing", s, e, peer);
            if let Some(reqs) = self.in_flight.get_mut(&peer) {
                reqs.retain(|r| !(r.start_height == s && r.end_height == e));
            }
            self.work_queue.push_front((s, e));
        }

        // 3. Remove work that we already have (chain advanced past it)
        while let Some(&(s, _e)) = self.work_queue.front() {
            if s <= my_height {
                self.work_queue.pop_front();
            } else {
                break;
            }
        }

        // 4. Assign work to peers with capacity
        let eligible: Vec<PeerId> = self.peer_heights
            .iter()
            .filter(|&(_, &h)| h > my_height)
            .map(|(&pid, _)| pid)
            .collect();

        let mut assignments = Vec::new();

        for peer in eligible {
            let current = self.in_flight.entry(peer).or_insert_with(Vec::new);
            while current.len() < MAX_IN_FLIGHT_PER_PEER {
                if let Some((s, e)) = self.work_queue.pop_front() {
                    if s <= my_height {
                        continue;
                    }
                    assignments.push((peer, s, e));
                    current.push(InFlightRequest {
                        start_height: s,
                        end_height: e,
                        sent_at: Instant::now(),
                    });
                } else {
                    break;
                }
            }
        }

        assignments
    }

    /// Is the sync fully complete? (no work left, nothing in flight)
    pub fn is_idle(&self) -> bool {
        self.work_queue.is_empty()
            && self.in_flight.values().all(|v| v.is_empty())
    }

    /// Status summary for logging
    pub fn status(&self) -> String {
        let in_flight_total: usize = self.in_flight.values().map(|v| v.len()).sum();
        format!("queue={}, in_flight={}, peers={}",
                self.work_queue.len(), in_flight_total, self.peer_heights.len())
    }

    /// Number of items remaining in the work queue
    #[allow(dead_code)]
    pub fn work_queue_len(&self) -> usize {
        self.work_queue.len()
    }

    /// Number of in-flight requests for a specific peer
    #[allow(dead_code)]
    pub fn in_flight_count(&self, peer: &PeerId) -> usize {
        self.in_flight.get(peer).map_or(0, |v| v.len())
    }
}

// ─── Swarm Construction ───────────────────────────────────────────────────────

pub fn create_swarm() -> Result<Swarm<BlockchainBehaviour>, Box<dyn std::error::Error>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(1))
                .validation_mode(gossipsub::ValidationMode::Strict)
                .message_id_fn(message_id_fn)
                .build()
                .map_err(std::io::Error::other)?;

            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

            let ping = ping::Behaviour::new(ping::Config::new().with_interval(PING_INTERVAL));

            // Direct sync: request-response with CBOR codec
            let direct_sync = request_response::cbor::Behaviour::new(
                [(
                    libp2p::StreamProtocol::new("/blockchain/sync/1.0.0"),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default(),
            );

            Ok(BlockchainBehaviour { gossipsub, mdns, ping, direct_sync })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}

// ─── GossipSub Messages (reduced: only new blocks, txs, discovery) ───────────

pub fn get_topic() -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(TOPIC)
}

/// Messages that still travel over GossipSub (broadcast)
#[derive(Debug, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// A newly mined block (broadcast to all)
    Block { block: Block, request_id: String },
    /// A new transaction (broadcast to all)
    Transaction(Transaction),
    /// Discovery: ask peers for their chain height
    RequestChain { request_id: String },
    /// Discovery: response with chain height
    ChainParams { best_hash: String, height: u32, request_id: String },
    /// Mempool sync: ask peers for their pending transactions
    RequestMempool,
    /// Mempool sync: response with pending transactions
    MempoolTxs { transactions: Vec<Transaction> },
}

pub fn publish_block(swarm: &mut Swarm<BlockchainBehaviour>, block: Block) {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let id: u64 = rng.r#gen();

    let msg = NetworkMessage::Block { block, request_id: id.to_string() };
    let json = serde_json::to_string(&msg).expect("Failed to serialize block");
    let topic = get_topic();
    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes()) {
        println!("Failed to publish block: {:?}", e);
    }
}

pub fn publish_transaction(swarm: &mut Swarm<BlockchainBehaviour>, tx: Transaction) {
    let msg = NetworkMessage::Transaction(tx);
    let json = serde_json::to_string(&msg).expect("Failed to serialize transaction");
    let topic = get_topic();
    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes()) {
        println!("Failed to publish transaction: {:?}", e);
    }
}

pub fn publish_mempool_request(swarm: &mut Swarm<BlockchainBehaviour>) {
    let msg = NetworkMessage::RequestMempool;
    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let topic = get_topic();
    let _ = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes());
}

pub fn publish_mempool_txs(swarm: &mut Swarm<BlockchainBehaviour>, transactions: Vec<Transaction>) {
    if transactions.is_empty() { return; }
    let msg = NetworkMessage::MempoolTxs { transactions };
    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let topic = get_topic();
    let _ = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes());
}

/// Re-broadcast all mempool transactions (ensures late-joining nodes get them)
pub fn rebroadcast_mempool(swarm: &mut Swarm<BlockchainBehaviour>, blockchain: &Blockchain) {
    if blockchain.mempool.is_empty() { return; }
    let count = blockchain.mempool.len();
    for tx in &blockchain.mempool {
        let msg = NetworkMessage::Transaction(tx.clone());
        let json = serde_json::to_string(&msg).expect("ser");
        let topic = get_topic();
        let _ = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes());
    }
    println!("📡 Re-broadcast {} mempool transactions", count);
}

// ─── Event Handling ───────────────────────────────────────────────────────────

pub fn handle_swarm_event(
    event: SwarmEvent<BlockchainBehaviourEvent>,
    swarm: &mut Swarm<BlockchainBehaviour>,
    blockchain: &mut Blockchain,
    peer_tracker: &mut PeerHealthTracker,
    sync_manager: &mut SyncManager,
) -> bool {
    match event {
        // ── mDNS: peer discovered ──
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            for (peer_id, multiaddr) in list {
                if let Err(e) = swarm.dial(multiaddr.clone()) {
                    println!("Failed to dial peer {}: {:?}", peer_id, e);
                }
                println!("mDNS discovered a new peer: {peer_id} at {multiaddr}");
                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
            }
            false
        }

        // ── mDNS: peer expired ──
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, multiaddr) in list {
                println!("mDNS peer expired: {peer_id} at {multiaddr}");
                swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                peer_tracker.remove_peer(&peer_id);
                sync_manager.remove_peer(&peer_id);
            }
            false
        }

        // ── Ping ──
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Ping(ping::Event {
            peer,
            result: Ok(_),
            ..
        })) => {
            peer_tracker.record_pong(peer);
            false
        }

        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Ping(ping::Event {
            peer,
            result: Err(e),
            ..
        })) => {
            println!("❌ PING failed to {}: {:?}", peer, e);
            false
        }

        // ── Direct Sync (request-response) ──
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::DirectSync(event)) => {
            match event {
                request_response::Event::Message { peer, message, .. } => {
                    match message {
                        // INCOMING REQUEST: peer wants blocks from us
                        request_response::Message::Request { request, channel, .. } => {
                            println!("📥 Sync request from {}: blocks {}-{} (locators: {})",
                                     peer, request.start_height, request.end_height, request.locators.is_some());

                            let start_height = if let Some(locators) = &request.locators {
                                if let Some(ancestor) = blockchain.find_common_ancestor(locators) {
                                     println!("🔱 Common ancestor found at {}", ancestor);
                                     ancestor + 1
                                } else {
                                     request.start_height
                                }
                            } else {
                                request.start_height
                            };

                            let blocks: Vec<Block> = (start_height..=request.end_height)
                                .filter_map(|i| blockchain.chain.get(i as usize).cloned())
                                .collect();

                            println!("📤 Sending {} blocks to {}", blocks.len(), peer);
                            let response = SyncResponse { blocks };
                            if let Err(e) = swarm.behaviour_mut().direct_sync.send_response(channel, response) {
                                println!("Failed to send sync response: {:?}", e);
                            }
                            false
                        }

                        // INCOMING RESPONSE: we received blocks we asked for
                        request_response::Message::Response { response, .. } => {
                            let count = response.blocks.len();
                            if count > 0 {
                                let first = response.blocks.first().unwrap().index;
                                let last = response.blocks.last().unwrap().index;
                                println!("📥 Received {} blocks ({}-{}) from {}", count, first, last, peer);
                                // Clear from in-flight
                                sync_manager.on_response(&peer, first, last);
                                let (added, failure) = blockchain.try_add_block_batch(response.blocks);
                                
                                if added > 0 {
                                    println!("✅ Added {} blocks. Chain height: {}", added, blockchain.chain.len() - 1);
                                    if sync_manager.is_idle() {
                                        println!("🎉 Sync complete! Height: {}", blockchain.chain.last().unwrap().index);
                                    }
                                }
                                
                                if let Some(fail) = failure {
                                    match fail {
                                        crate::blockchain::AddBlockResult::Orphan(parent_idx) => {
                                            println!("🔍 Batch hit orphan at parent {}. Requesting from {}...", parent_idx, peer);
                                            request_block(swarm, &peer, parent_idx);
                                        }
                                        crate::blockchain::AddBlockResult::Fork => {
                                            println!("🔱 Batch detected fork from peer {}. Triggering reorg sync...", peer);
                                            let peer_height = sync_manager.peer_heights.get(&peer).copied().unwrap_or(last);
                                            let locators = blockchain.create_locator_hashes();
                                            let req = crate::sync::SyncRequest {
                                                start_height: 1, 
                                                end_height: peer_height, 
                                                locators: Some(locators),
                                            };
                                            let _ = swarm.behaviour_mut().direct_sync.send_request(&peer, req);
                                        }
                                        crate::blockchain::AddBlockResult::NeedsReorg => {
                                            println!("🔄 Chains fully diverged with peer {}. Requesting full chain...", peer);
                                            let peer_height = sync_manager.peer_heights.get(&peer).copied().unwrap_or(last);
                                            let locators = blockchain.create_locator_hashes();
                                            let req = crate::sync::SyncRequest {
                                                start_height: 1, 
                                                end_height: peer_height, 
                                                locators: Some(locators),
                                            };
                                            let _ = swarm.behaviour_mut().direct_sync.send_request(&peer, req);
                                        }
                                        _ => println!("⚠️ Batch processing stopped due to {:?}", fail),
                                    }
                                }
                                
                                if added > 0 {
                                    true // signal: chain changed
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        }
                    }
                }
                request_response::Event::OutboundFailure { peer, error, .. } => {
                    println!("⚠️  Sync request to {} failed: {:?}", peer, error);
                    // Re-queue any work assigned to this peer
                    sync_manager.remove_peer(&peer);
                    false
                }
                request_response::Event::InboundFailure { peer, error, .. } => {
                    println!("⚠️  Sync response to {} failed: {:?}", peer, error);
                    false
                }
                _ => false,
            }
        }

        // ── GossipSub (broadcasts: new blocks, txs, discovery) ──
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Gossipsub(event)) => {
            match event {
                gossipsub::Event::Message { propagation_source: peer_id, message, .. } => {
                    let data = String::from_utf8_lossy(&message.data);
                    match serde_json::from_str::<NetworkMessage>(&data) {
                        // Discovery: peer asks for our chain status
                        Ok(NetworkMessage::RequestChain { request_id }) => {
                            let best = blockchain.chain.last().unwrap();
                            let msg = NetworkMessage::ChainParams {
                                best_hash: best.hash.clone(),
                                height: best.index,
                                request_id,
                            };
                            let json = serde_json::to_string(&msg).expect("ser");
                            let topic = get_topic();
                            let _ = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes());
                            false
                        }

                        // Discovery: peer tells us their height → feed SyncManager
                        Ok(NetworkMessage::ChainParams { height, request_id: _, best_hash: _ }) => {
                            println!("Peer {} is at height {}", peer_id, height);
                            let my_height = blockchain.chain.last().unwrap().index;
                            sync_manager.record_peer_height(peer_id, height, my_height);
                            // No immediate sync trigger — tick() will handle scheduling
                            false
                        }

                        // New mined block (gossip broadcast)
                        Ok(NetworkMessage::Block { block, request_id: _ }) => {
                            // Peer definitely has this block, so they are at least at this height
                            let my_height = blockchain.chain.len() as u32;
                            sync_manager.record_peer_height(peer_id, block.index, my_height);

                            match blockchain.try_add_block(block.clone()) {
                                crate::blockchain::AddBlockResult::Added => {
                                    println!("\n⛏️  New block {} from peer {}", block.index, peer_id);
                                    println!("Chain height: {}", blockchain.chain.len() - 1);
                                    true
                                }
                                crate::blockchain::AddBlockResult::Buffered => {
                                    let my_height = blockchain.chain.len() as u32;
                                    if block.index > my_height {
                                        println!("📦 Buffered future block {}. Triggering priority sync for {}-{}...", 
                                            block.index, my_height, block.index - 1);
                                        // Force re-request of the missing gap
                                        sync_manager.work_queue.push_front((my_height, block.index - 1));
                                    }
                                    false
                                },
                                crate::blockchain::AddBlockResult::Exists => false,
                                crate::blockchain::AddBlockResult::Orphan(parent_idx) => {
                                    println!("🔍 Asking {} for parent block {}", peer_id, parent_idx);
                                    request_block(swarm, &peer_id, parent_idx);
                                    false
                                }
                                crate::blockchain::AddBlockResult::Fork => {
                                    println!("🔱 Fork detected from peer {}. Triggering reorg sync...", peer_id);
                                    let peer_height = sync_manager.peer_heights.get(&peer_id).copied().unwrap_or(block.index);
                                    let locators = blockchain.create_locator_hashes();
                                    let req = crate::sync::SyncRequest {
                                        start_height: 1, 
                                        end_height: peer_height, 
                                        locators: Some(locators),
                                    };
                                    let _ = swarm.behaviour_mut().direct_sync.send_request(&peer_id, req);
                                    false
                                }
                                crate::blockchain::AddBlockResult::Invalid | crate::blockchain::AddBlockResult::NeedsReorg => {
                                    println!("Block {} from {} rejected.", block.index, peer_id);
                                    false
                                }
                            }
                        }

                        // Transaction
                        Ok(NetworkMessage::Transaction(tx)) => {
                            if blockchain.add_to_mempool(tx) {
                                println!("Transaction received and added to mempool.");
                            }
                            false
                        }

                        // Mempool sync: peer is asking for our mempool
                        Ok(NetworkMessage::RequestMempool) => {
                            if !blockchain.mempool.is_empty() {
                                println!("📤 Sending {} mempool txs to peers", blockchain.mempool.len());
                                publish_mempool_txs(swarm, blockchain.mempool.clone());
                            }
                            false
                        }

                        // Mempool sync: received mempool transactions from a peer
                        Ok(NetworkMessage::MempoolTxs { transactions }) => {
                            let mut added = 0;
                            for tx in transactions {
                                if blockchain.add_to_mempool(tx) {
                                    added += 1;
                                }
                            }
                            if added > 0 {
                                println!("📥 Received {} new mempool transactions", added);
                            }
                            false
                        }

                        Err(e) => {
                            println!("Failed to deserialize gossip from {}: {}", peer_id, e);
                            false
                        }
                    }
                }

                gossipsub::Event::Subscribed { peer_id, topic: _ } => {
                    // New peer joined topic → ask for their chain height + mempool
                    println!("🔄 New peer {} subscribed. Asking for chain status...", peer_id);
                    use rand::Rng;
                    let mut rng = rand::thread_rng();
                    let id: u64 = rng.r#gen();
                    let msg = NetworkMessage::RequestChain { request_id: id.to_string() };
                    let json = serde_json::to_string(&msg).expect("ser");
                    let t = get_topic();
                    let _ = swarm.behaviour_mut().gossipsub.publish(t, json.as_bytes());
                    // Also request mempool from peers
                    publish_mempool_request(swarm);
                    false
                }

                _ => false,
            }
        }

        // ── Connection lifecycle ──
        SwarmEvent::NewListenAddr { address, .. } => {
            println!("Listening on {address}");
            false
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            println!("Connection established with {}", peer_id);
            peer_tracker.add_peer(peer_id);
            false
        }
        SwarmEvent::ConnectionClosed { peer_id, cause, num_established, .. } => {
            if num_established > 0 {
                return false;
            }
            println!("Connection closed with {}: {:?}", peer_id, cause);
            peer_tracker.remove_peer(&peer_id);
            sync_manager.remove_peer(&peer_id);
            false
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            println!("Outgoing connection error to {:?}: {:?}", peer_id, error);
            false
        }
        _ => false,
    }
}

// ─── Stale Peer Cleanup ──────────────────────────────────────────────────────

pub fn check_and_disconnect_stale_peers(
    swarm: &mut Swarm<BlockchainBehaviour>,
    peer_tracker: &mut PeerHealthTracker,
) {
    let stale_peers = peer_tracker.check_stale_peers();

    for peer_id in stale_peers {
        println!("⚠️  Peer {} is stale, disconnecting...", peer_id);

        if let Err(e) = swarm.disconnect_peer_id(peer_id) {
            println!("Failed to disconnect peer {}: {:?}", peer_id, e);
        } else {
            peer_tracker.remove_peer(&peer_id);
        }
    }
}
// Helper to request a single block from a peer
pub fn request_block(
    swarm: &mut Swarm<BlockchainBehaviour>,
    peer: &PeerId,
    height: u32
) {
    let request = SyncRequest { start_height: height, end_height: height, locators: None };
    swarm.behaviour_mut().direct_sync.send_request(peer, request);
    println!("📡 Requesting orphan parent block {} from {}", height, peer);
}
