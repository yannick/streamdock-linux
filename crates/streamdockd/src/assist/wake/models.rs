//! The wake word models (H15-68): the built-in ones and uploads, stored as `<id>.tflite` plus a
//! microWakeWord manifest `<id>.json` in `wakewords/` next to the config file. An upload is
//! checked by the same parser that runs it (op set, input/output, size) and by its manifest.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// At most this many uploaded models.
const MAX_UPLOADS: usize = 20;

/// A model the UI can offer.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub wake_word: String,
    pub languages: Vec<String>,
    /// `probability_cutoff`: the default threshold.
    pub cutoff: f32,
    /// `sliding_window_size`.
    pub window: usize,
    pub builtin: bool,
    pub size: usize,
    pub author: String,
}

/// Why a model can't be stored, found or loaded.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0}")]
    Invalid(String),
    #[error("no wake word model {0:?}")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
}

/// The models: built in and in `dir`.
#[derive(Debug, Clone)]
pub struct Models {
    dir: PathBuf,
}

impl Models {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `wakewords/` next to `config`.
    #[must_use]
    pub fn beside(config: &Path) -> Self {
        Self::new(
            config
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("wakewords"),
        )
    }

    /// The built-in models, then the uploaded ones by id. Unreadable uploads are skipped.
    #[must_use]
    pub fn list(&self) -> Vec<ModelInfo> {
        let mut list: Vec<ModelInfo> = wakeword::builtin::MODELS.iter().map(builtin_info).collect();
        let mut uploads: Vec<ModelInfo> = self
            .uploaded_ids()
            .into_iter()
            .filter_map(|id| self.uploaded(&id).ok().map(|(info, _)| info))
            .collect();
        uploads.sort_by(|a, b| a.id.cmp(&b.id));
        list.extend(uploads);
        list
    }

    /// The model `id` with its bytes.
    pub fn load(&self, id: &str) -> Result<(ModelInfo, Vec<u8>), ModelError> {
        if let Some(b) = wakeword::builtin::find(id) {
            return Ok((builtin_info(b), b.tflite.to_vec()));
        }
        check_id(id)?;
        self.uploaded(id)
    }

    /// Checks and stores an upload; its id comes from the manifest's model file name or wake
    /// word. Replaces an earlier upload with the same id.
    pub fn save(&self, tflite: &[u8], manifest: &[u8]) -> Result<ModelInfo, ModelError> {
        wakeword::Model::new(tflite).map_err(|e| ModelError::Invalid(e.to_string()))?;
        let mut info = parse_manifest(manifest)?;
        info.size = tflite.len();
        if wakeword::builtin::find(&info.id).is_some() {
            return Err(ModelError::Invalid(format!(
                "{:?} is a built-in wake word; rename the model",
                info.id
            )));
        }
        let ids = self.uploaded_ids();
        if !ids.contains(&info.id) && ids.len() >= MAX_UPLOADS {
            return Err(ModelError::Invalid(format!(
                "at most {MAX_UPLOADS} uploaded wake words"
            )));
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| io_error(&self.dir, &e))?;
        write(&self.dir.join(format!("{}.tflite", info.id)), tflite)?;
        write(
            &self.dir.join(format!("{}.json", info.id)),
            manifest_json(&info).as_bytes(),
        )?;
        Ok(info)
    }

    /// Removes an uploaded model.
    pub fn delete(&self, id: &str) -> Result<(), ModelError> {
        if wakeword::builtin::find(id).is_some() {
            return Err(ModelError::Invalid("built-in wake words can't be deleted".into()));
        }
        check_id(id)?;
        let tflite = self.dir.join(format!("{id}.tflite"));
        if !tflite.exists() {
            return Err(ModelError::NotFound(id.to_owned()));
        }
        std::fs::remove_file(&tflite).map_err(|e| io_error(&tflite, &e))?;
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.json")));
        Ok(())
    }

    fn uploaded_ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".tflite").map(str::to_owned))
            .filter(|id| check_id(id).is_ok())
            .collect()
    }

    fn uploaded(&self, id: &str) -> Result<(ModelInfo, Vec<u8>), ModelError> {
        let tflite_path = self.dir.join(format!("{id}.tflite"));
        let tflite = match std::fs::read(&tflite_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ModelError::NotFound(id.to_owned()));
            }
            Err(e) => return Err(io_error(&tflite_path, &e)),
        };
        let manifest_path = self.dir.join(format!("{id}.json"));
        let manifest = std::fs::read(&manifest_path).map_err(|e| io_error(&manifest_path, &e))?;
        let mut info = parse_manifest(&manifest)?;
        id.clone_into(&mut info.id);
        info.size = tflite.len();
        Ok((info, tflite))
    }
}

fn builtin_info(b: &wakeword::builtin::Builtin) -> ModelInfo {
    ModelInfo {
        id: b.id.to_owned(),
        wake_word: b.wake_word.to_owned(),
        languages: b.languages.iter().map(|&l| l.to_owned()).collect(),
        cutoff: b.cutoff,
        window: b.window,
        builtin: true,
        size: b.tflite.len(),
        author: "Kevin Ahrendt (ESPHome)".to_owned(),
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> ModelError {
    ModelError::Io(format!("{}: {e}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), ModelError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| io_error(&tmp, &e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_error(path, &e))
}

/// Ids are file names: lowercase letters, digits and `_`, 1–40 characters.
fn check_id(id: &str) -> Result<(), ModelError> {
    let ok = (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(ModelError::Invalid(format!("bad model id {id:?}")))
    }
}

/// An id from a name: lowercase, other characters as `_`.
fn slug(name: &str) -> String {
    let mut id: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    id.truncate(40);
    id.trim_matches('_').to_owned()
}

/// Reads a microWakeWord manifest (`"type": "micro"`, version 2).
fn parse_manifest(bytes: &[u8]) -> Result<ModelInfo, ModelError> {
    let bad = |m: &str| ModelError::Invalid(format!("manifest: {m}"));
    let v: Value = serde_json::from_slice(bytes).map_err(|e| bad(&format!("not JSON ({e})")))?;
    if v.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "micro")
    {
        return Err(bad("not a microWakeWord model (type must be \"micro\")"));
    }
    let wake_word = v
        .get("wake_word")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|w| (1..=64).contains(&w.chars().count()))
        .ok_or_else(|| bad("wake_word must be 1-64 characters"))?;
    let languages: Vec<String> = v
        .get("trained_languages")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    if languages.len() > 20 || languages.iter().any(|l| !(2..=8).contains(&l.len())) {
        return Err(bad("trained_languages must be up to 20 language codes"));
    }
    let micro = v.get("micro").ok_or_else(|| bad("no \"micro\" settings"))?;
    #[allow(clippy::cast_possible_truncation, reason = "a 0-1 cutoff")]
    let cutoff = micro
        .get("probability_cutoff")
        .and_then(Value::as_f64)
        .filter(|c| (0.0..1.0).contains(c))
        .ok_or_else(|| bad("micro.probability_cutoff must be 0-1"))? as f32;
    let window = micro
        .get("sliding_window_size")
        .and_then(Value::as_u64)
        .and_then(|w| usize::try_from(w).ok())
        .filter(|w| (1..=100).contains(w))
        .ok_or_else(|| bad("micro.sliding_window_size must be 1-100"))?;
    if micro
        .get("feature_step_size")
        .and_then(Value::as_u64)
        .is_some_and(|s| s != 10)
    {
        return Err(bad("micro.feature_step_size must be 10 (ms)"));
    }
    let from_file = v
        .get("model")
        .and_then(Value::as_str)
        .and_then(|m| m.strip_suffix(".tflite"))
        .map(slug);
    let id = from_file
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| slug(wake_word));
    check_id(&id)?;
    Ok(ModelInfo {
        id,
        wake_word: wake_word.to_owned(),
        languages,
        cutoff,
        window,
        builtin: false,
        size: 0,
        author: v
            .get("author")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect(),
    })
}

/// The manifest we store: the fields we use, in microWakeWord's format.
fn manifest_json(info: &ModelInfo) -> String {
    serde_json::json!({
        "type": "micro",
        "wake_word": info.wake_word,
        "author": info.author,
        "model": format!("{}.tflite", info.id),
        "trained_languages": info.languages,
        "version": 2,
        "micro": {
            "probability_cutoff": info.cutoff,
            "sliding_window_size": info.window,
            "feature_step_size": 10,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("streamdockd-wakewords-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const MANIFEST: &str = r#"{"type": "micro", "wake_word": "Hey Dock", "author": "me", "model": "hey_dock.tflite",
        "trained_languages": ["en", "de"], "version": 2,
        "micro": {"probability_cutoff": 0.9, "feature_step_size": 10, "sliding_window_size": 5}}"#;

    #[test]
    fn built_in_models_are_listed_and_loadable() {
        let m = Models::new(dir("builtin"));
        let ids: Vec<String> = m.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["okay_nabu", "hey_jarvis", "alexa", "hey_mycroft"]);
        let (info, bytes) = m.load("hey_jarvis").unwrap();
        assert_eq!((info.cutoff, info.window, info.builtin), (0.97, 5, true));
        assert!(wakeword::Model::new(&bytes).is_ok());
        assert!(matches!(m.load("nope"), Err(ModelError::NotFound(_))));
        assert!(matches!(m.load("../etc"), Err(ModelError::Invalid(_))));
    }

    #[test]
    fn uploads_are_checked_stored_listed_and_deleted() {
        let m = Models::new(dir("upload"));
        let tflite = wakeword::builtin::find("alexa").unwrap().tflite;
        let info = m.save(tflite, MANIFEST.as_bytes()).unwrap();
        assert_eq!(
            (info.id.as_str(), info.wake_word.as_str(), info.cutoff),
            ("hey_dock", "Hey Dock", 0.9)
        );
        assert_eq!(info.languages, ["en", "de"]);
        assert!(m.list().iter().any(|i| i.id == "hey_dock" && !i.builtin));
        let (loaded, bytes) = m.load("hey_dock").unwrap();
        assert_eq!(loaded.wake_word, "Hey Dock");
        assert_eq!(bytes, tflite);
        m.delete("hey_dock").unwrap();
        assert!(matches!(m.load("hey_dock"), Err(ModelError::NotFound(_))));
        assert!(m.delete("okay_nabu").is_err(), "built-in");
    }

    #[test]
    fn bad_uploads_are_refused() {
        let m = Models::new(dir("bad"));
        let tflite = wakeword::builtin::find("alexa").unwrap().tflite;
        assert!(m.save(b"not a model", MANIFEST.as_bytes()).is_err());
        assert!(m.save(&tflite[..tflite.len() / 2], MANIFEST.as_bytes()).is_err());
        assert!(m.save(tflite, b"{").is_err());
        let wrong_step = MANIFEST.replace("\"feature_step_size\": 10", "\"feature_step_size\": 20");
        assert!(m.save(tflite, wrong_step.as_bytes()).is_err());
        let builtin_name = MANIFEST.replace("hey_dock.tflite", "alexa.tflite");
        assert!(m.save(tflite, builtin_name.as_bytes()).is_err());
        let no_cutoff = MANIFEST.replace("\"probability_cutoff\": 0.9,", "");
        assert!(m.save(tflite, no_cutoff.as_bytes()).is_err());
    }
}
