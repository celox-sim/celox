//! IR-independent compiler analyses shared by Celox's source and machine IRs.
//!
//! This crate deliberately owns no SIR or MIR types. Callers translate their
//! block, instruction, and memory-effect identities to dense indices and
//! immutable event tables before invoking an analysis.

pub mod cfg;
pub mod cfg_order;
pub mod dag_schedule;
pub mod dependence;
pub mod interval;
pub mod memory;
pub mod memory_ssa;
pub mod ssa;
