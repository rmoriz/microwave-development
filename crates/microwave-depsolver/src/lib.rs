//! Cookbook dependency constraint solver.
//!
//! This crate is a pure Rust port of the upstream Chef Infra Server dependency
//! solver. It parses Chef cookbook versions and constraints, then performs a
//! deterministic backtracking search over a universe of cookbook versions.

mod solver;
mod version;

pub use solver::{
    Goal, InvalidConstraints, NoSolution, PackageVersion, ResolvedPackage, Solution, SolveError,
    SolveOptions, World, DEFAULT_TIMEOUT_MS,
};
pub use version::{Constraint, ConstraintError, Operator, Version, VersionError};
