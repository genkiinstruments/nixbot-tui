use std::io::{self, Write};

use anyhow::{Context, Result};
use clap::Parser;
use jiff::Timestamp;

use nixbot_tui::{
    api::Client,
    app::{Row, Scope, sort_rows},
    config,
    format::{age, glyph, progress_text, short_sha},
    jump::{self, Selector},
    runtime,
    tasks::{Tasks, pr_info},
};

/// Watch and control nixbot CI builds.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Open this build; its repository is -R or the current checkout's origin.
    #[arg(conflicts_with = "dump")]
    build: Option<u64>,
    /// The build's repository.
    #[arg(
        short = 'R',
        long,
        value_name = "[FORGE/]OWNER/NAME",
        requires = "build"
    )]
    repo: Option<String>,
    /// Open this attribute's log: its name or an unambiguous part of it.
    #[arg(long, requires = "build", conflicts_with = "effect")]
    attr: Option<String>,
    /// Open this effect's log: its name, kind/name, or an unambiguous part of it.
    #[arg(long, requires = "build")]
    effect: Option<String>,
    /// Follow the opened log live.
    #[arg(short, long, requires = "build")]
    follow: bool,
    /// Print the builds list once and exit instead of starting the TUI.
    #[arg(long)]
    dump: bool,
    /// With --dump, only builds still building or running effects.
    #[arg(long, requires = "dump")]
    active: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        build,
        repo,
        attr,
        effect,
        follow,
        dump,
        active,
    } = Cli::parse();
    let clients = config::load()?
        .into_iter()
        .map(|host| {
            Client::new(&host.url, host.token).with_context(|| format!("server {}", host.url))
        })
        .collect::<Result<Vec<_>>>()?;

    if dump {
        let scope = if active { Scope::Active } else { Scope::Recent };

        return match dump_rows(clients, scope).await {
            // `--dump | head` closing the pipe early is not a failure.
            Err(err) if is_broken_pipe(&err) => Ok(()),
            result => result,
        };
    }

    let startup = match build {
        Some(number) => {
            let selector = match (&attr, &effect) {
                (Some(attr), _) => Selector::Attr(attr),
                (None, Some(effect)) => Selector::Effect(effect),
                (None, None) => Selector::Build,
            };
            Some(jump::resolve(&clients, repo.as_deref(), number, selector, follow).await?)
        }
        None => None,
    };

    runtime::run(clients, startup).await
}

async fn dump_rows(clients: Vec<Client>, scope: Scope) -> Result<()> {
    let (msg_tx, _msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut rows = Tasks::new(clients, msg_tx).rows(scope).await?;
    sort_rows(&mut rows);
    let now = Timestamp::now();
    let mut out = io::stdout().lock();

    for Row {
        key,
        build,
        progress,
        effects,
        ..
    } in rows
    {
        let pr = match build.pr_number {
            Some(n) => {
                let title = pr_info(&key.repo, n).await.map(|pr| pr.title);
                format!("#{n} {}", title.unwrap_or_default())
            }
            None => String::new(),
        };
        let progress = progress_text(&build, progress);
        let effects = effects.map(|e| e.to_string()).unwrap_or_default();

        writeln!(
            out,
            "{} {:<11} #{:<5} {:<28} {:<32} {:<8} {:<12} {:<10} {:>6}  {pr}",
            glyph(&build.status),
            build.status.label(),
            build.number,
            key.repo.to_string(),
            build.branch,
            short_sha(&build.commit_sha),
            progress,
            effects,
            age(build.created_at, now),
        )?;
    }

    Ok(())
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
}
