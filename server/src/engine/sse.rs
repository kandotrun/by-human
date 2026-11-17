//! Incremental SSE framing. UTF-8 is decoded only after a complete line.

use super::{Result, protocol_error};

#[derive(Debug)]
pub(super) struct SseEvent {
    pub event: String,
    pub data: String,
}

#[derive(Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
    event: String,
    data: Vec<String>,
    skip_lf: bool,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        let bytes = if self.skip_lf && !bytes.is_empty() {
            self.skip_lf = false;
            bytes.strip_prefix(b"\n").unwrap_or(bytes)
        } else {
            bytes
        };
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self
            .pending
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
        {
            self.skip_lf = self.pending[end] == b'\r' && end + 1 == self.pending.len();
            let delimiter =
                if self.pending[end] == b'\r' && self.pending.get(end + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            let line = std::str::from_utf8(&self.pending[..end])
                .map_err(|_| protocol_error())?
                .to_owned();
            self.pending.drain(..end + delimiter);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(SseEvent {
                        event: std::mem::take(&mut self.event),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                } else {
                    self.event.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, raw_value) = line.split_once(':').unwrap_or((&line, ""));
            let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);
            match field {
                "event" => self.event = value.to_owned(),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        Ok(events)
    }
}
