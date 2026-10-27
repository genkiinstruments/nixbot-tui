use std::io::{self, Write};

use anyhow::{Context, Result};
use clap::Parser;
use jiff::Timestamp;

use nixbot_tui::{
    api::Client,
    app::{Row, Scope, sort_rows},
    config,
    format::{age, glyph, progress_text, short_sha},
    runtime,
    tasks::{Tasks, pr_info},
};

/// Watch and control nixbot CI builds.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Print the builds list once and exit instead of starting the TUI.
    #[arg(long)]
    dump: bool,
    /// With --dump, include recently finished builds.
    #[arg(long, short, requires = "dump")]
    all: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli { dump, all } = Cli::parse();
    let clients = config::load()?
        .into_iter()
        .map(|host| {
            Client::new(&host.url, host.token).with_context(|| format!("server {}", host.url))
        })
        .collect::<Result<Vec<_>>>()?;

    if dump {
        let scope = if all { Scope::Recent } else { Scope::Active };

        return match dump_rows(clients, scope).await {
            // `--dump | head` closing the pipe early is not a failure.
            Err(err) if is_broken_pipe(&err) => Ok(()),
            result => result,
        };
    }

    runtime::run(clients).await
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

        writeln!(
            out,
            "{} {:<11} #{:<5} {:<28} {:<32} {:<8} {:<12} {:>6}  {pr}",
            glyph(&build.status),
            build.status.label(),
            build.number,
            key.repo.to_string(),
            build.branch,
            short_sha(&build.commit_sha),
            progress,
            age(build.created_at, now),
        )?;
    }

    Ok(())
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
}
