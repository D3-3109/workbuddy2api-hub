use crate::{
    connection::{Connection, Head, MAX_HEADERS},
    request::{token, PoolKey},
    Error, ErrorKind, Inner, Result,
};
use bytes::{Buf, Bytes, BytesMut};
use std::{fmt, sync::Arc, time::Duration};
use tokio::time::timeout;
use wbcore::types::InResponse;

#[derive(Clone, Copy)]
enum Framing {
    Length(u64),
    Chunked { remaining: u64, need_crlf: bool },
    Close,
    Done,
}

struct Reader {
    connection: Connection,
    framing: Framing,
    reusable: bool,
}

/// A streaming response. All statuses are returned; there is no automatic
/// redirect, error-body buffering, decompression, or replay. Dropping/cancelling
/// a partially consumed body closes the connection rather than pooling it.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    reader: Option<Reader>,
    inner: Arc<Inner>,
    key: PoolKey,
    wait: Duration,
    finished: bool,
    defer_release: bool,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

fn protocol(phase: &'static str) -> Error {
    Error::new(ErrorKind::Protocol, phase)
}

fn framing(head: &Head, is_head: bool, request_close: bool) -> Result<(Framing, bool)> {
    if head.status == 101 {
        return Err(protocol("unsupported response upgrade"));
    }
    let mut length = None;
    let mut encodings = Vec::new();
    let mut close = request_close;
    let mut keep_alive = false;
    for (name, value) in &head.headers {
        if name.eq_ignore_ascii_case("content-length") {
            for part in value.split(',') {
                let part = part.trim();
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(protocol("response framing"));
                }
                let parsed = part
                    .parse::<u64>()
                    .map_err(|_| protocol("response framing"))?;
                if length.is_some_and(|n| n != parsed) {
                    return Err(protocol("conflicting response lengths"));
                }
                length = Some(parsed);
            }
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            encodings.extend(value.split(',').map(|v| v.trim().to_ascii_lowercase()));
        }
        if name.eq_ignore_ascii_case("connection") {
            for value in value.split(',') {
                close |= value.trim().eq_ignore_ascii_case("close");
                keep_alive |= value.trim().eq_ignore_ascii_case("keep-alive");
            }
        }
    }
    // Ambiguous framing cannot be safely reused, even when a tolerant HTTP
    // parser could choose one of the conflicting interpretations.
    if !encodings.is_empty() && (length.is_some() || encodings != ["chunked"]) {
        return Err(protocol("unsupported or ambiguous response framing"));
    }
    let reusable = !close && (head.version == 1 || keep_alive);
    if is_head || head.status == 304 || head.status == 204 {
        if head.status == 204 && (length.is_some_and(|n| n != 0) || !encodings.is_empty()) {
            return Err(protocol("body forbidden for response status"));
        }
        return Ok((Framing::Done, reusable));
    }
    if !encodings.is_empty() {
        return Ok((
            Framing::Chunked {
                remaining: 0,
                need_crlf: false,
            },
            reusable,
        ));
    }
    match length {
        Some(0) => Ok((Framing::Done, reusable)),
        Some(length) => Ok((Framing::Length(length), reusable)),
        None => Ok((Framing::Close, false)),
    }
}

impl Response {
    pub(crate) fn new(
        head: Head,
        connection: Connection,
        inner: Arc<Inner>,
        key: PoolKey,
        wait: Duration,
        is_head: bool,
        request_close: bool,
    ) -> Result<Self> {
        let (framing, reusable) = framing(&head, is_head, request_close)?;
        let mut response = Self {
            status: head.status,
            headers: head.headers,
            reader: Some(Reader {
                connection,
                framing,
                reusable,
            }),
            inner,
            key,
            wait,
            finished: false,
            defer_release: false,
        };
        if matches!(framing, Framing::Done) {
            let reader = response.reader.take().expect("new body reader");
            response.finish(reader);
        }
        Ok(response)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Set the per-read streaming idle timeout, without imposing a total body
    /// deadline. Zero and timeouts above one day are rejected.
    pub fn set_read_timeout(&mut self, wait: Duration) -> Result<()> {
        if wait.is_zero() || wait > Duration::from_secs(86_400) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "read timeout configuration",
            ));
        }
        self.wait = wait;
        Ok(())
    }

    fn finish(&mut self, reader: Reader) {
        self.finished = true;
        if self.defer_release {
            self.reader = Some(reader);
            return;
        }
        if reader.reusable && reader.connection.buffer.is_empty() {
            self.inner
                .pool
                .lock()
                .put(self.key.clone(), reader.connection);
        }
    }

    /// Returns decoded body bytes (HTTP chunk framing removed), bounded to a
    /// socket-read sized chunk. EOF before a declared body end is an error.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>> {
        if self.finished {
            return Ok(None);
        }
        // Holding the socket in the future (not self) makes cancellation safe:
        // dropping this future closes it, including during a partial chunk line.
        let mut reader = self
            .reader
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Cancelled, "cancelled body read").transmitted())?;
        let chunk = reader.next(self.wait).await.map_err(Error::transmitted)?;
        if matches!(reader.framing, Framing::Done) {
            self.finish(reader);
        } else {
            self.reader = Some(reader);
        }
        Ok(chunk)
    }

    /// Collect at most `limit` decoded bytes. Overflow closes the connection.
    pub async fn collect(mut self, limit: usize) -> Result<Bytes> {
        // Delay pooling until the final chunk passes the size limit as well.
        self.defer_release = true;
        let mut output = BytesMut::new();
        while let Some(chunk) = self.next_chunk().await? {
            if chunk.len() > limit.saturating_sub(output.len()) {
                return Err(
                    Error::new(ErrorKind::BodyTooLarge, "response body limit").transmitted()
                );
            }
            output.extend_from_slice(&chunk);
        }
        self.defer_release = false;
        if let Some(reader) = self.reader.take() {
            self.finish(reader);
        }
        Ok(output.freeze())
    }

    /// Convenience adapter for the sans-IO response parsers in other crates.
    pub async fn into_in_response(self, limit: usize) -> Result<InResponse> {
        let status = self.status;
        let headers = self.headers.clone();
        let body = self.collect(limit).await?.to_vec();
        Ok(InResponse {
            status,
            headers,
            body,
        })
    }
}

impl Reader {
    async fn required(&mut self, wait: Duration) -> Result<()> {
        if self.connection.read_more(wait, "response body").await? == 0 {
            return Err(Error::new(ErrorKind::Truncated, "response body"));
        }
        Ok(())
    }

    async fn line(&mut self, wait: Duration, limit: usize) -> Result<Bytes> {
        timeout(wait, async {
            loop {
                if let Some(end) = self.connection.buffer.windows(2).position(|w| w == b"\r\n") {
                    if end > limit {
                        return Err(protocol("chunk metadata limit"));
                    }
                    let line = self.connection.buffer.split_to(end).freeze();
                    self.connection.buffer.advance(2);
                    return Ok(line);
                }
                if self.connection.buffer.len() > limit {
                    return Err(protocol("chunk metadata limit"));
                }
                self.required(wait).await?;
            }
        })
        .await
        .map_err(|_| Error::new(ErrorKind::Timeout, "chunk metadata"))?
    }

    async fn trailers(&mut self, wait: Duration) -> Result<()> {
        let mut size = 0;
        let mut count = 0;
        loop {
            let line = self.line(wait, 8192).await?;
            size += line.len() + 2;
            if size > MAX_HEADERS {
                return Err(protocol("response trailer limit"));
            }
            if line.is_empty() {
                return Ok(());
            }
            count += 1;
            if count > 128 {
                return Err(protocol("response trailer limit"));
            }
            let colon = line
                .iter()
                .position(|b| *b == b':')
                .ok_or_else(|| protocol("response trailer"))?;
            let name = &line[..colon];
            let value = &line[colon + 1..];
            if !token(name)
                || value.iter().any(|b| *b == 127 || (*b < 32 && *b != 9))
                || [
                    b"content-length".as_slice(),
                    b"transfer-encoding",
                    b"connection",
                    b"host",
                    b"trailer",
                ]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
            {
                return Err(protocol("response trailer"));
            }
        }
    }

    async fn next(&mut self, wait: Duration) -> Result<Option<Bytes>> {
        loop {
            match self.framing {
                Framing::Done => return Ok(None),
                Framing::Close => {
                    if self.connection.buffer.is_empty()
                        && self.connection.read_more(wait, "response body").await? == 0
                    {
                        self.framing = Framing::Done;
                        return Ok(None);
                    }
                    return Ok(Some(self.connection.buffer.split().freeze()));
                }
                Framing::Length(remaining) => {
                    if self.connection.buffer.is_empty() {
                        self.required(wait).await?;
                    }
                    let size = self
                        .connection
                        .buffer
                        .len()
                        .min(remaining.min(usize::MAX as u64) as usize);
                    let chunk = self.connection.buffer.split_to(size).freeze();
                    self.framing = if size as u64 == remaining {
                        Framing::Done
                    } else {
                        Framing::Length(remaining - size as u64)
                    };
                    return Ok(Some(chunk));
                }
                Framing::Chunked {
                    remaining,
                    need_crlf,
                } => {
                    if remaining != 0 {
                        if self.connection.buffer.is_empty() {
                            self.required(wait).await?;
                        }
                        let size = self
                            .connection
                            .buffer
                            .len()
                            .min(remaining.min(usize::MAX as u64) as usize);
                        let chunk = self.connection.buffer.split_to(size).freeze();
                        self.framing = Framing::Chunked {
                            remaining: remaining - size as u64,
                            need_crlf: size as u64 == remaining,
                        };
                        return Ok(Some(chunk));
                    }
                    if need_crlf {
                        while self.connection.buffer.len() < 2 {
                            self.required(wait).await?;
                        }
                        if &self.connection.buffer[..2] != b"\r\n" {
                            return Err(protocol("chunk terminator"));
                        }
                        self.connection.buffer.advance(2);
                    }
                    let line = self.line(wait, 8192).await?;
                    let mut parts = line.splitn(2, |b| *b == b';');
                    let size = parts.next().unwrap_or_default();
                    if size.is_empty() || size.len() > 16 || !size.iter().all(u8::is_ascii_hexdigit)
                    {
                        return Err(protocol("chunk size"));
                    }
                    if parts
                        .next()
                        .is_some_and(|ext| ext.iter().any(|b| *b == 127 || (*b < 32 && *b != 9)))
                    {
                        return Err(protocol("chunk extension"));
                    }
                    let size = u64::from_str_radix(
                        std::str::from_utf8(size).map_err(|_| protocol("chunk size"))?,
                        16,
                    )
                    .map_err(|_| protocol("chunk size"))?;
                    if size == 0 {
                        timeout(wait, self.trailers(wait))
                            .await
                            .map_err(|_| Error::new(ErrorKind::Timeout, "response trailers"))??;
                        self.framing = Framing::Done;
                        return Ok(None);
                    }
                    self.framing = Framing::Chunked {
                        remaining: size,
                        need_crlf: false,
                    };
                }
            }
        }
    }
}
