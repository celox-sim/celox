#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

//! Source-neutral symbolic lowering core.
//!
//! Both source adapters project parser-owned identities and metadata before
//! entering this module. Source-language crates must not be imported here.

pub mod artifact;
pub mod assembly;
mod comb_startup;
pub mod flattening;
pub mod parallel;
pub mod pow;
pub mod remap;
pub mod width;

/// The state objects `instruction` may read, each in part or whole: a Load's
/// or Commit's source, or every object a call reads.
///
/// The frontend makes no native kernel, the one instruction that may read
/// state no list names.
pub(crate) fn read_objects<A>(
    instruction: &celox_sir::SIRInstruction<A>,
) -> impl Iterator<Item = &A> {
    instruction
        .state_reads()
        .objects()
        .expect("the frontend makes no native kernels")
}

/// The state objects `instruction` may write, each in part or whole: a
/// Store's or Commit's destination, or every object a call writes (see
/// [`read_objects`]).
pub(crate) fn written_objects<A>(
    instruction: &celox_sir::SIRInstruction<A>,
) -> impl Iterator<Item = &A> {
    instruction
        .state_writes()
        .objects()
        .expect("the frontend makes no native kernels")
}
