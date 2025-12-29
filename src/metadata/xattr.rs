//! Extended attributes (xattr) storage
//!
//! Stores extended attributes for filesystem inodes using sled database.
//! Supports standard xattr operations: get, set, list, remove.
//! Supports Apple-specific xattr namespaces (com.apple.*, user.*, etc.)

use crate::error::{Error, Result};
use sled::{Db, Tree};
use std::path::Path;
use tracing::{debug, trace};

/// Maximum xattr name length (Linux standard)
const XATTR_NAME_MAX: usize = 255;

/// Maximum xattr value size (64KB)
const XATTR_SIZE_MAX: usize = 65536;

/// Extended attribute store using sled
///
/// Stores xattrs keyed by (inode_id, xattr_name) -> value.
/// All xattr values are stored as raw bytes.
pub struct XattrStore {
    /// Sled database reference
    #[allow(dead_code)]
    db: Db,
    /// Extended attributes tree
    xattrs: Tree,
}

impl XattrStore {
    /// Open or create an xattr store at the given path
    ///
    /// # Arguments
    /// * `path` - Path to the database directory
    ///
    /// # Returns
    /// A new XattrStore instance
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = sled::open(path.as_ref())?;
        let xattrs = db.open_tree("xattrs")?;

        debug!("XattrStore opened at {:?}", path.as_ref());
        Ok(Self { db, xattrs })
    }

    /// Create an in-memory xattr store (primarily for testing)
    ///
    /// # Returns
    /// A new in-memory XattrStore instance
    ///
    /// # Errors
    /// Returns an error if the temporary database cannot be created
    #[allow(dead_code)]
    pub fn in_memory() -> Result<Self> {
        let db = sled::Config::new().temporary(true).open()?;
        let xattrs = db.open_tree("xattrs")?;

        debug!("In-memory XattrStore created");
        Ok(Self { db, xattrs })
    }

    /// Create a composite key from inode and xattr name
    ///
    /// Key format: 8 bytes (inode as big-endian u64) + xattr name bytes
    fn make_key(inode: u64, name: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(8 + name.len());
        key.extend_from_slice(&inode.to_be_bytes());
        key.extend_from_slice(name.as_bytes());
        key
    }

    /// Create a prefix key for scanning all xattrs of an inode
    ///
    /// Returns the 8-byte inode prefix for range scans
    fn make_prefix(inode: u64) -> [u8; 8] {
        inode.to_be_bytes()
    }

    /// Validate xattr name
    ///
    /// Ensures the name is not empty and contains valid characters.
    /// Common namespaces on macOS:
    /// - com.apple.* (Apple system attributes)
    /// - user.* (User-defined attributes)
    /// - security.* (Security-related attributes)
    /// - system.* (System attributes)
    fn validate_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(Error::Internal("Extended attribute name cannot be empty".to_string()));
        }

        if name.len() > XATTR_NAME_MAX {
            return Err(Error::Internal(format!(
                "Extended attribute name too long: {} bytes (max {})",
                name.len(),
                XATTR_NAME_MAX
            )));
        }

        // Ensure name doesn't contain null bytes (required for proper storage)
        if name.contains('\0') {
            return Err(Error::Internal("Extended attribute name cannot contain null bytes".to_string()));
        }

        Ok(())
    }

    /// Set an extended attribute
    ///
    /// # Arguments
    /// * `inode` - The inode number
    /// * `name` - The xattr name (e.g., "com.apple.metadata:kMDItemWhereFroms")
    /// * `value` - The xattr value as bytes
    ///
    /// # Returns
    /// Ok(()) on success
    ///
    /// # Errors
    /// Returns an error if the name is invalid or database operation fails
    pub fn set(&self, inode: u64, name: &str, value: &[u8]) -> Result<()> {
        Self::validate_name(name)?;

        if value.len() > XATTR_SIZE_MAX {
            return Err(Error::Internal(format!(
                "Extended attribute value too large: {} bytes (max {})",
                value.len(),
                XATTR_SIZE_MAX
            )));
        }

        let key = Self::make_key(inode, name);
        self.xattrs.insert(key, value)?;

        trace!("Set xattr {} for inode {} ({} bytes)", name, inode, value.len());
        Ok(())
    }

    /// Get an extended attribute
    ///
    /// # Arguments
    /// * `inode` - The inode number
    /// * `name` - The xattr name
    ///
    /// # Returns
    /// Some(value) if the xattr exists, None otherwise
    ///
    /// # Errors
    /// Returns an error if the name is invalid or database operation fails
    pub fn get(&self, inode: u64, name: &str) -> Result<Option<Vec<u8>>> {
        Self::validate_name(name)?;

        let key = Self::make_key(inode, name);
        match self.xattrs.get(key)? {
            Some(value) => {
                trace!("Got xattr {} for inode {} ({} bytes)", name, inode, value.len());
                Ok(Some(value.to_vec()))
            }
            None => {
                trace!("Xattr {} not found for inode {}", name, inode);
                Ok(None)
            }
        }
    }

    /// List all extended attribute names for an inode
    ///
    /// # Arguments
    /// * `inode` - The inode number
    ///
    /// # Returns
    /// A vector of xattr names
    ///
    /// # Errors
    /// Returns an error if database operation fails
    pub fn list(&self, inode: u64) -> Result<Vec<String>> {
        let prefix = Self::make_prefix(inode);
        let mut names = Vec::new();

        for result in self.xattrs.scan_prefix(&prefix) {
            let (key, _) = result?;

            // Extract the name portion (everything after the 8-byte inode prefix)
            if key.len() > 8 {
                let name_bytes = &key[8..];
                match std::str::from_utf8(name_bytes) {
                    Ok(name) => names.push(name.to_string()),
                    Err(e) => {
                        // Log but continue - shouldn't happen with valid UTF-8 names
                        debug!("Invalid UTF-8 in xattr name for inode {}: {}", inode, e);
                    }
                }
            }
        }

        trace!("Listed {} xattrs for inode {}", names.len(), inode);
        Ok(names)
    }

    /// Remove an extended attribute
    ///
    /// # Arguments
    /// * `inode` - The inode number
    /// * `name` - The xattr name
    ///
    /// # Returns
    /// Ok(()) on success (even if the xattr didn't exist)
    ///
    /// # Errors
    /// Returns an error if the name is invalid or database operation fails
    pub fn remove(&self, inode: u64, name: &str) -> Result<()> {
        Self::validate_name(name)?;

        let key = Self::make_key(inode, name);
        self.xattrs.remove(key)?;

        trace!("Removed xattr {} for inode {}", name, inode);
        Ok(())
    }

    /// Remove all extended attributes for an inode
    ///
    /// This is typically called when deleting a file/directory.
    ///
    /// # Arguments
    /// * `inode` - The inode number
    ///
    /// # Returns
    /// The number of xattrs removed
    ///
    /// # Errors
    /// Returns an error if database operation fails
    pub fn remove_all(&self, inode: u64) -> Result<usize> {
        let prefix = Self::make_prefix(inode);
        let mut count = 0;

        // Collect keys to remove (can't remove while iterating)
        let mut keys_to_remove = Vec::new();
        for result in self.xattrs.scan_prefix(&prefix) {
            let (key, _) = result?;
            keys_to_remove.push(key.to_vec());
        }

        // Remove all collected keys
        for key in keys_to_remove {
            self.xattrs.remove(key)?;
            count += 1;
        }

        debug!("Removed {} xattrs for inode {}", count, inode);
        Ok(count)
    }

    /// Flush all pending changes to disk
    ///
    /// # Returns
    /// Ok(()) on success
    ///
    /// # Errors
    /// Returns an error if the flush operation fails
    pub fn flush(&self) -> Result<()> {
        self.xattrs.flush()?;
        Ok(())
    }

    /// Get the total number of extended attributes in the store
    ///
    /// Primarily useful for statistics and testing.
    ///
    /// # Returns
    /// The total count of xattrs across all inodes
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.xattrs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_store() {
        let store = XattrStore::in_memory().unwrap();
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn test_set_and_get() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.test";
        let value = b"test value";

        store.set(inode, name, value).unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_get_nonexistent() {
        let store = XattrStore::in_memory().unwrap();

        let result = store.get(42, "user.nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_apple_namespace() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 100;
        let name = "com.apple.metadata:kMDItemWhereFroms";
        let value = b"https://example.com";

        store.set(inode, name, value).unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_list_empty() {
        let store = XattrStore::in_memory().unwrap();

        let names = store.list(42).unwrap();
        assert_eq!(names.len(), 0);
    }

    #[test]
    fn test_list_multiple() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        store.set(inode, "user.attr1", b"value1").unwrap();
        store.set(inode, "user.attr2", b"value2").unwrap();
        store.set(inode, "com.apple.test", b"value3").unwrap();

        let mut names = store.list(inode).unwrap();
        names.sort();

        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "com.apple.test");
        assert_eq!(names[1], "user.attr1");
        assert_eq!(names[2], "user.attr2");
    }

    #[test]
    fn test_list_isolation() {
        let store = XattrStore::in_memory().unwrap();

        // Different inodes shouldn't interfere
        store.set(1, "user.attr1", b"value1").unwrap();
        store.set(2, "user.attr2", b"value2").unwrap();
        store.set(3, "user.attr3", b"value3").unwrap();

        let names1 = store.list(1).unwrap();
        let names2 = store.list(2).unwrap();
        let names3 = store.list(3).unwrap();

        assert_eq!(names1.len(), 1);
        assert_eq!(names2.len(), 1);
        assert_eq!(names3.len(), 1);
    }

    #[test]
    fn test_remove() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.test";

        store.set(inode, name, b"value").unwrap();
        assert!(store.get(inode, name).unwrap().is_some());

        store.remove(inode, name).unwrap();
        assert!(store.get(inode, name).unwrap().is_none());
    }

    #[test]
    fn test_remove_nonexistent() {
        let store = XattrStore::in_memory().unwrap();

        // Should succeed even if xattr doesn't exist
        store.remove(42, "user.nonexistent").unwrap();
    }

    #[test]
    fn test_remove_all() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        store.set(inode, "user.attr1", b"value1").unwrap();
        store.set(inode, "user.attr2", b"value2").unwrap();
        store.set(inode, "com.apple.test", b"value3").unwrap();

        // Also add xattrs for a different inode
        store.set(100, "user.other", b"other").unwrap();

        let count = store.remove_all(inode).unwrap();
        assert_eq!(count, 3);

        let names = store.list(inode).unwrap();
        assert_eq!(names.len(), 0);

        // Other inode should still have its xattrs
        let other_names = store.list(100).unwrap();
        assert_eq!(other_names.len(), 1);
    }

    #[test]
    fn test_remove_all_empty() {
        let store = XattrStore::in_memory().unwrap();

        let count = store.remove_all(42).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_update_value() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.test";

        store.set(inode, name, b"original").unwrap();
        store.set(inode, name, b"updated").unwrap();

        let value = store.get(inode, name).unwrap().unwrap();
        assert_eq!(value, b"updated");
    }

    #[test]
    fn test_binary_values() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.binary";
        let value: Vec<u8> = vec![0x00, 0xFF, 0x42, 0xAB, 0xCD, 0xEF];

        store.set(inode, name, &value).unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_empty_value() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.empty";
        let value: &[u8] = b"";

        store.set(inode, name, value).unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_large_value() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.large";
        let value = vec![0x42; 10000]; // 10KB of 0x42

        store.set(inode, name, &value).unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_invalid_name_empty() {
        let store = XattrStore::in_memory().unwrap();

        let result = store.set(42, "", b"value");
        assert!(result.is_err());

        let result = store.get(42, "");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_name_null() {
        let store = XattrStore::in_memory().unwrap();

        let result = store.set(42, "user.test\0null", b"value");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_name_too_long() {
        let store = XattrStore::in_memory().unwrap();

        let long_name = format!("user.{}", "a".repeat(XATTR_NAME_MAX));
        let result = store.set(42, &long_name, b"value");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_value_too_large() {
        let store = XattrStore::in_memory().unwrap();

        let large_value = vec![0u8; XATTR_SIZE_MAX + 1];
        let result = store.set(42, "user.test", &large_value);
        assert!(result.is_err());
    }

    #[test]
    fn test_special_characters_in_name() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        // Valid special characters in xattr names
        let names = vec![
            "user.test-attr",
            "user.test_attr",
            "user.test.attr",
            "com.apple.metadata:kMDItemWhereFroms",
            "security.selinux",
        ];

        for name in names {
            store.set(inode, name, b"value").unwrap();
            let retrieved = store.get(inode, name).unwrap().unwrap();
            assert_eq!(retrieved, b"value");
        }
    }

    #[test]
    fn test_long_name() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let long_name = format!("user.{}", "a".repeat(200));

        store.set(inode, &long_name, b"value").unwrap();
        let retrieved = store.get(inode, &long_name).unwrap().unwrap();
        assert_eq!(retrieved, b"value");
    }

    #[test]
    fn test_flush() {
        let store = XattrStore::in_memory().unwrap();

        store.set(42, "user.test", b"value").unwrap();
        store.flush().unwrap();

        // After flush, value should still be accessible
        let value = store.get(42, "user.test").unwrap().unwrap();
        assert_eq!(value, b"value");
    }

    #[test]
    fn test_unicode_names() {
        let store = XattrStore::in_memory().unwrap();

        let inode = 42;
        let name = "user.测试属性";

        store.set(inode, name, b"unicode test").unwrap();

        let retrieved = store.get(inode, name).unwrap().unwrap();
        assert_eq!(retrieved, b"unicode test");

        let names = store.list(inode).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], name);
    }
}
