//! Metadata storage module
//!
//! Stores encrypted filesystem metadata in SQLite.
//! All metadata is encrypted before storage using the metadata key.

mod hardlinks;
mod inode;
mod store;
mod version;
mod xattr;

pub use hardlinks::HardLinkStore;
pub use inode::{FileType, Inode, InodeAttributes};
pub use store::MetadataStore;
pub use version::{FileVersion, VersionManager};
pub use xattr::XattrStore;
