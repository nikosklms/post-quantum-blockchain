# Conflict Scenario Experiment

To create a conflict, we need two nodes to have different versions of the blockchain (a "fork").
Since normal `mine` commands broadcast immediately, nodes tend to stay in sync.
We have added a `mine_local` command to force a hidden fork.

## Steps

### 1. Start Two Nodes
**Terminal 1 (Node A):**
`cargo run`

**Terminal 2 (Node B):**
`cargo run`

*Wait for discovery (mDNS logs).*

### 2. Create a Fork
We will create two conflicting chains starting from the same Genesis block.

**On Node A (The "Short" Chain):**
We'll mine one block locally so Node B doesn't see it yet.
```bash
mine_local bob 10
# Node A Height: 2 (Genesis + Block 1A)
```

**On Node B (The "Long" Chain):**
We'll mine two blocks locally so Node A doesn't see them yet.
```bash
mine_local dave 5
mine_local dave 5
# Node B Height: 3 (Genesis + Block 1B + Block 2B)
```

**Result:**
- Node A has a chain of length 2 ending in Block 1A.
- Node B has a chain of length 3 ending in Block 2B.
- They are in conflict! (Both have parent=Genesis, but different blocks).

### 3. Resolve Conflict (Consensus)
According to the Longest Chain Rule, the longest valid chain wins. Node B has the longer chain (Length 3).

**On Node B:**
Broadcast your longer chain to the network manually.
```bash
sync
```

**On Node A:**
Watch the logs. You should see:
`Received full chain from peer ... (len 3)`
`Replacing current chain with new longer chain (len 3)`

### 4. Verify
**On Node A:** run `chain`.
You should see 3 blocks (Genesis, Block 1B, Block 2B).
Your local Block 1A has been discarded (orphaned) because the network consensus chose the longer chain.

## How Sync Works

1.  **When**: In this experiment, sync happens **manually when you type `sync`**.
    - We use `mine_local` to simulate a network partition (blocks hidden from peers).
    - Typing `sync` acts like the connection being restored.

2.  **How**: The `sync` command serializes and broadcasts the **entire blockchain** (headers + transactions) as a JSON object.
    - Peers receive the full chain.
    - If the received chain is **longer** and **valid**, they replace their current chain with it.
    - This is a simplified implementation. Real blockchains (like Bitcoin) typically exchange **headers first** (to check length/work) and then download only the missing blocks to save bandwidth. here we send everything for simplicity.
