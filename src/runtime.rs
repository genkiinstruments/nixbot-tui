//! The terminal event loop: keys, ticks, task results and log feeds become messages.

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::{
    api::Client,
    app::{App, Feed, LogView, Msg},
    jump::Startup,
    tasks::Tasks,
    ui,
};

const TICK: Duration = Duration::from_secs(1);

pub async fn run(clients: Vec<Client>, startup: Option<Startup>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, clients, startup).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    clients: Vec<Client>,
    startup: Option<Startup>,
) -> Result<()> {
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel();
    let tasks = Tasks::new(clients, msg_tx);
    tasks.watch_events();

    let mut app = App::new(Instant::now());

    if let Some(Startup {
        row,
        detail,
        jump,
        should_follow,
    }) = startup
    {
        for cmd in app.open_build(row, detail, jump, should_follow) {
            tasks.spawn(cmd);
        }
    }
    let mut terminal_events = EventStream::new();
    let mut ticks = tokio::time::interval(TICK);

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        let msg = tokio::select! {
            event = terminal_events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => Msg::Key(key),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err.into()),
                None => return Ok(()),
            },
            Some(msg) = msg_rx.recv() => msg,
            feed = next_feed(&mut app.log) => Msg::Log(feed),
            instant = ticks.tick() => Msg::Tick(instant.into_std()),
        };

        let mut msgs = vec![msg];
        // Take whatever else is ready, so a burst of log lines costs one redraw.
        msgs.extend(std::iter::from_fn(|| msg_rx.try_recv().ok()));

        if let Some(log) = &mut app.log {
            msgs.extend(std::iter::from_fn(|| log.try_feed()).map(Msg::Log));
        }

        for msg in msgs {
            for cmd in app.update(msg) {
                tasks.spawn(cmd);
            }
        }
    }

    Ok(())
}

/// The open log's next feed; never ready while no log is open.
async fn next_feed(log: &mut Option<LogView>) -> Feed {
    match log {
        Some(log) => log.next_feed().await,
        None => std::future::pending().await,
    }
}
