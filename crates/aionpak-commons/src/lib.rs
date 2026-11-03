//! Reading and writing Aion client `.pak` archives and the XML and HTML files inside them.

pub mod compare;
pub mod error;
pub mod manifest;
pub mod names;
pub mod ops;
pub mod optimize;
pub mod ordered;
pub mod pak;
pub mod progress;
pub mod tree;
pub mod verify;
pub mod xml;

pub use error::{Error, Result};
