use std::fmt;

use jiff::Timestamp;
use serde::Deserialize;

/// Build, attribute and effect statuses as the server spells them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(from = "String")]
pub enum Status {
    Pending,
    Evaluating,
    Building,
    Running,
    Succeeded,
    SkippedLocal,
    Skipped,
    Failed,
    FailedEval,
    DependencyFailed,
    CachedFailure,
    Cancelled,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Failed,
    Running,
    Queued,
    Cancelled,
    Succeeded,
}

impl From<String> for Status {
    fn from(s: String) -> Self {
        match s.as_str() {
            "pending" => Self::Pending,
            "evaluating" => Self::Evaluating,
            "building" => Self::Building,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "skipped_local" => Self::SkippedLocal,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            "failed_eval" => Self::FailedEval,
            "dependency_failed" => Self::DependencyFailed,
            "cached_failure" => Self::CachedFailure,
            "cancelled" => Self::Cancelled,
            _ => Self::Other(s),
        }
    }
}

impl Status {
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Pending | Self::Other(_) => Outcome::Queued,
            Self::Evaluating | Self::Building | Self::Running => Outcome::Running,
            Self::Succeeded | Self::SkippedLocal | Self::Skipped => Outcome::Succeeded,
            Self::Failed | Self::FailedEval | Self::DependencyFailed | Self::CachedFailure => {
                Outcome::Failed
            }
            Self::Cancelled => Outcome::Cancelled,
        }
    }

    /// Still queued or in progress, so it can be cancelled and will change.
    pub fn is_active(&self) -> bool {
        matches!(self.outcome(), Outcome::Queued | Outcome::Running)
    }

    pub fn is_failure(&self) -> bool {
        matches!(self.outcome(), Outcome::Failed | Outcome::Cancelled)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Evaluating => "evaluating",
            Self::Building => "building",
            Self::Running => "running",
            Self::Succeeded => "built",
            Self::SkippedLocal => "cached",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::FailedEval => "eval failed",
            Self::DependencyFailed => "dependency failed",
            Self::CachedFailure => "failed (cached)",
            Self::Cancelled => "cancelled",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct RepoRef {
    pub forge: String,
    pub owner: String,
    pub name: String,
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Repo {
    #[serde(flatten)]
    pub repo: RepoRef,
    pub url: String,
    #[serde(rename = "enabled")]
    pub is_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Build {
    pub id: i64,
    pub number: u64,
    pub commit_sha: String,
    pub branch: String,
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub status: Status,
    pub error: Option<String>,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub eval_queue: Option<EvalQueue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EvalQueue {
    pub position: Option<u32>,
    pub blocked_by: Vec<BuildRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildRef {
    pub owner: String,
    pub project_name: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QueueEntry {
    #[serde(flatten)]
    pub build: Build,
    pub forge: String,
    pub owner: String,
    pub project_name: String,
    pub url: String,
}

impl QueueEntry {
    pub fn repo(&self) -> RepoRef {
        RepoRef {
            forge: self.forge.clone(),
            owner: self.owner.clone(),
            name: self.project_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildPage {
    pub items: Vec<Build>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildDetail {
    pub build: Build,
    pub attributes: Vec<Attribute>,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Attribute {
    pub attr: String,
    pub status: Status,
    #[serde(rename = "cached")]
    pub is_cached: bool,
    pub error: Option<String>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Effect {
    pub id: u64,
    pub kind: String,
    pub name: String,
    pub status: Status,
    pub error: Option<String>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

impl Effect {
    /// `name` for onPush effects, `kind/name` for event effects, as `nbo` shows them.
    pub fn label(&self) -> String {
        if self.kind == "push" {
            self.name.clone()
        } else {
            format!("{}/{}", self.kind, self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EffectRun {
    pub status: Status,
    pub error: Option<String>,
    pub skip_reason: Option<String>,
}

/// A cue from `/api/events` that something about a build changed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusEvent {
    pub build_id: Option<i64>,
    pub attr: Option<String>,
    pub status: Option<Status>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogDerivation {
    pub idx: u32,
    pub name: String,
}

/// One event of an attribute's structured log stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    State(Vec<LogDerivation>),
    Drv { idx: u32, name: String },
    Line { text: String },
    Phase { idx: u32, phase: String },
    DrvDone { idx: u32, status: String },
    Done,
}

impl LogEvent {
    /// `None` for event types this client does not know.
    pub fn parse(event: &str, data: &str) -> serde_json::Result<Option<Self>> {
        #[derive(Deserialize)]
        struct Drv {
            idx: u32,
            name: String,
        }
        #[derive(Deserialize)]
        struct Line {
            text: String,
        }
        #[derive(Deserialize)]
        struct Phase {
            idx: u32,
            phase: String,
        }
        #[derive(Deserialize)]
        struct DrvDone {
            idx: u32,
            status: String,
        }
        Ok(Some(match event {
            "state" => Self::State(serde_json::from_str(data)?),
            "drv" => {
                let Drv { idx, name } = serde_json::from_str(data)?;
                Self::Drv { idx, name }
            }
            "line" => {
                let Line { text } = serde_json::from_str(data)?;
                Self::Line { text }
            }
            "phase" => {
                let Phase { idx, phase } = serde_json::from_str(data)?;
                Self::Phase { idx, phase }
            }
            "drv-done" => {
                let DrvDone { idx, status } = serde_json::from_str(data)?;
                Self::DrvDone { idx, status }
            }
            "done" => Self::Done,
            _ => return Ok(None),
        }))
    }
}

/// One event of an effect run's raw output stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectEvent {
    Text(String),
    Done,
}

impl EffectEvent {
    pub fn parse(event: &str, data: &str) -> serde_json::Result<Option<Self>> {
        #[derive(Deserialize)]
        struct Text {
            text: String,
        }
        Ok(match event {
            "text" => Some(Self::Text(serde_json::from_str::<Text>(data)?.text)),
            "done" => Some(Self::Done),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use eventsource_stream::Eventsource;
    use futures_util::StreamExt;

    use super::*;

    fn fixture<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn decodes_repos() {
        let repos: Vec<Repo> = fixture(include_str!("../../tests/fixtures/repos.json"));
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].repo.to_string(), "example/project");
        assert_eq!(repos[0].repo.forge, "github");
        assert!(repos[0].is_enabled);
    }

    #[test]
    fn decodes_queue() {
        let queue: Vec<QueueEntry> = fixture(include_str!("../../tests/fixtures/queue.json"));
        assert_eq!(queue.len(), 3);
        assert_eq!(queue[0].repo().to_string(), "example/project");
        assert_eq!(queue[0].build.status, Status::Building);
        assert_eq!(queue[0].build.pr_number, Some(42));
        assert_eq!(queue[1].build.pr_number, None);
        let eval_queue = queue[2].build.eval_queue.as_ref().unwrap();
        assert_eq!(eval_queue.position, Some(1));
        assert_eq!(eval_queue.blocked_by[0].number, 121);
    }

    #[test]
    fn decodes_build_page() {
        let page: BuildPage = fixture(include_str!("../../tests/fixtures/builds.json"));
        let statuses: Vec<_> = page.items.iter().map(|b| b.status.clone()).collect();
        assert_eq!(
            statuses,
            [
                Status::Evaluating,
                Status::Building,
                Status::Failed,
                Status::Succeeded,
                Status::Cancelled
            ]
        );
        assert!(page.items[0].finished_at.is_none());
        assert!(page.items[2].finished_at.is_some());
    }

    #[test]
    fn decodes_build_detail_with_attributes_and_effects() {
        let detail: BuildDetail = fixture(include_str!("../../tests/fixtures/build_running.json"));
        assert_eq!(detail.build.number, 120);
        assert_eq!(detail.attributes.len(), 12);
        let cached = detail
            .attributes
            .iter()
            .filter(|a| a.status == Status::SkippedLocal)
            .count();
        assert_eq!(cached, 3);

        let detail: BuildDetail = fixture(include_str!("../../tests/fixtures/build_effects.json"));
        let running = &detail.effects[0];
        assert_eq!(
            (running.label(), &running.status),
            ("default.deploy".to_owned(), &Status::Running)
        );
        let event = detail
            .effects
            .iter()
            .find(|e| e.kind == "build_finished")
            .unwrap();
        assert_eq!(event.label(), "build_finished/report-linux");
    }

    #[test]
    fn decodes_an_eval_failure_without_attributes() {
        let detail: BuildDetail =
            fixture(include_str!("../../tests/fixtures/build_eval_failed.json"));
        assert_eq!(detail.build.status, Status::Failed);
        assert!(detail.attributes.is_empty());
        assert!(detail.build.error.unwrap().contains("conflicts"));
    }

    #[test]
    fn decodes_effect_run() {
        let run: EffectRun = fixture(include_str!("../../tests/fixtures/effect_run.json"));
        assert_eq!(run.status, Status::Running);
        assert_eq!(run.skip_reason, None);
    }

    #[test]
    fn unknown_statuses_are_kept() {
        let status: Status = fixture("\"exotic\"");
        assert_eq!(status, Status::Other("exotic".into()));
        assert_eq!(status.label(), "exotic");
    }

    /// Feeds the body in small chunks, as the network would.
    async fn sse(body: &'static [u8]) -> Vec<eventsource_stream::Event> {
        futures_util::stream::iter(body.chunks(7).map(Ok::<_, std::convert::Infallible>))
            .eventsource()
            .map(Result::unwrap)
            .collect()
            .await
    }

    #[tokio::test]
    async fn decodes_log_stream_events() {
        let events: Vec<_> = sse(include_bytes!("../../tests/fixtures/log_stream.sse"))
            .await
            .iter()
            .map(|e| LogEvent::parse(&e.event, &e.data).unwrap().unwrap())
            .collect();
        let LogEvent::State(state) = &events[0] else {
            panic!("{:?}", events[0])
        };
        assert_eq!(state[1].name, "source");
        assert_eq!(
            events[1],
            LogEvent::Line {
                text: "\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m: unused variable: `x`".into()
            }
        );
        assert!(matches!(&events[4], LogEvent::Drv { idx: 3, name } if name == "tests"));
        assert!(matches!(&events[5], LogEvent::Phase { phase, .. } if phase == "checkPhase"));
        assert!(matches!(&events[6], LogEvent::DrvDone { status, .. } if status == "built"));
        assert_eq!(events[7], LogEvent::Done);
    }

    #[tokio::test]
    async fn decodes_effect_stream_events() {
        let events: Vec<_> = sse(include_bytes!("../../tests/fixtures/effect_stream.sse"))
            .await
            .iter()
            .filter_map(|e| EffectEvent::parse(&e.event, &e.data).unwrap())
            .collect();
        assert!(matches!(&events[0], EffectEvent::Text(t) if t.starts_with("deploying")));
        assert_eq!(events[1], EffectEvent::Done);
    }

    #[tokio::test]
    async fn decodes_status_events() {
        let events: Vec<StatusEvent> = sse(include_bytes!("../../tests/fixtures/events.sse"))
            .await
            .iter()
            .map(|e| serde_json::from_str(&e.data).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].build_id, Some(3020));
        assert_eq!(events[0].status, Some(Status::Building));
        assert_eq!(events[1].status, Some(Status::Succeeded));
    }
}
