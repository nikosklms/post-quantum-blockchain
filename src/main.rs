mod transaction;
mod block;
mod blockchain;
mod network;
mod wallet;
mod sync;
#[cfg(test)]
mod sync_tests;

use transaction::Transaction;
use blockchain::Blockchain;
use wallet::Wallet;

use futures::stream::StreamExt;
use tokio::io::{self, AsyncBufReadExt};
use tokio::select;
use tokio::time::{interval, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize blockchain
    let mut blockchain = Blockchain::new();

    // Create a local wallet
    let wallet = Wallet::new();
    let public_key = wallet.get_public_key();
    println!("🔑 Local Wallet Created!");
    println!("Public Key: {}", public_key);
    println!("(Start mining to earn coins!)");

    // Create and configure the P2P swarm
    let mut swarm = network::create_swarm()?;

    // Create peer health tracker
    let mut peer_tracker = network::PeerHealthTracker::new(
        Duration::from_secs(5),  // ping interval
        3                         // timeout multiplier
    );

    // Create sync manager for parallel direct sync
    let mut sync_manager = network::SyncManager::new();
    
    // Check for stale peers every 1 second
    let mut check_interval = interval(Duration::from_secs(1));

    // Subscribe to the blocks topic
    let topic = network::get_topic();
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Listen on all interfaces, random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("\nBlockchain P2P Node Started!");
    println!("Commands:");
    println!("  trans <receiver> <amount>      - Create and broadcast a transaction");
    println!("  mine                           - Mine a new block with pending transactions");
    println!("  mine_local <receiver> <amount> - Mine a new block LOCALLY only (for conflicts)");
    println!("  chain                          - Print the blockchain");
    println!("  validate                       - Validate the chain");
    println!("  sync                           - Request missing blocks from peers");
    println!("  peers                          - List connected peers\n");

    // Read lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Channel for receiving mined blocks from the mining thread
    let (mining_sender, mut mining_receiver) = tokio::sync::mpsc::channel::<block::Block>(10);
    
    // Cancellation token for the active mining thread
    let mut current_mining_cancellation_token: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> = None;

    // Main event loop
    loop {
        select! {
            // Handle mined block from the background thread
            Some(mined_block) = mining_receiver.recv() => {
                println!("⛏️  Block mined successfully! Hash: {}", mined_block.hash);
                blockchain.try_add_block(mined_block.clone()); // Add to chain & clear mempool
                network::publish_block(&mut swarm, mined_block);
                println!("Block published to network.");
                current_mining_cancellation_token = None; // Reset token since mining is done
            }
            
            // Handle user input from stdin
            Ok(Some(line)) = stdin.next_line() => {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }

                match parts[0] {
                    "trans" => {
                        if parts.len() < 3 {
                            println!("Usage: trans <receiver> <amount>");
                            continue;
                        }

                        let amount: f64 = match parts[2].parse() {
                            Ok(a) => a,
                            Err(_) => {
                                println!("Invalid amount.");
                                continue;
                            }
                        };

                        let tx = Transaction::new(
                            public_key.clone(),
                            parts[1].to_string(),
                            amount,
                            &wallet
                        );
                        
                        println!("Transaction created: {} -> {} ({})", tx.sender, tx.receiver, tx.amount);
                        
                        // Add to local mempool
                        blockchain.add_to_mempool(tx.clone());
                        println!("Added to local mempool.");
                        
                        // Broadcast to network
                        network::publish_transaction(&mut swarm, tx);
                        println!("Broadcasted to network.");
                    }

                    "mine" => {
                        // Check if already mining
                        if current_mining_cancellation_token.is_some() {
                            println!("⚠️  Already mining! Wait for the current block.");
                            continue;
                        }

                        let txs: Vec<Transaction> = blockchain.mempool.iter().take(2000).cloned().collect();
                        if txs.is_empty() {
                            println!("Mempool is empty. (Nothing to mine)");
                            continue;
                        }

                        println!("Mining block with {} transactions...", txs.len());

                        // Prepare block data for the thread
                        let index = blockchain.chain.len() as u32;
                        let previous_hash = blockchain.chain.last().unwrap().hash.clone();
                        let txs_clone = txs.clone();

                        // Create cancellation token
                        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        current_mining_cancellation_token = Some(stop_signal.clone());

                        // Clone the transmitter and signal for the thread
                        let tx_mining = mining_sender.clone();
                        let stop_signal_thread = stop_signal.clone();
                        
                        // Spawn mining on a separate thread (non-blocking)
                        let mut block = block::Block::new(index, txs_clone, previous_hash);
                        
                        tokio::task::spawn_blocking(move || {
                            block.mine_block(&stop_signal_thread);
                            // Send the mined block back to the main thread (only if not cancelled)
                             if !stop_signal_thread.load(std::sync::atomic::Ordering::Relaxed) {
                                if let Err(e) = tx_mining.blocking_send(block) {
                                    println!("Error sending mined block: {}", e);
                                }
                             }
                        });
                        
                        println!("Mining started in background...");
                    }

                    "mine_local" => {
                        if parts.len() < 3 {
                            println!("Usage: mine_local <receiver> <amount>");
                            continue;
                        }

                        let amount: f64 = match parts[2].parse() {
                            Ok(a) => a,
                            Err(_) => {
                                println!("Invalid amount.");
                                continue;
                            }
                        };

                        let tx = Transaction::new(
                            public_key.clone(),
                            parts[1].to_string(),
                            amount,
                            &wallet
                        );

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
                        println!("Requesting chain status from peers...");
                        // Just send RequestChain — tick() will do the work
                        use rand::Rng;
                        let mut rng = rand::thread_rng();
                        let id: u64 = rng.r#gen();
                        let msg = network::NetworkMessage::RequestChain { request_id: id.to_string() };
                        let json = serde_json::to_string(&msg).expect("ser");
                        let topic = network::get_topic();
                        let _ = swarm.behaviour_mut().gossipsub.publish(topic, json.as_bytes());
                        println!("Sync scheduler active ({})", sync_manager.status());
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
                if network::handle_swarm_event(event, &mut swarm, &mut blockchain, &mut peer_tracker, &mut sync_manager) {
                    // If a valid block/chain was received, CANCEL any active mining
                    if let Some(token) = current_mining_cancellation_token.take() {
                        println!("⚡ New block received! Cancelling current mining job...");
                        token.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }

            // Periodic tick: stale peer cleanup + sync scheduler
            _ = check_interval.tick() => {
                network::check_and_disconnect_stale_peers(&mut swarm, &mut peer_tracker);
                // Drive the sync work-queue scheduler
                let my_height = blockchain.chain.last().unwrap().index;
                sync_manager.tick(&mut swarm, my_height);
            }
        }
    }
}