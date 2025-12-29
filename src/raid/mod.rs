//! RAID-style erasure coding across multiple Telegram accounts
//!
//! Provides Reed-Solomon erasure coding with configurable K-of-N recovery.
//! Presets: RAID5 (N-1 of N), RAID6 (N-2 of N), or custom K/N.

pub mod config;
pub mod erasure;
pub mod stripe;
pub mod health;

pub use config::{ErasureConfig, ErasurePreset, AccountConfig, PoolConfig};
pub use erasure::Encoder;
pub use stripe::{Stripe, StripeManager};
pub use health::{AccountHealth, AccountStatus, ArrayStatus};
