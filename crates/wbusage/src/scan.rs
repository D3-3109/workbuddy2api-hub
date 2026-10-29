//! Bounded streaming JSONL reader with a resumable, replacement-aware cursor.
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

pub(crate) const MAX_LINE: usize = 1024 * 1024;
const NL: u8 = 0x0a;

/// Called once per complete nonblank line. None means malformed/oversized.
/// Returns the offset after the last complete line, never after a partial tail.
pub(crate) fn scan_from(
    path: &Path,
    offset: u64,
    mut visit: impl FnMut(Option<Value>),
) -> io::Result<u64> {
    scan_range(path, offset, None, |_, row| visit(row))
}

/// `scan_from` that also reports each line's start offset and can stop at a
/// line boundary `end`, so a reader can revisit exactly the lines it indexed.
pub(crate) fn scan_range(
    path: &Path,
    offset: u64,
    end: Option<u64>,
    mut visit: impl FnMut(u64, Option<Value>),
) -> io::Result<u64> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    let mut over = false;
    let mut nonblank = false;
    let mut position = offset;
    let mut complete = offset;
    loop {
        if end.is_some_and(|end| position >= end) {
            break;
        }
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let chunk = match end {
            Some(end) => &chunk[..chunk.len().min((end - position) as usize)],
            None => chunk,
        };
        let newline = chunk.iter().position(|b| *b == NL);
        let n = newline.map(|i| i + 1).unwrap_or(chunk.len());
        nonblank |= chunk[..n].iter().any(|b| !b.is_ascii_whitespace());
        if !over {
            if line.len() + n <= MAX_LINE {
                line.extend_from_slice(&chunk[..n]);
            } else {
                line.clear();
                over = true;
            }
        }
        reader.consume(n);
        position += n as u64;
        if newline.is_some() {
            if nonblank {
                let row = if over {
                    None
                } else {
                    serde_json::from_slice::<Value>(&line)
                        .ok()
                        .filter(Value::is_object)
                };
                visit(complete, row);
            }
            complete = position;
            line.clear();
            over = false;
            nonblank = false;
        }
    }
    Ok(complete)
}

/// Re-read complete lines that a scan reported at `offsets`. `end` bounds the
/// last line (the next line's offset or the indexed end of file). Ascending,
/// dense offsets are streamed; anything else is read line by line. A line that
/// no longer parses as an object yields None.
pub(crate) fn read_rows(path: &Path, offsets: &[u64], end: u64) -> io::Result<Vec<Option<Value>>> {
    let mut out = Vec::with_capacity(offsets.len());
    let (Some(first), Some(last)) = (offsets.first(), offsets.last()) else {
        return Ok(out);
    };
    let ascending = offsets.windows(2).all(|w| w[0] < w[1]);
    if ascending && end > *last && end - first <= 4096 * offsets.len() as u64 + (1 << 20) {
        let mut want = offsets.iter().peekable();
        scan_range(path, *first, Some(end), |offset, row| {
            if want.peek() == Some(&&offset) {
                want.next();
                out.push(row);
            }
        })?;
        out.resize(offsets.len(), None);
        return Ok(out);
    }
    let mut reader = BufReader::with_capacity(16 * 1024, File::open(path)?);
    let mut line = Vec::new();
    for &offset in offsets {
        reader.seek(SeekFrom::Start(offset))?;
        line.clear();
        (&mut reader)
            .take(MAX_LINE as u64)
            .read_until(NL, &mut line)?;
        out.push(if line.last() == Some(&NL) {
            serde_json::from_slice::<Value>(&line)
                .ok()
                .filter(Value::is_object)
        } else {
            None
        });
    }
    Ok(out)
}

pub(crate) fn scan(path: &Path, mut visit: impl FnMut(Value)) -> io::Result<()> {
    scan_from(path, 0, |r| {
        if let Some(r) = r {
            visit(r)
        }
    })?;
    Ok(())
}

#[derive(Default, Clone)]
pub(crate) struct Cursor {
    pub offset: u64,
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    tail: Vec<u8>,
    head: Vec<u8>,
    #[cfg(unix)]
    identity: (u64, u64),
}
fn signature(file: &mut File, start: u64, count: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; count as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
impl Cursor {
    pub fn capture(path: &Path, offset: u64) -> io::Result<Self> {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        let n = offset.min(64);
        let tail = signature(&mut file, offset - n, n)?;
        let head = signature(&mut file, 0, n)?;
        Ok(Self {
            offset,
            size: meta.len(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            tail,
            head,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        })
    }
    /// Same file, same size and same mtime as when captured: nothing to fold.
    /// Only a positive answer is cheap; anything else needs `matches`. Recently
    /// modified files always take the content check, because a same-size
    /// in-place rewrite within one mtime tick is otherwise invisible.
    pub fn unchanged(&self, path: &Path) -> bool {
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        let settled = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(2));
        if !settled {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (meta.dev(), meta.ino()) != self.identity {
                return false;
            }
        }
        self.offset == self.size
            && meta.len() == self.size
            && meta.modified().ok() == self.modified
            && meta.created().ok() == self.created
    }
    pub fn matches(&self, path: &Path) -> io::Result<bool> {
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(self.offset == 0),
            Err(e) => return Err(e),
        };
        if meta.len() < self.offset || meta.created().ok() != self.created {
            return Ok(false);
        }
        if meta.len() == self.size && meta.modified().ok() != self.modified {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (meta.dev(), meta.ino()) != self.identity {
                return Ok(false);
            }
        }
        if self.offset == 0 {
            return Ok(true);
        }
        let mut file = File::open(path)?;
        let n = self.offset.min(64);
        Ok(signature(&mut file, self.offset - n, n)? == self.tail
            && signature(&mut file, 0, n)? == self.head)
    }
}
