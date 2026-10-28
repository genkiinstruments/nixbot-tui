use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::api::{QueueEntry, Status};

fn queue_rows() -> Vec<Row> {
    let queue: Vec<QueueEntry> =
        serde_json::from_str(include_str!("../../tests/fixtures/queue.json")).unwrap();

    queue
        .into_iter()
        .map(|entry| {
            let key = BuildKey {
                host: 0,
                repo: entry.repo(),
                number: entry.build.number,
            };
            let build_url = format!("https://nixbot.example.org/builds/{}", key.number);
            Row::new(key, entry.build, &entry.url, build_url)
        })
        .collect()
}

fn running_detail() -> BuildDetail {
    serde_json::from_str(include_str!("../../tests/fixtures/build_running.json")).unwrap()
}

fn effects_detail() -> BuildDetail {
    serde_json::from_str(include_str!("../../tests/fixtures/build_effects.json")).unwrap()
}

fn press(app: &mut App, code: KeyCode) -> Vec<Cmd> {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn type_keys(app: &mut App, keys: &str) -> Vec<Cmd> {
    keys.chars()
        .flat_map(|c| press(app, KeyCode::Char(c)))
        .collect()
}

/// The first tick after `secs` seconds.
fn tick(app: &mut App, start: Instant, secs: u64) -> Vec<Cmd> {
    app.update(Msg::Tick(start + Duration::from_secs(secs)))
}

/// An app showing the queue fixture, sorted newest build first.
fn loaded() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(start);
    assert_eq!(tick(&mut app, start, 0), [Cmd::LoadRows(Scope::Recent)]);

    app.update(Msg::Rows(Ok(queue_rows())));
    (app, start)
}

/// Opens build #120 (`feat/widget`) and loads its attributes.
fn open_detail(app: &mut App) -> BuildKey {
    type_keys(app, "G");
    let key = app.selected_row().unwrap().key.clone();
    assert_eq!(key.number, 120);
    assert_eq!(press(app, KeyCode::Enter), [Cmd::LoadDetail(key.clone())]);

    app.update(Msg::Detail(key.clone(), Ok(Box::new(running_detail()))));
    key
}

fn open_effects(app: &mut App) -> BuildKey {
    let key = app.rows[0].key.clone();
    press(app, KeyCode::Enter);
    app.update(Msg::Detail(key.clone(), Ok(Box::new(effects_detail()))));
    press(app, KeyCode::Tab);
    key
}

fn feed_tx(cmd: &Cmd) -> &FeedTx {
    match cmd {
        Cmd::LoadLog { feed_tx, .. } | Cmd::Follow { feed_tx, .. } => feed_tx,
        other => panic!("not a log command: {other:?}"),
    }
}

fn log(app: &App) -> &LogView {
    app.log.as_ref().unwrap()
}

#[test]
fn rows_sort_newest_first_and_look_up_each_pr_once() {
    let start = Instant::now();
    let mut app = App::new(start);
    tick(&mut app, start, 0);

    let cmds = app.update(Msg::Rows(Ok(queue_rows())));
    let numbers: Vec<u64> = app.rows.iter().map(|r| r.key.number).collect();
    assert_eq!(numbers, [122, 121, 120]);
    assert_eq!(app.table.selected(), Some(0));
    assert_eq!(
        cmds,
        [
            Cmd::LoadPr {
                repo: app.rows[0].key.repo.clone(),
                number: 41
            },
            Cmd::LoadPr {
                repo: app.rows[0].key.repo.clone(),
                number: 42
            },
        ]
    );
    assert_eq!(app.update(Msg::Rows(Ok(queue_rows()))), []);
}

#[test]
fn selection_follows_the_build_across_refreshes() {
    let (mut app, _) = loaded();
    type_keys(&mut app, "j");
    assert_eq!(app.selected_row().unwrap().key.number, 121);

    let mut rows = queue_rows();
    rows.retain(|r| r.key.number != 122);
    app.update(Msg::Rows(Ok(rows)));
    assert_eq!(app.selected_row().unwrap().key.number, 121);
}

#[test]
fn navigation_clamps_to_the_rows() {
    let (mut app, _) = loaded();
    type_keys(&mut app, "kkk");
    assert_eq!(app.table.selected(), Some(0));

    type_keys(&mut app, "jjjjj");
    assert_eq!(app.table.selected(), Some(2));

    type_keys(&mut app, "g");
    assert_eq!(app.table.selected(), Some(0));
}

#[test]
fn a_failed_refresh_keeps_the_rows() {
    let (mut app, _) = loaded();
    app.update(Msg::Rows(Err("connection refused".into())));
    assert_eq!(app.rows.len(), 3);
    assert_eq!(app.rows_error.as_deref(), Some("connection refused"));
}

#[test]
fn toggling_the_active_filter_reloads_at_once() {
    let (mut app, _) = loaded();
    assert_eq!(type_keys(&mut app, "a"), [Cmd::LoadRows(Scope::Active)]);
    assert_eq!(type_keys(&mut app, "a"), [], "waits for the load in flight");

    app.update(Msg::Rows(Ok(queue_rows())));
    assert_eq!(app.scope, Scope::Recent);
}

#[test]
fn restart_asks_first_and_only_yes_sends() {
    let (mut app, _) = loaded();
    let key = app.selected_row().unwrap().key.clone();
    let restart = Action::new(Verb::Restart, Target::Build);
    assert_eq!(type_keys(&mut app, "r"), []);
    assert_eq!(
        app.modal,
        Some(Modal::Confirm {
            key: key.clone(),
            action: restart.clone()
        })
    );

    assert_eq!(type_keys(&mut app, "x"), [], "other keys keep asking");
    assert!(app.modal.is_some());

    assert_eq!(type_keys(&mut app, "n"), []);
    assert_eq!(app.modal, None);

    type_keys(&mut app, "r");
    assert_eq!(
        type_keys(&mut app, "y"),
        [Cmd::Control {
            key,
            action: restart
        }]
    );
    assert_eq!(app.modal, None);
    assert_eq!(
        app.flash.as_ref().map(|f| f.text.as_str()),
        Some("sending: restart the build…"),
        "something shows before the server answers"
    );
}

#[test]
fn esc_declines_a_confirmation() {
    let (mut app, _) = loaded();
    type_keys(&mut app, "c");
    assert!(app.modal.is_some());

    assert_eq!(press(&mut app, KeyCode::Esc), []);
    assert_eq!(app.modal, None);
    assert!(!app.should_quit);
}

#[test]
fn cancelling_a_finished_build_is_refused_without_asking() {
    let (mut app, _) = loaded();
    let mut rows = queue_rows();

    for row in &mut rows {
        row.build.status = Status::Succeeded;
    }

    app.update(Msg::Rows(Ok(rows)));
    type_keys(&mut app, "c");
    assert_eq!(app.modal, None);
    assert_eq!(app.flash.as_ref().unwrap().kind, FlashKind::Error);
}

#[test]
fn action_results_are_flashed_then_expire_and_trigger_a_refresh() {
    let (mut app, start) = loaded();
    app.update(Msg::ActionDone {
        key: app.rows[0].key.clone(),
        action: Action::new(Verb::Cancel, Target::Build),
        result: Err("HTTP 403 Forbidden: not allowed".into()),
    });
    let flash = app.flash.clone().unwrap();
    assert_eq!(flash.kind, FlashKind::Error);
    assert_eq!(
        flash.text,
        "cancel the build failed: HTTP 403 Forbidden: not allowed"
    );

    assert_eq!(tick(&mut app, start, 1), [Cmd::LoadRows(Scope::Recent)]);
    assert!(app.flash.is_some());

    tick(&mut app, start, 7);
    assert!(app.flash.is_none());
}

#[test]
fn open_prefers_the_pull_request() {
    let (mut app, _) = loaded();
    assert_eq!(
        type_keys(&mut app, "o"),
        [Cmd::Open(
            "https://github.com/example/project/pull/41".into()
        )]
    );

    app.update(Msg::Pr {
        repo: app.rows[0].key.repo.clone(),
        number: 41,
        info: Some(PrInfo {
            title: "Add a widget".into(),
            url: "https://github.com/example/project/pull/41#from-gh".into(),
        }),
    });
    assert_eq!(
        type_keys(&mut app, "o"),
        [Cmd::Open(
            "https://github.com/example/project/pull/41#from-gh".into()
        )]
    );

    type_keys(&mut app, "j");
    assert_eq!(
        type_keys(&mut app, "o"),
        [Cmd::Open("https://nixbot.example.org/builds/121".into())],
        "no PR: the build page"
    );
}

#[test]
fn detail_sorts_running_before_queued_before_done() {
    let (mut app, _) = loaded();
    open_detail(&mut app);

    let view = app.detail.as_ref().unwrap();
    let statuses: Vec<&Status> = view.attributes().iter().map(|a| &a.status).collect();
    let first_done = statuses.iter().position(|s| !s.is_active()).unwrap();
    assert!(statuses[..first_done].iter().all(|s| s.is_active()));
    assert_eq!(statuses[0], &Status::Building);
    assert_eq!(view.row.progress.unwrap().total, 12);
    assert_eq!(
        view.selected_attr().unwrap().attr,
        "checks.aarch64-darwin.clippy"
    );
}

#[test]
fn failures_sort_first() {
    let (mut app, _) = loaded();
    let key = app.rows[0].key.clone();
    press(&mut app, KeyCode::Enter);

    let mut detail = running_detail();
    detail.attributes[11].status = Status::Failed;
    let failed = detail.attributes[11].attr.clone();
    app.update(Msg::Detail(key, Ok(Box::new(detail))));
    assert_eq!(app.detail.unwrap().attributes()[0].attr, failed);
}

#[test]
fn stale_detail_responses_are_ignored() {
    let (mut app, _) = loaded();
    press(&mut app, KeyCode::Enter);

    let mut other = app.rows[1].key.clone();
    other.number = 999;
    app.update(Msg::Detail(other, Ok(Box::new(running_detail()))));
    assert!(app.detail.as_ref().unwrap().detail.is_none());
}

#[test]
fn filter_narrows_attributes_and_esc_clears_it() {
    let (mut app, _) = loaded();
    open_detail(&mut app);
    type_keys(&mut app, "/plug");
    assert!(app.detail.as_ref().unwrap().is_editing_filter);

    press(&mut app, KeyCode::Enter);
    let view = app.detail.as_ref().unwrap();
    let names: Vec<&str> = view
        .visible_attrs()
        .iter()
        .map(|a| a.attr.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "checks.aarch64-darwin.plugin",
            "checks.aarch64-linux.plugin-host"
        ]
    );
    assert!(!view.is_editing_filter);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.detail.as_ref().unwrap().visible_attrs().len(), 12);

    press(&mut app, KeyCode::Esc);
    assert!(app.detail.is_none(), "a second Esc goes back");
}

#[test]
fn selection_survives_a_detail_refresh() {
    let (mut app, _) = loaded();
    let key = open_detail(&mut app);
    type_keys(&mut app, "jj");
    let selected = app
        .detail
        .as_ref()
        .unwrap()
        .selected_attr()
        .unwrap()
        .attr
        .clone();

    let mut refreshed = running_detail();
    refreshed.attributes.reverse();
    app.update(Msg::Detail(key, Ok(Box::new(refreshed))));
    let view = app.detail.as_ref().unwrap();
    assert_eq!(view.selected_attr().unwrap().attr, selected);
}

#[test]
fn attribute_and_build_actions_from_the_detail() {
    let (mut app, _) = loaded();
    let key = open_detail(&mut app);
    type_keys(&mut app, "c");
    assert_eq!(
        app.modal,
        Some(Modal::Confirm {
            key: key.clone(),
            action: Action::new(
                Verb::Cancel,
                Target::Attr("checks.aarch64-darwin.clippy".into())
            ),
        })
    );

    press(&mut app, KeyCode::Esc);
    type_keys(&mut app, "R");
    assert_eq!(
        app.modal,
        Some(Modal::Confirm {
            key,
            action: Action::new(Verb::Restart, Target::Build),
        })
    );
}

#[test]
fn effect_actions_target_one_effect_or_all() {
    let (mut app, _) = loaded();
    let key = open_effects(&mut app);
    let view = app.detail.as_ref().unwrap();
    assert_eq!(view.pane, Pane::Effects);
    assert_eq!(view.selected_effect().unwrap().status, Status::Running);

    type_keys(&mut app, "c");
    let deploy = Target::Effect {
        name: "default.deploy".into(),
        kind: "push".into(),
    };
    assert_eq!(
        app.modal,
        Some(Modal::Confirm {
            key: key.clone(),
            action: Action::new(Verb::Cancel, deploy)
        })
    );

    press(&mut app, KeyCode::Esc);
    type_keys(&mut app, "E");
    assert_eq!(
        app.modal,
        Some(Modal::Confirm {
            key,
            action: Action::new(Verb::Restart, Target::Effects)
        })
    );
}

#[test]
fn enter_tails_a_finished_attribute_and_f_follows_it() {
    let (mut app, _) = loaded();
    let key = open_detail(&mut app);
    type_keys(&mut app, "G");
    let source = LogSource::Attr("checks.aarch64-linux.server".into());
    let load = press(&mut app, KeyCode::Enter);
    assert_eq!(load.len(), 1, "only the history: {load:?}");
    let Cmd::LoadLog {
        key: load_key,
        source: load_source,
        feed_tx: history_tx,
    } = &load[0]
    else {
        panic!("{load:?}")
    };
    assert_eq!((load_key, load_source), (&key, &source));

    app.update(Msg::Log(Feed::History(Ok("one\ntwo\n".into()))));
    let follow = type_keys(&mut app, "f");
    let Cmd::Follow {
        source: follow_source,
        ..
    } = &follow[0]
    else {
        panic!("{follow:?}")
    };
    assert_eq!(follow_source, &source);
    assert!(log(&app).is_following);
    assert_eq!(
        feed_tx(&follow[0]),
        history_tx,
        "history still arriving is kept"
    );

    app.update(Msg::Log(Feed::Lines(vec![
        "three\r".into(),
        "progress 10%\rprogress 100%".into(),
    ])));
    assert_eq!(log(&app).lines, ["one", "two", "three", "progress 100%"]);

    assert_eq!(type_keys(&mut app, "f"), []);
    assert!(!log(&app).is_following);
    assert!(
        feed_tx(&follow[0]).0.is_closed(),
        "unfollowing stops the stream"
    );
}

#[test]
fn enter_follows_what_is_still_running() {
    let (mut app, _) = loaded();
    open_detail(&mut app);
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(cmds.as_slice(), [Cmd::LoadLog { .. }, Cmd::Follow { .. }]),
        "{cmds:?}"
    );
    assert!(log(&app).is_following);

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    type_keys(&mut app, "g");
    open_effects(&mut app);
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(cmds.as_slice(), [Cmd::Follow { .. }]), "{cmds:?}");
}

#[test]
fn a_replayed_stream_replaces_what_was_shown() {
    let (mut app, _) = loaded();
    open_effects(&mut app);
    type_keys(&mut app, "f");
    app.update(Msg::Log(Feed::Chunk("first try\n".into())));
    app.update(Msg::Log(Feed::Clear));
    app.update(Msg::Log(Feed::Chunk("first try\nmore\n".into())));
    assert_eq!(log(&app).lines, ["first try", "more"]);
}

#[test]
fn history_goes_before_lines_that_streamed_in_first() {
    let (mut app, _) = loaded();
    open_detail(&mut app);
    let cmds = type_keys(&mut app, "f");
    assert_eq!(cmds.len(), 2, "history and the stream: {cmds:?}");

    app.update(Msg::Log(Feed::Lines(vec!["live".into()])));
    app.update(Msg::Log(Feed::History(Ok("old\n".into()))));
    assert_eq!(log(&app).lines, ["old", "live"]);
}

#[test]
fn following_an_effect_replays_its_stream_and_buffers_partial_lines() {
    let (mut app, _) = loaded();
    let key = open_effects(&mut app);
    let cmds = type_keys(&mut app, "f");
    let source = LogSource::Effect {
        id: 25034,
        kind: "push".into(),
        name: "default.deploy".into(),
    };
    assert_eq!(
        cmds,
        [Cmd::Follow {
            key,
            source,
            feed_tx: feed_tx(&cmds[0]).clone()
        }],
        "no separate history request"
    );

    app.update(Msg::Log(Feed::Chunk("deploy".into())));
    assert!(log(&app).lines.is_empty());

    app.update(Msg::Log(Feed::Chunk("ing\ndone".into())));
    assert_eq!(log(&app).lines, ["deploying"]);

    app.update(Msg::Log(Feed::End(Some("succeeded".into()))));
    let view = log(&app);
    assert_eq!(view.lines, ["deploying", "done"]);
    assert!(!view.is_following);
    assert_eq!(view.note.as_deref(), Some("succeeded"));
}

#[test]
fn leaving_a_log_detaches_its_stream() {
    let (mut app, _) = loaded();
    open_detail(&mut app);
    let cmds = type_keys(&mut app, "f");
    assert_eq!(press(&mut app, KeyCode::Esc), []);
    assert!(app.log.is_none());

    for cmd in &cmds {
        assert!(feed_tx(cmd).0.is_closed());
    }
}

#[test]
fn the_end_of_a_stream_refreshes_the_detail() {
    let (mut app, start) = loaded();
    let key = open_detail(&mut app);
    type_keys(&mut app, "f");
    app.update(Msg::Log(Feed::End(None)));
    assert_eq!(tick(&mut app, start, 1), [Cmd::LoadDetail(key)]);
}

#[test]
fn scrolling_up_stops_autoscroll_and_end_resumes_it() {
    let (mut app, _) = loaded();
    open_detail(&mut app);
    type_keys(&mut app, "f");
    app.log_height = 10;
    let lines: Vec<String> = (0..100).map(|i| i.to_string()).collect();
    app.update(Msg::Log(Feed::Lines(lines)));
    assert_eq!(log(&app).top(10), 90);

    type_keys(&mut app, "kk");
    assert_eq!(log(&app).scroll, Scroll::At(88));

    app.update(Msg::Log(Feed::Lines(vec!["new".into()])));
    assert_eq!(log(&app).top(10), 88, "stays put");

    type_keys(&mut app, "jjj");
    assert_eq!(log(&app).scroll, Scroll::End, "back at the end");

    type_keys(&mut app, "g");
    assert_eq!(log(&app).top(10), 0);

    type_keys(&mut app, "G");
    assert_eq!(log(&app).top(10), 91);
}

#[test]
fn status_events_refresh_on_the_next_tick_without_overlap() {
    let (mut app, start) = loaded();
    let key = open_detail(&mut app);
    let build_id = app.detail.as_ref().unwrap().row.build.id;
    app.update(Msg::StatusChanged {
        build_id: Some(build_id),
    });
    assert_eq!(
        tick(&mut app, start, 1),
        [Cmd::LoadRows(Scope::Recent), Cmd::LoadDetail(key.clone())]
    );

    app.update(Msg::StatusChanged {
        build_id: Some(build_id),
    });
    assert_eq!(tick(&mut app, start, 2), [], "both still loading");

    app.update(Msg::Rows(Ok(queue_rows())));
    app.update(Msg::Detail(key.clone(), Ok(Box::new(running_detail()))));
    assert_eq!(
        tick(&mut app, start, 3),
        [Cmd::LoadRows(Scope::Recent), Cmd::LoadDetail(key)]
    );
}

#[test]
fn events_for_other_builds_leave_the_detail_alone() {
    let (mut app, start) = loaded();
    open_detail(&mut app);
    app.update(Msg::StatusChanged { build_id: Some(-1) });
    assert_eq!(tick(&mut app, start, 1), [Cmd::LoadRows(Scope::Recent)]);
}

#[test]
fn polling_falls_back_when_no_events_arrive() {
    let (mut app, start) = loaded();
    assert_eq!(tick(&mut app, start, 14), []);
    assert_eq!(tick(&mut app, start, 15), [Cmd::LoadRows(Scope::Recent)]);
}

#[test]
fn help_opens_anywhere_and_any_key_closes_it() {
    let (mut app, _) = loaded();
    type_keys(&mut app, "?");
    assert_eq!(app.modal, Some(Modal::Help));

    type_keys(&mut app, "q");
    assert_eq!(app.modal, None);
    assert!(!app.should_quit);

    type_keys(&mut app, "q");
    assert!(app.should_quit);
}

#[test]
fn ctrl_c_always_quits() {
    let (mut app, _) = loaded();
    type_keys(&mut app, "r");
    app.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(app.should_quit);
}

#[test]
fn opening_a_build_from_the_command_line_lands_on_the_log() {
    let start = Instant::now();
    let row = queue_rows().remove(0);

    let mut app = App::new(start);
    let cmds = app.open_build(row.clone(), effects_detail(), Jump::Effect(25034), false);
    assert!(
        matches!(cmds.as_slice(), [Cmd::Follow { .. }]),
        "a running effect is followed: {cmds:?}"
    );
    assert_eq!(app.detail.as_ref().unwrap().pane, Pane::Effects);

    let mut app = App::new(start);
    let attr = Jump::Attr("checks.aarch64-linux.server".into());
    let cmds = app.open_build(row.clone(), running_detail(), attr, false);
    assert!(matches!(cmds.as_slice(), [Cmd::LoadLog { .. }]), "{cmds:?}");

    let mut app = App::new(start);
    assert_eq!(app.open_build(row, running_detail(), Jump::Build, true), []);
    assert!(app.log.is_none());
    press(&mut app, KeyCode::Esc);
    assert!(app.detail.is_none(), "Esc goes on to the builds list");
}

fn restart_in_view(app: &mut App, key: &BuildKey) -> Action {
    type_keys(app, "r");
    let Some(Modal::Confirm { action, .. }) = app.modal.clone() else {
        panic!("{:?}", app.modal)
    };
    assert!(matches!(
        type_keys(app, "y").as_slice(),
        [Cmd::Control { .. }]
    ));

    let cmds = app.update(Msg::ActionDone {
        key: key.clone(),
        action: action.clone(),
        result: Ok(()),
    });
    assert!(
        matches!(cmds.as_slice(), [Cmd::Follow { .. }]),
        "the restarted run is followed: {cmds:?}"
    );
    action
}

#[test]
fn restarting_the_effect_in_view_follows_it_from_the_start() {
    let (mut app, _) = loaded();
    let key = open_effects(&mut app);
    type_keys(&mut app, "Gk");
    press(&mut app, KeyCode::Enter);
    app.update(Msg::Log(Feed::History(Ok("the failed run\n".into()))));

    restart_in_view(&mut app, &key);
    let view = log(&app);
    assert!(view.is_following);
    assert!(
        view.lines.is_empty(),
        "the old run's log goes: {:?}",
        view.lines
    );
}

#[test]
fn a_restarted_push_effect_is_followed_on_its_new_run() {
    let (mut app, _) = loaded();
    let key = open_effects(&mut app);
    type_keys(&mut app, "G");
    press(&mut app, KeyCode::Enter);
    restart_in_view(&mut app, &key);

    let mut detail = effects_detail();
    let schema = detail
        .effects
        .iter_mut()
        .find(|e| e.name == "default.schema")
        .unwrap();
    schema.id = 30001;
    schema.status = Status::Pending;
    let cmds = app.update(Msg::Detail(key, Ok(Box::new(detail))));

    let Some(Cmd::Follow {
        source: LogSource::Effect { id, .. },
        ..
    }) = cmds.first()
    else {
        panic!("the new run is followed: {cmds:?}")
    };
    assert_eq!(*id, 30001);
    assert!(matches!(
        log(&app).source,
        LogSource::Effect { id: 30001, .. }
    ));
}

#[test]
fn restarting_the_attribute_in_view_follows_it_again() {
    let (mut app, _) = loaded();
    let key = open_detail(&mut app);
    type_keys(&mut app, "G");
    press(&mut app, KeyCode::Enter);
    app.update(Msg::Log(Feed::History(Ok("old\n".into()))));

    restart_in_view(&mut app, &key);
    assert!(log(&app).lines.is_empty());
}

#[test]
fn other_restarts_leave_the_log_alone() {
    let (mut app, _) = loaded();
    let key = open_effects(&mut app);
    type_keys(&mut app, "G");
    press(&mut app, KeyCode::Enter);
    let other = Action::new(
        Verb::Restart,
        Target::Effect {
            name: "comment.bench".into(),
            kind: "check".into(),
        },
    );
    let failed = Action::new(Verb::Restart, Target::Effects);

    for (action, result) in [(other, Ok(())), (failed, Err("HTTP 403".into()))] {
        let msg = Msg::ActionDone {
            key: key.clone(),
            action,
            result,
        };
        assert_eq!(app.update(msg), []);
    }
}
