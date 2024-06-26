use std::collections::HashMap;

use bitcoin::{block::Header, BlockHash, FilterHash, FilterHeader};

use crate::chain::checkpoints::HeaderCheckpoint;

use super::{cfheader_batch::CFHeaderBatch, error::CFHeaderSyncError};

type InternalChain = Vec<(FilterHeader, FilterHash)>;

const INITIAL_BUFFER_SIZE: usize = 20_000;

pub(crate) enum AppendAttempt {
    // Nothing to do yet
    AddedToQueue,
    // We sucessfully extended the current chain and should broadcast the next round of CF header messages
    Extended,
    // We found a conflict in the peers CF header messages at this index
    Conflict(u32),
}

// Mapping from an append attempt to a message the node can handle
pub(crate) enum CFHeaderSyncResult {
    AddedToQueue,
    ReadyForNext,
    Dispute(BlockHash),
}
#[derive(Debug)]
pub(crate) struct CFHeaderChain {
    anchor_checkpoint: HeaderCheckpoint,
    header_chain: InternalChain,
    merged_queue: HashMap<u32, InternalChain>,
    block_to_hash: HashMap<BlockHash, FilterHash>,
    prev_stophash_request: Option<BlockHash>,
    quorum_required: usize,
}

impl CFHeaderChain {
    pub(crate) fn new(anchor_checkpoint: HeaderCheckpoint, quorum_required: usize) -> Self {
        Self {
            anchor_checkpoint,
            header_chain: Vec::new(),
            merged_queue: HashMap::new(),
            block_to_hash: HashMap::with_capacity(INITIAL_BUFFER_SIZE),
            prev_stophash_request: None,
            quorum_required,
        }
    }

    pub(crate) async fn append(
        &mut self,
        peer_id: u32,
        cf_headers: CFHeaderBatch,
    ) -> Result<AppendAttempt, CFHeaderSyncError> {
        self.merged_queue.insert(peer_id, cf_headers.inner());
        self.try_merge().await
    }

    async fn try_merge(&mut self) -> Result<AppendAttempt, CFHeaderSyncError> {
        let staged_headers = self.merged_queue.values().count();
        if staged_headers.ge(&self.quorum_required) {
            self.append_or_conflict().await
        } else {
            Ok(AppendAttempt::AddedToQueue)
        }
    }

    async fn append_or_conflict(&mut self) -> Result<AppendAttempt, CFHeaderSyncError> {
        let ready = self
            .merged_queue
            .values_mut()
            .collect::<Vec<&mut Vec<(FilterHeader, FilterHash)>>>();
        // Take any reference from the queue, we will start comparing the other peers to this one
        let reference_peer = ready.first().expect("all quorums have at least one peer");
        // Move over the peers, skipping the reference
        for peer in ready.iter().skip(1) {
            // Iterate over each index in the reference
            for index in 0..reference_peer.len() {
                // Take the reference header
                let (header, _) = reference_peer[index];
                // Compare it to the other peer
                if let Some(comparitor) = peer.get(index) {
                    if header.ne(&comparitor.0) {
                        return Ok(AppendAttempt::Conflict(self.height() + index as u32 + 1));
                    }
                }
            }
        }
        // Made it through without finding any conflicts, we can extend the current chain by the reference
        self.header_chain.extend_from_slice(reference_peer);
        // Reset the merge queue
        self.merged_queue.clear();
        Ok(AppendAttempt::Extended)
    }

    pub(crate) fn height(&self) -> u32 {
        self.anchor_checkpoint.height + self.header_chain.len() as u32
    }

    pub(crate) fn prev_header(&self) -> Option<FilterHeader> {
        if self.header_chain.is_empty() {
            None
        } else {
            Some(self.header_chain.last().unwrap().0)
        }
    }

    pub(crate) fn set_last_stop_hash(&mut self, stop_hash: BlockHash) {
        self.prev_stophash_request = Some(stop_hash)
    }

    pub(crate) fn last_stop_hash_request(&mut self) -> &Option<BlockHash> {
        &self.prev_stophash_request
    }

    fn adjusted_height(&self, height: u32) -> Option<u32> {
        height.checked_sub(self.anchor_checkpoint.height + 1)
    }

    pub(crate) fn filter_hash_at_height(&self, height: u32) -> Option<FilterHash> {
        let adjusted_height = self.adjusted_height(height);
        match adjusted_height {
            Some(height) => {
                if let Some((_, hash)) = self.header_chain.get(height as usize) {
                    Some(*hash)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    pub(crate) fn map_len(&self) -> usize {
        self.block_to_hash.len()
    }

    pub(crate) fn clear_queue(&mut self) {
        self.merged_queue.clear()
    }

    pub(crate) async fn join(&mut self, headers: &[Header]) {
        headers
            .iter()
            .zip(self.header_chain.iter().map(|(_, hash)| hash))
            .for_each(|(header, hash)| {
                self.block_to_hash.insert(header.block_hash(), *hash);
            })
    }

    pub(crate) fn hash_at(&self, block: &BlockHash) -> Option<&FilterHash> {
        self.block_to_hash.get(block)
    }

    pub(crate) fn quorum_required(&self) -> usize {
        self.quorum_required
    }
}
