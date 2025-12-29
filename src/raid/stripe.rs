//! Stripe management for distributing chunks across accounts
//!
//! A stripe represents a set of chunks (data + parity) derived from a single
//! data block, distributed across multiple accounts for redundancy.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A stripe represents chunks from one data block distributed across accounts
///
/// Each stripe contains N chunks (K data + parity) that together can
/// reconstruct the original data block. Any K chunks are sufficient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stripe {
    /// Unique stripe identifier
    pub stripe_id: u64,

    /// Original data size before encoding
    pub original_size: u64,

    /// Chunk size in bytes
    pub chunk_size: usize,

    /// Number of data chunks (K)
    pub data_chunks: usize,

    /// Total chunks including parity (N)
    pub total_chunks: usize,

    /// Chunk locations indexed by chunk number (0 to N-1)
    pub chunks: Vec<ChunkLocation>,

    /// Timestamp when stripe was created (Unix seconds)
    pub created_at: i64,
}

/// Location of a chunk within the storage pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocation {
    /// Chunk index within the stripe (0 to N-1)
    pub chunk_index: u8,

    /// Account ID where this chunk is stored
    pub account_id: u8,

    /// Message ID in the account's Saved Messages
    pub message_id: Option<i32>,

    /// Whether this is a data chunk (vs parity)
    pub is_data: bool,

    /// Hash of the chunk data for verification
    pub hash: Option<String>,

    /// Whether this chunk has been verified as readable
    pub verified: bool,
}

impl ChunkLocation {
    /// Create a new chunk location
    pub fn new(chunk_index: u8, account_id: u8, is_data: bool) -> Self {
        ChunkLocation {
            chunk_index,
            account_id,
            message_id: None,
            is_data,
            hash: None,
            verified: false,
        }
    }

    /// Set the message ID after upload
    pub fn with_message_id(mut self, message_id: i32) -> Self {
        self.message_id = Some(message_id);
        self
    }

    /// Set the hash for verification
    pub fn with_hash(mut self, hash: String) -> Self {
        self.hash = Some(hash);
        self
    }

    /// Mark as verified
    pub fn mark_verified(mut self) -> Self {
        self.verified = true;
        self
    }
}

impl Stripe {
    /// Create a new stripe with the given ID and parameters
    pub fn new(
        stripe_id: u64,
        original_size: u64,
        chunk_size: usize,
        data_chunks: usize,
        total_chunks: usize,
    ) -> Self {
        Stripe {
            stripe_id,
            original_size,
            chunk_size,
            data_chunks,
            total_chunks,
            chunks: Vec::with_capacity(total_chunks),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }

    /// Add a chunk location to the stripe
    pub fn add_chunk(&mut self, location: ChunkLocation) {
        self.chunks.push(location);
    }

    /// Get chunk location by index
    pub fn get_chunk(&self, index: u8) -> Option<&ChunkLocation> {
        self.chunks.iter().find(|c| c.chunk_index == index)
    }

    /// Get all chunks for a specific account
    pub fn chunks_for_account(&self, account_id: u8) -> Vec<&ChunkLocation> {
        self.chunks
            .iter()
            .filter(|c| c.account_id == account_id)
            .collect()
    }

    /// Check if all chunks have been uploaded (have message IDs)
    pub fn is_complete(&self) -> bool {
        self.chunks.len() == self.total_chunks
            && self.chunks.iter().all(|c| c.message_id.is_some())
    }

    /// Count available chunks (those with message IDs)
    pub fn available_count(&self) -> usize {
        self.chunks.iter().filter(|c| c.message_id.is_some()).count()
    }

    /// Check if stripe can be reconstructed (has at least K chunks)
    pub fn can_reconstruct(&self) -> bool {
        self.available_count() >= self.data_chunks
    }

    /// Get missing chunk indices
    pub fn missing_chunks(&self) -> Vec<u8> {
        let present: std::collections::HashSet<u8> =
            self.chunks.iter().map(|c| c.chunk_index).collect();

        (0..self.total_chunks as u8)
            .filter(|i| !present.contains(i))
            .collect()
    }
}

/// Manages stripe creation and distribution across accounts
pub struct StripeManager {
    /// Number of data chunks (K)
    data_chunks: usize,

    /// Total chunks (N)
    total_chunks: usize,

    /// Next stripe ID to assign
    next_stripe_id: u64,

    /// Account assignment strategy
    assignment_strategy: AssignmentStrategy,
}

/// Strategy for assigning chunks to accounts
#[derive(Debug, Clone, Copy, Default)]
pub enum AssignmentStrategy {
    /// Round-robin across all accounts
    #[default]
    RoundRobin,

    /// Distribute evenly based on current load
    LoadBalanced,

    /// Assign to accounts with highest priority first
    PriorityBased,
}

impl StripeManager {
    /// Create a new stripe manager
    pub fn new(data_chunks: usize, total_chunks: usize) -> Self {
        StripeManager {
            data_chunks,
            total_chunks,
            next_stripe_id: 1,
            assignment_strategy: AssignmentStrategy::RoundRobin,
        }
    }

    /// Create with a specific assignment strategy
    pub fn with_strategy(mut self, strategy: AssignmentStrategy) -> Self {
        self.assignment_strategy = strategy;
        self
    }

    /// Set the next stripe ID (for recovery/continuation)
    pub fn set_next_stripe_id(&mut self, id: u64) {
        self.next_stripe_id = id;
    }

    /// Get the number of data chunks
    pub fn data_chunks(&self) -> usize {
        self.data_chunks
    }

    /// Get the total number of chunks
    pub fn total_chunks(&self) -> usize {
        self.total_chunks
    }

    /// Create a new stripe for a data block
    ///
    /// # Arguments
    /// * `original_size` - Size of the original data in bytes
    /// * `chunk_size` - Size of each encoded chunk
    /// * `available_accounts` - List of account IDs available for storage
    ///
    /// # Returns
    /// A new Stripe with chunk assignments
    pub fn create_stripe(
        &mut self,
        original_size: u64,
        chunk_size: usize,
        available_accounts: &[u8],
    ) -> Stripe {
        let stripe_id = self.next_stripe_id;
        self.next_stripe_id += 1;

        let mut stripe = Stripe::new(
            stripe_id,
            original_size,
            chunk_size,
            self.data_chunks,
            self.total_chunks,
        );

        // Assign chunks to accounts
        let assignments = self.assign_chunks(available_accounts);
        for (chunk_index, account_id) in assignments.iter().enumerate() {
            let is_data = chunk_index < self.data_chunks;
            let location = ChunkLocation::new(chunk_index as u8, *account_id, is_data);
            stripe.add_chunk(location);
        }

        stripe
    }

    /// Assign chunks to accounts based on current strategy
    fn assign_chunks(&self, available_accounts: &[u8]) -> Vec<u8> {
        match self.assignment_strategy {
            AssignmentStrategy::RoundRobin => {
                self.assign_round_robin(available_accounts)
            }
            AssignmentStrategy::LoadBalanced | AssignmentStrategy::PriorityBased => {
                // For now, fall back to round-robin
                // Full implementation would track load/priority
                self.assign_round_robin(available_accounts)
            }
        }
    }

    /// Round-robin assignment across accounts
    fn assign_round_robin(&self, available_accounts: &[u8]) -> Vec<u8> {
        let num_accounts = available_accounts.len();
        (0..self.total_chunks)
            .map(|i| available_accounts[i % num_accounts])
            .collect()
    }

    /// Calculate how chunks should be redistributed when an account fails
    ///
    /// # Arguments
    /// * `stripe` - The stripe to redistribute
    /// * `failed_account` - Account ID that has failed
    /// * `available_accounts` - Remaining available accounts
    ///
    /// # Returns
    /// Map of chunk_index -> new_account_id for chunks that need to be moved
    pub fn plan_redistribution(
        &self,
        stripe: &Stripe,
        failed_account: u8,
        available_accounts: &[u8],
    ) -> HashMap<u8, u8> {
        let mut redistributions = HashMap::new();

        // Find chunks on the failed account
        let affected_chunks: Vec<u8> = stripe
            .chunks
            .iter()
            .filter(|c| c.account_id == failed_account)
            .map(|c| c.chunk_index)
            .collect();

        // Exclude the failed account from available accounts
        let remaining: Vec<u8> = available_accounts
            .iter()
            .copied()
            .filter(|&id| id != failed_account)
            .collect();

        if remaining.is_empty() {
            return redistributions;
        }

        // Assign affected chunks to remaining accounts (round-robin)
        for (i, chunk_index) in affected_chunks.iter().enumerate() {
            let new_account = remaining[i % remaining.len()];
            redistributions.insert(*chunk_index, new_account);
        }

        redistributions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stripe_creation() {
        let stripe = Stripe::new(1, 1024, 512, 2, 3);
        assert_eq!(stripe.stripe_id, 1);
        assert_eq!(stripe.original_size, 1024);
        assert_eq!(stripe.chunk_size, 512);
        assert_eq!(stripe.data_chunks, 2);
        assert_eq!(stripe.total_chunks, 3);
        assert!(stripe.chunks.is_empty());
    }

    #[test]
    fn test_chunk_location() {
        let location = ChunkLocation::new(0, 1, true)
            .with_message_id(12345)
            .with_hash("abc123".to_string())
            .mark_verified();

        assert_eq!(location.chunk_index, 0);
        assert_eq!(location.account_id, 1);
        assert!(location.is_data);
        assert_eq!(location.message_id, Some(12345));
        assert_eq!(location.hash, Some("abc123".to_string()));
        assert!(location.verified);
    }

    #[test]
    fn test_stripe_add_chunks() {
        let mut stripe = Stripe::new(1, 1024, 512, 2, 3);

        stripe.add_chunk(ChunkLocation::new(0, 0, true));
        stripe.add_chunk(ChunkLocation::new(1, 1, true));
        stripe.add_chunk(ChunkLocation::new(2, 2, false));

        assert_eq!(stripe.chunks.len(), 3);
        assert!(stripe.get_chunk(0).is_some());
        assert!(stripe.get_chunk(1).is_some());
        assert!(stripe.get_chunk(2).is_some());
        assert!(stripe.get_chunk(3).is_none());
    }

    #[test]
    fn test_stripe_is_complete() {
        let mut stripe = Stripe::new(1, 1024, 512, 2, 3);

        stripe.add_chunk(ChunkLocation::new(0, 0, true).with_message_id(1));
        stripe.add_chunk(ChunkLocation::new(1, 1, true).with_message_id(2));

        // Not complete - missing chunk 2
        assert!(!stripe.is_complete());

        stripe.add_chunk(ChunkLocation::new(2, 2, false).with_message_id(3));

        // Now complete
        assert!(stripe.is_complete());
    }

    #[test]
    fn test_stripe_can_reconstruct() {
        let mut stripe = Stripe::new(1, 1024, 512, 2, 3);

        // Add 2 chunks with message IDs (K=2)
        stripe.add_chunk(ChunkLocation::new(0, 0, true).with_message_id(1));
        stripe.add_chunk(ChunkLocation::new(1, 1, true).with_message_id(2));

        assert!(stripe.can_reconstruct());
        assert_eq!(stripe.available_count(), 2);
    }

    #[test]
    fn test_stripe_missing_chunks() {
        let mut stripe = Stripe::new(1, 1024, 512, 2, 3);

        stripe.add_chunk(ChunkLocation::new(0, 0, true));
        stripe.add_chunk(ChunkLocation::new(2, 2, false));

        let missing = stripe.missing_chunks();
        assert_eq!(missing, vec![1]);
    }

    #[test]
    fn test_stripe_chunks_for_account() {
        let mut stripe = Stripe::new(1, 1024, 512, 2, 4);

        stripe.add_chunk(ChunkLocation::new(0, 0, true));
        stripe.add_chunk(ChunkLocation::new(1, 1, true));
        stripe.add_chunk(ChunkLocation::new(2, 0, false)); // Same account as chunk 0
        stripe.add_chunk(ChunkLocation::new(3, 1, false));

        let account_0_chunks = stripe.chunks_for_account(0);
        assert_eq!(account_0_chunks.len(), 2);
        assert_eq!(account_0_chunks[0].chunk_index, 0);
        assert_eq!(account_0_chunks[1].chunk_index, 2);
    }

    #[test]
    fn test_stripe_manager_creation() {
        let manager = StripeManager::new(3, 5);
        assert_eq!(manager.data_chunks(), 3);
        assert_eq!(manager.total_chunks(), 5);
    }

    #[test]
    fn test_stripe_manager_create_stripe() {
        let mut manager = StripeManager::new(2, 3);
        let accounts = vec![0, 1, 2];

        let stripe = manager.create_stripe(1024, 512, &accounts);

        assert_eq!(stripe.stripe_id, 1);
        assert_eq!(stripe.chunks.len(), 3);
        assert_eq!(stripe.data_chunks, 2);
        assert_eq!(stripe.total_chunks, 3);

        // Check assignments
        for chunk in &stripe.chunks {
            assert!(accounts.contains(&chunk.account_id));
        }

        // First 2 chunks should be data, last should be parity
        assert!(stripe.chunks[0].is_data);
        assert!(stripe.chunks[1].is_data);
        assert!(!stripe.chunks[2].is_data);
    }

    #[test]
    fn test_stripe_manager_increments_id() {
        let mut manager = StripeManager::new(2, 3);
        let accounts = vec![0, 1, 2];

        let stripe1 = manager.create_stripe(1024, 512, &accounts);
        let stripe2 = manager.create_stripe(2048, 512, &accounts);

        assert_eq!(stripe1.stripe_id, 1);
        assert_eq!(stripe2.stripe_id, 2);
    }

    #[test]
    fn test_stripe_manager_round_robin() {
        let mut manager = StripeManager::new(4, 6);
        let accounts = vec![0, 1, 2];

        let stripe = manager.create_stripe(1024, 256, &accounts);

        // With 3 accounts and 6 chunks, should see pattern: 0, 1, 2, 0, 1, 2
        assert_eq!(stripe.chunks[0].account_id, 0);
        assert_eq!(stripe.chunks[1].account_id, 1);
        assert_eq!(stripe.chunks[2].account_id, 2);
        assert_eq!(stripe.chunks[3].account_id, 0);
        assert_eq!(stripe.chunks[4].account_id, 1);
        assert_eq!(stripe.chunks[5].account_id, 2);
    }

    #[test]
    fn test_plan_redistribution() {
        let mut manager = StripeManager::new(2, 3);
        let accounts = vec![0, 1, 2];

        let stripe = manager.create_stripe(1024, 512, &accounts);

        // Plan redistribution if account 0 fails
        let remaining = vec![1, 2];
        let plan = manager.plan_redistribution(&stripe, 0, &remaining);

        // Should have a plan for chunk 0 (was on account 0)
        assert!(plan.contains_key(&0));
        let new_account = plan.get(&0).unwrap();
        assert!(remaining.contains(new_account));
    }

    #[test]
    fn test_set_next_stripe_id() {
        let mut manager = StripeManager::new(2, 3);
        manager.set_next_stripe_id(100);

        let accounts = vec![0, 1, 2];
        let stripe = manager.create_stripe(1024, 512, &accounts);

        assert_eq!(stripe.stripe_id, 100);
    }

    #[test]
    fn test_assignment_strategy() {
        let manager = StripeManager::new(2, 3)
            .with_strategy(AssignmentStrategy::LoadBalanced);

        assert_eq!(manager.data_chunks(), 2);
    }
}
