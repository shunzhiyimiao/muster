//! Minimal incremental Server-Sent-Events parser.
//!
//! Hand-rolled instead of pulling a dependency because the failure modes we must
//! survive are narrow and testable: HTTP chunks splitting an event mid-line or
//! mid-UTF-8-codepoint, CRLF endings, comment lines, multi-`data:` events and the
//! OpenAI `[DONE]` sentinel.
//!
//! Correctness detail: the buffer is `Vec<u8>` and we only ever cut at `\n`
//! (0x0A). 0x0A can never appear inside a multi-byte UTF-8 sequence, so each
//! extracted line is decoded independently and a codepoint split across network
//! chunks reassembles for free.

use crate::error::ProviderError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    /// One complete event's data payload (multiple `data:` lines joined by `\n`).
    Data(String),
    /// The `[DONE]` sentinel.
    Done,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a network chunk; returns every frame completed by it.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ProviderError> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();

        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // the \n itself
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8(line)
                .map_err(|e| ProviderError::StreamProtocol(format!("invalid utf-8 in sse line: {e}")))?;

            if line.is_empty() {
                // Event boundary.
                if let Some(frame) = self.flush_event() {
                    frames.push(frame);
                }
            } else if let Some(payload) = line.strip_prefix("data:") {
                self.data_lines.push(payload.strip_prefix(' ').unwrap_or(payload).to_owned());
            } else {
                // Comments (":…") and fields we don't use (event:, id:, retry:) are ignored.
            }
        }
        Ok(frames)
    }

    /// Flush a trailing event that was not terminated by a blank line (lenient
    /// EOF handling — some proxies drop the final separator, or even the final
    /// newline of the last line).
    pub fn finish(&mut self) -> Option<SseFrame> {
        if !self.buf.is_empty() {
            let line_bytes = std::mem::take(&mut self.buf);
            if let Ok(mut line) = String::from_utf8(line_bytes) {
                if line.ends_with('\r') {
                    line.pop();
                }
                if let Some(payload) = line.strip_prefix("data:") {
                    self.data_lines
                        .push(payload.strip_prefix(' ').unwrap_or(payload).to_owned());
                }
            }
            // Invalid UTF-8 in a truncated tail is dropped: at EOF there is no
            // continuation coming that could complete the codepoint.
        }
        self.flush_event()
    }

    fn flush_event(&mut self) -> Option<SseFrame> {
        if self.data_lines.is_empty() {
            return None;
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.trim() == "[DONE]" {
            Some(SseFrame::Done)
        } else {
            Some(SseFrame::Data(payload))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(parser: &mut SseParser, chunks: &[&[u8]]) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(parser.feed(c).expect("feed"));
        }
        if let Some(f) = parser.finish() {
            out.push(f);
        }
        out
    }

    #[test]
    fn parses_simple_events_and_done() {
        let mut p = SseParser::new();
        let frames = feed_all(
            &mut p,
            &[b"data: {\"a\":1}\n\n", b"data: [DONE]\n\n"],
        );
        assert_eq!(
            frames,
            vec![SseFrame::Data("{\"a\":1}".into()), SseFrame::Done]
        );
    }

    #[test]
    fn survives_chunk_split_mid_line() {
        let mut p = SseParser::new();
        let frames = feed_all(&mut p, &[b"da", b"ta: {\"a\":", b"1}\n", b"\n"]);
        assert_eq!(frames, vec![SseFrame::Data("{\"a\":1}".into())]);
    }

    #[test]
    fn survives_chunk_split_mid_codepoint() {
        // "审" = E5 AE A1 — split between continuation bytes.
        let bytes = "data: 审\n\n".as_bytes();
        let (a, b) = bytes.split_at(7); // cuts inside the multi-byte char
        let mut p = SseParser::new();
        let frames = feed_all(&mut p, &[a, b]);
        assert_eq!(frames, vec![SseFrame::Data("审".into())]);
    }

    #[test]
    fn handles_crlf_and_comments() {
        let mut p = SseParser::new();
        let frames = feed_all(
            &mut p,
            &[b": keep-alive\r\n", b"data: x\r\n", b"\r\n"],
        );
        assert_eq!(frames, vec![SseFrame::Data("x".into())]);
    }

    #[test]
    fn joins_multiple_data_lines() {
        let mut p = SseParser::new();
        let frames = feed_all(&mut p, &[b"data: l1\ndata: l2\n\n"]);
        assert_eq!(frames, vec![SseFrame::Data("l1\nl2".into())]);
    }

    #[test]
    fn flushes_unterminated_trailing_event() {
        let mut p = SseParser::new();
        let mut frames = p.feed(b"data: tail").unwrap();
        assert!(frames.is_empty());
        if let Some(f) = p.finish() {
            frames.push(f);
        }
        assert_eq!(frames, vec![SseFrame::Data("tail".into())]);
    }
}
