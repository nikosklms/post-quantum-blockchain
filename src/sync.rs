use serde::{Deserialize, Serialize};
use crate::block::Block;

/// Direct sync request: ask a specific peer for blocks in a height range
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub start_height: u32,
    pub end_height: u32,
}

/// Direct sync response: a batch of blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    pub blocks: Vec<Block>,
}
