//! Hard link tracking for Time Machine deduplication support
//!
//! Maintains mappings between inodes and their associated paths, enabling
//! proper hard link semantics for backup systems like Time Machine.

use crate::error::{Error, Result};
use sled::{Db, Tree};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Hard link tracker using sled database
///
/// Tracks the relationship between inodes and paths, maintaining:
/// - Link count per inode
/// - Multiple paths pointing to the same inode
pub struct HardLinkStore {
    /// Sled database
    db: Db,
    /// Inode -> link count tree
    link_counts: Tree,
    /// Inode -> paths mapping tree
    inode_paths: Tree,
}

impl HardLinkStore {
    /// Open or create a hard link store
    ///
    /// # Arguments
    /// * `path` - Path to the database directory
    ///
    /// # Returns
    /// A new `HardLinkStore` instance
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = sled::open(path.as_ref())?;
        let link_counts = db.open_tree("link_counts")?;
        let inode_paths = db.open_tree("inode_paths")?;

        debug!("Opened hard link store at {:?}", path.as_ref());

        Ok(HardLinkStore {
            db,
            link_counts,
            inode_paths,
        })
    }

    /// Create a new hard link for an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    /// * `path` - The path to associate with this inode
    ///
    /// # Returns
    /// The new link count after adding this link
    ///
    /// # Errors
    /// Returns an error if the database operation fails
    pub fn create_link(&self, inode: u64, path: &Path) -> Result<u64> {
        let inode_key = inode.to_be_bytes();

        // Get current paths for this inode
        let mut paths = self.get_paths_internal(inode)?;

        // Check if this path already exists
        if paths.contains(&path.to_path_buf()) {
            debug!("Hard link already exists: inode={}, path={:?}", inode, path);
            return Ok(paths.len() as u64);
        }

        // Add the new path
        paths.push(path.to_path_buf());

        // Serialize and store the updated paths
        let paths_bytes = bincode::serialize(&paths)?;
        self.inode_paths.insert(&inode_key, paths_bytes)?;

        // Update link count
        let new_count = paths.len() as u64;
        self.link_counts
            .insert(&inode_key, &new_count.to_be_bytes())?;

        debug!(
            "Created hard link: inode={}, path={:?}, count={}",
            inode, path, new_count
        );

        Ok(new_count)
    }

    /// Remove a hard link from an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    /// * `path` - The path to remove
    ///
    /// # Returns
    /// The new link count after removing this link
    ///
    /// # Errors
    /// Returns an error if the database operation fails or the path doesn't exist
    pub fn remove_link(&self, inode: u64, path: &Path) -> Result<u64> {
        let inode_key = inode.to_be_bytes();
        let path_str = path.to_string_lossy().to_string();

        // Get current paths for this inode
        let mut paths = self.get_paths_internal(inode)?;

        // Find and remove the path
        let original_len = paths.len();
        paths.retain(|p| p != path);

        if paths.len() == original_len {
            warn!(
                "Attempted to remove non-existent hard link: inode={}, path={:?}",
                inode, path
            );
            return Err(Error::PathNotFound(path_str));
        }

        let new_count = paths.len() as u64;

        if new_count == 0 {
            // Last link removed, clean up completely
            self.inode_paths.remove(&inode_key)?;
            self.link_counts.remove(&inode_key)?;
            debug!(
                "Removed last hard link: inode={}, path={:?}",
                inode, path
            );
        } else {
            // Update paths and count
            let paths_bytes = bincode::serialize(&paths)?;
            self.inode_paths.insert(&inode_key, paths_bytes)?;
            self.link_counts
                .insert(&inode_key, &new_count.to_be_bytes())?;
            debug!(
                "Removed hard link: inode={}, path={:?}, remaining={}",
                inode, path, new_count
            );
        }

        Ok(new_count)
    }

    /// Get the link count for an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    ///
    /// # Returns
    /// The number of hard links to this inode (0 if not tracked)
    pub fn get_link_count(&self, inode: u64) -> u64 {
        let inode_key = inode.to_be_bytes();

        self.link_counts
            .get(&inode_key)
            .ok()
            .flatten()
            .and_then(|bytes| {
                if bytes.len() >= 8 {
                    let count_bytes: [u8; 8] = bytes[..8].try_into().ok()?;
                    Some(u64::from_be_bytes(count_bytes))
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    /// Get all paths associated with an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    ///
    /// # Returns
    /// A vector of paths pointing to this inode (empty if not tracked)
    pub fn get_paths(&self, inode: u64) -> Vec<PathBuf> {
        self.get_paths_internal(inode).unwrap_or_default()
    }

    /// Check if this is the last hard link to an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    ///
    /// # Returns
    /// `true` if this is the last (or only) link, `false` otherwise
    pub fn is_last_link(&self, inode: u64) -> bool {
        self.get_link_count(inode) <= 1
    }

    /// Internal method to get paths with error handling
    fn get_paths_internal(&self, inode: u64) -> Result<Vec<PathBuf>> {
        let inode_key = inode.to_be_bytes();

        match self.inode_paths.get(&inode_key)? {
            Some(bytes) => {
                let paths: Vec<PathBuf> = bincode::deserialize(&bytes)?;
                Ok(paths)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Flush all pending writes to disk
    ///
    /// # Errors
    /// Returns an error if the flush operation fails
    pub fn flush(&self) -> Result<()> {
        self.db.flush()?;
        debug!("Flushed hard link store to disk");
        Ok(())
    }

    /// Get the total number of tracked inodes
    ///
    /// # Returns
    /// The number of inodes with hard links
    pub fn inode_count(&self) -> usize {
        self.link_counts.len()
    }

    /// Remove all tracking data for an inode
    ///
    /// Useful for cleanup operations when an inode is deleted.
    ///
    /// # Arguments
    /// * `inode` - The inode number to remove
    ///
    /// # Errors
    /// Returns an error if the database operation fails
    pub fn remove_inode(&self, inode: u64) -> Result<()> {
        let inode_key = inode.to_be_bytes();
        self.inode_paths.remove(&inode_key)?;
        self.link_counts.remove(&inode_key)?;
        debug!("Removed all hard link data for inode={}", inode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_create_and_get_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 100;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        // Create first link
        let count = store.create_link(inode, &path1).unwrap();
        assert_eq!(count, 1);
        assert_eq!(store.get_link_count(inode), 1);

        // Create second link
        let count = store.create_link(inode, &path2).unwrap();
        assert_eq!(count, 2);
        assert_eq!(store.get_link_count(inode), 2);

        // Check paths
        let paths = store.get_paths(inode);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&path1));
        assert!(paths.contains(&path2));
    }

    #[test]
    fn test_remove_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 200;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        // Create two links
        store.create_link(inode, &path1).unwrap();
        store.create_link(inode, &path2).unwrap();

        // Remove one link
        let count = store.remove_link(inode, &path1).unwrap();
        assert_eq!(count, 1);
        assert_eq!(store.get_link_count(inode), 1);

        let paths = store.get_paths(inode);
        assert_eq!(paths.len(), 1);
        assert!(paths.contains(&path2));
        assert!(!paths.contains(&path1));
    }

    #[test]
    fn test_remove_last_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 300;
        let path = PathBuf::from("/test/path");

        // Create and remove link
        store.create_link(inode, &path).unwrap();
        let count = store.remove_link(inode, &path).unwrap();

        assert_eq!(count, 0);
        assert_eq!(store.get_link_count(inode), 0);
        assert!(store.get_paths(inode).is_empty());
    }

    #[test]
    fn test_is_last_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 400;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        // No links
        assert!(store.is_last_link(inode));

        // One link
        store.create_link(inode, &path1).unwrap();
        assert!(store.is_last_link(inode));

        // Two links
        store.create_link(inode, &path2).unwrap();
        assert!(!store.is_last_link(inode));

        // Back to one link
        store.remove_link(inode, &path1).unwrap();
        assert!(store.is_last_link(inode));
    }

    #[test]
    fn test_duplicate_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 500;
        let path = PathBuf::from("/test/path");

        // Create same link twice
        let count1 = store.create_link(inode, &path).unwrap();
        let count2 = store.create_link(inode, &path).unwrap();

        assert_eq!(count1, 1);
        assert_eq!(count2, 1);
        assert_eq!(store.get_link_count(inode), 1);
    }

    #[test]
    fn test_remove_nonexistent_link() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 600;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        store.create_link(inode, &path1).unwrap();

        // Try to remove a path that doesn't exist
        let result = store.remove_link(inode, &path2);
        assert!(result.is_err());
    }

    #[test]
    fn test_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let inode = 700;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        // Create links and close store
        {
            let store = HardLinkStore::open(temp_dir.path()).unwrap();
            store.create_link(inode, &path1).unwrap();
            store.create_link(inode, &path2).unwrap();
            store.flush().unwrap();
        }

        // Reopen and verify
        {
            let store = HardLinkStore::open(temp_dir.path()).unwrap();
            assert_eq!(store.get_link_count(inode), 2);
            let paths = store.get_paths(inode);
            assert_eq!(paths.len(), 2);
            assert!(paths.contains(&path1));
            assert!(paths.contains(&path2));
        }
    }

    #[test]
    fn test_inode_count() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        assert_eq!(store.inode_count(), 0);

        store.create_link(100, &PathBuf::from("/test/path1")).unwrap();
        assert_eq!(store.inode_count(), 1);

        store.create_link(200, &PathBuf::from("/test/path2")).unwrap();
        assert_eq!(store.inode_count(), 2);

        store.create_link(100, &PathBuf::from("/test/path3")).unwrap();
        assert_eq!(store.inode_count(), 2);
    }

    #[test]
    fn test_remove_inode() {
        let temp_dir = TempDir::new().unwrap();
        let store = HardLinkStore::open(temp_dir.path()).unwrap();

        let inode = 800;
        let path1 = PathBuf::from("/test/path1");
        let path2 = PathBuf::from("/test/path2");

        store.create_link(inode, &path1).unwrap();
        store.create_link(inode, &path2).unwrap();

        store.remove_inode(inode).unwrap();

        assert_eq!(store.get_link_count(inode), 0);
        assert!(store.get_paths(inode).is_empty());
        assert_eq!(store.inode_count(), 0);
    }
}
