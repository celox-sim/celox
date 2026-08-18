#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

//! Source-neutral symbolic lowering core.
//!
//! Both source adapters project parser-owned identities and metadata before
//! entering this module. Source-language crates must not be imported here.

pub mod artifact;
pub mod assembly;
pub mod flattening;
pub mod remap;
pub mod width;
