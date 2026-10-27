use tokio::sync::mpsc;

use super::{BuildKey, LogSource};

const MAX_LINES: usize = 50_000;

/// What the tasks feeding one log view send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    /// The stored log, fetched when the view opens.
    History(Result<String, String>),
    /// Whole lines from an attribute's structured stream.
    Lines(Vec<String>),
    /// Raw effect output, split anywhere.
    Chunk(String),
    /// A stream is about to replay the log from the start.
    Clear,
    /// The stream ended; the note says how, if there is something to say.
    End(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Keep the newest line in view as output arrives.
    End,
    /// The first line shown.
    At(usize),
}

#[derive(Debug)]
pub struct LogView {
    pub key: BuildKey,
    pub source: LogSource,
    pub web_url: String,
    pub lines: Vec<String>,
    /// Effect output arrives in chunks; this holds the unfinished last line.
    partial: String,
    pub scroll: Scroll,
    pub note: Option<String>,
    pub is_following: bool,
    /// Tasks feeding this view end when it closes or swaps the channel: their sends fail.
    feed_tx: mpsc::UnboundedSender<Feed>,
    feed_rx: mpsc::UnboundedReceiver<Feed>,
}

impl LogView {
    pub fn new(key: BuildKey, source: LogSource, web_url: String) -> Self {
        let (feed_tx, feed_rx) = mpsc::unbounded_channel();

        Self {
            key,
            source,
            web_url,
            lines: Vec::new(),
            partial: String::new(),
            scroll: Scroll::End,
            note: None,
            is_following: false,
            feed_tx,
            feed_rx,
        }
    }

    pub fn feed_tx(&self) -> mpsc::UnboundedSender<Feed> {
        self.feed_tx.clone()
    }

    /// Detaches every task feeding the view.
    pub fn reconnect(&mut self) {
        (self.feed_tx, self.feed_rx) = mpsc::unbounded_channel();
    }

    pub async fn next_feed(&mut self) -> Feed {
        self.feed_rx
            .recv()
            .await
            .expect("the view holds a sender, so its channel stays open")
    }

    pub fn try_feed(&mut self) -> Option<Feed> {
        self.feed_rx.try_recv().ok()
    }

    pub fn apply(&mut self, feed: Feed) {
        let Self {
            lines,
            partial,
            scroll,
            note,
            is_following,
            ..
        } = self;

        match feed {
            Feed::History(Ok(text)) => {
                // Live lines may have arrived first; history goes before them.
                let mut history: Vec<String> = text.lines().map(sanitize).collect();
                history.append(lines);
                *lines = history;
            }
            Feed::History(Err(err)) => *note = Some(err),
            Feed::Lines(new) => lines.extend(new.iter().map(|l| sanitize(l))),
            Feed::Chunk(text) => {
                partial.push_str(&text);

                if let Some(end) = partial.rfind('\n') {
                    let rest = partial.split_off(end + 1);
                    let complete = std::mem::replace(partial, rest);
                    lines.extend(complete.lines().map(sanitize));
                }
            }
            Feed::Clear => {
                lines.clear();
                partial.clear();
            }
            Feed::End(end_note) => {
                if !partial.is_empty() {
                    lines.push(sanitize(&std::mem::take(partial)));
                }

                *is_following = false;
                *note = end_note.or(note.take());
            }
        }

        if let Some(excess) = lines.len().checked_sub(MAX_LINES) {
            lines.drain(..excess);

            if let Scroll::At(top) = scroll {
                *top = top.saturating_sub(excess);
            }
        }
    }

    /// Clears what a previous run of the stream showed before it is replayed.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.partial.clear();
        self.note = None;
    }

    /// First line shown in a body of `height` lines.
    pub fn top(&self, height: usize) -> usize {
        let bottom = self.lines.len().saturating_sub(height);

        match self.scroll {
            Scroll::End => bottom,
            Scroll::At(top) => top.min(bottom),
        }
    }

    pub fn scroll_by(&mut self, delta: isize, height: usize) {
        let bottom = self.lines.len().saturating_sub(height);
        let top = self.top(height).saturating_add_signed(delta).min(bottom);
        self.scroll = if top == bottom && delta > 0 {
            Scroll::End
        } else {
            Scroll::At(top)
        };
    }
}

/// Carriage returns redraw a line in a terminal; keep what would be left on screen.
fn sanitize(line: &str) -> String {
    let line = line
        .rsplit('\r')
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    line.replace('\t', "    ")
}
