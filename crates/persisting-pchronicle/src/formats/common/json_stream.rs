use std::io::{self, BufRead, Read};
use std::path::Path;

/// Top-level JSON stream shapes supported by local trajectory datasources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonStreamShape {
    Object,
    Array,
    Ndjson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonRecordLocation {
    ArrayElement(usize),
    NdjsonLine(usize),
}

impl std::fmt::Display for JsonRecordLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArrayElement(ordinal) => write!(formatter, "array element {ordinal}"),
            Self::NdjsonLine(line) => write!(formatter, "JSONL line {line}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonStreamVisit {
    pub(crate) record_count: usize,
    pub(crate) peak_record_bytes: usize,
}

/// Bounded reader that tracks source bytes and rejects reads past `maximum`.
pub(crate) struct BoundedCountingReader<R> {
    inner: R,
    bytes_read: u64,
    maximum: u64,
}

impl<R> BoundedCountingReader<R> {
    pub(crate) fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            bytes_read: 0,
            maximum,
        }
    }

    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for BoundedCountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.bytes_read);
        if remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("trajectory input exceeded {} bytes", self.maximum),
            ));
        }
        let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = self.inner.read(&mut buffer[..maximum])?;
        self.bytes_read += read as u64;
        Ok(read)
    }
}

/// Feeds one top-level JSON object from a buffered stream into serde without
/// first copying the whole object into an intermediate `Vec`.
///
/// The reader tracks object depth and stops at the matching `}`. Bytes consumed
/// are counted against `maximum`; oversized objects fail closed.
pub(crate) struct ScopedJsonObjectReader<'a, R: ?Sized> {
    inner: &'a mut R,
    maximum: usize,
    bytes: usize,
    depth: usize,
    started: bool,
    finished: bool,
    in_string: bool,
    escaped: bool,
}

impl<'a, R: BufRead + ?Sized> ScopedJsonObjectReader<'a, R> {
    pub(crate) fn new(inner: &'a mut R, maximum: usize) -> Self {
        Self {
            inner,
            maximum,
            bytes: 0,
            depth: 0,
            started: false,
            finished: false,
            in_string: false,
            escaped: false,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }
}

impl<R: BufRead + ?Sized> Read for ScopedJsonObjectReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.finished || buffer.is_empty() {
            return Ok(0);
        }

        let available = self.inner.fill_buf()?;
        if available.is_empty() {
            if self.started && !self.finished {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated JSON object in array",
                ));
            }
            return Ok(0);
        }

        let mut take = 0_usize;
        for &byte in available.iter().take(buffer.len()) {
            if self.bytes == self.maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "JSON array record exceeded max_record_bytes {}",
                        self.maximum
                    ),
                ));
            }

            take += 1;
            self.bytes += 1;
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                }
                continue;
            }

            match byte {
                b'"' => self.in_string = true,
                b'{' | b'[' => {
                    self.started = true;
                    self.depth = self.depth.checked_add(1).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "JSON nesting depth overflow")
                    })?;
                }
                b'}' | b']' => {
                    if !self.started || self.depth == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected JSON closing delimiter",
                        ));
                    }
                    self.depth -= 1;
                    if self.depth == 0 {
                        if byte != b'}' {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "JSON array element must be a JSON object",
                            ));
                        }
                        self.finished = true;
                        break;
                    }
                }
                _ => {
                    if !self.started {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "JSON array element must be an object",
                        ));
                    }
                }
            }
        }

        buffer[..take].copy_from_slice(&available[..take]);
        self.inner.consume(take);
        Ok(take)
    }
}

/// Reads one top-level JSON object into `record`, reusing the caller's
/// allocation across elements.
///
/// serde_json's reader-based deserializer pulls a single byte per `Read::read`
/// call, which makes parsing through `Read` several times slower than parsing a
/// slice. Copying one bounded element into a reused buffer keeps peak memory at
/// `maximum` while letting callers use the much faster slice parser.
pub(crate) fn read_bounded_json_object<R: BufRead + ?Sized>(
    reader: &mut R,
    record: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<()> {
    record.clear();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unterminated JSON object in array",
            ));
        }

        // Never scan more than one byte past the budget so an oversized element
        // fails without walking the rest of the stream.
        let scan_limit = maximum
            .saturating_sub(record.len())
            .saturating_add(1)
            .min(available.len());
        let mut take = 0_usize;
        let mut complete = false;
        for &byte in &available[..scan_limit] {
            take += 1;
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth = depth.checked_add(1).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "JSON nesting depth overflow")
                    })?;
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected JSON closing delimiter",
                        ));
                    }
                    depth -= 1;
                    if depth == 0 {
                        if byte != b'}' {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "JSON array element must be a JSON object",
                            ));
                        }
                        complete = true;
                        break;
                    }
                }
                _ => {
                    if depth == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "JSON array element must be an object",
                        ));
                    }
                }
            }
        }

        if record.len().saturating_add(take) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSON array record exceeded max_record_bytes {maximum}"),
            ));
        }
        record.extend_from_slice(&available[..take]);
        reader.consume(take);
        if complete {
            return Ok(());
        }
    }
}

pub(crate) fn read_bounded_line<R: BufRead + ?Sized>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if buffer.len().saturating_add(end) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSONL record exceeded max_record_bytes {maximum}"),
            ));
        }
        buffer.extend_from_slice(&available[..end]);
        let ended = available[end - 1] == b'\n';
        reader.consume(end);
        if ended {
            return Ok(buffer.len());
        }
    }
}

pub(crate) fn trim_ascii_whitespace(mut input: &[u8]) -> &[u8] {
    while input.first().is_some_and(u8::is_ascii_whitespace) {
        input = &input[1..];
    }
    while input.last().is_some_and(u8::is_ascii_whitespace) {
        input = &input[..input.len() - 1];
    }
    input
}

pub(crate) fn first_non_whitespace<R: BufRead + ?Sized>(reader: &mut R) -> io::Result<Option<u8>> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        if let Some(index) = available
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
        {
            let first = available[index];
            reader.consume(index);
            return Ok(Some(first));
        }
        let length = available.len();
        reader.consume(length);
    }
}

pub(crate) fn is_ndjson(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "jsonl" | "ndjson"))
}

pub(crate) fn detect_json_stream_shape<R: BufRead + ?Sized>(
    path: &Path,
    reader: &mut R,
) -> io::Result<JsonStreamShape> {
    if is_ndjson(path) {
        return Ok(JsonStreamShape::Ndjson);
    }
    match first_non_whitespace(reader)? {
        Some(b'{') => Ok(JsonStreamShape::Object),
        Some(b'[') => Ok(JsonStreamShape::Array),
        Some(other) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported JSON stream start byte 0x{other:02x}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "JSON input is empty",
        )),
    }
}

/// Dispatch all supported top-level JSON shapes while keeping array and
/// NDJSON records bounded. The object callback receives the original reader,
/// allowing callers to deserialize it directly without an intermediate copy.
pub(crate) fn visit_json_stream<R, S, O, F>(
    path: &Path,
    reader: &mut R,
    max_record_bytes: usize,
    state: &mut S,
    visit_object: O,
    mut visit_record: F,
) -> io::Result<JsonStreamVisit>
where
    R: BufRead + ?Sized,
    O: FnOnce(&mut R, &mut S) -> io::Result<()>,
    F: FnMut(&[u8], JsonRecordLocation, &mut S) -> io::Result<()>,
{
    let shape = detect_json_stream_shape(path, reader)?;
    let mut peak_record_bytes = 0_usize;
    let record_count = match shape {
        JsonStreamShape::Object => {
            visit_object(reader, state)?;
            1
        }
        JsonStreamShape::Array => {
            for_each_json_array_record(reader, max_record_bytes, |record, ordinal| {
                peak_record_bytes = peak_record_bytes.max(record.len());
                visit_record(record, JsonRecordLocation::ArrayElement(ordinal), state)
            })?
        }
        JsonStreamShape::Ndjson => {
            for_each_ndjson_line(reader, max_record_bytes, |record, line| {
                peak_record_bytes = peak_record_bytes.max(record.len());
                visit_record(record, JsonRecordLocation::NdjsonLine(line), state)
            })?
        }
    };
    Ok(JsonStreamVisit {
        record_count,
        peak_record_bytes,
    })
}

/// Stream each top-level object in a JSON array through `visit`, one bounded
/// element at a time.
///
/// `visit` receives the raw bytes of a single element, valid until the next
/// iteration. Peak memory stays within `max_record_bytes` because the backing
/// buffer is reused across elements.
pub(crate) fn for_each_json_array_record<R, F>(
    reader: &mut R,
    max_record_bytes: usize,
    mut visit: F,
) -> io::Result<usize>
where
    R: BufRead + ?Sized,
    F: FnMut(&[u8], usize) -> io::Result<()>,
{
    let mut record = Vec::new();
    anyhow_io_ensure(
        first_non_whitespace(reader)? == Some(b'['),
        "JSON array must start with '['",
    )?;
    reader.consume(1);

    let mut first = true;
    let mut ordinal = 0_usize;
    loop {
        if !first {
            match first_non_whitespace(reader)? {
                Some(b']') => {
                    reader.consume(1);
                    anyhow_io_ensure(
                        first_non_whitespace(reader)?.is_none(),
                        "trailing content after JSON array",
                    )?;
                    return Ok(ordinal);
                }
                Some(b',') => reader.consume(1),
                Some(other) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("JSON array expected ',' or ']', found byte 0x{other:02x}"),
                    ));
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unterminated JSON array",
                    ));
                }
            }
        }

        match first_non_whitespace(reader)? {
            Some(b']') if first => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "JSON array contains no objects",
                ));
            }
            Some(b'{') => {}
            Some(other) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("JSON array element must be an object, found byte 0x{other:02x}"),
                ));
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated JSON array",
                ));
            }
        }

        ordinal += 1;
        read_bounded_json_object(reader, &mut record, max_record_bytes)?;
        visit(&record, ordinal)?;
        first = false;
    }
}

/// Stream non-empty NDJSON/JSONL records through `visit`.
pub(crate) fn for_each_ndjson_line<R, F>(
    reader: &mut R,
    max_record_bytes: usize,
    mut visit: F,
) -> io::Result<usize>
where
    R: BufRead + ?Sized,
    F: FnMut(&[u8], usize) -> io::Result<()>,
{
    let mut buffer = Vec::new();
    let mut line_number = 0_usize;
    let mut count = 0_usize;
    loop {
        let read = read_bounded_line(reader, &mut buffer, max_record_bytes)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        let record = trim_ascii_whitespace(&buffer);
        if record.is_empty() {
            continue;
        }
        count += 1;
        visit(record, line_number)?;
    }
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON input contains no objects",
        ));
    }
    Ok(count)
}

fn anyhow_io_ensure(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            message.to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn scoped_object_reader_stops_at_object_end_without_extra_copy() {
        let mut input = Cursor::new(br#"{"a":1},{"b":2}]"#);
        let mut scoped = ScopedJsonObjectReader::new(&mut input, 64);
        let mut buffer = Vec::new();
        scoped.read_to_end(&mut buffer).unwrap();
        assert_eq!(buffer, br#"{"a":1}"#);
        assert!(scoped.finished);
        assert_eq!(input.position(), 7);
    }

    #[test]
    fn scoped_object_reader_enforces_max_record_bytes() {
        let mut input = Cursor::new(br#"{"message":"hello-world"}]"#);
        let mut scoped = ScopedJsonObjectReader::new(&mut input, 8);
        let error = scoped.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("max_record_bytes 8"));
    }

    #[test]
    fn for_each_json_array_record_yields_one_element_at_a_time() {
        let mut input = Cursor::new(br#"[ {"a":1}, {"b":2} ]"#);
        let mut values = Vec::new();
        let count = for_each_json_array_record(&mut input, 64, |record, _ordinal| {
            values.push(serde_json::from_slice::<serde_json::Value>(record).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(values[0]["a"], 1);
        assert_eq!(values[1]["b"], 2);
    }

    #[test]
    fn read_bounded_json_object_reuses_the_record_buffer() {
        let mut input = Cursor::new(br#"{"a":{"nested":[1,2]}}{"b":2}"#);
        let mut record = Vec::new();
        read_bounded_json_object(&mut input, &mut record, 64).unwrap();
        assert_eq!(record, br#"{"a":{"nested":[1,2]}}"#);
        let capacity = record.capacity();
        read_bounded_json_object(&mut input, &mut record, 64).unwrap();
        assert_eq!(record, br#"{"b":2}"#);
        assert_eq!(record.capacity(), capacity);
    }

    #[test]
    fn read_bounded_json_object_enforces_max_record_bytes() {
        let mut input = Cursor::new(br#"{"message":"hello-world"}]"#);
        let error = read_bounded_json_object(&mut input, &mut Vec::new(), 8).unwrap_err();
        assert!(error.to_string().contains("max_record_bytes 8"));
    }

    #[test]
    fn read_bounded_json_object_keeps_braces_inside_strings_out_of_depth() {
        let mut input = Cursor::new(br#"{"text":"a}b{c\"}"}"#);
        let mut record = Vec::new();
        read_bounded_json_object(&mut input, &mut record, 64).unwrap();
        assert_eq!(record, br#"{"text":"a}b{c\"}"}"#);
    }
}
