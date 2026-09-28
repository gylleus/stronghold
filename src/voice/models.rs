//! Locating and downloading speech models.

// ---------------------------------------------------------------------------
// Kokoro (text-to-speech)
// ---------------------------------------------------------------------------

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

    /// `<data dir>/speech-models`.
    pub fn speech_models_dir() -> PathBuf {
        crate::claude::hook::data_dir().join("speech-models")
    }

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
