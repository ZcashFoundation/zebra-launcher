//! A size-rotated copy of zebrad's output on disk, for support after a crash.
//!
//! Writes happen on a dedicated thread. The supervisor never blocks on disk:
//! when the channel to the writer is full, lines are dropped and counted.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

use launcher_types::LogStream;

/// Where and how large the log file may grow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotatingLogConfig {
    /// The current file; rotated files get a `.1`, `.2`, ... suffix.
    pub path: PathBuf,
    /// Rotate when the current file would exceed this size.
    pub max_bytes: u64,
    /// How many rotated files to keep.
    pub keep: usize,
}

impl RotatingLogConfig {
    /// 10 MB per file, three rotated files kept.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_bytes: 10 * 1024 * 1024,
            keep: 3,
        }
    }
}

/// Sends lines to the writer thread.
#[derive(Clone, Debug)]
pub struct LogFileWriter {
    tx: SyncSender<Vec<u8>>,
    dropped: Arc<AtomicU64>,
}

impl LogFileWriter {
    /// Opens (or creates) the log file and starts the writer thread.
    pub fn spawn(config: RotatingLogConfig) -> io::Result<Self> {
        let mut file = RotatingFile::open(config)?;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4096);
        thread::Builder::new()
            .name("zebrad-log-writer".into())
            .spawn(move || {
                // The loop ends when every sender is dropped.
                while let Ok(line) = rx.recv() {
                    if let Err(err) = file.write_line(&line) {
                        tracing::warn!(?err, "could not write to the zebrad log file");
                    }
                }
                let _ = file.flush();
            })?;
        Ok(Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Queues one line. stderr lines are marked, launcher notes bracketed.
    pub fn write_line(&self, stream: LogStream, line: &str) {
        let prefix = match stream {
            LogStream::Stdout => "",
            LogStream::Stderr => "[stderr] ",
            LogStream::Launcher => "[launcher] ",
        };
        let mut bytes = Vec::with_capacity(prefix.len() + line.len() + 1);
        bytes.extend_from_slice(prefix.as_bytes());
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        match self.tx.try_send(bytes) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// How many lines were dropped because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

struct RotatingFile {
    config: RotatingLogConfig,
    file: File,
    size: u64,
}

impl RotatingFile {
    fn open(config: RotatingLogConfig) -> io::Result<Self> {
        if let Some(parent) = config.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.path)?;
        let size = file.metadata()?.len();
        Ok(Self { config, file, size })
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let len = line.len() as u64;
        if self.size > 0
            && self.size + len > self.config.max_bytes
            && let Err(err) = self.rotate()
        {
            // Keep the on-disk log alive rather than freezing it: lose the
            // current file's content and try rotating again next time.
            tracing::warn!(
                ?err,
                "could not rotate the zebrad log file; truncating it instead"
            );
            self.truncate()?;
        }
        self.file.write_all(line)?;
        self.size += len;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let path = &self.config.path;
        let rotated = |n: usize| -> PathBuf {
            let mut name = path.as_os_str().to_owned();
            name.push(format!(".{n}"));
            PathBuf::from(name)
        };
        if self.config.keep == 0 {
            // Removing and reopening the open file fails on Windows without
            // POSIX delete semantics, so empty it in place instead.
            return self.truncate();
        } else {
            let _ = fs::remove_file(rotated(self.config.keep));
            for n in (1..self.config.keep).rev() {
                let from = rotated(n);
                if from.exists() {
                    fs::rename(&from, rotated(n + 1))?;
                }
            }
            fs::rename(path, rotated(1))?;
        }
        self.file = OpenOptions::new().create(true).append(true).open(path)?;
        self.size = 0;
        Ok(())
    }

    /// Empties the current file in place.
    fn truncate(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.size = 0;
        Ok(())
    }
}

/// Lists the current and rotated files that exist, newest first.
pub fn existing_files(config: &RotatingLogConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if config.path.exists() {
        files.push(config.path.clone());
    }
    for n in 1..=config.keep {
        let mut name = config.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        let path = Path::new(&name).to_path_buf();
        if path.exists() {
            files.push(path);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_the_size_limit_is_reached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = RotatingLogConfig {
            path: dir.path().join("zebrad.log"),
            max_bytes: 100,
            keep: 2,
        };
        let mut file = RotatingFile::open(config.clone()).expect("open");
        // Ten 30-byte lines: 300 bytes, so at least two rotations.
        for i in 0..10 {
            file.write_line(format!("line {i:02} {}\n", "x".repeat(21)).as_bytes())
                .expect("write");
        }
        file.flush().expect("flush");
        let files = existing_files(&config);
        assert_eq!(files.len(), 3, "{files:?}");
        for path in &files {
            let len = fs::metadata(path).expect("metadata").len();
            assert!(len <= 100, "{path:?} is {len} bytes");
        }
        // The newest lines are in the current file.
        let current = fs::read_to_string(&config.path).expect("read");
        assert!(current.contains("line 09"), "{current}");
    }

    #[test]
    fn a_failed_rotation_truncates_and_rotation_is_retried_later() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = RotatingLogConfig {
            path: dir.path().join("zebrad.log"),
            max_bytes: 100,
            keep: 1,
        };
        // Renaming the current file over a non-empty directory fails everywhere.
        let blocker = dir.path().join("zebrad.log.1");
        fs::create_dir(&blocker).expect("mkdir");
        fs::write(blocker.join("inner"), b"x").expect("write inner");

        let mut file = RotatingFile::open(config.clone()).expect("open");
        let line = |i: usize| format!("line {i:02} {}\n", "x".repeat(21));
        for i in 0..10 {
            file.write_line(line(i).as_bytes()).expect("write");
        }
        file.flush().expect("flush");
        let current = fs::read_to_string(&config.path).expect("read");
        assert!(current.len() <= 100, "{current}");
        assert!(current.contains("line 09"), "{current}");

        // Once the obstacle is gone, rotation works again.
        fs::remove_dir_all(&blocker).expect("rmdir");
        for i in 10..20 {
            file.write_line(line(i).as_bytes()).expect("write");
        }
        file.flush().expect("flush");
        assert!(blocker.is_file(), "rotation resumed");
        let current = fs::read_to_string(&config.path).expect("read");
        assert!(current.contains("line 19"), "{current}");
    }

    #[test]
    fn keep_zero_truncates_the_current_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = RotatingLogConfig {
            path: dir.path().join("zebrad.log"),
            max_bytes: 100,
            keep: 0,
        };
        let mut file = RotatingFile::open(config.clone()).expect("open");
        for i in 0..10 {
            file.write_line(format!("line {i:02} {}\n", "x".repeat(21)).as_bytes())
                .expect("write");
        }
        file.flush().expect("flush");
        assert_eq!(existing_files(&config), std::slice::from_ref(&config.path));
        let current = fs::read_to_string(&config.path).expect("read");
        assert!(current.len() <= 100, "{current}");
        assert!(current.starts_with("line 09"), "{current}");
    }

    #[test]
    fn writer_thread_persists_lines_and_marks_streams() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = RotatingLogConfig::new(dir.path().join("logs").join("zebrad.log"));
        let writer = LogFileWriter::spawn(config.clone()).expect("spawn");
        writer.write_line(LogStream::Stdout, "hello");
        writer.write_line(LogStream::Stderr, "oops");
        writer.write_line(LogStream::Launcher, "note");
        assert_eq!(writer.dropped(), 0);
        drop(writer);
        // Dropping the last sender ends the thread; poll until it has flushed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let content = fs::read_to_string(&config.path).unwrap_or_default();
            if content == "hello\n[stderr] oops\n[launcher] note\n" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "log file content: {content:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
