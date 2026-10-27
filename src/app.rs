//! Application state and its update function: messages in, commands out, no I/O.

mod log;
mod refresh;

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::{
    actions::{Action, Target, Verb},
    api::{Attribute, Build, BuildDetail, Effect, RepoRef},
    format::Progress,
};

pub use self::{
    log::{Feed, LogView, Scroll, sanitize},
    refresh::Refresh,
};

/// Lines of history fetched when a log opens.
pub const LOG_TAIL: u32 = 2000;
/// Status events are only cues and can be missed while reconnecting, so poll as well.
const ROWS_POLL: Duration = Duration::from_secs(15);
const DETAIL_POLL: Duration = Duration::from_secs(5);
const FLASH_FOR: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuildKey {
    /// Index into the configured hosts.
    pub host: usize,
    pub repo: RepoRef,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: BuildKey,
    pub build: Build,
    pub progress: Option<Progress>,
    pub build_url: String,
    /// The pull request on the forge, derived from the repository URL.
    pub pr_url: Option<String>,
}

impl Row {
    pub fn new(key: BuildKey, build: Build, repo_url: &str, build_url: String) -> Self {
        let pr_url = build
            .pr_number
            .filter(|_| key.repo.forge == "github")
            .map(|n| {
                let repo_url = repo_url.trim_end_matches('/').trim_end_matches(".git");
                format!("{repo_url}/pull/{n}")
            });

        Self {
            key,
            build,
            progress: None,
            build_url,
            pr_url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub title: String,
    pub url: String,
}

/// Which builds the list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Active,
    /// Active builds and the most recent finished ones.
    Recent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    Attr(String),
    Effect { id: u64, label: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Attrs,
    Effects,
}

#[derive(Debug)]
pub struct DetailView {
    pub row: Row,
    pub detail: Option<BuildDetail>,
    pub pane: Pane,
    pub attrs: TableState,
    pub effects: TableState,
    pub filter: String,
    pub is_editing_filter: bool,
    pub error: Option<String>,
    refresh: Refresh,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    Confirm { key: BuildKey, action: Action },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flash {
    pub text: String,
    pub kind: FlashKind,
    until: Instant,
}

/// Where a log task sends its output; equal when both ends belong to the same view.
#[derive(Debug, Clone)]
pub struct FeedTx(pub mpsc::UnboundedSender<Feed>);

impl PartialEq for FeedTx {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    Tick(Instant),
    Rows(Result<Vec<Row>, String>),
    Detail(BuildKey, Result<Box<BuildDetail>, String>),
    Log(Feed),
    /// Something changed on a host; `build_id` is the database id, not the number.
    StatusChanged {
        build_id: Option<i64>,
    },
    ActionDone {
        action: Action,
        result: Result<(), String>,
    },
    Pr {
        repo: RepoRef,
        number: u64,
        info: Option<PrInfo>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    LoadRows(Scope),
    LoadDetail(BuildKey),
    LoadLog {
        key: BuildKey,
        source: LogSource,
        feed_tx: FeedTx,
    },
    Follow {
        key: BuildKey,
        source: LogSource,
        feed_tx: FeedTx,
    },
    Control {
        key: BuildKey,
        action: Action,
    },
    Open(String),
    LoadPr {
        repo: RepoRef,
        number: u64,
    },
}

#[derive(Debug)]
pub struct App {
    pub rows: Vec<Row>,
    pub table: TableState,
    pub scope: Scope,
    pub rows_error: Option<String>,
    pub is_loaded: bool,
    pub detail: Option<DetailView>,
    pub log: Option<LogView>,
    pub modal: Option<Modal>,
    pub flash: Option<Flash>,
    /// `None` while the lookup runs or after it failed.
    pub prs: HashMap<(RepoRef, u64), Option<PrInfo>>,
    /// Height of the log body, set while drawing, for paging.
    pub log_height: u16,
    pub should_quit: bool,
    now: Instant,
    rows_refresh: Refresh,
}

impl App {
    pub fn new(now: Instant) -> Self {
        Self {
            rows: Vec::new(),
            table: TableState::default(),
            scope: Scope::default(),
            rows_error: None,
            is_loaded: false,
            detail: None,
            log: None,
            modal: None,
            flash: None,
            prs: HashMap::new(),
            log_height: 0,
            should_quit: false,
            now,
            rows_refresh: Refresh::default(),
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Tick(now) => self.on_tick(now),
            Msg::Rows(rows) => self.on_rows(rows),
            Msg::Detail(key, detail) => {
                self.on_detail(&key, detail);
                vec![]
            }
            Msg::Log(feed) => {
                let Self { log, detail, .. } = self;
                let is_end = matches!(feed, Feed::End(_));

                if let Some(log) = log {
                    log.apply(feed);
                }

                if is_end && let Some(view) = detail {
                    view.refresh.request();
                }

                vec![]
            }
            Msg::StatusChanged { build_id } => {
                let Self {
                    detail,
                    rows_refresh,
                    ..
                } = self;
                rows_refresh.request();

                if let Some(view) = detail
                    && build_id.is_none_or(|id| id == view.row.build.id)
                {
                    view.refresh.request();
                }

                vec![]
            }
            Msg::ActionDone { action, result } => {
                match result {
                    Ok(()) => self.flash(format!("requested: {action}"), FlashKind::Info),
                    Err(err) => self.flash(format!("{action} failed: {err}"), FlashKind::Error),
                }

                let Self {
                    detail,
                    rows_refresh,
                    ..
                } = self;
                rows_refresh.request();

                if let Some(view) = detail {
                    view.refresh.request();
                }

                vec![]
            }
            Msg::Pr { repo, number, info } => {
                self.prs.insert((repo, number), info);
                vec![]
            }
        }
    }

    fn on_tick(&mut self, now: Instant) -> Vec<Cmd> {
        let Self {
            scope,
            detail,
            flash,
            now: previous_now,
            rows_refresh,
            ..
        } = self;
        *previous_now = now;

        if flash.as_ref().is_some_and(|f| f.until <= now) {
            *flash = None;
        }

        let mut cmds = Vec::new();

        if rows_refresh.is_due(now, Some(ROWS_POLL)) {
            rows_refresh.start(now);
            cmds.push(Cmd::LoadRows(*scope));
        }

        if let Some(DetailView { row, refresh, .. }) = detail {
            let poll = row.build.status.is_active().then_some(DETAIL_POLL);

            if refresh.is_due(now, poll) {
                refresh.start(now);
                cmds.push(Cmd::LoadDetail(row.key.clone()));
            }
        }

        cmds
    }

    /// Loads the rows now rather than on the next tick.
    fn load_rows(&mut self) -> Vec<Cmd> {
        let Self {
            scope,
            now,
            rows_refresh,
            ..
        } = self;

        if rows_refresh.is_loading() {
            rows_refresh.request();
            return vec![];
        }

        rows_refresh.start(*now);
        vec![Cmd::LoadRows(*scope)]
    }

    fn on_rows(&mut self, rows: Result<Vec<Row>, String>) -> Vec<Cmd> {
        let selected = self.selected_row().map(|r| r.key.clone());
        let Self {
            rows: shown,
            table,
            rows_error,
            is_loaded,
            detail,
            prs,
            rows_refresh,
            ..
        } = self;
        rows_refresh.finish();
        *is_loaded = true;

        let mut rows = match rows {
            Ok(rows) => rows,
            Err(err) => {
                *rows_error = Some(err);
                return vec![];
            }
        };

        rows.sort_by(|a, b| {
            let is_active = |r: &Row| r.build.status.is_active();
            is_active(b)
                .cmp(&is_active(a))
                .then(b.build.created_at.cmp(&a.build.created_at))
        });
        *shown = rows;
        *rows_error = None;

        let index = selected
            .and_then(|key| shown.iter().position(|r| r.key == key))
            .or(table.selected());
        select_clamped(table, index, shown.len());

        if let Some(view) = detail
            && let Some(row) = shown.iter().find(|r| r.key == view.row.key)
        {
            view.row = Row {
                progress: view.row.progress,
                ..row.clone()
            };
        }

        let mut cmds = Vec::new();

        for Row { key, build, .. } in shown.iter() {
            let Some(number) = build.pr_number else {
                continue;
            };

            let pr = (key.repo.clone(), number);

            if key.repo.forge == "github" && !prs.contains_key(&pr) {
                prs.insert(pr, None);
                cmds.push(Cmd::LoadPr {
                    repo: key.repo.clone(),
                    number,
                });
            }
        }

        cmds
    }

    fn on_detail(&mut self, key: &BuildKey, detail: Result<Box<BuildDetail>, String>) {
        let Some(view) = &mut self.detail else {
            return;
        };

        if &view.row.key != key {
            return;
        }

        view.refresh.finish();

        match detail {
            Ok(detail) => view.show(*detail),
            Err(err) => view.error = Some(err),
        }
    }

    fn flash(&mut self, text: String, kind: FlashKind) {
        self.flash = Some(Flash {
            text,
            kind,
            until: self.now + FLASH_FOR,
        });
    }

    fn select_row<F>(&mut self, to: F)
    where
        F: FnOnce(usize) -> usize,
    {
        let Self { rows, table, .. } = self;
        let index = to(table.selected().unwrap_or(0));
        select_clamped(table, Some(index), rows.len());
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.table.selected().and_then(|i| self.rows.get(i))
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![];
        }

        if let Some(modal) = self.modal.take() {
            return self.on_modal_key(modal, key);
        }

        let is_typing = self.detail.as_ref().is_some_and(|d| d.is_editing_filter);

        if key.code == KeyCode::Char('?') && !is_typing {
            self.modal = Some(Modal::Help);
            return vec![];
        }

        if self.log.is_some() {
            self.on_log_key(key)
        } else if self.detail.is_some() {
            self.on_detail_key(key)
        } else {
            self.on_list_key(key)
        }
    }

    fn on_modal_key(&mut self, modal: Modal, key: KeyEvent) -> Vec<Cmd> {
        match (modal, key.code) {
            (Modal::Confirm { key, action }, KeyCode::Char('y' | 'Y')) => {
                vec![Cmd::Control { key, action }]
            }
            (Modal::Confirm { .. }, KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc) => vec![],
            (modal @ Modal::Confirm { .. }, _) => {
                self.modal = Some(modal);
                vec![]
            }
            (Modal::Help, _) => vec![],
        }
    }

    /// Asks before sending a control request; cancelling something finished is refused here.
    fn confirm(&mut self, key: BuildKey, action: Action, is_active: bool) {
        if action.verb == Verb::Cancel && !is_active {
            self.flash(
                format!("cannot {action}: it is not running"),
                FlashKind::Error,
            );
            return;
        }

        self.modal = Some(Modal::Confirm { key, action });
    }

    /// The pull request if the build has one, otherwise the build's page.
    fn open_url(&self, row: &Row) -> String {
        let Row {
            key,
            build,
            build_url,
            pr_url,
            ..
        } = row;
        let pr = build.pr_number.and_then(|n| {
            let known = self.prs.get(&(key.repo.clone(), n)).cloned().flatten();
            known.map(|info| info.url).or_else(|| pr_url.clone())
        });

        pr.unwrap_or_else(|| build_url.clone())
    }

    fn on_list_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.select_row(|i| i.saturating_add(1)),
            KeyCode::Char('k') | KeyCode::Up => self.select_row(|i| i.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.select_row(|_| 0),
            KeyCode::Char('G') | KeyCode::End => self.select_row(|_| usize::MAX),
            KeyCode::Char('a') => {
                self.scope = match self.scope {
                    Scope::Active => Scope::Recent,
                    Scope::Recent => Scope::Active,
                };
                return self.load_rows();
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                if let Some(row) = self.selected_row().cloned() {
                    return self.open_detail(row);
                }
            }
            KeyCode::Char(c @ ('r' | 'c')) => {
                if let Some(Row { key, build, .. }) = self.selected_row().cloned() {
                    let is_active = build.status.is_active();
                    self.confirm(key, Action::new(verb(c), Target::Build), is_active);
                }
            }
            KeyCode::Char('o') => {
                if let Some(row) = self.selected_row() {
                    return vec![Cmd::Open(self.open_url(row))];
                }
            }
            KeyCode::Char('b') => {
                if let Some(row) = self.selected_row() {
                    return vec![Cmd::Open(row.build_url.clone())];
                }
            }
            _ => {}
        }

        vec![]
    }

    fn open_detail(&mut self, row: Row) -> Vec<Cmd> {
        let key = row.key.clone();
        let mut refresh = Refresh::default();
        refresh.start(self.now);
        self.detail = Some(DetailView {
            row,
            detail: None,
            pane: Pane::Attrs,
            attrs: TableState::default(),
            effects: TableState::default(),
            filter: String::new(),
            is_editing_filter: false,
            error: None,
            refresh,
        });

        vec![Cmd::LoadDetail(key)]
    }

    fn on_detail_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        let Some(view) = &mut self.detail else {
            return vec![];
        };

        if view.is_editing_filter {
            view.edit_filter(key.code);
            return vec![];
        }

        match key.code {
            KeyCode::Esc if !view.filter.is_empty() => {
                view.filter.clear();
                view.reset_attr_selection();
            }
            KeyCode::Char('q' | 'h') | KeyCode::Esc | KeyCode::Left => self.detail = None,
            KeyCode::Tab | KeyCode::BackTab => {
                view.pane = match view.pane {
                    Pane::Attrs if !view.effects().is_empty() => Pane::Effects,
                    _ => Pane::Attrs,
                };
            }
            KeyCode::Char('j') | KeyCode::Down => view.select(|i| i.saturating_add(1)),
            KeyCode::Char('k') | KeyCode::Up => view.select(|i| i.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => view.select(|_| 0),
            KeyCode::Char('G') | KeyCode::End => view.select(|_| usize::MAX),
            KeyCode::Char('/') => {
                view.pane = Pane::Attrs;
                view.is_editing_filter = true;
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => return self.open_log(false),
            KeyCode::Char('f') => return self.open_log(true),
            KeyCode::Char(c @ ('r' | 'c')) => {
                if let Some((target, is_active)) = view.selected_target() {
                    let key = view.row.key.clone();
                    self.confirm(key, Action::new(verb(c), target), is_active);
                }
            }
            KeyCode::Char(c @ ('R' | 'C')) => {
                let Row { key, build, .. } = &view.row;
                let (key, is_active) = (key.clone(), build.status.is_active());
                self.confirm(key, Action::new(verb(c), Target::Build), is_active);
            }
            KeyCode::Char(c @ ('E' | 'X')) => {
                let is_active = view.effects().iter().any(|e| e.status.is_active());
                let verb = if c == 'E' {
                    Verb::Restart
                } else {
                    Verb::Cancel
                };
                let key = view.row.key.clone();
                self.confirm(key, Action::new(verb, Target::Effects), is_active);
            }
            KeyCode::Char('o') => {
                let row = view.row.clone();
                return vec![Cmd::Open(self.open_url(&row))];
            }
            KeyCode::Char('b') => return vec![Cmd::Open(view.row.build_url.clone())],
            _ => {}
        }

        vec![]
    }

    fn open_log(&mut self, should_follow: bool) -> Vec<Cmd> {
        let Some(view) = &self.detail else {
            return vec![];
        };

        let Row { key, build_url, .. } = &view.row;
        let (source, web_url) = match view.pane {
            Pane::Attrs => {
                let Some(Attribute { attr, .. }) = view.selected_attr() else {
                    return vec![];
                };

                (
                    LogSource::Attr(attr.clone()),
                    format!("{build_url}/logs/{attr}"),
                )
            }
            Pane::Effects => {
                let Some(effect) = view.selected_effect() else {
                    return vec![];
                };

                let source = LogSource::Effect {
                    id: effect.id,
                    label: effect.label(),
                };
                (source, build_url.clone())
            }
        };

        let key = key.clone();
        let log = LogView::new(key.clone(), source.clone(), web_url);
        let feed_tx = log.feed_tx();
        self.log = Some(log);

        // An effect stream replays its history itself; an attribute stream has none.
        if should_follow && matches!(source, LogSource::Effect { .. }) {
            return self.start_follow();
        }

        let mut cmds = vec![Cmd::LoadLog {
            key,
            source,
            feed_tx: FeedTx(feed_tx),
        }];

        if should_follow {
            cmds.extend(self.start_follow());
        }

        cmds
    }

    fn start_follow(&mut self) -> Vec<Cmd> {
        let Some(log) = &mut self.log else {
            return vec![];
        };

        log.is_following = true;
        log.scroll = Scroll::End;

        // The effect stream replays the log, so whatever was shown goes, with any task feeding it.
        if matches!(log.source, LogSource::Effect { .. }) {
            log.reconnect();
            log.clear();
        }

        vec![Cmd::Follow {
            key: log.key.clone(),
            source: log.source.clone(),
            feed_tx: FeedTx(log.feed_tx()),
        }]
    }

    fn on_log_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        let height = usize::from(self.log_height);
        let page = height.saturating_sub(1).max(1).cast_signed();
        let Some(log) = &mut self.log else {
            return vec![];
        };

        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('q' | 'h') | KeyCode::Esc | KeyCode::Left => self.log = None,
            KeyCode::Char('d') if is_ctrl => log.scroll_by(page / 2, height),
            KeyCode::Char('u') if is_ctrl => log.scroll_by(-page / 2, height),
            KeyCode::Char('j') | KeyCode::Down => log.scroll_by(1, height),
            KeyCode::Char('k') | KeyCode::Up => log.scroll_by(-1, height),
            KeyCode::PageDown | KeyCode::Char(' ') => log.scroll_by(page, height),
            KeyCode::PageUp => log.scroll_by(-page, height),
            KeyCode::Char('g') | KeyCode::Home => log.scroll = Scroll::At(0),
            KeyCode::Char('G') | KeyCode::End => log.scroll = Scroll::End,
            KeyCode::Char('f') if log.is_following => {
                log.reconnect();
                log.is_following = false;
            }
            KeyCode::Char('f') => return self.start_follow(),
            KeyCode::Char('o') => return vec![Cmd::Open(log.web_url.clone())],
            KeyCode::Char(c @ ('r' | 'c')) => {
                let LogView { key, source, .. } = log;
                let key = key.clone();
                let target = self.detail.as_ref().and_then(|view| view.target_of(source));

                if let Some((target, is_active)) = target {
                    self.confirm(key, Action::new(verb(c), target), is_active);
                }
            }
            _ => {}
        }

        vec![]
    }
}

/// `r`/`R` restart, `c`/`C` cancel.
fn verb(key: char) -> Verb {
    match key.to_ascii_lowercase() {
        'c' => Verb::Cancel,
        _ => Verb::Restart,
    }
}

fn effect_target(effect: &Effect) -> Target {
    Target::Effect {
        name: effect.name.clone(),
        kind: effect.kind.clone(),
    }
}

impl DetailView {
    pub fn attributes(&self) -> &[Attribute] {
        self.detail.as_ref().map_or(&[], |d| &d.attributes)
    }

    pub fn effects(&self) -> &[Effect] {
        self.detail.as_ref().map_or(&[], |d| &d.effects)
    }

    pub fn visible_attrs(&self) -> Vec<&Attribute> {
        let filter = self.filter.to_lowercase();
        self.attributes()
            .iter()
            .filter(|a| a.attr.to_lowercase().contains(&filter))
            .collect()
    }

    pub fn selected_attr(&self) -> Option<&Attribute> {
        let index = self.attrs.selected()?;
        self.visible_attrs().get(index).copied()
    }

    pub fn selected_effect(&self) -> Option<&Effect> {
        self.effects().get(self.effects.selected()?)
    }

    /// Takes a fresh detail, keeping the selection on the same attribute and effect.
    fn show(&mut self, mut detail: BuildDetail) {
        let selected_attr = self.selected_attr().map(|a| a.attr.clone());
        let selected_effect = self.selected_effect().map(|e| e.id);

        detail.attributes.sort_by(|a, b| {
            let outcome = a.status.outcome().cmp(&b.status.outcome());
            outcome.then_with(|| a.attr.cmp(&b.attr))
        });
        detail.effects.sort_by_key(|e| e.status.outcome());

        self.row.progress = Some(Progress::of(&detail.attributes));
        self.row.build = detail.build.clone();
        self.detail = Some(detail);
        self.error = None;

        let (attr_index, attrs_len) = {
            let visible = self.visible_attrs();
            let index = selected_attr.and_then(|name| visible.iter().position(|a| a.attr == name));
            (index, visible.len())
        };
        let (effect_index, effects_len) = {
            let effects = self.effects();
            let index = selected_effect.and_then(|id| effects.iter().position(|e| e.id == id));
            (index, effects.len())
        };
        select_clamped(&mut self.attrs, attr_index, attrs_len);
        select_clamped(&mut self.effects, effect_index, effects_len);
    }

    fn edit_filter(&mut self, code: KeyCode) {
        let Self {
            filter,
            is_editing_filter,
            ..
        } = self;

        match code {
            KeyCode::Enter => *is_editing_filter = false,
            KeyCode::Esc => {
                *is_editing_filter = false;
                filter.clear();
            }
            KeyCode::Backspace => {
                filter.pop();
            }
            KeyCode::Char(c) => filter.push(c),
            _ => return,
        }

        self.reset_attr_selection();
    }

    fn reset_attr_selection(&mut self) {
        let len = self.visible_attrs().len();
        select_clamped(&mut self.attrs, Some(0), len);
    }

    /// Moves the selection of the focused pane.
    fn select<F>(&mut self, to: F)
    where
        F: FnOnce(usize) -> usize,
    {
        let len = match self.pane {
            Pane::Attrs => self.visible_attrs().len(),
            Pane::Effects => self.effects().len(),
        };
        let state = match self.pane {
            Pane::Attrs => &mut self.attrs,
            Pane::Effects => &mut self.effects,
        };
        let index = to(state.selected().unwrap_or(0));
        select_clamped(state, Some(index), len);
    }

    fn selected_target(&self) -> Option<(Target, bool)> {
        match self.pane {
            Pane::Attrs => {
                let Attribute { attr, status, .. } = self.selected_attr()?;
                Some((Target::Attr(attr.clone()), status.is_active()))
            }
            Pane::Effects => {
                let effect = self.selected_effect()?;
                Some((effect_target(effect), effect.status.is_active()))
            }
        }
    }

    /// The control target behind a log, and whether it is still running.
    fn target_of(&self, source: &LogSource) -> Option<(Target, bool)> {
        match source {
            LogSource::Attr(name) => {
                let attr = self.attributes().iter().find(|a| &a.attr == name)?;
                Some((Target::Attr(name.clone()), attr.status.is_active()))
            }
            LogSource::Effect { id, .. } => {
                let effect = self.effects().iter().find(|e| e.id == *id)?;
                Some((effect_target(effect), effect.status.is_active()))
            }
        }
    }
}

fn select_clamped(state: &mut TableState, index: Option<usize>, len: usize) {
    state.select(match len {
        0 => None,
        _ => Some(index.unwrap_or(0).min(len - 1)),
    });
}

#[cfg(test)]
mod tests;
