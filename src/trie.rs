//! Prefix trie implementation for TiKV.
//!
//! This module provides a prefix trie data structure that stores its nodes in TiKV.
//! While primarily used internally by ergokv for efficient batch retrieval operations,
//! it is also available for building custom abstractions on top of TiKV/ergokv.
//!
//! The trie supports basic operations like insertion, removal, and retrieval,
//! as well as prefix-based searches and streaming of all stored keys.
//! All operations are performed within a TiKV transaction context.
use async_stream::try_stream;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, SetPreventDuplicates};
use tikv_client::{Error as TikvError, Transaction};

use std::collections::HashSet;

/// A node in the prefix trie.
///
/// Each node can store a key (if it represents the end of a stored string)
/// and maintains a set of child characters that lead to other nodes.
#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
struct TrieNode {
    #[serde_as(as = "SetPreventDuplicates<_>")]
    children: HashSet<char>,
    key: Option<String>,
}

/// A prefix trie implementation that stores its nodes in TiKV.
///
/// The trie uses a prefix string to namespace its nodes in the TiKV keyspace,
/// preventing conflicts with other data stored in the same TiKV cluster.
///
/// Try to not be comedic and nameclash with structures with a Store derive,
/// as unexpected things might happen.
#[derive(Clone, Debug)]
pub struct PrefixTrie {
    prefix: String,
}

impl PrefixTrie {
    /// Creates a new prefix trie with the given namespace prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// # use ergokv::PrefixTrie;
    /// let trie = PrefixTrie::new("my_namespace");
    /// ```
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    /// Generates a TiKV key for a trie node at the given path.
    fn node_key(&self, path: &str) -> Vec<u8> {
        format!("{}:trie:node:{}", self.prefix, path)
            .into_bytes()
    }

    /// Retrieves a node from TiKV at the given path.
    async fn get_node(
        &self,
        txn: &mut Transaction,
        path: &str,
    ) -> Result<Option<TrieNode>, TikvError> {
        if let Some(data) = txn
            .get(self.node_key(path))
            .await?
            .map(|d| {
                ciborium::de::from_reader(d.as_slice()).ok()
            })
            .flatten()
        {
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    /// Stores a node in TiKV at the given path.
    async fn put_node(
        &self,
        txn: &mut Transaction,
        path: &str,
        node: &TrieNode,
    ) -> Result<(), TikvError> {
        let mut data = Vec::new();
        ciborium::ser::into_writer(node, &mut data).map_err(
            |e| {
                TikvError::StringError(format!(
                    "Deserialization failed: {e}"
                ))
            },
        )?;
        txn.put(self.node_key(path), data).await?;
        Ok(())
    }

    /// Inserts a key into the trie.
    ///
    /// Empty strings are not allowed as keys.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is empty or if the TiKV operation fails.
    pub async fn insert(
        &self,
        txn: &mut Transaction,
        key: &str,
    ) -> Result<(), TikvError> {
        println!("inserting {}", key);

        if key.is_empty() {
            Err(TikvError::StringError(
                "Empty string keys are not allowed".into(),
            ))?
        }

        // Update root node with first character
        let first_char = key.chars().next().unwrap();
        let mut root = self
            .get_node(txn, "")
            .await?
            .unwrap_or_else(|| TrieNode {
                key: None,
                children: HashSet::new(),
            });
        root.children.insert(first_char);
        self.put_node(txn, "", &root).await?;

        let mut current_path = String::new();

        for (i, c) in key.chars().enumerate() {
            current_path.push(c);

            let mut node = self
                .get_node(txn, &current_path)
                .await?
                .unwrap_or_else(|| TrieNode {
                    key: None,
                    children: HashSet::new(),
                });

            if i < key.len() - 1 {
                node.children
                    .insert(key.chars().nth(i + 1).unwrap());
                self.put_node(txn, &current_path, &node).await?;
            } else {
                node.key = Some(key.to_string());
                self.put_node(txn, &current_path, &node).await?;
            }
        }

        Ok(())
    }

    /// Retrieves a key from the trie.
    ///
    /// Returns `None` if the key doesn't exist.
    pub async fn get(
        &self,
        txn: &mut Transaction,
        key: &str,
    ) -> Result<Option<String>, TikvError> {
        println!("retrieving {}", key);

        let mut current_path = String::new();

        for (i, c) in key.chars().enumerate() {
            current_path.push(c);

            if let Some(node) =
                self.get_node(txn, &current_path).await?
            {
                if i < key.len() - 1
                    && !node.children.contains(
                        &key.chars().nth(i + 1).unwrap(),
                    )
                {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }

        Ok(self
            .get_node(txn, &current_path)
            .await?
            .and_then(|node| node.key))
    }

    /// Finds all keys in the trie that start with the given prefix.
    ///
    /// Returns a vector of matching keys in no particular order.
    pub fn find_by_prefix<'a, 'b>(
        &'a self,
        txn: &'a mut Transaction,
        prefix: &'b str,
    ) -> impl Stream<Item = Result<String, TikvError>> + 'a {
        let prefix = prefix.to_string();

        try_stream! {
            let mut queue = vec![prefix.to_string()];

            while let Some(path) = queue.pop() {
                if let Some(node) = self.get_node(txn, &path).await? {
                    if let Some(key) = node.key {
                        yield key;
                    }

                    for c in node.children {
                        let mut child_path = path.clone();
                        child_path.push(c);
                        queue.push(child_path);
                    }
                }
            }
        }
    }

    /// Returns a stream of all keys stored in the trie.
    ///
    /// The keys are yielded in no particular order. This method is memory-efficient
    /// as it doesn't need to load all keys at once.
    pub fn all<'a>(
        &'a self,
        txn: &'a mut Transaction,
    ) -> impl Stream<Item = Result<String, TikvError>> + 'a {
        try_stream! {
            let mut queue = Vec::new();

            // Get root node's children
            if let Some(root) = self.get_node(txn, "").await? {
                for c in root.children {
                    queue.push(c.to_string());
                }
            }

            while let Some(path) = queue.pop() {
                if let Some(node) = self.get_node(txn, &path).await? {
                    if let Some(key) = node.key {
                        yield key;
                    }

                    for c in node.children {
                        let mut child_path = path.clone();
                        child_path.push(c);
                        queue.push(child_path);
                    }
                }
            }
        }
    }

    /// Removes a key from the trie.
    ///
    /// If the key doesn't exist, this operation is a no-op.
    /// The operation also cleans up any nodes that become unused after the removal.
    pub async fn remove(
        &self,
        txn: &mut Transaction,
        key: &str,
    ) -> Result<(), TikvError> {
        let mut current_path = String::new();

        for (i, c) in key.chars().enumerate() {
            current_path.push(c);

            if let Some(mut node) =
                self.get_node(txn, &current_path).await?
            {
                if i == key.len() - 1 {
                    node.key = None;
                    if node.children.is_empty() {
                        txn.delete(self.node_key(&current_path))
                            .await?;
                    } else {
                        self.put_node(txn, &current_path, &node)
                            .await?;
                    }
                } else {
                    let next_char =
                        key.chars().nth(i + 1).unwrap();
                    let next_path =
                        format!("{}{}", current_path, next_char);

                    if let Some(child) =
                        self.get_node(txn, &next_path).await?
                    {
                        if child.key.is_none()
                            && child.children.is_empty()
                        {
                            node.children.remove(&next_char);
                        }
                    }

                    if node.key.is_none()
                        && node.children.is_empty()
                    {
                        txn.delete(self.node_key(&current_path))
                            .await?;
                    } else {
                        self.put_node(txn, &current_path, &node)
                            .await?;
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalCluster;
    use futures::TryStreamExt;
    use tempfile::TempDir;

    async fn setup(
    ) -> (LocalCluster, PrefixTrie, Transaction, TempDir) {
        let tmp =
            TempDir::new().expect("Failed to create temp dir");
        let cluster = LocalCluster::start(tmp.path())
            .expect("Failed to start TiKV cluster");
        let client = cluster
            .spawn_client()
            .await
            .expect("Failed to spawn client");
        let txn = client
            .begin_optimistic()
            .await
            .expect("Failed to start transaction");

        (cluster, PrefixTrie::new("test"), txn, tmp)
    }

    #[tokio::test]
    async fn test_basic_operations() -> Result<(), TikvError> {
        let (_cluster, trie, mut txn, _tmp) = setup().await;

        trie.insert(&mut txn, "hello").await?;
        trie.insert(&mut txn, "help").await?;
        trie.insert(&mut txn, "helper").await?;
        trie.insert(&mut txn, "hell").await?;

        assert_eq!(
            trie.get(&mut txn, "help").await?,
            Some("help".to_string())
        );
        assert_eq!(trie.get(&mut txn, "hel").await?, None);

        // Collect prefix stream results
        let mut results = Vec::new();
        let stream = trie.find_by_prefix(&mut txn, "hel");
        {
            futures::pin_mut!(stream);
            while let Ok(Some(key)) = stream.try_next().await {
                results.push(key);
            }
            results.sort();
        }

        assert_eq!(
            results,
            vec![
                "hell".to_string(),
                "hello".to_string(),
                "help".to_string(),
                "helper".to_string()
            ]
        );

        trie.remove(&mut txn, "help").await?;
        assert_eq!(trie.get(&mut txn, "help").await?, None);

        // Check prefix after removal
        let mut results = Vec::new();
        let stream = trie.find_by_prefix(&mut txn, "hel");
        {
            futures::pin_mut!(stream);
            while let Ok(Some(key)) = stream.try_next().await {
                results.push(key);
            }
            results.sort();
        }

        assert_eq!(
            results,
            vec![
                "hell".to_string(),
                "hello".to_string(),
                "helper".to_string()
            ]
        );

        txn.commit().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_empty_and_single_char() -> Result<(), TikvError>
    {
        let (_cluster, trie, mut txn, _tmp) = setup().await;

        // Empty string
        assert!(trie.insert(&mut txn, "").await.is_err());
        assert_eq!(trie.get(&mut txn, "").await?, None);

        // Single character
        trie.insert(&mut txn, "x").await?;
        assert_eq!(
            trie.get(&mut txn, "x").await?,
            Some("x".to_string())
        );

        // Check prefix for single char
        let mut results = Vec::new();
        let stream = trie.find_by_prefix(&mut txn, "x");
        {
            futures::pin_mut!(stream);
            while let Ok(Some(key)) = stream.try_next().await {
                results.push(key);
            }
            results.sort();
        }
        assert_eq!(results, vec!["x".to_string()]);

        txn.commit().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_overlapping_prefixes() -> Result<(), TikvError>
    {
        let (_cluster, trie, mut txn, _tmp) = setup().await;

        trie.insert(&mut txn, "a").await?;
        trie.insert(&mut txn, "ab").await?;
        trie.insert(&mut txn, "abc").await?;

        // Test all prefixes exist
        assert_eq!(
            trie.get(&mut txn, "a").await?,
            Some("a".to_string())
        );
        assert_eq!(
            trie.get(&mut txn, "ab").await?,
            Some("ab".to_string())
        );
        assert_eq!(
            trie.get(&mut txn, "abc").await?,
            Some("abc".to_string())
        );

        // Remove middle one
        trie.remove(&mut txn, "ab").await?;
        assert_eq!(trie.get(&mut txn, "ab").await?, None);
        assert_eq!(
            trie.get(&mut txn, "a").await?,
            Some("a".to_string())
        );
        assert_eq!(
            trie.get(&mut txn, "abc").await?,
            Some("abc".to_string())
        );

        txn.commit().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_all_stream() -> Result<(), TikvError> {
        let (_cluster, trie, mut txn, _tmp) = setup().await;

        // Insert some test data
        trie.insert(&mut txn, "foo").await?;
        trie.insert(&mut txn, "bar").await?;
        trie.insert(&mut txn, "baz").await?;
        trie.insert(&mut txn, "quux").await?;

        // Collect all keys and sort them for comparison
        let mut results = Vec::new();
        let stream = trie.all(&mut txn);

        {
            futures::pin_mut!(stream);
            while let Ok(Some(key)) = stream.try_next().await {
                results.push(key);
            }
            results.sort();

            assert_eq!(
                results,
                vec![
                    "bar".to_string(),
                    "baz".to_string(),
                    "foo".to_string(),
                    "quux".to_string()
                ]
            );
        }

        txn.commit().await?;
        Ok(())
    }
}
