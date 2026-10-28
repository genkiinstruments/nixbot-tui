//! The async work behind each [`Cmd`]: API calls, log streams and `gh`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{
    StreamExt,
    future::try_join_all,
    stream::{self, TryStreamExt},
};
use tokio::{process::Command, sync::mpsc};

use crate::{
    api::{Build, Client, EffectEvent, EffectRun, LogEvent, RepoRef, Status},
    app::{BuildKey, Cmd, Feed, FeedTx, LOG_TAIL, LogSource, Msg, PrInfo, Row, Scope},
    format::{EffectSummary, Progress},
};

/// Builds read per host besides the queue, newest first.
const RECENT_BUILDS: usize = 30;
/// Build details fetched at once for the progress column.
const DETAIL_CONCURRENCY: usize = 8;
const EVENTS_RECONNECT: Duration = Duration::from_secs(3);
/// An effect's stream ends just before its status is written.
const EFFECT_STATUS_POLLS: u32 = 40;
const EFFECT_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Wait before streaming a log again whose stream ended while it was still due to run.
const STREAM_RECONNECT: Duration = Duration::from_secs(2);

/// Runs commands in the background and reports back with messages.
#[derive(Debug, Clone)]
pub struct Tasks {
    clients: Arc<[Client]>,
    msg_tx: mpsc::UnboundedSender<Msg>,
    /// Counts of builds with nothing left to run; they only change when a restart changes the
    /// build's status.
    finished: Arc<Mutex<HashMap<BuildKey, Finished>>>,
}

#[derive(Debug, Clone)]
struct Counts {
    progress: Progress,
    effects: EffectSummary,
    /// Attribute and effect names, for the search.
    names: Vec<String>,
}

/// The counts of a build that had nothing left to run when it had this status.
#[derive(Debug, Clone)]
struct Finished {
    build_id: i64,
    status: Status,
    counts: Counts,
}

impl Tasks {
    pub fn new(clients: Vec<Client>, msg_tx: mpsc::UnboundedSender<Msg>) -> Self {
        Self {
            clients: clients.into(),
            msg_tx,
            finished: Arc::default(),
        }
    }

    pub fn spawn(&self, cmd: Cmd) {
        let tasks = self.clone();
        tokio::spawn(async move { tasks.run(cmd).await });
    }

    async fn run(self, cmd: Cmd) {
        let Self {
            clients, msg_tx, ..
        } = &self;
        let msg = match cmd {
            Cmd::LoadRows(scope) => Msg::Rows(self.rows(scope).await.map_err(|e| format!("{e:#}"))),
            Cmd::LoadDetail(key) => {
                let client = &clients[key.host];
                let detail = client.build(&key.repo, key.number).await;
                Msg::Detail(key, detail.map(Box::new).map_err(|e| e.to_string()))
            }
            Cmd::LoadLog {
                key,
                source,
                feed_tx: FeedTx(feed_tx),
            } => {
                let history = history(&clients[key.host], &key, &source).await;
                // A closed view no longer wants it.
                let _ = feed_tx.send(Feed::History(history));
                return;
            }
            Cmd::Follow {
                key,
                source,
                feed_tx: FeedTx(feed_tx),
            } => {
                let client = &clients[key.host];
                let note = match source {
                    LogSource::Attr(attr) => follow_attr(client, &key, &attr, &feed_tx).await,
                    LogSource::Effect { id, .. } => {
                        follow_effect(client, &key.repo, id, &feed_tx).await
                    }
                };

                if let Some(note) = note {
                    let _ = feed_tx.send(Feed::End(note));
                }

                return;
            }
            Cmd::Control { key, action } => {
                let result = clients[key.host]
                    .control(&key.repo, key.number, &action)
                    .await;
                Msg::ActionDone {
                    key,
                    action,
                    result: result.map_err(|e| e.to_string()),
                }
            }
            Cmd::Open(url) => match open::that_detached(&url) {
                Ok(()) => return,
                Err(err) => Msg::Failed(format!("opening {url}: {err}")),
            },
            Cmd::LoadPr { repo, number } => Msg::Pr {
                info: pr_info(&repo, number).await,
                repo,
                number,
            },
        };

        // The event loop is gone only while quitting.
        let _ = msg_tx.send(msg);
    }

    /// Rows for every host; the load fails if any host does, rather than showing a partial list.
    pub async fn rows(&self, scope: Scope) -> anyhow::Result<Vec<Row>> {
        let hosts = self.clients.iter().enumerate();
        let per_host =
            try_join_all(hosts.map(|(host, client)| self.host_rows(host, client, scope)));
        Ok(per_host.await?.into_iter().flatten().collect())
    }

    async fn host_rows(
        &self,
        host: usize,
        client: &Client,
        scope: Scope,
    ) -> anyhow::Result<Vec<Row>> {
        let repos: HashMap<RepoRef, String> = client
            .repos()
            .await?
            .into_iter()
            .filter(|r| r.is_enabled)
            .map(|r| (r.repo, r.url))
            .collect();

        let queued: Vec<(RepoRef, Build)> = client
            .queue()
            .await?
            .into_iter()
            .map(|entry| (entry.repo(), entry.build))
            .collect();

        // Finished builds are read even for the active scope: their effects may still run.
        let pages = try_join_all(repos.keys().map(|repo| async move {
            let page = client.builds(repo, 1).await?;
            anyhow::Ok((repo.clone(), page))
        }))
        .await?;

        let is_queued = |repo: &RepoRef, number: u64| {
            queued.iter().any(|(r, b)| r == repo && b.number == number)
        };
        let mut recent: Vec<(RepoRef, Build)> = pages
            .into_iter()
            .flat_map(|(repo, page)| page.items.into_iter().map(move |b| (repo.clone(), b)))
            .filter(|(repo, build)| !is_queued(repo, build.number))
            .collect();
        recent.sort_by_key(|(_, build)| std::cmp::Reverse(build.created_at));
        recent.truncate(RECENT_BUILDS);

        let rows = queued.iter().cloned().chain(recent).map(|(repo, build)| {
            let repo_url = repos.get(&repo).cloned().unwrap_or_default();
            let build_url = client.build_page(&repo, build.number);
            let key = BuildKey {
                host,
                repo,
                number: build.number,
            };
            Row::new(key, build, &repo_url, build_url)
        });

        let mut rows: Vec<Row> = stream::iter(rows)
            .map(|row| self.with_summary(client, row))
            .buffered(DETAIL_CONCURRENCY)
            .try_collect()
            .await?;

        if scope == Scope::Active {
            rows.retain(Row::is_active);
        }

        Ok(rows)
    }

    /// Attribute and effect counts, from the cache once nothing about the build can change.
    async fn with_summary(&self, client: &Client, mut row: Row) -> anyhow::Result<Row> {
        let Row { key, build, .. } = &row;
        let cached = self
            .finished
            .lock()
            .expect("the summary cache is never poisoned: nothing panics holding it")
            .get(key)
            .filter(|finished| finished.status == build.status)
            .map(|finished| finished.counts.clone());

        let counts = match cached {
            Some(counts) => counts,
            None => {
                let detail = client.build(&key.repo, key.number).await?;
                let attrs = detail.attributes.iter().map(|a| a.attr.clone());
                let effects = detail.effects.iter().map(|e| e.name.clone());
                Counts {
                    progress: Progress::of(&detail.attributes),
                    effects: EffectSummary::of(&detail.effects),
                    names: attrs.chain(effects).collect(),
                }
            }
        };

        let Counts {
            progress,
            effects,
            names,
        } = counts.clone();
        row.progress = Some(progress);
        row.effects = Some(effects);
        row.names = names;

        if !row.is_active() {
            let Row { key, build, .. } = &row;
            let finished = Finished {
                build_id: build.id,
                status: build.status.clone(),
                counts,
            };
            self.finished
                .lock()
                .expect("the summary cache is never poisoned: nothing panics holding it")
                .insert(key.clone(), finished);
        }

        Ok(row)
    }

    /// Cues a refresh whenever a build changes, reconnecting for as long as the app runs.
    pub fn watch_events(&self) {
        for (host, client) in self.clients.iter().enumerate() {
            let client = client.clone();
            let tasks = self.clone();

            tokio::spawn(async move {
                loop {
                    if let Ok(mut events) = client.status_events().await {
                        // Changes may have been missed while disconnected.
                        if !tasks.changed(host, None) {
                            return;
                        }

                        while let Some(Ok(event)) = events.next().await {
                            if !tasks.changed(host, event.build_id) {
                                return;
                            }
                        }
                    }

                    tokio::time::sleep(EVENTS_RECONNECT).await;
                }
            });
        }
    }

    /// Forgets the cached counts of a changed build (all of the host's when unknown), since a
    /// finished build can still gain effects, and tells the app. False once the app is gone.
    fn changed(&self, host: usize, build_id: Option<i64>) -> bool {
        self.finished
            .lock()
            .expect("the summary cache is never poisoned: nothing panics holding it")
            .retain(|key, finished| {
                key.host != host || build_id.is_some_and(|id| id != finished.build_id)
            });

        self.msg_tx.send(Msg::StatusChanged { build_id }).is_ok()
    }
}

async fn history(client: &Client, key: &BuildKey, source: &LogSource) -> Result<String, String> {
    match source {
        LogSource::Attr(attr) => client
            .log_text(&key.repo, key.number, attr, LOG_TAIL)
            .await
            .map_err(|e| e.to_string()),
        LogSource::Effect { id, .. } => {
            match client.effect_run_text(&key.repo, *id, LOG_TAIL).await {
                Ok(text) => Ok(text),
                Err(err) if err.is_not_found() => match client.effect_run(&key.repo, *id).await {
                    Ok(run) => Err(no_log(&run)),
                    Err(err) => Err(err.to_string()),
                },
                Err(err) => Err(err.to_string()),
            }
        }
    }
}

/// Why an effect run has no log: it was skipped or has not started.
fn no_log(run: &EffectRun) -> String {
    let EffectRun {
        status,
        error,
        skip_reason,
        ..
    } = run;

    match skip_reason.as_ref().or(error.as_ref()) {
        Some(reason) => format!("no log: {}: {reason}", status.label()),
        None => format!("no log: {}", status.label()),
    }
}

/// Streams an attribute's live output; the result is the note to end with, `None` if the view
/// went away first. A pending attribute, restarted or not yet scheduled, has nothing to stream
/// yet, so it is streamed again until it has run.
async fn follow_attr(
    client: &Client,
    key: &BuildKey,
    attr: &str,
    feed_tx: &mpsc::UnboundedSender<Feed>,
) -> Option<Option<String>> {
    let mut names: HashMap<u32, String> = HashMap::new();

    loop {
        match stream_attr(client, key, attr, feed_tx, &mut names).await {
            Streamed::Closed => return None,
            Streamed::Failed(err) => return Some(Some(err)),
            Streamed::Ended { .. } => {}
        }

        let status = match client.build(&key.repo, key.number).await {
            Ok(detail) => detail
                .attributes
                .into_iter()
                .find(|a| a.attr == attr)
                .map(|a| a.status),
            Err(err) => return Some(Some(err.to_string())),
        };

        match status {
            Some(status) if status.is_active() => {
                tokio::select! {
                    () = feed_tx.closed() => return None,
                    () = tokio::time::sleep(STREAM_RECONNECT) => {}
                }
            }
            status => return Some(status.map(|s| format!("finished: {}", s.label()))),
        }
    }
}

/// One connection to an attribute's stream; `names` carries derivation names across them.
async fn stream_attr(
    client: &Client,
    key: &BuildKey,
    attr: &str,
    feed_tx: &mpsc::UnboundedSender<Feed>,
    names: &mut HashMap<u32, String>,
) -> Streamed {
    let mut events = match client.log_stream(&key.repo, key.number, attr).await {
        Ok(events) => events,
        Err(err) => return Streamed::Failed(err.to_string()),
    };

    let mut has_output = false;

    loop {
        let event = tokio::select! {
            () = feed_tx.closed() => return Streamed::Closed,
            event = events.next() => event,
        };

        let line = match event {
            None | Some(Ok(LogEvent::Done)) => return Streamed::Ended { has_output },
            Some(Err(err)) => return Streamed::Failed(err.to_string()),
            Some(Ok(LogEvent::State(derivations))) => {
                *names = derivations.into_iter().map(|d| (d.idx, d.name)).collect();
                continue;
            }
            Some(Ok(LogEvent::Drv { idx, name })) => {
                let line = format!("── {name} ──");
                names.insert(idx, name);
                line
            }
            Some(Ok(LogEvent::Phase { idx, phase })) => {
                format!(
                    "── {}: {phase} ──",
                    names.get(&idx).map_or(attr, String::as_str)
                )
            }
            Some(Ok(LogEvent::DrvDone { idx, status })) => {
                let name = names.get(&idx).map_or(attr, String::as_str);
                format!("── {name}: {} ──", Status::from(status).label())
            }
            Some(Ok(LogEvent::Line { text })) => text,
        };

        has_output = true;

        if feed_tx.send(Feed::Lines(vec![line])).is_err() {
            return Streamed::Closed;
        }
    }
}

/// How one connection to an effect run's stream ended.
enum Streamed {
    /// The view went away.
    Closed,
    Failed(String),
    Ended {
        has_output: bool,
    },
}

/// Streams an effect run's log from the start; the result is as for [`follow_effect`]. A run
/// that has not started, or is still running when its stream ends, is streamed again.
async fn follow_effect(
    client: &Client,
    repo: &RepoRef,
    run_id: u64,
    feed_tx: &mpsc::UnboundedSender<Feed>,
) -> Option<Option<String>> {
    let mut has_shown_output = false;

    loop {
        let has_output = match stream_effect(client, repo, run_id, feed_tx, has_shown_output).await
        {
            Streamed::Closed => return None,
            Streamed::Failed(err) => return Some(Some(err)),
            Streamed::Ended { has_output } => has_output,
        };
        has_shown_output |= has_output;

        let run = if has_output {
            final_effect_run(client, repo, run_id).await
        } else {
            client.effect_run(repo, run_id).await
        };

        match run {
            Err(err) => return Some(Some(err.to_string())),
            Ok(run) if run.status.is_active() => {
                tokio::select! {
                    () = feed_tx.closed() => return None,
                    () = tokio::time::sleep(STREAM_RECONNECT) => {}
                }
            }
            Ok(run) if has_shown_output => {
                return Some(Some(format!("finished: {}", run.status.label())));
            }
            Ok(run) => return Some(Some(no_log(&run))),
        }
    }
}

/// One connection; a stream replays the log, so `replaces` clears what an earlier one showed.
async fn stream_effect(
    client: &Client,
    repo: &RepoRef,
    run_id: u64,
    feed_tx: &mpsc::UnboundedSender<Feed>,
    mut replaces: bool,
) -> Streamed {
    let mut events = match client.effect_stream(repo, run_id, LOG_TAIL).await {
        Ok(events) => events,
        Err(err) => return Streamed::Failed(err.to_string()),
    };

    let mut has_output = false;

    loop {
        let event = tokio::select! {
            () = feed_tx.closed() => return Streamed::Closed,
            event = events.next() => event,
        };

        let text = match event {
            None | Some(Ok(EffectEvent::Done)) => return Streamed::Ended { has_output },
            Some(Err(err)) => return Streamed::Failed(err.to_string()),
            Some(Ok(EffectEvent::Text(text))) => text,
        };

        if std::mem::take(&mut replaces) && feed_tx.send(Feed::Clear).is_err() {
            return Streamed::Closed;
        }

        has_output = true;

        if feed_tx.send(Feed::Chunk(text)).is_err() {
            return Streamed::Closed;
        }
    }
}

async fn final_effect_run(
    client: &Client,
    repo: &RepoRef,
    run_id: u64,
) -> crate::api::Result<EffectRun> {
    let mut run = client.effect_run(repo, run_id).await?;

    for _ in 0..EFFECT_STATUS_POLLS {
        if !run.status.is_active() {
            break;
        }

        tokio::time::sleep(EFFECT_STATUS_POLL_INTERVAL).await;
        run = client.effect_run(repo, run_id).await?;
    }

    Ok(run)
}

/// The pull request's title from `gh`, which has the user's forge credentials.
pub async fn pr_info(repo: &RepoRef, number: u64) -> Option<PrInfo> {
    #[derive(serde::Deserialize)]
    struct Pr {
        title: String,
        url: String,
    }

    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &repo.to_string(),
        ])
        .args(["--json", "title,url"])
        .kill_on_drop(true)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    let Pr { title, url } = serde_json::from_slice(&output.stdout).ok()?;
    Some(PrInfo { title, url })
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    use super::*;

    const PROJECT: &str = "/api/repos/github/example/project";

    fn json(body: &'static str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    #[tokio::test]
    async fn finished_builds_are_reread_after_a_status_event() {
        let server = MockServer::start().await;
        let mounts = [
            Mock::given(path("/api/repos"))
                .respond_with(json(include_str!("../tests/fixtures/repos.json"))),
            Mock::given(path("/api/queue")).respond_with(json("[]")),
            Mock::given(path(format!("{PROJECT}/builds")))
                .respond_with(json(include_str!("../tests/fixtures/builds.json"))),
            Mock::given(path("/api/repos/github/example/website/builds"))
                .respond_with(json(r#"{"items": [], "page": 1, "has_next": false}"#)),
            Mock::given(path_regex(format!("^{PROJECT}/builds/11[5-9]$"))).respond_with(json(
                include_str!("../tests/fixtures/build_eval_failed.json"),
            )),
        ];

        for mock in mounts {
            mock.mount(&server).await;
        }

        let reads_of_116 = Mock::given(method("GET"))
            .and(path(format!("{PROJECT}/builds/116")))
            .respond_with(json(include_str!(
                "../tests/fixtures/build_eval_failed.json"
            )))
            .with_priority(1)
            .expect(2)
            .mount_as_scoped(&server)
            .await;

        let client = Client::new(&server.uri(), None).unwrap();
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel();
        let tasks = Tasks::new(vec![client], msg_tx);

        assert_eq!(tasks.rows(Scope::Recent).await.unwrap().len(), 5);
        tasks.rows(Scope::Recent).await.unwrap();
        assert!(tasks.changed(0, Some(3019)));
        assert_eq!(
            msg_rx.recv().await,
            Some(Msg::StatusChanged {
                build_id: Some(3019)
            })
        );

        let rows = tasks.rows(Scope::Active).await.unwrap();
        let numbers: Vec<u64> = rows.iter().map(|r| r.key.number).collect();
        assert_eq!(numbers, [119, 118], "only the builds still going");
        drop(reads_of_116);
    }
}
