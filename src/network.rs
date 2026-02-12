use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use libp2p::{
    gossipsub, mdns, noise, PeerId,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Swarm, ping,
};
use crate::block::Block;
use crate::blockchain::Blockchain;

const TOPIC: &str = "blocks";
const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT_MULTIPLIER: u32 = 3;

// Custom NetworkBehaviour combining GossipSub + mDNS + Ping
#[derive(NetworkBehaviour)]
pub struct BlockchainBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub ping: ping::Behaviour,
}

/// Track peer health based on ping/pong
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

    /// Update the last pong time for a peer
    pub fn record_pong(&mut self, peer_id: PeerId) {
        self.last_pong.insert(peer_id, Instant::now());
    }

    /// Register a new peer
    pub fn add_peer(&mut self, peer_id: PeerId) {
        self.last_pong.insert(peer_id, Instant::now());
    }

    /// Remove a peer
    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.last_pong.remove(peer_id);
    }

    /// Check for stale peers and return list of peers to disconnect
    pub fn check_stale_peers(&self) -> Vec<PeerId> {
        let now = Instant::now();
        self.last_pong
            .iter()
            .filter(|(_, last_time)| now.duration_since(**last_time) > self.timeout)
            .map(|(peer_id, _)| *peer_id)
            .collect()
    }
}

/// Create and configure the libp2p Swarm
pub fn create_swarm() -> Result<Swarm<BlockchainBehaviour>, Box<dyn std::error::Error>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            // Content-address messages by hashing their data
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

            Ok(BlockchainBehaviour {
                gossipsub,
                mdns,
                ping
            })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}

/// Get the GossipSub topic for block broadcasting
pub fn get_topic() -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(TOPIC)
}

/// Publish a mined block to the network


/// Publish a mined block to the network
pub fn publish_block(
    swarm: &mut Swarm<BlockchainBehaviour>,
    block: Block,
) {
    let msg = NetworkMessage::Block(block);
    let json = serde_json::to_string(&msg).expect("Failed to serialize block");
    let topic = get_topic();
    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes()) {
        println!("Failed to publish block: {:?}", e);
    }
}

/// Publish the full chain to the network (Sync)
pub fn publish_chain(
    swarm: &mut Swarm<BlockchainBehaviour>,
    chain: Vec<Block>,
) {
    let msg = NetworkMessage::Chain(chain);
    let json = serde_json::to_string(&msg).expect("Failed to serialize chain");
    let topic = get_topic();
    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes()) {
        println!("Failed to publish chain: {:?}", e);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum NetworkMessage {
    Block(Block),
    Chain(Vec<Block>),
}

/// Handle a swarm event
pub fn handle_swarm_event(
    event: SwarmEvent<BlockchainBehaviourEvent>,
    swarm: &mut Swarm<BlockchainBehaviour>,
    blockchain: &mut Blockchain,
    peer_tracker: &mut PeerHealthTracker,
) {
    match event {
        // mDNS: new peer discovered
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            for (peer_id, multiaddr) in list {
                if let Err(e) = swarm.dial(multiaddr.clone()) {
                    println!("Failed to dial peer {}: {:?}", peer_id, e);
                }
                println!("mDNS discovered a new peer: {peer_id} at {multiaddr}");
                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
            }
        }

        // mDNS: peer expired
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, multiaddr) in list {
                println!("mDNS peer expired: {peer_id} at {multiaddr}");
                swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                peer_tracker.remove_peer(&peer_id);
            }
        }

        // Ping: Pong received
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Ping(ping::Event {
            peer,
            result: Ok(rtt),
            ..
        })) => {
            //println!("🏓 PONG received from {} (RTT: {:?})", peer, rtt);
            peer_tracker.record_pong(peer);
        }

        // Ping: Ping failed
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Ping(ping::Event {
            peer,
            result: Err(e),
            ..
        })) => {
            println!("❌ PING failed to {}: {:?}", peer, e);
        }

        // GossipSub: received a data from the network
        SwarmEvent::Behaviour(BlockchainBehaviourEvent::Gossipsub(
            gossipsub::Event::Message {
                propagation_source: peer_id,
                message,
                ..
            },
        )) => {
            let data = String::from_utf8_lossy(&message.data);
            match serde_json::from_str::<NetworkMessage>(&data) {
                Ok(NetworkMessage::Block(block)) => {
                    println!("\nReceived block {} from peer {}", block.index, peer_id);
                    if blockchain.try_add_block(block) {
                        println!("Block added to chain! Chain length: {}", blockchain.chain.len());
                    } else {
                        println!("Block rejected (invalid).");
                    }
                }
                Ok(NetworkMessage::Chain(chain)) => {
                    println!("\nReceived full chain from peer {} (len {})", peer_id, chain.len());
                    if blockchain.replace_chain(chain) {
                        println!("Chain replaced! New length: {}", blockchain.chain.len());
                    } else {
                        println!("Chain rejected (shorter or invalid).");
                    }
                }
                Err(e) => {
                    println!("Failed to deserialize message from peer {}: {}", peer_id, e);
                }
            }
        }

        // New listen address
        SwarmEvent::NewListenAddr { address, .. } => {
            println!("Listening on {address}");
        }

        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            println!("Connection established with {}", peer_id);
            peer_tracker.add_peer(peer_id);
        }

        SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
            println!("Connection closed with {}: {:?}", peer_id, cause);
            peer_tracker.remove_peer(&peer_id);
        }

        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            println!("Outgoing connection error to {:?}: {:?}", peer_id, error);
        }

        _ => {}
    }
}

/// Check for and disconnect stale peers
pub fn check_and_disconnect_stale_peers(
    swarm: &mut Swarm<BlockchainBehaviour>,
    peer_tracker: &mut PeerHealthTracker,
) {
    let stale_peers = peer_tracker.check_stale_peers();
    
    for peer_id in stale_peers {
        println!("⚠️  Peer {} is stale (no pong for {}s), disconnecting...", 
                 peer_id, 
                 PING_INTERVAL.as_secs() * PING_TIMEOUT_MULTIPLIER as u64);
        
        if let Err(e) = swarm.disconnect_peer_id(peer_id) {
            println!("Failed to disconnect peer {}: {:?}", peer_id, e);
        } else {
            peer_tracker.remove_peer(&peer_id);
        }
    }
}