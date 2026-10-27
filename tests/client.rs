use futures_util::StreamExt;
use nixbot_tui::{
    actions::{Action, Target, Verb},
    api::{Client, Error, LogEvent, RepoRef, Status},
    config::Token,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn repo() -> RepoRef {
    RepoRef {
        forge: "github".into(),
        owner: "example".into(),
        name: "project".into(),
    }
}

fn client(server: &MockServer) -> Client {
    Client::new(&server.uri(), Some(Token::new("secret"))).unwrap()
}

#[tokio::test]
async fn control_actions_post_with_the_token() {
    let server = MockServer::start().await;
    let base = "/api/repos/github/example/project/builds/120";
    let cases = [
        (
            Action::new(Verb::Restart, Target::Build),
            format!("{base}/restart"),
        ),
        (
            Action::new(Verb::Cancel, Target::Build),
            format!("{base}/cancel"),
        ),
        (
            Action::new(
                Verb::Restart,
                Target::Attr("checks.x86_64-linux.tests".into()),
            ),
            format!("{base}/attrs/checks.x86_64-linux.tests/restart"),
        ),
        (
            Action::new(Verb::Cancel, Target::Effects),
            format!("{base}/effects/cancel"),
        ),
    ];
    for (action, expected) in cases {
        let _guard = Mock::given(method("POST"))
            .and(path(expected.as_str()))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"number": 120, "action": "restart"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        client(&server)
            .control(&repo(), 120, &action)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn single_effect_actions_pass_name_and_kind() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/repos/github/example/project/builds/7/effects/restart",
        ))
        .and(query_param("name", "preview"))
        .and(query_param("kind", "pull_request"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let action = Action::new(
        Verb::Restart,
        Target::Effect {
            name: "preview".into(),
            kind: "pull_request".into(),
        },
    );
    client(&server).control(&repo(), 7, &action).await.unwrap();
}

#[tokio::test]
async fn control_errors_carry_the_server_detail() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({"detail": "not allowed"})),
        )
        .mount(&server)
        .await;
    let err = client(&server)
        .control(&repo(), 1, &Action::new(Verb::Cancel, Target::Build))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "HTTP 403 Forbidden: not allowed");
}

#[tokio::test]
async fn control_without_a_token_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let anonymous = Client::new(&server.uri(), None).unwrap();
    let err = anonymous
        .control(&repo(), 1, &Action::new(Verb::Restart, Target::Build))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NoToken(_)), "{err}");
}

#[tokio::test]
async fn reads_send_the_token_too() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/queue"))
        .and(header("authorization", "Bearer secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(include_str!("fixtures/queue.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let queue = client(&server).queue().await.unwrap();
    assert_eq!(queue[0].build.status, Status::Building);
}

#[tokio::test]
async fn not_found_is_recognisable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("no log"))
        .mount(&server)
        .await;
    let err = client(&server)
        .effect_run_text(&repo(), 5, 100)
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "{err}");
}

#[tokio::test]
async fn log_stream_yields_typed_events() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/repos/github/example/project/builds/120/logs/checks.x86_64-linux.tests/stream",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(include_str!("fixtures/log_stream.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let events: Vec<LogEvent> = client(&server)
        .log_stream(&repo(), 120, "checks.x86_64-linux.tests")
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(events.len(), 8);
    assert!(matches!(events[0], LogEvent::State(_)));
    assert_eq!(events.last(), Some(&LogEvent::Done));
}

#[tokio::test]
async fn status_events_skip_the_retry_preamble() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(include_str!("fixtures/events.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let events: Vec<_> = client(&server)
        .status_events()
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].status, Some(Status::Succeeded));
}

#[test]
fn web_urls_keep_a_base_path() {
    let client = Client::new("https://ci.example.org/nixbot/", None).unwrap();
    assert_eq!(
        client.build_page(&repo(), 3),
        "https://ci.example.org/nixbot/repos/github/example/project/builds/3"
    );
    assert_eq!(
        client.log_page(&repo(), 3, "checks.x86_64-linux.tests"),
        "https://ci.example.org/nixbot/repos/github/example/project/builds/3/logs/checks.x86_64-linux.tests"
    );
}
