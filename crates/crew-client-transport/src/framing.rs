//! Framing bounds apply before extending a line buffer or decoding JSON.

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Oversized,
    Unterminated,
    InvalidUtf8,
    Empty,
    LineBreak,
}

/// Validate one record without accepting a second newline-delimited request.
pub fn validate_record(text: &str) -> Result<(), FrameError> {
    if text.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized);
    }
    if text.trim().is_empty() {
        return Err(FrameError::Empty);
    }
    if text.bytes().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err(FrameError::LineBreak);
    }
    Ok(())
}

/// Feed a fixed-size IO chunk. Each extension is checked before allocation.
/// A partial record at EOF is uncertain framing, even when its JSON is valid.
#[derive(Default)]
pub struct LineBuffer {
    bytes: Vec<u8>,
    failed: bool,
}

impl LineBuffer {
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, FrameError> {
        if self.failed {
            return Err(FrameError::Unterminated);
        }
        let result = self.feed_inner(chunk);
        if result.is_err() {
            self.failed = true;
            self.bytes.clear();
        }
        result
    }

    fn feed_inner(&mut self, mut chunk: &[u8]) -> Result<Vec<String>, FrameError> {
        let mut records = Vec::new();
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let length = newline.unwrap_or(chunk.len());
            if length > MAX_FRAME_BYTES.saturating_sub(self.bytes.len()) {
                return Err(FrameError::Oversized);
            }
            // Exact growth never reserves more than the frame cap.
            self.bytes.reserve_exact(length);
            self.bytes.extend_from_slice(&chunk[..length]);
            match newline {
                None => break,
                Some(_) => {
                    let mut bytes = std::mem::take(&mut self.bytes);
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                    let text = String::from_utf8(bytes).map_err(|_| FrameError::InvalidUtf8)?;
                    validate_record(&text)?;
                    records.push(text);
                    chunk = &chunk[length + 1..];
                }
            }
        }
        Ok(records)
    }

    pub fn finish(&self) -> Result<(), FrameError> {
        if self.failed || !self.bytes.is_empty() {
            Err(FrameError::Unterminated)
        } else {
            Ok(())
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_lines_crlf_and_limit_are_checked_before_growth() {
        let mut line = LineBuffer::default();
        assert!(line.feed(b"{\"id\":").unwrap().is_empty());
        assert_eq!(
            line.feed(b"1}\r\n{\"id\":2}\n").unwrap(),
            ["{\"id\":1}", "{\"id\":2}"]
        );
        let mut exact = LineBuffer::default();
        assert!(exact.feed(&vec![b'x'; MAX_FRAME_BYTES]).unwrap().is_empty());
        assert_eq!(exact.buffered_bytes(), MAX_FRAME_BYTES);
        assert_eq!(exact.feed(b"x"), Err(FrameError::Oversized));
        assert_eq!(exact.buffered_bytes(), 0);
    }

    #[test]
    fn uncertain_utf8_empty_and_multiple_record_submission_fail_closed() {
        let mut partial = LineBuffer::default();
        partial.feed(b"{\"id\":1}").unwrap();
        assert_eq!(partial.finish(), Err(FrameError::Unterminated));
        assert_eq!(
            LineBuffer::default().feed(b"\xff\n"),
            Err(FrameError::InvalidUtf8)
        );
        assert_eq!(LineBuffer::default().feed(b"\n"), Err(FrameError::Empty));
        assert_eq!(validate_record("{}\n{}"), Err(FrameError::LineBreak));
    }
}
