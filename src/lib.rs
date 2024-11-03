//! # ergokv
//!
//! `ergokv` is a library for easy integration with TiKV, providing derive macros for automatic CRUD operations.
//!
//! ## Usage
//!
//! Add this to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! ergokv = "0.1"
//! ```
//!
//! Then, in your Rust file:
//!
//! ```rust
//! use ergokv::Store;
//! use serde::{Serialize, Deserialize};
//! use uuid::Uuid;
//!
//! #[derive(Store, Serialize, Deserialize)]
//! struct User {
//!     #[key]
//!     id: Uuid,
//!     #[index]
//!     username: String,
//!     email: String,
//! }
//! ```
//!
//! This will generate `load`, `save`, `delete`, `by_username`, `set_username`, and `set_email` methods for `User`.

pub use ergokv_macro::Store;

pub use ciborium;
pub use serde_json;

mod local_cluster;
mod trie;

pub use local_cluster::LocalCluster;
pub use trie::PrefixTrie;

/// Helper function to connect to a single or multiple TiKV pd-server
pub async fn connect(
    endpoints: Vec<&str>,
) -> Result<tikv_client::TransactionClient, tikv_client::Error> {
    tikv_client::TransactionClient::new(endpoints).await
}
