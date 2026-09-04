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

pub(super) fn sanitize_detail(detail: &str) -> String {
    let detail = detail.replace(['\n', '\r'], " ");
    let Some(http_offset) = detail.find(" returned HTTP ") else {
        return detail;
    };
    let Some(body_offset) = detail[http_offset..].find(": ") else {
        return detail;
    };
    let body_offset = http_offset + body_offset;

    format!("{}: <redacted>", &detail[..body_offset])
}

pub(super) fn ensure_all_passed(passed: usize, total: usize) -> Result<(), BenchmarkFailure> {
    if passed == total {
        Ok(())
    } else {
        Err(BenchmarkFailure { passed, total })
    }
}
