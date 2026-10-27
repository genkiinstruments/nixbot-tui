use std::time::Duration;

use nixbot_tui::{
    api::{Client, RepoRef},
    app::{BuildKey, Cmd, Feed, FeedTx, LogSource, LogView},
    tasks::Tasks,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const RUN: &str = "/api/repos/github/example/project/effects/runs/7";

fn run_with_status(status: &str) -> ResponseTemplate {
    let mut run: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/effect_run.json")).unwrap();
    run["status"] = status.into();
    ResponseTemplate::new(200).set_body_json(run)
}

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_owned(), "text/event-stream")
}

fn key() -> BuildKey {
    BuildKey {
        host: 0,
        repo: RepoRef {
            forge: "github".into(),
            owner: "example".into(),
            name: "project".into(),
        },
        number: 118,
    }
}

fn source() -> LogSource {
    LogSource::Effect {
        id: 7,
        label: "default.deploy".into(),
    }
}

/// Everything fed to the view until the follow task ends.
async fn follow(server: &MockServer) -> Vec<Feed> {
    let client = Client::new(&server.uri(), None).unwrap();
    let (msg_tx, _msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut view = LogView::new(key(), source(), String::new());
    Tasks::new(vec![client], msg_tx).spawn(Cmd::Follow {
        key: key(),
        source: source(),
        feed_tx: FeedTx(view.feed_tx()),
    });

    let mut feeds = Vec::new();

    loop {
        let feed = tokio::time::timeout(Duration::from_secs(20), view.next_feed())
            .await
            .expect("the follow task ends with Feed::End");
        let is_end = matches!(feed, Feed::End(_));
        feeds.push(feed);

        if is_end {
            return feeds;
        }
    }
}

#[tokio::test]
async fn following_a_run_that_has_not_started_waits_for_its_output() {
    let server = MockServer::start().await;
    // Not started: the stream ends at once and the run is still pending.
    Mock::given(method("GET"))
        .and(path(format!("{RUN}/stream")))
        .respond_with(sse("event: done\ndata: {}\n\n"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(RUN))
        .respond_with(run_with_status("pending"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{RUN}/stream")))
        .respond_with(sse(include_str!("fixtures/effect_stream_running.sse")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(RUN))
        .respond_with(run_with_status("succeeded"))
        .mount(&server)
        .await;

    let feeds = follow(&server).await;
    let mut view = LogView::new(key(), source(), String::new());

    for feed in feeds.clone() {
        view.apply(feed);
    }

    assert!(
        !feeds.contains(&Feed::Clear),
        "nothing to replace: {feeds:?}"
    );
    assert_eq!(
        view.lines,
        [
            "building '/nix/store/q2m8f0dz4x6vy3h1k9w7c5b0n8r2s4t6-effect-env.drv'...",
            "connecting to the signing host...",
            "signing the bundles...",
            "signed project.app",
            "packaging done",
        ]
    );
    assert_eq!(view.note.as_deref(), Some("finished: built"));
}
