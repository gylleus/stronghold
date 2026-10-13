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

// ---------------------------------------------------------------------------------------------
// Text-to-speech: Kokoro v1.0 (sherpa-onnx `tts-models` release), downloaded on first use
// ---------------------------------------------------------------------------------------------

/// The Kokoro TTS model: where it comes from, where it lives, and fetching it on first use.
pub mod kokoro {
    use std::fs;
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use sha2::{Digest, Sha256};

    /// Release archive of the model, from sherpa-onnx's `tts-models` release.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ModelRelease {
        /// Directory the archive unpacks into (its top-level folder).
        pub name: &'static str,
        pub url: &'static str,
        pub size: u64,
        pub sha256: &'static str,
        /// Model file inside the directory.
        pub model_file: &'static str,
    }

    /// Kokoro v1.0 (82M params, 54 voices incl. `af_heart`), fp32 weights.
    pub const KOKORO_V1_0: ModelRelease = ModelRelease {
        name: "kokoro-multi-lang-v1_0",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2",
        size: 349_906_910,
        sha256: "c5f7e2d2caf082bc1d20fb70334a61d99d20b484500aad32e7cf84c128ea3298",
        model_file: "model.onnx",
    };

    /// Kokoro v1.0 with int8-quantized weights: a third of the size, slightly lower quality.
    pub const KOKORO_INT8_V1_0: ModelRelease = ModelRelease {
        name: "kokoro-int8-multi-lang-v1_0",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-multi-lang-v1_0.tar.bz2",
        size: 132_303_094,
        sha256: "4c3052abaa60943a341f193888cf6abd68787dae6ab8ae5c925a706caa247e4e",
        model_file: "model.int8.onnx",
    };

    /// The model the Speaker uses.
    pub const DEFAULT: ModelRelease = KOKORO_V1_0;

    /// Written into the model directory once it is fully unpacked.
    const COMPLETE_MARKER: &str = ".complete";

    /// Files the Speaker needs from an unpacked model directory.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct KokoroPaths {
        pub dir: PathBuf,
        pub model: PathBuf,
        pub voices: PathBuf,
        pub tokens: PathBuf,
        pub lexicon: PathBuf,
        pub espeak_data: PathBuf,
        /// Jieba dictionary; multi-lingual Kokoro refuses to load without it.
        pub dict: PathBuf,
    }

    impl KokoroPaths {
        pub fn in_dir(dir: &Path, release: &ModelRelease) -> Self {
            KokoroPaths {
                dir: dir.to_path_buf(),
                model: dir.join(release.model_file),
                voices: dir.join("voices.bin"),
                tokens: dir.join("tokens.txt"),
                lexicon: dir.join("lexicon-us-en.txt"),
                espeak_data: dir.join("espeak-ng-data"),
                dict: dir.join("dict"),
            }
        }

        fn all_present(&self) -> bool {
            [&self.model, &self.voices, &self.tokens, &self.lexicon]
                .iter()
                .all(|p| p.is_file())
                && self.espeak_data.is_dir()
                && self.dict.is_dir()
        }

        /// Whether every file the model needs is present.
        pub fn is_complete(&self) -> bool {
            self.all_present()
        }
    }

    /// Download progress, reported while fetching the archive.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Progress {
        Downloading { done: u64, total: u64 },
        Verifying,
        Unpacking,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum ModelError {
        #[error("download of {url} failed: {message}")]
        Download { url: String, message: String },
        #[error("downloaded archive is {actual} bytes, expected {expected}")]
        Size { expected: u64, actual: u64 },
        #[error("downloaded archive sha256 {actual} does not match {expected}")]
        Checksum { expected: String, actual: String },
        #[error("unpacked model in {0} is missing files")]
        Incomplete(PathBuf),
        #[error("{context}: {source}")]
        Io {
            context: String,
            #[source]
            source: io::Error,
        },
    }

    fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> ModelError {
        let context = context.into();
        move |source| ModelError::Io { context, source }
    }

    pub use super::speech_models_dir;

    /// Where `release` lives under `root`, whether or not it has been downloaded.
    pub fn paths_in(root: &Path, release: &ModelRelease) -> KokoroPaths {
        KokoroPaths::in_dir(&root.join(release.name), release)
    }

    /// The unpacked model under `root`, if it is complete.
    pub fn installed(root: &Path, release: &ModelRelease) -> Option<KokoroPaths> {
        let paths = paths_in(root, release);
        (paths.dir.join(COMPLETE_MARKER).is_file() && paths.all_present()).then_some(paths)
    }

    /// Returns the installed model under `root`, downloading, verifying (size and sha256)
    /// and unpacking it first if needed. Blocking; run it off the UI thread.
    pub fn ensure(
        root: &Path,
        release: &ModelRelease,
        mut progress: impl FnMut(Progress),
    ) -> Result<KokoroPaths, ModelError> {
        if let Some(paths) = installed(root, release) {
            return Ok(paths);
        }
        fs::create_dir_all(root).map_err(io_err(format!("create {}", root.display())))?;
        let archive = root.join(format!("{}.tar.bz2.part", release.name));
        let result = download_and_unpack(root, release, &archive, &mut progress);
        let _ = fs::remove_file(&archive);
        result
    }

    fn download_and_unpack(
        root: &Path,
        release: &ModelRelease,
        archive: &Path,
        progress: &mut impl FnMut(Progress),
    ) -> Result<KokoroPaths, ModelError> {
        let digest = download(release, archive, progress)?;
        install_archive(root, release, archive, &digest, progress)
    }

    /// Verifies a downloaded archive (with sha256 `digest`) and unpacks it into place.
    fn install_archive(
        root: &Path,
        release: &ModelRelease,
        archive: &Path,
        digest: &str,
        progress: &mut impl FnMut(Progress),
    ) -> Result<KokoroPaths, ModelError> {
        progress(Progress::Verifying);
        verify(release, archive, digest)?;
        progress(Progress::Unpacking);
        let staging = root.join(format!(".{}.unpacking", release.name));
        let _ = fs::remove_dir_all(&staging);
        unpack(archive, &staging)?;
        let final_dir = root.join(release.name);
        let _ = fs::remove_dir_all(&final_dir);
        fs::rename(staging.join(release.name), &final_dir)
            .map_err(io_err(format!("move model into {}", final_dir.display())))?;
        let _ = fs::remove_dir_all(&staging);
        let paths = KokoroPaths::in_dir(&final_dir, release);
        if !paths.all_present() {
            return Err(ModelError::Incomplete(final_dir));
        }
        fs::write(final_dir.join(COMPLETE_MARKER), release.sha256)
            .map_err(io_err("write completion marker"))?;
        Ok(paths)
    }

    /// Streams the archive to `dest`, returning its sha256 (hex).
    fn download(
        release: &ModelRelease,
        dest: &Path,
        progress: &mut impl FnMut(Progress),
    ) -> Result<String, ModelError> {
        let fail = |message: String| ModelError::Download {
            url: release.url.to_owned(),
            message,
        };
        // curl (built into macOS) streams to us so the archive is hashed as it is written.
        let mut curl = Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error"])
            .arg(release.url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| fail(format!("run curl: {e}")))?;
        let mut stdout = curl.stdout.take().expect("piped stdout");
        let mut file =
            fs::File::create(dest).map_err(io_err(format!("create {}", dest.display())))?;
        let copied = copy_hashing(&mut stdout, &mut file, release.size, progress);
        let exit = curl.wait().map_err(|e| fail(e.to_string()))?;
        if !exit.success() {
            let mut err = String::new();
            if let Some(mut stderr) = curl.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            return Err(fail(err.trim().to_owned()));
        }
        copied.map_err(|e| fail(e.to_string()))
    }

    /// Copies `reader` to `writer`, reporting progress, and returns the hex sha256 of the bytes.
    fn copy_hashing(
        reader: &mut impl Read,
        writer: &mut impl Write,
        total: u64,
        progress: &mut impl FnMut(Progress),
    ) -> io::Result<String> {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        let mut last_report = u64::MAX;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            done += n as u64;
            // Report about every megabyte, not every read.
            if last_report == u64::MAX || done - last_report >= 1 << 20 {
                last_report = done;
                progress(Progress::Downloading { done, total });
            }
        }
        writer.flush()?;
        progress(Progress::Downloading { done, total });
        Ok(hex(&hasher.finalize()))
    }

    fn verify(release: &ModelRelease, archive: &Path, sha256: &str) -> Result<(), ModelError> {
        let actual = fs::metadata(archive)
            .map_err(io_err(format!("stat {}", archive.display())))?
            .len();
        if actual != release.size {
            return Err(ModelError::Size {
                expected: release.size,
                actual,
            });
        }
        if !sha256.eq_ignore_ascii_case(release.sha256) {
            return Err(ModelError::Checksum {
                expected: release.sha256.to_owned(),
                actual: sha256.to_owned(),
            });
        }
        Ok(())
    }

    fn unpack(archive: &Path, into: &Path) -> Result<(), ModelError> {
        fs::create_dir_all(into).map_err(io_err(format!("create {}", into.display())))?;
        let out = Command::new("tar")
            .arg("-xjf")
            .arg(archive)
            .arg("-C")
            .arg(into)
            .output()
            .map_err(io_err("run tar"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(ModelError::Io {
                context: format!("unpack {}", archive.display()),
                source: io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
            })
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Why the app's Speaker has no model: it never downloads without Karl's go-ahead.
    pub const NOT_INSTALLED: &str = "The King's voice is not downloaded";

    /// Size of [`DEFAULT`]'s archive, for the download button.
    pub const SIZE_LABEL: &str = "333 MB";

    /// A download of the model started from a button, running [`ensure`] on its own thread.
    pub struct Fetch {
        status: std::sync::Arc<std::sync::Mutex<super::DownloadStatus>>,
    }

    impl Fetch {
        /// Starts downloading `release` into `root`; `repaint` is called on every change.
        pub fn start(
            root: PathBuf,
            release: ModelRelease,
            repaint: Option<Box<dyn Fn() + Send + Sync>>,
        ) -> Self {
            use super::DownloadStatus as S;
            let status = std::sync::Arc::new(std::sync::Mutex::new(S::Downloading {
                done: 0,
                total: Some(release.size),
            }));
            let shared = status.clone();
            std::thread::Builder::new()
                .name("kokoro-download".into())
                .spawn(move || {
                    let set = |s: S| {
                        *shared.lock().unwrap_or_else(|e| e.into_inner()) = s;
                        if let Some(repaint) = &repaint {
                            repaint();
                        }
                    };
                    let mut last_mb = u64::MAX;
                    let result = ensure(&root, &release, |p| match p {
                        Progress::Downloading { done, total } => {
                            // Repaint once per MB, not per chunk.
                            if done >> 20 != last_mb {
                                last_mb = done >> 20;
                                set(S::Downloading {
                                    done,
                                    total: Some(total),
                                });
                            }
                        }
                        Progress::Verifying | Progress::Unpacking => set(S::Extracting),
                    });
                    match result {
                        Ok(_) => set(S::Done),
                        Err(e) => {
                            log::warn!("kokoro download failed: {e}");
                            set(S::Failed(e.to_string()));
                        }
                    }
                })
                .expect("spawn the kokoro download thread");
            Self { status }
        }

        pub fn status(&self) -> super::DownloadStatus {
            self.status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        /// Fraction done while downloading.
        pub fn progress(&self) -> Option<f32> {
            match self.status() {
                super::DownloadStatus::Downloading {
                    done,
                    total: Some(total),
                } if total > 0 => Some((done as f64 / total as f64) as f32),
                super::DownloadStatus::Extracting | super::DownloadStatus::Done => Some(1.0),
                _ => None,
            }
        }

        pub fn is_running(&self) -> bool {
            matches!(
                self.status(),
                super::DownloadStatus::Downloading { .. } | super::DownloadStatus::Extracting
            )
        }
    }

    /// Speaker names from the model's ONNX metadata (`speaker_names`), in speaker-id order.
    /// The metadata sits at the end of the file, so only the tail is read.
    pub fn read_speaker_names(model: &Path) -> io::Result<Vec<String>> {
        use std::io::{Seek, SeekFrom};
        const TAIL: u64 = 256 * 1024;
        let mut file = fs::File::open(model)?;
        let len = file.metadata()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL)))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        metadata_value(&tail, "speaker_names")
            .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
            .ok_or_else(|| io::Error::other("no speaker_names in model metadata"))
    }

    /// Finds a `StringStringEntryProto{key, value}` with `key` in serialized ONNX bytes:
    /// field 1 (key) is followed directly by field 2 (value), both length-delimited.
    fn metadata_value(bytes: &[u8], key: &str) -> Option<String> {
        let mut needle = vec![0x0a, key.len() as u8];
        needle.extend_from_slice(key.as_bytes());
        let start = bytes.windows(needle.len()).rposition(|w| w == needle)? + needle.len();
        let rest = bytes.get(start..)?;
        if *rest.first()? != 0x12 {
            return None;
        }
        let (len, used) = varint(&rest[1..])?;
        let value = rest.get(1 + used..1 + used + len as usize)?;
        String::from_utf8(value.to_vec()).ok()
    }

    fn varint(bytes: &[u8]) -> Option<(u64, usize)> {
        let mut value = 0u64;
        for (i, b) in bytes.iter().enumerate().take(10) {
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some((value, i + 1));
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn copy_hashing_hashes_and_reports_progress() {
            let data = vec![7u8; 3 << 20];
            let mut out = Vec::new();
            let mut reports = Vec::new();
            let digest = copy_hashing(&mut &data[..], &mut out, data.len() as u64, &mut |p| {
                reports.push(p)
            })
            .unwrap();
            assert_eq!(out, data);
            assert_eq!(digest, hex(&Sha256::digest(&data)));
            assert_eq!(
                reports.last(),
                Some(&Progress::Downloading {
                    done: data.len() as u64,
                    total: data.len() as u64
                })
            );
            assert!(reports.len() >= 3);
        }

        #[test]
        fn verify_rejects_wrong_size_and_hash() {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("a.tar.bz2");
            fs::write(&archive, b"hello").unwrap();
            let sha = hex(&Sha256::digest(b"hello"));
            let release = |size, sha256| ModelRelease {
                size,
                sha256,
                ..DEFAULT
            };
            let leaked: &'static str = Box::leak(sha.clone().into_boxed_str());
            assert!(verify(&release(5, leaked), &archive, &sha).is_ok());
            assert!(matches!(
                verify(&release(6, leaked), &archive, &sha),
                Err(ModelError::Size { .. })
            ));
            assert!(matches!(
                verify(&release(5, DEFAULT.sha256), &archive, &sha),
                Err(ModelError::Checksum { .. })
            ));
        }

        #[test]
        fn installs_a_verified_archive() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            // Build a tiny archive laid out like the release.
            let src = root.join("src");
            let model = src.join(DEFAULT.name);
            for sub in ["espeak-ng-data", "dict"] {
                fs::create_dir_all(model.join(sub)).unwrap();
            }
            for file in [
                "model.onnx",
                "voices.bin",
                "tokens.txt",
                "lexicon-us-en.txt",
            ] {
                fs::write(model.join(file), b"x").unwrap();
            }
            let archive = root.join("model.tar.bz2");
            let status = Command::new("tar")
                .arg("-cjf")
                .arg(&archive)
                .arg("-C")
                .arg(&src)
                .arg(DEFAULT.name)
                .status()
                .unwrap();
            assert!(status.success());
            let bytes = fs::read(&archive).unwrap();
            let release = ModelRelease {
                size: bytes.len() as u64,
                sha256: Box::leak(hex(&Sha256::digest(&bytes)).into_boxed_str()),
                ..DEFAULT
            };
            assert_eq!(installed(root, &release), None);
            let mut steps = Vec::new();
            let paths = install_archive(root, &release, &archive, release.sha256, &mut |p| {
                steps.push(p)
            })
            .unwrap();
            assert_eq!(steps, [Progress::Verifying, Progress::Unpacking]);
            assert_eq!(installed(root, &release), Some(paths));
            assert!(!root.join(format!(".{}.unpacking", release.name)).exists());
        }

        #[test]
        fn installed_requires_the_marker() {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths_in(dir.path(), &DEFAULT);
            fs::create_dir_all(&paths.espeak_data).unwrap();
            fs::create_dir_all(&paths.dict).unwrap();
            for f in [&paths.model, &paths.voices, &paths.tokens, &paths.lexicon] {
                fs::write(f, b"x").unwrap();
            }
            assert_eq!(installed(dir.path(), &DEFAULT), None);
            fs::write(paths.dir.join(COMPLETE_MARKER), b"").unwrap();
            assert_eq!(installed(dir.path(), &DEFAULT), Some(paths));
        }

        #[test]
        fn reads_speaker_names_from_onnx_metadata() {
            // key/value entry as serialized inside ModelProto.metadata_props.
            let mut bytes = vec![0u8; 100];
            let value = "af_alloy,af_heart,bm_george";
            bytes.extend([0x72, 0]); // metadata_props tag + (unused) length
            bytes.extend([0x0a, 13]);
            bytes.extend(b"speaker_names");
            bytes.extend([0x12, value.len() as u8]);
            bytes.extend(value.as_bytes());
            let dir = tempfile::tempdir().unwrap();
            let model = dir.path().join("model.onnx");
            fs::write(&model, &bytes).unwrap();
            assert_eq!(
                read_speaker_names(&model).unwrap(),
                ["af_alloy", "af_heart", "bm_george"]
            );
        }

        #[test]
        fn varint_decodes_multi_byte_lengths() {
            assert_eq!(varint(&[0xac, 0x02]), Some((300, 2)));
            assert_eq!(varint(&[0x05]), Some((5, 1)));
        }
    }
}
