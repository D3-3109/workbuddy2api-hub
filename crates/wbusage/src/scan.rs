//! Bounded streaming JSONL reader with a resumable, replacement-aware cursor.
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

pub(crate) const MAX_LINE: usize = 1024 * 1024;

/// Called once per complete nonblank line. None means malformed/oversized.
/// Returns the offset after the last complete line, never after a partial tail.
pub(crate) fn scan_from(
    path: &Path,
    offset: u64,
    mut visit: impl FnMut(Option<Value>),
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
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let n = end.map(|i| i + 1).unwrap_or(chunk.len());
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
        if end.is_some() {
            complete = position;
            if nonblank {
                let row = if over {
                    None
                } else {
                    serde_json::from_slice::<Value>(&line)
                        .ok()
                        .filter(Value::is_object)
                };
                visit(row);
            }
            line.clear();
            over = false;
            nonblank = false;
        }
    }
    Ok(complete)
}

pub(crate) fn scan(path: &Path, mut visit: impl FnMut(Value)) -> io::Result<()> {
    scan_from(path, 0, |r| {
        if let Some(r) = r {
            visit(r)
        }
    })?;
    Ok(())
}

#[derive(Default)]
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
