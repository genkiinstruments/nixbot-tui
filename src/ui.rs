use ansi_to_tui::IntoText;
use jiff::Timestamp;
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Cell, Clear, Paragraph, Row as TableRow, Table, Wrap},
};

use crate::{
    api::{Attribute, Build, Effect, Outcome, Status},
    app::{App, DetailView, FlashKind, LogSource, LogView, Modal, Pane, Row, Scope},
    format::{age, glyph, progress_text, queue_note, short_sha},
};

const LIST_KEYS: &str = "j/k move · ⏎ open · a active only · r restart · c cancel · o PR · b build page · ? help · q quit";
const DETAIL_KEYS: &str = "⏎ log · f follow · Tab pane · / filter · r/c item · R/C build · \
     E/X effects · o PR · ? help · q back";
const LOG_KEYS: &str = "f follow · j/k scroll · g/G top/end · r/c restart/cancel · o web · q back";
const FILTER_KEYS: &str = "type to filter attributes · ⏎ keep · Esc clear";

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(header_line(app), header);

    let keys = if let Some(log) = &app.log {
        app.log_height = body.height.saturating_sub(2);
        draw_log(frame, body, log);
        LOG_KEYS
    } else if let Some(view) = &mut app.detail {
        let is_editing_filter = view.is_editing_filter;
        let title = app
            .prs
            .get(&(
                view.row.key.repo.clone(),
                view.row.build.pr_number.unwrap_or_default(),
            ))
            .cloned()
            .flatten()
            .map(|pr| pr.title);
        draw_detail(frame, body, view, title.as_deref());

        if is_editing_filter {
            FILTER_KEYS
        } else {
            DETAIL_KEYS
        }
    } else {
        draw_list(frame, body, app);
        LIST_KEYS
    };

    frame.render_widget(Line::from(keys).dark_gray(), footer);

    match &app.modal {
        Some(Modal::Confirm { key, action }) => {
            let text = Text::from(vec![
                Line::from(format!("{action} of {} #{}?", key.repo, key.number)),
                Line::default(),
                Line::from("y confirm · n cancel").dark_gray(),
            ]);
            popup(frame, " confirm ", text, 60);
        }
        Some(Modal::Help) => popup(frame, " keys ", help(), 76),
        None => {}
    }
}

fn header_line(app: &App) -> Line<'_> {
    let building = app
        .rows
        .iter()
        .filter(|r| r.build.status.is_active())
        .count();
    let running_effects = app
        .rows
        .iter()
        .filter(|r| !r.build.status.is_active() && r.effects.is_some_and(|e| e.is_active()))
        .count();
    let scope = match app.scope {
        Scope::Recent => "recent builds",
        Scope::Active => "active builds",
    };
    let mut spans = vec![
        Span::from(" nixbot ").bold().reversed(),
        Span::from(format!(
            " {building} building · {running_effects} running effects · showing {scope} "
        )),
    ];

    if let Some(err) = &app.rows_error {
        spans.push(Span::from(format!(" {err} ")).red());
    }

    if let Some(flash) = &app.flash {
        let style = match flash.kind {
            FlashKind::Info => Style::new().green(),
            FlashKind::Error => Style::new().red(),
        };
        spans.push(Span::styled(format!(" {} ", flash.text), style));
    }

    Line::from(spans)
}

fn status_style(status: &Status) -> Style {
    use Outcome::*;

    match status.outcome() {
        Failed | Cancelled => Style::new().red(),
        Running => Style::new().yellow(),
        Queued => Style::new().dark_gray(),
        Succeeded => Style::new().green(),
    }
}

fn status_cell(status: &Status) -> Cell<'static> {
    Cell::from(glyph(status)).style(status_style(status))
}

/// `nbo` calls a successful build of a cached output "cached".
fn attr_label(attr: &Attribute) -> &str {
    match attr.status {
        Status::Succeeded if attr.is_cached => "cached",
        ref status => status.label(),
    }
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let now = Timestamp::now();
    let block = Block::bordered().title(" builds ");

    if app.rows.is_empty() {
        let text = match (app.is_loaded, app.scope) {
            (false, _) => "loading…",
            (true, Scope::Active) => "nothing building or running effects · a shows recent builds",
            (true, Scope::Recent) => "no builds",
        };
        frame.render_widget(Paragraph::new(text).dark_gray().block(block), area);
        return;
    }

    let width = |text: &str| u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    let repo_width = app
        .rows
        .iter()
        .map(|r| width(&r.key.repo.to_string()))
        .max();
    let branch_width = app.rows.iter().map(|r| width(&r.build.branch)).max();
    let (repo_width, branch_width) = (
        repo_width.unwrap_or(0).max(4),
        branch_width.unwrap_or(0).max(6),
    );

    let rows: Vec<TableRow> = app
        .rows
        .iter()
        .map(|row| {
            let Row { key, build, .. } = row;
            let pr = build.pr_number.map(|n| {
                let title = app.prs.get(&(key.repo.clone(), n)).cloned().flatten();
                title.map_or(format!("#{n}"), |pr| format!("#{n} {}", pr.title))
            });

            TableRow::new([
                status_cell(&build.status),
                Cell::from(format!("#{}", build.number)),
                Cell::from(key.repo.to_string()),
                Cell::from(build.branch.clone()),
                Cell::from(pr.unwrap_or_default()),
                Cell::from(short_sha(&build.commit_sha).to_owned()).dark_gray(),
                Cell::from(progress_text(build, row.progress)).style(status_style(&build.status)),
                effects_cell(row),
                Cell::from(age(build.created_at, now)).dark_gray(),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(6),
            Constraint::Length(repo_width.min(28)),
            Constraint::Length(branch_width.min(32)),
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Length(11),
            Constraint::Length(6),
        ],
    )
    .header(
        TableRow::new([
            "",
            "build",
            "repo",
            "branch",
            "pull request",
            "commit",
            "progress",
            "effects",
            "age",
        ])
        .bold(),
    )
    .row_highlight_style(Modifier::REVERSED)
    .block(block);
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn effects_cell(row: &Row) -> Cell<'static> {
    let Some(effects) = row.effects else {
        return Cell::default();
    };

    let style = match effects {
        e if e.running > 0 => Style::new().yellow(),
        e if e.failed > 0 => Style::new().red(),
        e if e.queued > 0 => Style::new().dark_gray(),
        _ => Style::new().green(),
    };
    Cell::from(effects.to_string()).style(style)
}

fn draw_detail(frame: &mut Frame, area: Rect, view: &mut DetailView, pr_title: Option<&str>) {
    let [summary, body] =
        Layout::vertical([Constraint::Length(5), Constraint::Fill(1)]).areas(area);
    frame.render_widget(summary_text(view, pr_title), summary);

    let has_effects = !view.effects().is_empty();
    let [attrs, effects] = if !has_effects {
        [body, Rect::default()]
    } else if body.width >= 120 {
        Layout::horizontal([Constraint::Percentage(62), Constraint::Fill(1)]).areas(body)
    } else {
        Layout::vertical([Constraint::Percentage(62), Constraint::Fill(1)]).areas(body)
    };

    draw_attrs(frame, attrs, view);

    if has_effects {
        draw_effects(frame, effects, view);
    }
}

fn summary_text<'a>(view: &'a DetailView, pr_title: Option<&'a str>) -> Paragraph<'a> {
    let Row { key, build, .. } = &view.row;
    let Build {
        number,
        commit_sha,
        branch,
        pr_number,
        pr_author,
        status,
        error,
        created_at,
        ..
    } = build;
    let mut status_line = vec![
        Span::from(format!("{} #{number}  ", key.repo)).bold(),
        Span::styled(
            format!("{} {}", glyph(status), status.label()),
            status_style(status),
        ),
    ];

    if let Some(note) = queue_note(build) {
        status_line.push(Span::from(format!(" · {note}")).dark_gray());
    }

    let mut lines = vec![
        Line::from(status_line),
        Line::from(format!(
            "branch {branch} · commit {} · {} ago{}",
            short_sha(commit_sha),
            age(*created_at, Timestamp::now()),
            pr_author
                .as_ref()
                .map_or(String::new(), |a| format!(" · by {a}")),
        )),
    ];

    if let Some(n) = pr_number {
        lines.push(Line::from(format!(
            "PR #{n} {}",
            pr_title.unwrap_or_default()
        )));
    }

    if let Some(err) = error.as_ref().or(view.error.as_ref()) {
        lines.push(Line::from(err.as_str()).red());
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::bordered())
}

fn pane_block(title: String, is_focused: bool) -> Block<'static> {
    let style = if is_focused {
        Style::new().cyan()
    } else {
        Style::new().dark_gray()
    };
    Block::bordered().title(title).border_style(style)
}

fn duration(started: Option<Timestamp>, finished: Option<Timestamp>) -> String {
    match started {
        Some(started) => age(started, finished.unwrap_or_else(Timestamp::now)),
        None => String::new(),
    }
}

fn draw_attrs(frame: &mut Frame, area: Rect, view: &mut DetailView) {
    let attrs = view.visible_attrs();
    let filter = match (&view.filter, view.is_editing_filter) {
        (filter, true) => format!(" /{filter}▏"),
        (filter, false) if !filter.is_empty() => format!(" /{filter}"),
        _ => String::new(),
    };
    let title = format!(
        " attributes {}/{}{filter} ",
        attrs.len(),
        view.attributes().len()
    );
    let rows: Vec<TableRow> = attrs
        .iter()
        .map(|attr| {
            let Attribute {
                attr: name,
                status,
                started_at,
                finished_at,
                ..
            } = attr;

            TableRow::new([
                status_cell(status),
                Cell::from(name.clone()),
                Cell::from(attr_label(attr).to_owned()).style(status_style(status)),
                Cell::from(duration(*started_at, *finished_at)).dark_gray(),
            ])
        })
        .collect();
    let placeholder = if view.detail.is_none() {
        "loading…"
    } else {
        ""
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(7),
        ],
    )
    .row_highlight_style(Modifier::REVERSED)
    .block(pane_block(title, view.pane == Pane::Attrs).title_bottom(placeholder));
    frame.render_stateful_widget(table, area, &mut view.attrs);
}

fn draw_effects(frame: &mut Frame, area: Rect, view: &mut DetailView) {
    let rows: Vec<TableRow> = view
        .effects()
        .iter()
        .map(|effect| {
            let Effect {
                kind,
                name,
                status,
                started_at,
                finished_at,
                ..
            } = effect;

            TableRow::new([
                status_cell(status),
                Cell::from(name.clone()),
                Cell::from(kind.clone()).dark_gray(),
                Cell::from(status.label().to_owned()).style(status_style(status)),
                Cell::from(duration(*started_at, *finished_at)).dark_gray(),
            ])
        })
        .collect();
    let title = format!(" effects {} ", view.effects().len());
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(7),
        ],
    )
    .row_highlight_style(Modifier::REVERSED)
    .block(pane_block(title, view.pane == Pane::Effects));
    frame.render_stateful_widget(table, area, &mut view.effects);
}

fn draw_log(frame: &mut Frame, area: Rect, log: &LogView) {
    let LogView {
        key,
        source,
        lines,
        note,
        is_following,
        ..
    } = log;
    let height = usize::from(area.height.saturating_sub(2));
    let top = log.top(height);
    let shown = &lines[top..lines.len().min(top + height)];
    let name = match source {
        LogSource::Attr(attr) => attr.as_str(),
        LogSource::Effect { label, .. } => label.as_str(),
    };
    let mut title = vec![Span::from(format!(" {name} · {} #{} ", key.repo, key.number)).bold()];

    if *is_following {
        title.push(Span::from(" following ").black().on_yellow());
    }

    let position = format!(" {}-{}/{} ", top + 1, top + shown.len(), lines.len());
    let mut block = Block::bordered()
        .title(Line::from(title))
        .title_bottom(position);

    if let Some(note) = note {
        block = block.title_bottom(Line::from(format!(" {note} ")).right_aligned());
    }

    let text: Vec<Line> = if shown.is_empty() {
        let placeholder = if *is_following {
            "waiting for output…"
        } else {
            "no output · f follows it live"
        };
        vec![Line::from(placeholder).dark_gray()]
    } else {
        shown.iter().map(|line| ansi_line(line)).collect()
    };

    frame.render_widget(Paragraph::new(text).block(block), area);
}

/// One log line with its SGR colours; anything ansi-to-tui cannot parse is shown plain.
fn ansi_line(line: &str) -> Line<'static> {
    let parsed = line
        .into_text()
        .ok()
        .and_then(|text| text.lines.into_iter().next());
    parsed.unwrap_or_else(|| Line::from(line.to_owned()))
}

fn popup(frame: &mut Frame, title: &str, text: Text, width: u16) {
    let height = u16::try_from(text.height())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(title).border_style(Color::Cyan)),
        area,
    );
}

fn help() -> Text<'static> {
    let section = |name: &'static str| Line::from(name).bold().cyan();
    let key = |keys: &'static str, what: &'static str| {
        Line::from(vec![
            Span::from(format!("  {keys:<14}")).bold(),
            Span::from(what),
        ])
    };

    Text::from(vec![
        section("builds"),
        key("j/k ↑/↓ g/G", "move, first, last"),
        key("⏎ l", "open the build"),
        key("a", "only builds still building or running effects, or all"),
        key("r / c", "restart / cancel the build"),
        key("o / b", "open the pull request / the build page"),
        section("build"),
        key("Tab", "switch between attributes and effects"),
        key("⏎ / f", "show the log / follow it live"),
        key("/", "filter attributes"),
        key("r / c", "restart / cancel the selected attribute or effect"),
        key("R / C", "restart / cancel the whole build"),
        key("E / X", "restart / cancel all effects"),
        section("log"),
        key("f", "follow live output, or stop"),
        key("j/k ^d/^u", "scroll a line / half a page"),
        key("g / G", "top / end (end keeps following)"),
        key("o", "open the log in the browser"),
        section("everywhere"),
        key("q Esc", "back, or quit from the builds list"),
        key("? / ^c", "help / quit"),
    ])
}
