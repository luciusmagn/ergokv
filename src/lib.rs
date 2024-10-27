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
