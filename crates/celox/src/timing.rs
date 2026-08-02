//! Timing policy is owned by `celox-sir-opt`'s `timing` feature so compiler
//! crates do not inspect the target architecture independently.

pub use celox_sir_opt::timing::now;
