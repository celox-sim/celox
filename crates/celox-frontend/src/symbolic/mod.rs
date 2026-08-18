//! Shared symbolic lowering core.
//!
//! Both source adapters lower into this internal vocabulary before scheduling.
//! Some local identities in this layer are still Veryl-shaped; keeping the
//! compatibility core private makes that debt explicit and prevents those
//! identities from crossing the public frontend boundary.

pub(crate) mod artifact;
pub(crate) mod assembly;
pub(crate) mod bitaccess;
pub(crate) mod bitslicer;
pub(crate) mod case;
pub(crate) mod context_width;
pub(crate) mod ff;
pub(crate) mod flattening;
pub(crate) mod global_ff;
pub(crate) mod logic_tree;
pub(crate) mod registry;
pub(crate) mod types;
