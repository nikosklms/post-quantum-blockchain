mod transaction;
mod block;
mod blockchain;
mod network;

use transaction::Transaction;
use blockchain::Blockchain;

use futures::stream::StreamExt;
use tokio::io::{self, AsyncBufReadExt};
use tokio::select;
use tokio::time::{interval, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize blockchain
    let mut blockchain = Blockchain::new();

    // Create and configure the P2P swarm
    let mut swarm = network::create_swarm()?;

    // Create peer health tracker
    let mut peer_tracker = network::PeerHealthTracker::new(
        Duration::from_secs(5),  // ping interval
        3                         // timeout multiplier
    );
    
    // Check for stale peers every 1 second
    let mut check_interval = interval(Duration::from_secs(1));

    // Subscribe to the blocks topic
    let topic = network::get_topic();
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Listen on all interfaces, random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("\nBlockchain P2P Node Started!");
    println!("Commands:");
    println!("  mine <sender> <receiver> <amount>       - Mine a new block and broadcast");
    println!("  mine_local <sender> <receiver> <amount> - Mine a new block LOCALLY only (for conflicts)");
    println!("  chain                                   - Print the blockchain");
    println!("  validate                                - Validate the chain");
    println!("  sync                                    - Broadcast local chain to peers");
    println!("  peers                                   - List connected peers\n");

    // Read lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Main event loop
    loop {
        select! {
            // Handle user input from stdin
            Ok(Some(line)) = stdin.next_line() => {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }

                match parts[0] {
                    "mine" => {
                        if parts.len() < 4 {
                            println!("Usage: mine <sender> <receiver> <amount>");
                            continue;
                        }

                        let amount: f64 = match parts[3].parse() {
                            Ok(a) => a,
                            Err(_) => {
                                println!("Invalid amount.");
                                continue;
                            }
                        };

                        let tx = Transaction {
                            sender: parts[1].to_string(),
                            receiver: parts[2].to_string(),
                            amount,
                        };

                        // Mine the block
                        blockchain.add_block(vec![tx]);
                        let block = blockchain.chain.last().unwrap();
                        println!("Block mined! Hash: {}", block.hash);

                        // Broadcast to the network
                        network::publish_block(&mut swarm, block.clone());
                        println!("Block published to network.");
                    }

                    "mine_local" => {
                        if parts.len() < 4 {
                            println!("Usage: mine_local <sender> <receiver> <amount>");
                            continue;
                        }

                        let amount: f64 = match parts[3].parse() {
                            Ok(a) => a,
                            Err(_) => {
                                println!("Invalid amount.");
                                continue;
                            }
                        };

                        let tx = Transaction {
                            sender: parts[1].to_string(),
                            receiver: parts[2].to_string(),
                            amount,
                        };

                        // Mine the block locally (DO NOT BROADCAST)
                        blockchain.add_block(vec![tx]);
                        let block = blockchain.chain.last().unwrap();
                        println!("Local Block mined (hidden from peers)! Hash: {}", block.hash);
                    }

                    "chain" => {
                        println!("{}", blockchain);
                    }

                    "validate" => {
                        if blockchain.validate_chain() {
                            println!("Blockchain is valid!");
                        } else {
                            println!("Blockchain is NOT valid!");
                        }
                    }

                    "sync" => {
                        println!("Broadcasting chain to peers...");
                        network::publish_chain(&mut swarm, blockchain.chain.clone());
                        println!("Chain broadcasted.");
                    }

                    "peers" => {
                        let peers: Vec<_> = swarm.connected_peers().collect();
                        println!("Connected peers ({}):", peers.len());
                        for peer in peers {
                            println!("  - {peer}");
                        }
                    }

                    _ => {
                        println!("Unknown command. Try: mine, chain, validate, sync, peers");
                    }
                }
            }

            // Handle swarm events (peer discovery, incoming blocks)
            event = swarm.select_next_some() => {
                network::handle_swarm_event(event, &mut swarm, &mut blockchain, &mut peer_tracker);
            }

            // Check for stale peers every 1 second
            _ = check_interval.tick() => {
                network::check_and_disconnect_stale_peers(&mut swarm, &mut peer_tracker);
            }
        }
    }
}