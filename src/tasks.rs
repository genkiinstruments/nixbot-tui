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
    format::Progress,
};

/// Finished builds shown per host in [`Scope::Recent`].
const RECENT_FINISHED: usize = 30;
/// Build details fetched at once for the progress column.
const DETAIL_CONCURRENCY: usize = 8;
const EVENTS_RECONNECT: Duration = Duration::from_secs(3);
/// An effect's stream ends just before its status is written.
const EFFECT_STATUS_POLLS: u32 = 40;
const EFFECT_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Runs commands in the background and reports back with messages.
#[derive(Debug, Clone)]
pub struct Tasks {
    clients: Arc<[Client]>,
    msg_tx: mpsc::UnboundedSender<Msg>,
    /// Attribute counts of finished builds, which only change when a restart changes the status.
    finished: Arc<Mutex<HashMap<BuildKey, (Status, Progress)>>>,
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

        let mut builds: Vec<(RepoRef, Build)> = client
            .queue()
            .await?
            .into_iter()
            .map(|entry| (entry.repo(), entry.build))
            .collect();

        if scope == Scope::Recent {
            let pages = try_join_all(repos.keys().map(|repo| async move {
                let page = client.builds(repo, 1).await?;
                anyhow::Ok((repo.clone(), page))
            }))
            .await?;

            let mut finished: Vec<(RepoRef, Build)> = pages
                .into_iter()
                .flat_map(|(repo, page)| page.items.into_iter().map(move |b| (repo.clone(), b)))
                .filter(|(_, build)| !build.status.is_active())
                .collect();
            finished.sort_by_key(|(_, build)| std::cmp::Reverse(build.created_at));
            finished.truncate(RECENT_FINISHED);
            builds.extend(finished);
        }

        let rows = builds.into_iter().map(|(repo, build)| {
            let repo_url = repos.get(&repo).cloned().unwrap_or_default();
            let build_url = client.build_page(&repo, build.number);
            let key = BuildKey {
                host,
                repo,
                number: build.number,
            };
            Row::new(key, build, &repo_url, build_url)
        });

        stream::iter(rows)
            .map(|row| self.with_progress(client, row))
            .buffered(DETAIL_CONCURRENCY)
            .try_collect()
            .await
    }

    async fn with_progress(&self, client: &Client, mut row: Row) -> anyhow::Result<Row> {
        let Row { key, build, .. } = &row;
        let cached = self
            .finished
            .lock()
            .expect("the progress cache is never poisoned: nothing panics holding it")
            .get(key)
            .filter(|(status, _)| status == &build.status)
            .map(|&(_, progress)| progress);

        let progress = match cached {
            Some(progress) => progress,
            None => {
                let detail = client.build(&key.repo, key.number).await?;
                let progress = Progress::of(&detail.attributes);

                if !build.status.is_active() {
                    self.finished
                        .lock()
                        .expect("the progress cache is never poisoned: nothing panics holding it")
                        .insert(key.clone(), (build.status.clone(), progress));
                }

                progress
            }
        };

        row.progress = Some(progress);
        Ok(row)
    }

    /// Cues a refresh whenever a build changes, reconnecting for as long as the app runs.
    pub fn watch_events(&self) {
        for client in self.clients.iter() {
            let client = client.clone();
            let msg_tx = self.msg_tx.clone();

            tokio::spawn(async move {
                loop {
                    if let Ok(mut events) = client.status_events().await {
                        // Changes may have been missed while disconnected.
                        if msg_tx.send(Msg::StatusChanged { build_id: None }).is_err() {
                            return;
                        }

                        while let Some(Ok(event)) = events.next().await {
                            let msg = Msg::StatusChanged {
                                build_id: event.build_id,
                            };

                            if msg_tx.send(msg).is_err() {
                                return;
                            }
                        }
                    }

                    tokio::time::sleep(EVENTS_RECONNECT).await;
                }
            });
        }
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
/// went away first.
async fn follow_attr(
    client: &Client,
    key: &BuildKey,
    attr: &str,
    feed_tx: &mpsc::UnboundedSender<Feed>,
) -> Option<Option<String>> {
    let mut events = match client.log_stream(&key.repo, key.number, attr).await {
        Ok(events) => events,
        Err(err) => return Some(Some(err.to_string())),
    };

    let mut names: HashMap<u32, String> = HashMap::new();

    loop {
        let event = tokio::select! {
            () = feed_tx.closed() => return None,
            event = events.next() => event,
        };

        let line = match event {
            None | Some(Ok(LogEvent::Done)) => break,
            Some(Err(err)) => return Some(Some(err.to_string())),
            Some(Ok(LogEvent::State(derivations))) => {
                names = derivations.into_iter().map(|d| (d.idx, d.name)).collect();
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

        if feed_tx.send(Feed::Lines(vec![line])).is_err() {
            return None;
        }
    }

    let detail = client.build(&key.repo, key.number).await;
    let status = detail.ok().and_then(|detail| {
        let found = detail.attributes.into_iter().find(|a| a.attr == attr);
        found.map(|a| a.status)
    });
    Some(status.map(|s| format!("finished: {}", s.label())))
}

/// Streams an effect run's log from the start; the result is as for [`follow_attr`].
async fn follow_effect(
    client: &Client,
    repo: &RepoRef,
    run_id: u64,
    feed_tx: &mpsc::UnboundedSender<Feed>,
) -> Option<Option<String>> {
    let mut events = match client.effect_stream(repo, run_id, LOG_TAIL).await {
        Ok(events) => events,
        Err(err) => return Some(Some(err.to_string())),
    };

    let mut has_output = false;

    loop {
        let event = tokio::select! {
            () = feed_tx.closed() => return None,
            event = events.next() => event,
        };

        match event {
            None | Some(Ok(EffectEvent::Done)) => break,
            Some(Err(err)) => return Some(Some(err.to_string())),
            Some(Ok(EffectEvent::Text(text))) => {
                has_output = true;

                if feed_tx.send(Feed::Chunk(text)).is_err() {
                    return None;
                }
            }
        }
    }

    let note = match final_effect_run(client, repo, run_id).await {
        Ok(run) if has_output => format!("finished: {}", run.status.label()),
        Ok(run) => no_log(&run),
        Err(err) => err.to_string(),
    };
    Some(Some(note))
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
