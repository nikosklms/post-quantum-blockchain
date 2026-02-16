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

    // Create a new wallet (persistence removed for now)
    let wallet = Wallet::new();
    
    let public_key = wallet.get_public_key();
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

    // Re-broadcast mempool transactions every 30 seconds
    let mut rebroadcast_interval = interval(Duration::from_secs(30));

    // Subscribe to the blocks topic
    let topic = network::get_topic();
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    // Listen on all interfaces, random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("\nBlockchain P2P Node Started!");
    println!("Commands:");
    println!("  sendtoaddress <addr> <amount>  - Send coins to one recipient");
    println!("  sendmany <a1>,<a2> <v1>,<v2>   - Send coins to multiple recipients");
    println!("  mine                           - Mine a new block (earns reward!)");
    println!("  mine_batch <count>             - Mine N blocks (for testing sync)");
    println!("  mine_local                     - Mine a new block LOCALLY only (for conflicts)");
    println!("  balance                        - Show your wallet balance");
    println!("  utxos                          - Print all unspent transaction outputs");
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
    let mut mining_batch_remaining: u32 = 0;

    // Main event loop
    loop {
        select! {
            // Handle mined block from the background thread
            Some(mined_block) = mining_receiver.recv() => {
                println!("⛏️  Block mined successfully! Hash: {}", mined_block.hash);
                blockchain.try_add_block(mined_block.clone());
                let balance = blockchain.get_balance(&public_key);
                println!("Block published to network. Your balance: {} coins", balance);
                network::publish_block(&mut swarm, mined_block);

                // If executing a batch, start the next one immediately
                current_mining_cancellation_token = None;
                if mining_batch_remaining > 0 {
                    mining_batch_remaining -= 1;
                    start_mining_job(
                        &blockchain, 
                        &public_key, 
                        mining_sender.clone(), 
                        &mut current_mining_cancellation_token
                    );
                }
            }
            
            // Handle user input from stdin
            Ok(Some(line)) = stdin.next_line() => {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }

                match parts[0] {
                    "sendtoaddress" => {
                        if parts.len() < 3 {
                            println!("Usage: sendtoaddress <receiver> <amount>");
                            continue;
                        }

                        let amount: f64 = match parts[2].parse() {
                            Ok(a) => a,
                            Err(_) => {
                                println!("Invalid amount.");
                                continue;
                            }
                        };

                        let (inputs, total) = blockchain.find_spendable_outputs(&public_key, amount);
                        if total < amount {
                            println!("Insufficient balance! Have {}, need {}", total, amount);
                            continue;
                        }

                        let tx = Transaction::new(inputs, parts[1], amount, &wallet);

                        let change = total - amount;
                        if change > 0.0001 {
                            println!("Transaction: {} → {} (change: {})", amount, parts[1], change);
                        } else {
                            println!("Transaction: {} → {}", amount, parts[1]);
                        }

                        blockchain.add_to_mempool(tx.clone());
                        network::publish_transaction(&mut swarm, tx);
                        println!("Broadcasted to network.");
                    }

                    "sendmany" => {
                        // Usage: sendmany addr1,addr2,addr3 10,20,30
                        if parts.len() < 3 {
                            println!("Usage: sendmany <addr1>,<addr2> <amount1>,<amount2>");
                            continue;
                        }

                        let addrs: Vec<&str> = parts[1].split(',').collect();
                        let amounts: Vec<&str> = parts[2].split(',').collect();

                        if addrs.len() != amounts.len() {
                            println!("Mismatch: {} addresses but {} amounts", addrs.len(), amounts.len());
                            continue;
                        }

                        let mut recipients: Vec<(&str, f64)> = Vec::new();
                        let mut total_needed = 0.0;
                        let mut parse_error = false;

                        for (addr, amt_str) in addrs.iter().zip(amounts.iter()) {
                            match amt_str.parse::<f64>() {
                                Ok(amt) => {
                                    recipients.push((addr, amt));
                                    total_needed += amt;
                                }
                                Err(_) => {
                                    println!("Invalid amount: {}", amt_str);
                                    parse_error = true;
                                    break;
                                }
                            }
                        }
                        if parse_error { continue; }

                        let (inputs, total) = blockchain.find_spendable_outputs(&public_key, total_needed);
                        if total < total_needed {
                            println!("Insufficient balance! Have {}, need {}", total, total_needed);
                            continue;
                        }

                        let tx = Transaction::new_multi(inputs, recipients.clone(), &wallet);

                        println!("sendmany ({} recipients, total: {}):", recipients.len(), total_needed);
                        for (addr, amt) in &recipients {
                            println!("  {} → {}", amt, &addr[..std::cmp::min(16, addr.len())]);
                        }
                        let change = total - total_needed;
                        if change > 0.0001 {
                            println!("  {} → you (change)", change);
                        }

                        blockchain.add_to_mempool(tx.clone());
                        network::publish_transaction(&mut swarm, tx);
                        println!("Broadcasted to network.");
                    }

                    "mine" => {
                        // Check if already mining
                        if current_mining_cancellation_token.is_some() {
                            println!("⚠️  Already mining! Wait for the current block.");
                            continue;
                        }

                        // Stop any batch mining if single mine is called (though usually batch runs)
                        mining_batch_remaining = 0;
                        
                        start_mining_job(
                            &blockchain, 
                            &public_key, 
                            mining_sender.clone(), 
                            &mut current_mining_cancellation_token
                        );
                    }

                    "mine_batch" => {
                        // Check if already mining
                        if current_mining_cancellation_token.is_some() {
                            println!("⚠️  Already mining! Wait for the current block.");
                            continue;
                        }

                        let count: u32 = match parts.get(1) {
                            Some(s) => s.parse().unwrap_or(1),
                            None => 1,
                        };
                        println!("Starting batch mining of {} blocks...", count);
                        
                        if count > 0 {
                            mining_batch_remaining = count - 1;
                            start_mining_job(
                                &blockchain, 
                                &public_key, 
                                mining_sender.clone(), 
                                &mut current_mining_cancellation_token
                            );
                        }
                    }

                    "mine_local" => {
                        // Mine a local block with coinbase only (for testing conflicts)
                        let index = blockchain.chain.len() as u32;
                        let coinbase = Transaction::new_coinbase(&public_key, index);
                        blockchain.add_block(vec![coinbase]);
                        let block = blockchain.chain.last().unwrap();
                        println!("Local Block mined (hidden from peers)! Hash: {}", block.hash);
                        println!("Your balance: {} coins", blockchain.get_balance(&public_key));
                    }

                    "balance" => {
                        let balance = blockchain.get_balance(&public_key);
                        println!("Wallet: {}", &public_key[..16]);
                        println!("Balance: {} coins", balance);
                        println!("UTXOs: {}", blockchain.utxo_set.len());
                    }

                    "utxos" => {
                        if blockchain.utxo_set.is_empty() {
                            println!("No UTXOs. Mine a block first!");
                        } else {
                            println!("\n📦 UTXO Set ({} entries):", blockchain.utxo_set.len());
                            for ((txid, idx), output) in &blockchain.utxo_set {
                                println!("  ({}, {}) → {:.4} coins → {}",
                                    &txid[..8], idx, output.amount, &output.recipient[..16]);
                            }
                        }
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
                        println!("Unknown command. Try: mine, mine_batch, sendtoaddress, sendmany, balance, utxos, chain, validate, sync, peers");
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
                        
                        // Resuming batch mining on new tip
                        if mining_batch_remaining > 0 {
                            println!("🔄 Resuming batch mining on new tip...");
                            start_mining_job(
                                &blockchain, 
                                &public_key, 
                                mining_sender.clone(), 
                                &mut current_mining_cancellation_token
                            );
                        }
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

            // Periodic mempool re-broadcast
            _ = rebroadcast_interval.tick() => {
                network::rebroadcast_mempool(&mut swarm, &blockchain);
            }
        }
    }
}
// Helper function to start mining a single block
fn start_mining_job(
    blockchain: &Blockchain, 
    public_key: &str, 
    mining_sender: tokio::sync::mpsc::Sender<block::Block>,
    cancellation_token_store: &mut Option<std::sync::Arc<std::sync::atomic::AtomicBool>>
) {
    // Prepare block data
    let index = blockchain.chain.len() as u32;
    let previous_hash = blockchain.chain.last().unwrap().hash.clone();

    // Build transaction list: coinbase first, then mempool
    let coinbase = Transaction::new_coinbase(public_key, index);
    let reward = transaction::block_reward(index);
    let mut txs: Vec<Transaction> = vec![coinbase];
    let mempool_txs: Vec<Transaction> = blockchain.mempool.iter().take(2000).cloned().collect();
    txs.extend(mempool_txs);

    // Get current difficulty
    let difficulty = blockchain.get_difficulty();
    println!("Mining block {} ({} txs, reward: {} coins, difficulty: {})...",
        index, txs.len(), reward, difficulty);

    let txs_clone = txs.clone();

    // Create cancellation token
    let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    *cancellation_token_store = Some(stop_signal.clone());

    let tx_mining = mining_sender;
    let stop_signal_thread = stop_signal.clone();

    // Spawn mining on a separate thread
    let mut block = block::Block::new(index, txs_clone, previous_hash, difficulty);

    tokio::task::spawn_blocking(move || {
        block.mine_block(&stop_signal_thread);
        if !stop_signal_thread.load(std::sync::atomic::Ordering::Relaxed) {
            if let Err(e) = tx_mining.blocking_send(block) {
                println!("Error sending mined block: {}", e);
            }
        }
    });

    println!("Mining started in background...");
}
