use crate::{fs::Dir, valid_id, valid_video};
use parking_lot::RwLock;
use scribe_common::{Error, Result, now};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub const REMOVED_MESSAGE: &str =
    "Removed from the save folder. This video will not be transcribed or saved again.";
pub struct RemovalRegistry {
    root: PathBuf,
    state: RwLock<Value>,
}
impl RemovalRegistry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(path.as_ref())?;
        let root = std::fs::canonicalize(path)?;
        let dir = Dir::open(&root)?;
        let state = match root.join("removed-videos.json").symlink_metadata() {
            Ok(_) => {
                serde_json::from_slice(&dir.read("removed-videos.json", 32 * 1024 * 1024, None)?)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                json!({"version":1,"videos":{}})
            }
            Err(_) => {
                return Err(Error::new(
                    503,
                    "The removal exclusions could not be checked. Transcription is paused.",
                ));
            }
        };
        if !state["videos"].as_object().is_some_and(|v| {
            v.iter().all(|(id, r)| {
                valid_video(id)
                    && r.is_object()
                    && r["job_id"].as_str().is_some_and(valid_id)
                    && r["removed_at"].is_string()
            })
        }) {
            return Err(Error::new(
                503,
                "The removal exclusions could not be read. Transcription is paused to avoid restoring removed videos.",
            ));
        }
        Ok(Self {
            root,
            state: RwLock::new(state),
        })
    }
    pub fn contains(&self, id: &str) -> bool {
        self.state.read()["videos"].get(id).is_some()
    }
    pub fn records(&self) -> Value {
        self.state.read()["videos"].clone()
    }
    pub fn add(&self, jobs: &[Value]) -> Result<()> {
        let mut current = self.state.write();
        let mut next = current.clone();
        for job in jobs {
            if let (Some(video), Some(id)) = (job["video_id"].as_str(), job["id"].as_str())
                && valid_video(video)
                && valid_id(id)
                && next["videos"].get(video).is_none()
            {
                next["videos"][video] = json!({"job_id":id,"removed_at":now()})
            }
        }
        if next != *current {
            Dir::open(&self.root)?.atomic_json("removed-videos.json", &next)?;
            *current = next
        }
        Ok(())
    }
}
