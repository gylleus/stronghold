//! The app's log file: `~/Library/Logs/Myrstack/myrstack.log`, rotated by size.
//!
//! An app launched from Finder has its stderr thrown away, so every log line also goes to this
//! file. [`init`] sets up `env_logger` to write each line to stderr and to a [`RotatingFile`].

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The log file grows to this size before it is rotated.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Files kept: the live log plus `myrstack.log.1` … `.{KEEP - 1}`.
pub const KEEP: usize = 3;
const FILE_NAME: &str = "myrstack.log";

/// Where the log goes: `$MYRSTACK_DATA_DIR/myrstack.log` for a test instance, else
/// `~/Library/Logs/Myrstack/myrstack.log`.
pub fn path() -> PathBuf {
    if let Some(dir) = std::env::var_os("MYRSTACK_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join(FILE_NAME);
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_default()
        .join("Library")
        .join("Logs")
        .join(crate::APP_NAME)
        .join(FILE_NAME)
}

/// Installs the logger: `RUST_LOG` (default `info`) to stderr and the log file. Without a
/// writable log file it logs to stderr only, and says so.
pub fn init() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    let path = path();
    match RotatingFile::open(&path, MAX_BYTES, KEEP) {
        Ok(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(Tee { file })));
            builder.init();
        }
        Err(e) => {
            builder.init();
            log::warn!("no log file at {}: {e}", path.display());
        }
    }
}

/// Opens the log in Console, or reveals it in Finder when Console won't open it.
pub fn open_in_console() {
    let path = path();
    let opened = std::process::Command::new("open")
        .args(["-a", "Console"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !opened
        && let Err(e) = std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
    {
        log::warn!("open log {}: {e}", path.display());
    }
}

/// Writes each log line to stderr and to the log file.
struct Tee {
    file: RotatingFile,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// An append-only file that is renamed to `<name>.1` once it would grow past `max_bytes`,
/// shifting older ones up to `<name>.{keep - 1}` and dropping the oldest.
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: File,
    size: u64,
}

impl RotatingFile {
    pub fn open(path: &Path, max_bytes: u64, keep: usize) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = append(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            max_bytes,
            keep: keep.max(1),
            file,
            size,
        })
    }

    /// The `n`th rotated file (`n` ≥ 1).
    pub fn rotated(path: &Path, n: usize) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if self.keep == 1 {
            let _ = fs::remove_file(&self.path);
        } else {
            let _ = fs::remove_file(Self::rotated(&self.path, self.keep - 1));
            for n in (1..self.keep - 1).rev() {
                let from = Self::rotated(&self.path, n);
                if from.exists() {
                    fs::rename(&from, Self::rotated(&self.path, n + 1))?;
                }
            }
            fs::rename(&self.path, Self::rotated(&self.path, 1))?;
        }
        self.file = append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn appends_until_full_then_rotates_keeping_the_newest_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("app.log");
        let mut file = RotatingFile::open(&path, 10, 3).unwrap();
        for line in [
            "aaaa\n", "bbbb\n", "cccc\n", "dddd\n", "eeee\n", "ffff\n", "gggg\n",
        ] {
            file.write_all(line.as_bytes()).unwrap();
        }
        file.flush().unwrap();
        assert_eq!(read(&path), "gggg\n");
        assert_eq!(read(&RotatingFile::rotated(&path, 1)), "eeee\nffff\n");
        assert_eq!(read(&RotatingFile::rotated(&path, 2)), "cccc\ndddd\n");
        assert!(!RotatingFile::rotated(&path, 3).exists());
    }

    #[test]
    fn reopening_counts_what_is_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "123456789\n").unwrap();
        let mut file = RotatingFile::open(&path, 12, 2).unwrap();
        file.write_all(b"next\n").unwrap();
        assert_eq!(read(&path), "next\n");
        assert_eq!(read(&RotatingFile::rotated(&path, 1)), "123456789\n");
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut file = RotatingFile::open(&path, 4, 1).unwrap();
        file.write_all(b"a long line\n").unwrap();
        file.write_all(b"another\n").unwrap();
        assert_eq!(read(&path), "another\n");
        assert!(!RotatingFile::rotated(&path, 1).exists());
    }
}
