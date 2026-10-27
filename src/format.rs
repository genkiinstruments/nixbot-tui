//! Text shared by the TUI and `--dump`.

use jiff::{
    Timestamp, Unit,
    fmt::friendly::{Designator, Spacing, SpanPrinter},
};

use crate::api::{Attribute, Outcome, Status};

pub fn glyph(status: &Status) -> &'static str {
    match status.outcome() {
        Outcome::Failed => "✗",
        Outcome::Cancelled => "⊘",
        Outcome::Running => "⏵",
        Outcome::Queued => "·",
        Outcome::Succeeded => "✓",
    }
}

/// Elapsed time in at most two units, e.g. `3m12s`, `2h5m` or `3d2h`.
pub fn age(since: Timestamp, now: Timestamp) -> String {
    let elapsed = since.duration_until(now).max(jiff::SignedDuration::ZERO);
    let smallest = match elapsed.as_secs() {
        ..3600 => Unit::Second,
        3600..86400 => Unit::Minute,
        _ => Unit::Hour,
    };
    let span = jiff::Span::try_from(elapsed)
        .and_then(|span| {
            span.round(
                jiff::SpanRound::new()
                    .largest(Unit::Day)
                    .smallest(smallest)
                    .mode(jiff::RoundMode::Trunc)
                    .days_are_24_hours(),
            )
        })
        .unwrap_or_default();
    SpanPrinter::new()
        .designator(Designator::Compact)
        .spacing(Spacing::None)
        .span_to_string(&span)
}

pub fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Attribute counts for the builds list's progress column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
}

impl Progress {
    pub fn of(attributes: &[Attribute]) -> Self {
        Self {
            total: attributes.len(),
            done: attributes.iter().filter(|a| !a.status.is_active()).count(),
            failed: attributes.iter().filter(|a| a.status.is_failure()).count(),
        }
    }
}

impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.done, self.total)?;
        if self.failed > 0 {
            write!(f, " ✗{}", self.failed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_keeps_two_units() {
        let now: Timestamp = "2026-10-27T12:00:00Z".parse().unwrap();
        let cases = [
            ("2026-10-27T11:59:55Z", "5s"),
            ("2026-10-27T11:56:48Z", "3m12s"),
            ("2026-10-27T09:54:30Z", "2h5m"),
            ("2026-10-24T10:00:00Z", "3d2h"),
            ("2026-10-27T12:00:09Z", "0s"),
        ];
        for (since, expected) in cases {
            assert_eq!(age(since.parse().unwrap(), now), expected, "{since}");
        }
    }

    #[test]
    fn progress_counts_finished_and_failed() {
        let detail: crate::api::BuildDetail =
            serde_json::from_str(include_str!("../tests/fixtures/build_running.json")).unwrap();
        let progress = Progress::of(&detail.attributes);
        assert_eq!(
            progress,
            Progress {
                total: 12,
                done: 6,
                failed: 0
            }
        );
        assert_eq!(progress.to_string(), "6/12");
        let failing = Progress {
            failed: 2,
            ..progress
        };
        assert_eq!(failing.to_string(), "6/12 ✗2");
    }
}
