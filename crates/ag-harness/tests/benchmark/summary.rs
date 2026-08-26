use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BenchmarkFailure {
    passed: usize,
    total: usize,
}

impl fmt::Display for BenchmarkFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "benchmark failed: {} of {} cases passed",
            self.passed, self.total
        )
    }
}

impl Error for BenchmarkFailure {}

pub(super) fn ensure_all_passed(passed: usize, total: usize) -> Result<(), BenchmarkFailure> {
    if passed == total {
        Ok(())
    } else {
        Err(BenchmarkFailure { passed, total })
    }
}
