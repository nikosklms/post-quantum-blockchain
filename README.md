# Simple P2P Blockchain in Rust

A basic Proof-of-Work blockchain implementation with peer-to-peer networking using `libp2p`.

## Features
- **Proof of Work**: Mining with adjustable difficulty.
- **P2P Networking**: Decentralized peer discovery via mDNS and GossipSub.
- **Consensus**: Longest Chain Rule (Nakamoto Consensus).
- **Sync**: Full chain synchronization to resolve forks.

## Getting Started

### Prerequisites
- Rust and Cargo installed.

### Running a Node
```bash
cargo run
```

### Commands
- `mine <sender> <receiver> <amount>`: Mine a block and broadcast it.
- `mine_local <sender> <receiver> <amount>`: Mine a block **locally** (for testing conflicts).
- `sync`: Broadcast your full chain to peers (triggers consensus).
- `chain`: View the current blockchain.
- `peers`: View connected peers.

## Conflict Experiment
See [conflict_experiment.md](conflict_experiment.md) for a guide on how to simulate and resolve a blockchain fork.
