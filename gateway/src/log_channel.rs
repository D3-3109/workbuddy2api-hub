use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, LazyLock,
    },
    time::Duration,
};

const QUEUE_CAPACITY: usize = 2000;
const MAX_LINE_BYTES: usize = 8192;

enum Message {
    Line(String),
    Stop(SyncSender<()>),
}

#[derive(Default)]
struct Counters {
    dropped: AtomicU64,
    closed: AtomicBool,
    stopping: AtomicBool,
}

pub struct LogChannel {
    sender: Option<SyncSender<Message>>,
    counters: Arc<Counters>,
}

impl LogChannel {
    fn with_writer<W: Write + Send + 'static>(mut writer: W, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let counters = Arc::new(Counters::default());
        let status = counters.clone();
        let spawned = std::thread::Builder::new()
            .name("wb-stderr-writer".into())
            .spawn(move || {
                // Write everything already queued as one batch: one write and
                // one flush per burst instead of per line.
                let mut batch = Vec::with_capacity(16 * 1024);
                while let Ok(first) = receiver.recv() {
                    let mut stop = None;
                    let mut next = Some(first);
                    while let Some(message) = next.take() {
                        match message {
                            Message::Line(line) => batch.extend_from_slice(line.as_bytes()),
                            Message::Stop(done) => {
                                stop = Some(done);
                                break;
                            }
                        }
                        if batch.len() >= 64 * 1024 {
                            break;
                        }
                        next = receiver.try_recv().ok();
                    }
                    if !batch.is_empty()
                        && !status.closed.load(Ordering::Acquire)
                        && (writer.write_all(&batch).is_err() || writer.flush().is_err())
                    {
                        status.closed.store(true, Ordering::Release);
                    }
                    batch.clear();
                    if let Some(done) = stop {
                        let _ = done.send(());
                        return;
                    }
                }
            });
        if spawned.is_err() {
            counters.closed.store(true, Ordering::Release);
            Self {
                sender: None,
                counters,
            }
        } else {
            // The worker is intentionally detached: a blocked OS pipe must not
            // make shutdown wait for an uninterruptible stderr write.
            Self {
                sender: Some(sender),
                counters,
            }
        }
    }

    /// Returns the cumulative drop count on overflow, without waiting for I/O.
    pub fn emit(&self, level: &str, message: &str) -> Option<u64> {
        if self.counters.closed.load(Ordering::Acquire)
            || self.counters.stopping.load(Ordering::Acquire)
        {
            return None;
        }
        let mut line = format!("[{}] ", bounded(level, 16));
        line.push_str(bounded(message, MAX_LINE_BYTES - line.len() - 1));
        line.push('\n');
        match self
            .sender
            .as_ref()
            .map(|tx| tx.try_send(Message::Line(line)))
        {
            Some(Err(TrySendError::Full(_))) => {
                Some(self.counters.dropped.fetch_add(1, Ordering::Relaxed) + 1)
            }
            Some(Err(TrySendError::Disconnected(_))) | None => {
                self.counters.closed.store(true, Ordering::Release);
                None
            }
            Some(Ok(())) => None,
        }
    }

    pub fn dropped(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
    }
    pub fn closed(&self) -> bool {
        self.counters.closed.load(Ordering::Acquire)
    }

    /// A queued barrier waits for writes already accepted, not just an empty queue.
    /// Full/blocked channels are abandoned within the caller's fixed time budget.
    pub fn shutdown(&self, timeout: Duration) {
        if self.counters.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        let (done, received) = mpsc::sync_channel(1);
        if self
            .sender
            .as_ref()
            .is_some_and(|tx| tx.try_send(Message::Stop(done)).is_ok())
        {
            let _ = received.recv_timeout(timeout);
        }
    }
}

fn bounded(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

static STDERR: LazyLock<LogChannel> =
    LazyLock::new(|| LogChannel::with_writer(io::stderr(), QUEUE_CAPACITY));

pub fn emit(level: &str, message: &str) -> Option<u64> {
    STDERR.emit(level, message)
}
pub fn dropped() -> u64 {
    STDERR.dropped()
}
pub fn closed() -> bool {
    STDERR.closed()
}
pub fn shutdown(timeout: Duration) {
    if let Some(channel) = LazyLock::get(&STDERR) {
        channel.shutdown(timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};

    struct BlockedWriter {
        entered: Option<SyncSender<()>>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }
    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(tx) = self.entered.take() {
                let _ = tx.send(());
            }
            let (lock, signal) = &*self.release;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = signal.wait(open).unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_writer_does_not_block_producer_or_shutdown() {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, observed) = mpsc::sync_channel(1);
        let channel = LogChannel::with_writer(
            BlockedWriter {
                entered: Some(entered),
                release: release.clone(),
            },
            2,
        );
        channel.emit("INFO", "first");
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..10000 {
            channel.emit("WARN", "pipe not consumed");
        }
        channel.shutdown(Duration::from_millis(20));
        let elapsed = started.elapsed();
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        assert!(elapsed < Duration::from_secs(1));
        assert!(channel.dropped() >= 9998);
    }

    #[test]
    fn writes_happen_on_worker_and_barrier_waits_for_last_line() {
        #[derive(Clone)]
        struct Recorder(Arc<Mutex<Vec<(String, String)>>>);
        impl Write for Recorder {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().push((
                    std::thread::current().name().unwrap_or("").into(),
                    String::from_utf8_lossy(b).into_owned(),
                ));
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(vec![]));
        let channel = LogChannel::with_writer(Recorder(output.clone()), 4);
        channel.emit("INFO", "one");
        channel.emit("INFO", "two");
        channel.shutdown(Duration::from_secs(2));
        let rows = output.lock().unwrap();
        assert!(rows.iter().all(|(thread, _)| thread == "wb-stderr-writer"));
        let text: String = rows.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(text, "[INFO] one\n[INFO] two\n");
    }

    #[test]
    fn dead_stderr_does_not_panic_or_repeat_writes() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let channel = LogChannel::with_writer(Broken, 4);
        channel.emit("ERROR", "closed pipe");
        channel.shutdown(Duration::from_secs(2));
        assert!(channel.closed());
        assert_eq!(channel.emit("INFO", "ignored"), None);
    }

    #[test]
    fn queued_line_bytes_are_bounded_at_utf8_boundary() {
        let value = "界".repeat(5000);
        let prefix = bounded(&value, 100);
        assert_eq!(prefix.len(), 99);
        assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
    }
}
