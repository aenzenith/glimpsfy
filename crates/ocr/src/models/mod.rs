//! PP-OCR model files: the reviewed table of where they come from and what their bytes hash to,
//! and listing, downloading, verifying and removing them in the app's data folder.
//!
//! # The reviewed table
//!
//! [`MODELS`] is the only list of download URLs in the app. Every URL pins a commit of its
//! repository, so the bytes behind it cannot change, and every file carries its SHA-256 and its
//! size. A change to the table is reviewed like code: the new hash is taken from a download of the
//! file and compared with the hash the host publishes (Hugging Face shows the SHA-256 of every
//! large file and the git blob id of every small one), and the model's licence is checked (every
//! repository below is PaddlePaddle's own and Apache-2.0).
//!
//! # Rules this module keeps
//!
//! - Nothing is downloaded unless the user asks: [`ModelStore::download`] is the only function
//!   that fetches, and only the model download command (a button in Settings) calls it.
//! - Fetching, hashing into `<name>.part`, verifying before a load and reading verified bytes are
//!   `glimpsfy-models`', which holds the one connection of every model area and takes this table
//!   as [`reviewed`]: it accepts the URLs in the table and redirects to [`ALLOWED_HOSTS`] over
//!   HTTPS, and nothing else.
//! - An engine never hands a path to its runtime. It takes a file's bytes from
//!   [`VerifiedModel::bytes`], which checks the hash of exactly the bytes it returns, so no file
//!   can be swapped between the check and the load.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use glimpsfy_models::{ModelError, Reviewed};
use serde::Serialize;

use crate::{CancelToken, LanguageTag, OcrError, Result};

/// One file of a model, as the reviewed table names it.
pub use glimpsfy_models::ModelFile;

/// One downloadable model: the files PP-OCR needs to read one group of scripts. Every model has
/// the same three files: `det.onnx` (text detection), `rec.onnx` (text recognition) and `rec.yml`
/// (the recognition model's configuration, which holds its dictionary).
#[derive(Debug, PartialEq)]
pub struct ModelSpec {
    pub id: &'static str,
    /// The model's name in Settings; it is a product name and is not translated.
    pub title: &'static str,
    /// The languages the recognition model reads, as BCP 47 tags.
    pub languages: &'static [&'static str],
    /// How the detection model's answer becomes boxes.
    pub detector: Detector,
    pub files: &'static [ModelFile],
}

/// The post-processing values a detection model was published with (`PostProcess` in its
/// `inference.yml`). They belong to the model, so they are reviewed with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detector {
    /// A pixel is text when the model's probability for it is above this (`thresh`).
    pub threshold: f32,
    /// A region is a text box when its mean probability reaches this (`box_thresh`).
    pub box_threshold: f32,
    /// How far a region grows into a box, as a share of its area over its perimeter
    /// (`unclip_ratio`).
    pub unclip_ratio: f32,
}

/// The file names every model has; see [`ModelSpec`].
pub const DETECTION_FILE: &str = "det.onnx";
pub const RECOGNITION_FILE: &str = "rec.onnx";
pub const DICTIONARY_FILE: &str = "rec.yml";

/// The hosts a download may reach. `huggingface.co` serves the URLs in [`MODELS`] and answers with
/// a redirect to its content network, whose host names change by region and all end in `.hf.co`.
pub const ALLOWED_HOSTS: &[&str] = &["huggingface.co"];
/// Host name endings a redirect may lead to; see [`ALLOWED_HOSTS`].
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &[".hf.co"];

/// The reviewed table of models. Reviewed on 2026-11-22: every hash below was computed from a
/// download of the pinned URL and matched what Hugging Face publishes for the file (the SHA-256
/// of the `.onnx` files, the git blob id of the `.yml` files).
///
/// Every model pairs PP-OCRv6 small text detection, the tier PaddleOCR names for desktops, which
/// finds the lines of every script, with the recognition model of one group of scripts. The
/// first model is the one offered when no language asks for another. Each model lists English
/// too, because its dictionary holds the Latin alphabet and screens mix English into every
/// language.
pub const MODELS: &[ModelSpec] = &[
    // PP-OCRv5's Latin recognition reads Turkish (ç, ğ, ı, İ, ö, ş, ü) correctly where PP-OCRv6's
    // general model confuses ı, i, İ and Ğ; on the sample set it reads 99.76 % of the characters,
    // Vision 99.45 %. The languages are those of its model card, and every letter of each is in
    // its dictionary.
    ModelSpec {
        id: "ppocr-latin",
        title: "PP-OCRv5 Latin",
        languages: &[
            "en", "tr", "de", "fr", "es", "it", "pt", "nl", "pl", "cs", "sk", "hr", "bs", "sr-Latn", "sl", "da", "no",
            "sv", "is", "et", "lt", "hu", "sq", "cy", "ga", "id", "ms", "af", "sw", "tl", "uz", "oc", "mi", "la",
        ],
        detector: V6_SMALL_DETECTOR,
        files: &[
            V6_SMALL_DETECTION,
            // PP-OCRv5 mobile Latin recognition (latin_PP-OCRv5_mobile_rec).
            ModelFile {
                name: RECOGNITION_FILE,
                url: "https://huggingface.co/PaddlePaddle/latin_PP-OCRv5_mobile_rec_onnx/resolve/89d3a50e2c27e2e7cceeab0e944c25c807d5db4f/inference.onnx",
                sha256: "7888113072263cb471b93f66dd5e2ad70548dc526fa1ace760d0d973dd121498",
                bytes: 8_042_023,
            },
            // The recognition model's configuration; its `character_dict` is the dictionary.
            ModelFile {
                name: DICTIONARY_FILE,
                url: "https://huggingface.co/PaddlePaddle/latin_PP-OCRv5_mobile_rec_onnx/resolve/89d3a50e2c27e2e7cceeab0e944c25c807d5db4f/inference.yml",
                sha256: "0bbe984570f597af3638e50bdf2e8276f3ab26a61966096538b3b0d1849f5c84",
                bytes: 6_817,
            },
        ],
    },
    // PaddleOCR's current general model reads Chinese and Japanese with one dictionary of 18,708
    // characters.
    ModelSpec {
        id: "ppocr-v6",
        title: "PP-OCRv6",
        languages: &["ja", "zh-Hans", "zh-Hant", "en"],
        detector: V6_SMALL_DETECTOR,
        files: &[
            V6_SMALL_DETECTION,
            // PP-OCRv6 small recognition.
            ModelFile {
                name: RECOGNITION_FILE,
                url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/b8f84f0b80c529de40b4fbb3544b84fa7233a513/inference.onnx",
                sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
                bytes: 21_159_378,
            },
            // The recognition model's configuration; its `character_dict` is the dictionary.
            ModelFile {
                name: DICTIONARY_FILE,
                url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/b8f84f0b80c529de40b4fbb3544b84fa7233a513/inference.yml",
                sha256: "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1",
                bytes: 150_579,
            },
        ],
    },
    // PP-OCRv6 has no Cyrillic in its dictionary, so Russian keeps a PP-OCRv5 model. Its
    // dictionary holds the Russian, Ukrainian, Belarusian and Bulgarian alphabets.
    ModelSpec {
        id: "ppocr-cyrillic",
        title: "PP-OCRv5 Cyrillic",
        languages: &["ru", "uk", "be", "bg", "en"],
        detector: V6_SMALL_DETECTOR,
        files: &[
            V6_SMALL_DETECTION,
            // PP-OCRv5 mobile East Slavic recognition (eslav_PP-OCRv5_mobile_rec).
            ModelFile {
                name: RECOGNITION_FILE,
                url: "https://huggingface.co/PaddlePaddle/eslav_PP-OCRv5_mobile_rec_onnx/resolve/9a32171fc5718746875e1a261818884517975013/inference.onnx",
                sha256: "b3018ef2b09a0250b6e0c8e871c927098363e5fd4df890cc68e8358eb0aaf1bd",
                bytes: 7_887_627,
            },
            // The recognition model's configuration; its `character_dict` is the dictionary.
            ModelFile {
                name: DICTIONARY_FILE,
                url: "https://huggingface.co/PaddlePaddle/eslav_PP-OCRv5_mobile_rec_onnx/resolve/9a32171fc5718746875e1a261818884517975013/inference.yml",
                sha256: "025039bac23eb4a308efcefa4d58eab3af440767815c6ba6938468bf6353ee5a",
                bytes: 4_538,
            },
        ],
    },
];

/// PP-OCRv6 small text detection, which every model shares.
const V6_SMALL_DETECTION: ModelFile = ModelFile {
    name: DETECTION_FILE,
    url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/28fe5895c24fd108c19eb3e8479f4ab385fbfc62/inference.onnx",
    sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    bytes: 9_880_512,
};

/// The post-processing values PP-OCRv6 small detection was published with.
const V6_SMALL_DETECTOR: Detector = Detector { threshold: 0.2, box_threshold: 0.45, unclip_ratio: 1.4 };

/// The model in the table with this id.
pub fn spec(id: &str) -> Result<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id).ok_or_else(|| OcrError::ModelMissing(id.to_string()))
}

/// A model as Settings lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub title: String,
    pub languages: Vec<LanguageTag>,
    /// The size of the download in bytes.
    pub bytes: u64,
    /// Whether every file is on disk with the size the table names. The hashes are checked again
    /// when the engine loads the model.
    pub installed: bool,
}

/// How far a download is, reported while the files stream in.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub model: String,
    pub received: u64,
    pub total: u64,
}

/// The files of a model whose hashes were just checked, ready to load.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedModel {
    pub spec: &'static ModelSpec,
    pub dir: PathBuf,
}

impl VerifiedModel {
    /// The path of one of the model's files, by its name in the table.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The bytes of one of the model's files, for an engine to load. The hash is checked on the
    /// very bytes that are returned, so what the engine loads is what the table names even when
    /// the file was replaced after [`ModelStore::verified`] ran. A mismatch deletes the file.
    pub fn bytes(&self, name: &str) -> Result<Vec<u8>> {
        let file = self
            .spec
            .files
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| OcrError::ModelMissing(self.spec.id.to_string()))?;
        Ok(glimpsfy_models::read_verified(&self.dir, self.spec.id, file)?)
    }
}

/// The folder that holds the downloaded models, one subfolder per model id. The app passes
/// `<app data>/models`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelStore {
    root: PathBuf,
}

static SHARED_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Remembers the app's model folder for engines created without one ([`crate::engine`]). The
/// first call wins; the folder does not change while the app runs.
pub fn set_shared_root(root: impl Into<PathBuf>) {
    let _ = SHARED_ROOT.set(root.into());
}

/// The model folder [`set_shared_root`] remembered, if it was called.
pub fn shared_store() -> Option<ModelStore> {
    SHARED_ROOT.get().map(ModelStore::new)
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, spec: &ModelSpec) -> PathBuf {
        self.root.join(spec.id)
    }

    /// Every model in the table and whether it is downloaded. Reads file sizes only.
    pub fn list(&self) -> Vec<ModelInfo> {
        MODELS
            .iter()
            .map(|spec| {
                let installed = glimpsfy_models::installed(&self.dir(spec), spec.files);
                ModelInfo {
                    id: spec.id.to_string(),
                    title: spec.title.to_string(),
                    languages: spec.languages.iter().map(|l| LanguageTag::new(*l)).collect(),
                    bytes: spec.files.iter().map(|f| f.bytes).sum(),
                    installed,
                }
            })
            .collect()
    }

    /// Downloads the model's files from the reviewed table into its folder, reporting progress.
    /// Files already on disk with the right hash are kept. Blocks; the user starts it, `cancel`
    /// stops it, and `online` is asked before any connection (the app's offline switch).
    pub fn download(
        &self,
        id: &str,
        progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
        online: &dyn Fn() -> bool,
    ) -> Result<()> {
        let spec = spec(id)?;
        let files: Vec<&ModelFile> = MODELS.iter().flat_map(|m| m.files).collect();
        glimpsfy_models::download(
            &self.dir(spec),
            id,
            spec.files,
            &reviewed(&files),
            &mut |received, total| progress(Progress { model: id.to_string(), received, total }),
            &|| cancel.is_cancelled(),
            online,
        )?;
        Ok(())
    }

    /// Deletes a downloaded model. Removing a model that is not there succeeds.
    pub fn remove(&self, id: &str) -> Result<()> {
        Ok(glimpsfy_models::remove(&self.dir(spec(id)?))?)
    }

    /// Checks the hash of every file of the model before it is loaded. A missing file fails with
    /// [`OcrError::ModelMissing`]; a file whose bytes differ from the table is deleted and fails
    /// with [`OcrError::ChecksumMismatch`], so a damaged or replaced file is never loaded.
    pub fn verified(&self, id: &str) -> Result<VerifiedModel> {
        let spec = spec(id)?;
        let dir = self.dir(spec);
        glimpsfy_models::verify(&dir, id, spec.files)?;
        Ok(VerifiedModel { spec, dir })
    }
}

/// The table as the fetch checks it: every file of [`MODELS`] and the hosts they live on.
pub fn reviewed<'a>(files: &'a [&'a ModelFile]) -> Reviewed<'a> {
    Reviewed { files, hosts: ALLOWED_HOSTS, host_suffixes: ALLOWED_HOST_SUFFIXES }
}

impl From<ModelError> for OcrError {
    fn from(error: ModelError) -> Self {
        match error {
            ModelError::Download(reason) => OcrError::Download(reason),
            ModelError::Offline => OcrError::Offline,
            ModelError::Missing(model) => OcrError::ModelMissing(model),
            ModelError::ChecksumMismatch { model } => OcrError::ChecksumMismatch { model },
            ModelError::Cancelled => OcrError::Cancelled,
            ModelError::Io(error) => OcrError::Io(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_url_of_the_table_is_on_an_allowed_host() {
        let files: Vec<&ModelFile> = MODELS.iter().flat_map(|m| m.files).collect();
        for file in &files {
            assert!(glimpsfy_models::download::allowed(file.url, &reviewed(&files)).is_ok(), "{}", file.url);
        }
    }
}
