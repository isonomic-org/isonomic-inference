//! Server-sent-events framing: bytes in, complete events out.
//!
//! Only the current line and the current event's data are buffered, each
//! under a limit, so memory stays constant however long the stream runs.

use crate::diag::ProtocolError;
use crate::limits::Limits;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseFrame {
    /// The `event:` field, if the server sent one.
    pub event: Option<String>,
    /// The `data:` lines, joined with newlines.
    pub data: String,
}

#[derive(Debug)]
pub struct SseParser {
    limits: Limits,
    line: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
    /// A carriage return ended the last chunk; a following `\n` belongs to it.
    after_cr: bool,
    /// Bytes of a leading byte-order mark still to skip, per the standard.
    bom_left: &'static [u8],
}

impl SseParser {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            line: Vec::new(),
            event: None,
            data: String::new(),
            has_data: false,
            after_cr: false,
            bom_left: b"\xEF\xBB\xBF",
        }
    }

    /// Feed bytes; returns every event they completed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, ProtocolError> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if let Some((&first, rest)) = self.bom_left.split_first() {
                if byte == first {
                    self.bom_left = rest;
                    continue;
                }
                // Not a byte-order mark: the bytes skipped so far were data.
                let skipped = 3 - self.bom_left.len();
                self.bom_left = &[];
                self.line.extend_from_slice(&b"\xEF\xBB\xBF"[..skipped]);
            }
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    if let Some(frame) = self.end_line()? {
                        frames.push(frame);
                    }
                }
                _ => {
                    if self.line.len() >= self.limits.max_line_bytes {
                        return Err(ProtocolError::Limit {
                            what: "stream line bytes",
                            limit: self.limits.max_line_bytes,
                        });
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(frames)
    }

    /// Bytes held for a line or event not yet complete.
    pub fn buffered_bytes(&self) -> usize {
        self.line.len() + self.data.len()
    }

    /// Whether a partial line or event is still held.
    pub fn has_partial(&self) -> bool {
        !self.line.is_empty() || self.has_data
    }

    fn end_line(&mut self) -> Result<Option<SseFrame>, ProtocolError> {
        let raw = std::mem::take(&mut self.line);
        if raw.is_empty() {
            return Ok(self.dispatch());
        }
        let line = String::from_utf8(raw)
            .map_err(|_| ProtocolError::Invalid("stream line is not UTF-8".into()))?;
        if line.starts_with(':') {
            // A comment: servers send these as keep-alives.
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "data" => {
                let extra = value.len() + usize::from(self.has_data);
                if self.data.len() + extra > self.limits.max_event_bytes {
                    return Err(ProtocolError::Limit {
                        what: "stream event bytes",
                        limit: self.limits.max_event_bytes,
                    });
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = Some(value.to_string()),
            // `id` and `retry` concern reconnection, which the engine owns.
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseFrame> {
        let event = self.event.take();
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        let data = std::mem::take(&mut self.data);
        // Keep the buffer from growing past what one event needs.
        self.data.shrink_to(1024);
        Some(SseFrame { event, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(input: &str) -> Vec<SseFrame> {
        SseParser::new(Limits::default())
            .push(input.as_bytes())
            .unwrap()
    }

    #[test]
    fn parses_events_and_skips_comments() {
        let got = frames(": ping\n\nevent: a\ndata: one\ndata: two\n\ndata:x\n\n");
        assert_eq!(
            got,
            [
                SseFrame {
                    event: Some("a".into()),
                    data: "one\ntwo".into()
                },
                SseFrame {
                    event: None,
                    data: "x".into()
                },
            ]
        );
    }

    #[test]
    fn handles_crlf_split_across_pushes() {
        let mut parser = SseParser::new(Limits::default());
        assert!(parser.push(b"data: a\r").unwrap().is_empty());
        assert!(parser.push(b"\n\r").unwrap().len() == 1);
        assert!(!parser.has_partial());
    }

    #[test]
    fn a_leading_byte_order_mark_is_skipped() {
        let mut parser = SseParser::new(Limits::default());
        assert!(parser.push(b"\xEF\xBB").unwrap().is_empty());
        let got = parser.push(b"\xBFdata: x\n\n").unwrap();
        assert_eq!(got[0].data, "x");
        // Only at the very start.
        let got = frames("data: \u{feff}y\n\n");
        assert_eq!(got[0].data, "\u{feff}y");
    }

    #[test]
    fn a_character_split_across_pushes_survives() {
        let text = "data: caf\u{e9}\n\n".as_bytes();
        let mut parser = SseParser::new(Limits::default());
        assert!(parser.push(&text[..10]).unwrap().is_empty());
        assert_eq!(parser.push(&text[10..]).unwrap()[0].data, "caf\u{e9}");
    }
}
