//! Incremental parser for `text/event-stream` bodies, fed with arbitrary network chunks.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
    /// The previous chunk ended in CR, so a leading LF belongs to that line break.
    after_cr: bool,
    event: Option<String>,
    data: Option<String>,
}

impl Parser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Event> {
        let mut chunk = chunk;
        if std::mem::take(&mut self.after_cr) && chunk.first() == Some(&b'\n') {
            chunk = &chunk[1..];
        }
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            match self.buf[i] {
                b'\r' | b'\n' => {
                    let line = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
                    let crlf = self.buf[i] == b'\r';
                    i += 1;
                    if crlf {
                        match self.buf.get(i) {
                            Some(b'\n') => i += 1,
                            Some(_) => {}
                            None => self.after_cr = true,
                        }
                    }
                    start = i;
                    events.extend(self.line(&line));
                }
                _ => i += 1,
            }
        }
        self.buf.drain(..start);
        events
    }

    fn line(&mut self, line: &str) -> Option<Event> {
        if line.is_empty() {
            let event = self.event.take();
            return self.data.take().map(|data| Event {
                event: event.unwrap_or_else(|| "message".to_owned()),
                data,
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event: &str, data: &str) -> Event {
        Event {
            event: event.to_owned(),
            data: data.to_owned(),
        }
    }

    fn parse_in_chunks(body: &[u8], size: usize) -> Vec<Event> {
        let mut parser = Parser::default();
        body.chunks(size).flat_map(|c| parser.feed(c)).collect()
    }

    #[test]
    fn events_survive_any_chunking() {
        let body = include_bytes!("../tests/fixtures/log_stream.sse");
        let whole = parse_in_chunks(body, body.len());
        assert_eq!(
            whole.iter().map(|e| e.event.as_str()).collect::<Vec<_>>(),
            [
                "state", "line", "line", "line", "drv", "phase", "drv-done", "done"
            ]
        );
        for size in 1..17 {
            assert_eq!(parse_in_chunks(body, size), whole, "chunk size {size}");
        }
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        let events = parse_in_chunks(b"data: one\ndata:two\ndata\n\n", 3);
        assert_eq!(events, [event("message", "one\ntwo\n")]);
    }

    #[test]
    fn comments_retry_and_blank_blocks_are_skipped() {
        let body = b"retry: 3000\n\n: keepalive\n\nevent: status\ndata: {}\n\n";
        assert_eq!(parse_in_chunks(body, 5), [event("status", "{}")]);
    }

    #[test]
    fn cr_and_crlf_terminate_lines() {
        let body = b"event: a\r\ndata: 1\r\n\r\nevent: b\rdata: 2\r\r";
        for size in 1..body.len() {
            assert_eq!(
                parse_in_chunks(body, size),
                [event("a", "1"), event("b", "2")],
                "chunk size {size}"
            );
        }
    }

    #[test]
    fn utf8_split_across_chunks() {
        let body = "data: ── ✓\n\n".as_bytes();
        assert_eq!(parse_in_chunks(body, 1), [event("message", "── ✓")]);
    }

    #[test]
    fn incomplete_event_waits_for_the_blank_line() {
        let mut parser = Parser::default();
        assert_eq!(parser.feed(b"event: done\ndata: {}\n"), []);
        assert_eq!(parser.feed(b"\n"), [event("done", "{}")]);
    }
}
