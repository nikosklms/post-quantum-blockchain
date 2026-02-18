# Post-Quantum Blockchain

A quantum-resistant PoW blockchain implementation in Rust, featuring:

- **Post-Quantum Cryptography**: Uses **Falcon-512** signatures (via `fn-dsa`) for transaction security, resistant to Shor's algorithm.
- **Nakamoto Consensus**: Longest Chain Rule with readjusting difficulty.
- **P2P Networking (Local)**: Decentralized peer discovery via `libp2p`.
- **Fork Resolution**: Robust chain reorganization to resolve forks and maintain consensus.
- **UTXO Model**: Unspent Transaction Output model for tracking balances.

This is a complete **Full Node** implementation featuring **Quantum Resistance** by replacing standard ECDSA with **Falcon-512** signatures. It protects against future quantum computer attacks while providing a fully functional P2P node with wallet, mempool, and blockchain management.

### Running a Node
To run a node (e.g., node1), specify its data directory:
```bash
cargo run --release -- --datadir node1
```
To run multiple nodes, open separate terminals and change the directory (e.g., `node2`, `node3`).

