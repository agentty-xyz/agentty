//! Native public-surface qualification. Missing isolation is a failing test.

#[cfg(test)]
#[path = "support/sandbox_fixture.rs"]
mod fixture;

#[cfg(test)]
#[path = "support/sandbox_coverage.rs"]
mod coverage;

#[cfg(test)]
#[path = "support/sandbox_access.rs"]
mod access;

#[cfg(test)]
#[path = "support/sandbox_lifecycle.rs"]
mod lifecycle;

#[cfg(test)]
#[path = "support/sandbox_persistence.rs"]
mod persistence;

#[cfg(test)]
#[path = "support/sandbox_launcher.rs"]
mod launcher;
