//! `~/.cache/llm-tierbridge/`: raw downloads (keyed by sha256), the git clones, the newest
//! snapshot of every source (`latest.json`) and the last merged `catalog.json`. Nothing here is
//! ever committed to the data repo.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tierbridge_catalog::FetchMeta;
use tierbridge_catalog::models_dev::sha256_hex;

use crate::{Failure, failure};

/// The newest snapshot taken of one source (accepted into the data repo or not).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Latest {
    pub sha256: String,
    pub revision: String,
    pub fetched_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Latest {
    /// The HTTP facts to record in the observations' provenance (`None` unless it was downloaded).
    pub fn fetch_meta(&self) -> Option<FetchMeta> {
        self.url.as_ref().map(|url| FetchMeta {
            url: url.clone(),
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LatestFile {
    sources: BTreeMap<String, Latest>,
}

pub struct Cache {
    pub dir: PathBuf,
}

fn io(path: &Path, e: &std::io::Error) -> Failure {
    failure(format!("cache {}: {e}", path.display()))
}

impl Cache {
    /// `--cache-dir`, else `$TIERBRIDGE_CACHE_DIR`, else `~/.cache/llm-tierbridge`.
    pub fn resolve(arg: Option<&Path>) -> Result<Self, Failure> {
        let dir = arg
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("TIERBRIDGE_CACHE_DIR").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/llm-tierbridge"))
            })
            .ok_or_else(|| failure("no cache directory: pass --cache-dir"))?;
        Ok(Self { dir })
    }

    fn ensure(&self, sub: &str) -> Result<PathBuf, Failure> {
        let dir = self.dir.join(sub);
        fs::create_dir_all(&dir).map_err(|e| io(&dir, &e))?;
        Ok(dir)
    }

    /// Where the shallow clones of the git sources live.
    pub fn repos_dir(&self) -> Result<PathBuf, Failure> {
        self.ensure("repos")
    }

    pub fn tmp_dir(&self) -> Result<PathBuf, Failure> {
        self.ensure("tmp")
    }

    /// The last merged `catalog.json` and `report.merge.json`.
    pub fn last_dir(&self) -> Result<PathBuf, Failure> {
        self.ensure("last")
    }

    fn raw_path(&self, sha256: &str) -> PathBuf {
        self.dir.join("raw").join(sha256)
    }

    /// Stores a download under its sha256 and returns the hash.
    pub fn store_raw(&self, bytes: &[u8]) -> Result<String, Failure> {
        let sha = sha256_hex(bytes);
        let path = self.raw_path(&sha);
        if !path.exists() {
            self.ensure("raw")?;
            let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
            fs::write(&tmp, bytes).map_err(|e| io(&tmp, &e))?;
            fs::rename(&tmp, &path).map_err(|e| io(&path, &e))?;
        }
        Ok(sha)
    }

    /// The download with this hash; `None` when it is missing or no longer matches its name.
    pub fn load_raw(&self, sha256: &str) -> Option<Vec<u8>> {
        let bytes = fs::read(self.raw_path(sha256)).ok()?;
        (sha256_hex(&bytes) == sha256).then_some(bytes)
    }

    fn latest_path(&self) -> PathBuf {
        self.dir.join("latest.json")
    }

    pub fn latest(&self) -> Result<BTreeMap<String, Latest>, Failure> {
        let path = self.latest_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<LatestFile>(&bytes)
                .map(|f| f.sources)
                .map_err(|e| failure(format!("cache {}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(io(&path, &e)),
        }
    }

    pub fn save_latest(&self, sources: &BTreeMap<String, Latest>) -> Result<(), Failure> {
        fs::create_dir_all(&self.dir).map_err(|e| io(&self.dir, &e))?;
        let path = self.latest_path();
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        let file = LatestFile {
            sources: sources.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file).expect("latest serializes");
        bytes.push(b'\n');
        fs::write(&tmp, bytes).map_err(|e| io(&tmp, &e))?;
        fs::rename(&tmp, &path).map_err(|e| io(&path, &e))
    }

    /// Deletes the downloads nobody refers to any more. Best effort.
    pub fn prune_raw(&self, keep: &HashSet<String>) {
        let Ok(entries) = fs::read_dir(self.dir.join("raw")) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tb-cache-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn raw_is_keyed_by_sha256_and_checked_on_load() {
        let dir = scratch("raw");
        let cache = Cache { dir: dir.clone() };
        let sha = cache.store_raw(b"hello").unwrap();
        assert_eq!(sha, sha256_hex(b"hello"));
        assert_eq!(cache.load_raw(&sha).unwrap(), b"hello");
        fs::write(dir.join("raw").join(&sha), b"tampered").unwrap();
        assert!(cache.load_raw(&sha).is_none());
        assert!(cache.load_raw(&"0".repeat(64)).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn latest_round_trips_and_prune_keeps_what_is_referenced() {
        let dir = scratch("latest");
        let cache = Cache { dir: dir.clone() };
        assert!(cache.latest().unwrap().is_empty());
        let keep = cache.store_raw(b"keep").unwrap();
        let drop = cache.store_raw(b"drop").unwrap();
        let mut map = BTreeMap::new();
        map.insert(
            "models.dev".to_owned(),
            Latest {
                sha256: keep.clone(),
                revision: "r".into(),
                fetched_at_ms: 5,
                url: Some("https://x".into()),
                etag: None,
                last_modified: None,
            },
        );
        cache.save_latest(&map).unwrap();
        assert_eq!(cache.latest().unwrap(), map);
        assert_eq!(
            map["models.dev"].fetch_meta().map(|m| m.url),
            Some("https://x".to_owned())
        );
        cache.prune_raw(&HashSet::from([keep.clone()]));
        assert!(cache.load_raw(&keep).is_some());
        assert!(cache.load_raw(&drop).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
