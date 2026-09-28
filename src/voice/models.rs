//! Locating and downloading speech models.
//!
//! Models live in `<data dir>/speech-models/<name>/` (data dir: `$MYRSTACK_DATA_DIR`, else
//! `~/Library/Application Support/Myrstack`). Downloads run only when the user asks for one.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// Folder holding every speech model.
pub fn speech_models_dir() -> PathBuf {
    crate::claude::hook::data_dir().join("speech-models")
}

// ---------------------------------------------------------------------------------------------
// Downloads (shared by any model fetched as a `.tar.bz2` archive)
// ---------------------------------------------------------------------------------------------

/// Where a download stands, polled by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStatus {
    Downloading { done: u64, total: Option<u64> },
    Extracting,
    Done,
    Failed(String),
    Cancelled,
}

/// A model download running on its own thread (`curl` then `tar`, both built into macOS).
pub struct Download {
    status: Arc<Mutex<DownloadStatus>>,
    done_bytes: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    /// Downloads `url` (a `.tar.bz2`) and unpacks its single top folder as `dest`.
    /// `repaint` is called whenever the status changes.
    pub fn start(
        url: String,
        expected_size: Option<u64>,
        dest: PathBuf,
        repaint: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let status = Arc::new(Mutex::new(DownloadStatus::Downloading {
            done: 0,
            total: expected_size,
        }));
        let done_bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let job = DownloadJob {
            url,
            expected_size,
            dest,
            status: status.clone(),
            done_bytes: done_bytes.clone(),
            cancel: cancel.clone(),
            repaint,
        };
        thread::Builder::new()
            .name("model-download".into())
            .spawn(move || job.run())
            .expect("spawn the download thread");
        Self {
            status,
            done_bytes,
            cancel,
        }
    }

    pub fn status(&self) -> DownloadStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Fraction done, when the size is known.
    pub fn progress(&self) -> Option<f32> {
        match self.status() {
            DownloadStatus::Downloading {
                total: Some(total), ..
            } if total > 0 => {
                Some((self.done_bytes.load(Ordering::Relaxed) as f64 / total as f64) as f32)
            }
            DownloadStatus::Extracting | DownloadStatus::Done => Some(1.0),
            _ => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status(),
            DownloadStatus::Downloading { .. } | DownloadStatus::Extracting
        )
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct DownloadJob {
    url: String,
    expected_size: Option<u64>,
    dest: PathBuf,
    status: Arc<Mutex<DownloadStatus>>,
    done_bytes: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    repaint: Option<Box<dyn Fn() + Send + Sync>>,
}

impl DownloadJob {
    fn set(&self, status: DownloadStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
        if let Some(repaint) = &self.repaint {
            repaint();
        }
    }

    fn run(self) {
        let parent = self
            .dest
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let name = self.dest.file_name().unwrap_or_default().to_string_lossy();
        let work = parent.join(format!(".download-{name}"));
        let result = self.fetch_and_unpack(&work);
        let _ = std::fs::remove_dir_all(&work);
        match result {
            Ok(()) => self.set(DownloadStatus::Done),
            Err(_) if self.cancel.load(Ordering::Relaxed) => self.set(DownloadStatus::Cancelled),
            Err(err) => {
                log::warn!("model download from {} failed: {err}", self.url);
                self.set(DownloadStatus::Failed(err));
            }
        }
    }

    fn fetch_and_unpack(&self, work: &Path) -> Result<(), String> {
        let _ = std::fs::remove_dir_all(work);
        std::fs::create_dir_all(work).map_err(|e| format!("create {}: {e}", work.display()))?;
        let archive = work.join("model.tar.bz2");
        let mut curl = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(&archive)
            .arg(&self.url)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("run curl: {e}"))?;
        let exit = loop {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = curl.kill();
                let _ = curl.wait();
                return Err("cancelled".into());
            }
            if let Some(exit) = curl.try_wait().map_err(|e| e.to_string())? {
                break exit;
            }
            let done = std::fs::metadata(&archive).map_or(0, |m| m.len());
            self.done_bytes.store(done, Ordering::Relaxed);
            self.set(DownloadStatus::Downloading {
                done,
                total: self.expected_size,
            });
            thread::sleep(Duration::from_millis(250));
        };
        if !exit.success() {
            let mut err = String::new();
            if let Some(mut stderr) = curl.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            return Err(format!("download failed: {}", err.trim()));
        }

        self.set(DownloadStatus::Extracting);
        let unpacked = work.join("unpacked");
        std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
        let tar = Command::new("tar")
            .arg("-xjf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .output()
            .map_err(|e| format!("run tar: {e}"))?;
        if !tar.status.success() {
            return Err(format!(
                "unpacking failed: {}",
                String::from_utf8_lossy(&tar.stderr).trim()
            ));
        }
        let top = single_subdir(&unpacked)?;
        let _ = std::fs::remove_dir_all(&self.dest);
        std::fs::rename(&top, &self.dest)
            .map_err(|e| format!("move model to {}: {e}", self.dest.display()))
    }
}

/// The one folder an archive unpacked into.
fn single_subdir(dir: &Path) -> Result<PathBuf, String> {
    let dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    match dirs.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(format!(
            "unexpected archive layout ({} folders)",
            dirs.len()
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Speech-to-text: NVIDIA Parakeet TDT 0.6B v3 (int8), as used by Orca
// ---------------------------------------------------------------------------------------------

/// Folder name of the Parakeet model, both in Orca's and in Myrstack's model folder.
pub const PARAKEET_DIR: &str = "parakeet-tdt-0.6b-v3-int8";
/// Release asset of the same model on sherpa-onnx's GitHub.
pub const PARAKEET_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2";
/// Size of [`PARAKEET_URL`] in bytes.
pub const PARAKEET_ARCHIVE_BYTES: u64 = 487_170_055;
/// Size on disk once unpacked, for the download button.
pub const PARAKEET_SIZE_LABEL: &str = "640 MB";

/// The Parakeet transducer's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParakeetFiles {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

impl ParakeetFiles {
    /// The files in `dir`, if all four are there.
    pub fn in_dir(dir: &Path) -> Option<Self> {
        let files = Self {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
        };
        [&files.encoder, &files.decoder, &files.joiner, &files.tokens]
            .iter()
            .all(|p| p.is_file())
            .then_some(files)
    }
}

/// Orca's copy of the model, used read-only when present.
pub fn orca_parakeet_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| {
        d.data_dir()
            .join("orca")
            .join("speech-models")
            .join(PARAKEET_DIR)
    })
}

/// Where Myrstack downloads the model.
pub fn parakeet_dir() -> PathBuf {
    speech_models_dir().join(PARAKEET_DIR)
}

/// Myrstack's copy, else Orca's.
pub fn find_parakeet() -> Option<ParakeetFiles> {
    find_parakeet_in(&[Some(parakeet_dir()), orca_parakeet_dir()])
}

fn find_parakeet_in(dirs: &[Option<PathBuf>]) -> Option<ParakeetFiles> {
    dirs.iter().flatten().find_map(|d| ParakeetFiles::in_dir(d))
}

/// Starts downloading the Parakeet model into [`parakeet_dir`].
pub fn download_parakeet(repaint: Option<Box<dyn Fn() + Send + Sync>>) -> Download {
    Download::start(
        PARAKEET_URL.to_owned(),
        Some(PARAKEET_ARCHIVE_BYTES),
        parakeet_dir(),
        repaint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_model(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for f in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
    }

    #[test]
    fn finds_the_first_complete_model_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("ours");
        let orca = tmp.path().join("orca");
        assert_eq!(find_parakeet_in(&[Some(ours.clone()), None]), None);

        touch_model(&orca);
        let found = find_parakeet_in(&[Some(ours.clone()), Some(orca.clone())]).unwrap();
        assert_eq!(found.encoder, orca.join("encoder.int8.onnx"));

        touch_model(&ours);
        let found = find_parakeet_in(&[Some(ours.clone()), Some(orca)]).unwrap();
        assert_eq!(found.tokens, ours.join("tokens.txt"));

        std::fs::remove_file(ours.join("joiner.int8.onnx")).unwrap();
        assert_eq!(ParakeetFiles::in_dir(&ours), None);
    }

    #[test]
    fn archive_top_folder() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(single_subdir(tmp.path()).is_err());
        std::fs::create_dir(tmp.path().join("model")).unwrap();
        std::fs::write(tmp.path().join("README"), b"").unwrap();
        assert_eq!(single_subdir(tmp.path()).unwrap(), tmp.path().join("model"));
    }
}
