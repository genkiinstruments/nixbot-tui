//! Text shared by the TUI and `--dump`.

use jiff::{
    Timestamp, Unit,
    fmt::friendly::{Designator, Spacing, SpanPrinter},
};

use crate::api::{Attribute, Build, Outcome, Status};

/// The glyphs `nbo` prints, which counts a cancellation as a failure too.
pub fn glyph(status: &Status) -> &'static str {
    use Outcome::*;

    match status.outcome() {
        Failed | Cancelled => "✗",
        Running => "⏵",
        Queued => "·",
        Succeeded => "✓",
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

/// `queued for eval (pos N) behind owner/repo#N`, as `nbo` says it.
pub fn queue_note(build: &Build) -> Option<String> {
    let queue = build.eval_queue.as_ref()?;
    let position = queue.position.map(|p| format!(" (pos {p})"));
    let behind: Vec<String> = queue
        .blocked_by
        .iter()
        .map(|b| format!("{}/{}#{}", b.owner, b.project_name, b.number))
        .collect();
    let behind = (!behind.is_empty()).then(|| format!(" behind {}", behind.join(", ")));

    Some(format!(
        "queued for eval{}{}",
        position.unwrap_or_default(),
        behind.unwrap_or_default()
    ))
}

/// Attribute progress, or where the build is when it has no attributes (yet).
pub fn progress_text(build: &Build, progress: Option<Progress>) -> String {
    match (&build.status, progress) {
        (Status::Evaluating, _) => "evaluating".to_owned(),
        (Status::Pending, _) if build.eval_queue.is_some() => {
            let position = build.eval_queue.as_ref().and_then(|q| q.position);
            position.map_or("queued".to_owned(), |p| format!("queued #{p}"))
        }
        (_, Some(progress)) if progress.total > 0 => progress.to_string(),
        (status, _) => status.label().to_owned(),
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
    fn queued_builds_say_what_they_wait_for() {
        let queue: Vec<crate::api::QueueEntry> =
            serde_json::from_str(include_str!("../tests/fixtures/queue.json")).unwrap();
        let build = &queue[2].build;
        assert_eq!(
            queue_note(build).as_deref(),
            Some("queued for eval (pos 1) behind example/project#121")
        );
        assert_eq!(progress_text(build, None), "queued #1");
        assert_eq!(queue_note(&queue[0].build), None);
    }

    #[test]
    fn builds_without_attributes_show_their_status() {
        let detail: crate::api::BuildDetail =
            serde_json::from_str(include_str!("../tests/fixtures/build_eval_failed.json")).unwrap();
        let progress = Some(Progress::of(&detail.attributes));
        assert_eq!(progress_text(&detail.build, progress), "failed");
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
