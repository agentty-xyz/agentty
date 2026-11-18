//! Retry policy of one provider's requests: which transient failures are
//! retried, how often, and how long each retry waits, and the observer told
//! about each retry before its wait.

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};

/// HTTP status of an overloaded provider, such as Anthropic's
/// `overloaded_error`.
const OVERLOADED: u16 = 529;

/// Transient failure a [`RetryPolicy`] may retry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum RetryKind {
    /// HTTP `429` without a spent-quota marker; a `429` reporting a spent
    /// quota is never retried.
    RateLimited,
    /// HTTP `500`, `502`, `503`, or `504`.
    ServerError,
    /// HTTP `529`, an overloaded provider.
    Overloaded,
    /// The connection failed before any response; a timed-out request is
    /// never retried, since a slow model would likely time out again.
    Connection,
}

impl RetryKind {
    /// Returns the kind of an unsuccessful `status`, or `None` when no
    /// policy retries it.
    pub(crate) fn of_status(status: StatusCode) -> Option<Self> {
        match status.as_u16() {
            429 => Some(Self::RateLimited),
            500 | 502 | 503 | 504 => Some(Self::ServerError),
            OVERLOADED => Some(Self::Overloaded),
            _ => None,
        }
    }

    /// Returns this kind's bit in [`RetryPolicy`]'s retried set.
    const fn bit(self) -> u8 {
        match self {
            Self::RateLimited => 1,
            Self::ServerError => 1 << 1,
            Self::Overloaded => 1 << 2,
            Self::Connection => 1 << 3,
        }
    }
}

/// Random spread applied to each computed backoff, so concurrent clients
/// limited together do not retry together. A provider-declared
/// `Retry-After` is never shortened by it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Jitter {
    /// Wait exactly the computed backoff.
    #[default]
    None,
    /// Wait a uniformly random delay between zero and the backoff.
    Full,
    /// Wait half the backoff plus a uniformly random delay up to the other
    /// half.
    Equal,
}

impl Jitter {
    /// Spreads `backoff` by `sample`, a value in `[0, 1)`.
    pub(crate) fn apply(self, backoff: Duration, sample: f64) -> Duration {
        let sample = sample.clamp(0.0, 1.0);
        match self {
            Self::None => backoff,
            Self::Full => backoff.mul_f64(sample),
            Self::Equal => {
                let half = backoff / 2;

                half + backoff.saturating_sub(half).mul_f64(sample)
            }
        }
    }
}

/// How a provider's failed requests are retried.
///
/// The default retries a `429` without a spent-quota marker up to five
/// times, and one connection failure. Each retry waits an exponential
/// backoff from one second, doubled per status retry and capped at five
/// seconds, or the provider's `Retry-After`, given as delta-seconds or an
/// HTTP date and capped at sixty seconds, whichever is longer. Server
/// errors (`500`, `502`-`504`) and an overloaded `529` are opt-ins through
/// [`Self::with_retried`]; a spent quota (`402`, or a `429` reporting one)
/// and a timed-out request are never retried.
///
/// Unsuccessful statuses and connection failures have separate budgets: a
/// call sends at most [`Self::with_max_attempts`] requests answered with
/// retried statuses, and retries at most
/// [`Self::with_max_connection_retries`] connection failures beside them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryPolicy {
    base_delay: Duration,
    jitter: Jitter,
    max_attempts: u32,
    max_backoff: Duration,
    max_connection_retries: u32,
    max_retry_after: Duration,
    /// The [`RetryKind::bit`]s of the retried kinds.
    retried: u8,
}

impl RetryPolicy {
    /// Returns a policy that retries nothing, with the default delays and
    /// budgets for any kind [`Self::with_retried`] turns back on.
    pub fn none() -> Self {
        Self {
            retried: 0,
            ..Self::default()
        }
    }

    /// Sets the backoff before the first status retry, doubled for each
    /// later one; a connection retry waits it undoubled.
    #[must_use]
    pub fn with_base_delay(mut self, base_delay: Duration) -> Self {
        self.base_delay = base_delay;

        self
    }

    /// Sets the random spread of each computed backoff.
    #[must_use]
    pub fn with_jitter(mut self, jitter: Jitter) -> Self {
        self.jitter = jitter;

        self
    }

    /// Sets the most requests one call sends while each is answered with a
    /// retried status, the first included, so at most `max_attempts - 1`
    /// status retries; `0` reads as `1`, retrying no status. Connection
    /// retries do not count against it.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts.max(1);

        self
    }

    /// Sets the ceiling of the exponential backoff.
    #[must_use]
    pub fn with_max_backoff(mut self, max_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;

        self
    }

    /// Sets how many connection failures one call retries, beside its
    /// status attempts; they are retried only while
    /// [`RetryKind::Connection`] is, so `0` and turning that kind off both
    /// disable them.
    #[must_use]
    pub fn with_max_connection_retries(mut self, max_connection_retries: u32) -> Self {
        self.max_connection_retries = max_connection_retries;

        self
    }

    /// Sets the ceiling of a provider-declared `Retry-After`, so a
    /// per-minute limit can be waited out without honoring an unbounded
    /// instruction.
    #[must_use]
    pub fn with_max_retry_after(mut self, max_retry_after: Duration) -> Self {
        self.max_retry_after = max_retry_after;

        self
    }

    /// Sets whether failures of `kind` are retried.
    #[must_use]
    pub fn with_retried(mut self, kind: RetryKind, retried: bool) -> Self {
        if retried {
            self.retried |= kind.bit();
        } else {
            self.retried &= !kind.bit();
        }

        self
    }

    /// Returns whether a call that already retried `status_retries`
    /// unsuccessful statuses and `connection_retries` connection failures
    /// may retry a failure of `kind`.
    pub(crate) fn allows(
        &self,
        kind: RetryKind,
        status_retries: u32,
        connection_retries: u32,
    ) -> bool {
        let within_budget = match kind {
            RetryKind::Connection => connection_retries < self.max_connection_retries,
            RetryKind::RateLimited | RetryKind::ServerError | RetryKind::Overloaded => {
                status_retries.saturating_add(1) < self.max_attempts
            }
        };

        self.retried & kind.bit() != 0 && within_budget
    }

    /// Returns the provider's `Retry-After` in `headers`, read as
    /// delta-seconds or an HTTP date relative to `now`, capped at the
    /// policy's ceiling; a date already past reads as zero, and any other
    /// value as absent.
    pub(crate) fn retry_after(&self, headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
        let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
        let delay = match value.parse::<u64>() {
            Ok(seconds) => Duration::from_secs(seconds),
            Err(_) => httpdate::parse_http_date(value)
                .ok()?
                .duration_since(now)
                .unwrap_or_default(),
        };

        Some(delay.min(self.max_retry_after))
    }

    /// Returns the wait before status retry `retry`, counted from zero: the
    /// capped exponential backoff, spread by `sample`, or `retry_after`
    /// when longer.
    pub(crate) fn status_delay(
        &self,
        retry: u32,
        retry_after: Option<Duration>,
        sample: f64,
    ) -> Duration {
        let backoff = self
            .base_delay
            .saturating_mul(1_u32 << retry.min(31))
            .min(self.max_backoff);

        retry_after
            .unwrap_or_default()
            .max(self.jitter.apply(backoff, sample))
    }

    /// Returns the wait before a connection retry: the base delay, spread
    /// by `sample`.
    pub(crate) fn connection_delay(&self, sample: f64) -> Duration {
        self.jitter
            .apply(self.base_delay.min(self.max_backoff), sample)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(1),
            jitter: Jitter::None,
            max_attempts: 6,
            max_backoff: Duration::from_secs(5),
            max_connection_retries: 1,
            max_retry_after: Duration::from_mins(1),
            retried: RetryKind::RateLimited.bit() | RetryKind::Connection.bit(),
        }
    }
}

/// One retry a transport is about to wait for, reported to a
/// [`RetryObserver`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryNotice {
    /// How long the transport waits before sending the retry.
    pub after: Duration,
    /// One-based number of this retry within its call, counting status and
    /// connection retries together.
    pub attempt: u32,
    /// The failure being retried.
    pub kind: RetryKind,
    /// HTTP status of the failed response, or `None` for a connection
    /// failure.
    pub status: Option<u16>,
}

/// Observer told about each retry before its wait, such as a host that
/// shows a run waiting out a rate limit.
#[async_trait]
pub trait RetryObserver: Send + Sync {
    /// Reports `notice`; the transport waits for this call before its
    /// delay, and cancelling the request while either runs stops the call.
    async fn retrying(&self, notice: RetryNotice);
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod tests;
