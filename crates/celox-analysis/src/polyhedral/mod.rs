//! Experimental affine scheduling for bounded, static-control regions.
//!
//! The objective and permutable-band construction follow Bondhugula et al.,
//! CC 2008, sections 3.2–3.6. This first implementation specializes parameters
//! before scheduling. Universal dependence constraints are imposed at the
//! vertices of bounded rational dependence polyhedra (the primal counterpart
//! of the paper's affine-Farkas construction). Schedule coefficients are
//! integers; they are synthesized, not selected from a list of loop rewrites.
//!
//! No floating-point tolerances enter legality decisions. Checked rational
//! arithmetic, coefficient bounds, and work limits can reject a region. A
//! caller must retain the original program on any error. This module does not
//! silently approximate a failed proof or promise general Pluto coverage.

// Domains and bands intentionally contain ranges as elements.
#![allow(clippy::single_range_in_vec_init)]

mod geometry;
mod integer;
mod rational;
mod scan;
mod schedule;
pub mod tuning;

use std::ops::Range;

pub use scan::{Bound, ScanDimension, ScanPlan, ScanStatement, Tile, scan};
pub use schedule::{Schedule, ScheduleOptions, ScheduleStats, schedule, verify_schedule};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    ArithmeticOverflow,
    WorkLimit,
    NoSchedule,
    NonIntegralScan,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid affine region: {reason}"),
            Self::ArithmeticOverflow => {
                f.write_str("affine arithmetic exceeded supported exact bounds")
            }
            Self::WorkLimit => f.write_str("affine analysis work limit reached"),
            Self::NoSchedule => f.write_str("no schedule found within the coefficient bounds"),
            Self::NonIntegralScan => f.write_str("non-unimodular scan is not supported"),
        }
    }
}

impl std::error::Error for Error {}

pub(super) type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Affine {
    pub coefficients: Vec<i64>,
    pub constant: i64,
}

impl Affine {
    pub fn new(coefficients: impl Into<Vec<i64>>, constant: i64) -> Self {
        Self {
            coefficients: coefficients.into(),
            constant,
        }
    }

    pub fn constant(dimensions: usize, constant: i64) -> Self {
        Self::new(vec![0; dimensions], constant)
    }

    pub fn axis(dimensions: usize, axis: usize) -> Self {
        let mut coefficients = vec![0; dimensions];
        coefficients[axis] = 1;
        Self::new(coefficients, 0)
    }

    pub fn evaluate(&self, point: &[i64]) -> Result<i64> {
        if point.len() != self.coefficients.len() {
            return Err(Error::Invalid("affine expression dimension mismatch"));
        }
        let mut value = i128::from(self.constant);
        for (&coefficient, &coordinate) in self.coefficients.iter().zip(point) {
            value = value
                .checked_add(i128::from(coefficient) * i128::from(coordinate))
                .ok_or(Error::ArithmeticOverflow)?;
        }
        i64::try_from(value).map_err(|_| Error::ArithmeticOverflow)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    /// A finite bounding box. Upper endpoints are exclusive.
    pub bounds: Vec<Range<i64>>,
    /// Additional affine inequalities, each interpreted as expression >= 0.
    pub constraints: Vec<Affine>,
}

impl Domain {
    pub fn rectangular(bounds: impl Into<Vec<Range<i64>>>) -> Self {
        Self {
            bounds: bounds.into(),
            constraints: Vec::new(),
        }
    }

    pub fn contains(&self, point: &[i64]) -> Result<bool> {
        if point.len() != self.bounds.len() {
            return Err(Error::Invalid("domain dimension mismatch"));
        }
        if self
            .bounds
            .iter()
            .zip(point)
            .any(|(range, value)| !range.contains(value))
        {
            return Ok(false);
        }
        for constraint in &self.constraints {
            if constraint.evaluate(point)? < 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    /// Caller-proven, non-aliasing memory object identity.
    pub object: usize,
    /// Element subscripts. All accesses to an object must name the same
    /// non-overlapping element partition, including the same element width.
    pub subscripts: Vec<Affine>,
    pub kind: AccessKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub domain: Domain,
    /// Original lexicographic execution order, including scalar sequence
    /// dimensions. Distinct statement instances must have distinct times.
    pub original_schedule: Vec<Affine>,
    /// All reads/writes of one atomic statement, in program order. Adapters
    /// must also model scalar recurrences or reject them, and exclude events,
    /// traps, unknown aliases, and accesses with unproved wraparound.
    pub accesses: Vec<Access>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub statements: Vec<Statement>,
}

#[derive(Debug)]
pub(super) struct Work {
    pub remaining: usize,
    pub used: usize,
}

impl Work {
    pub fn spend(&mut self) -> Result<()> {
        if self.remaining == 0 {
            return Err(Error::WorkLimit);
        }
        self.remaining -= 1;
        self.used += 1;
        Ok(())
    }
}
