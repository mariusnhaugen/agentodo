use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agentodo_core::{Actor, Source, Store};
use anyhow::{Context, Result};

/// What we remember about the store file to notice writes from elsewhere
/// (the CLI or an agent via MCP). Length guards against coarse mtimes.
type Fingerprint = (SystemTime, u64);

/// Access to the on-disk store that never holds data across edits.
///
/// Every mutation reloads the file, applies one change and saves, so writes
/// made by other processes while the TUI is open are never overwritten.
#[derive(Debug)]
pub struct Repo {
    path: PathBuf,
    seen: Option<Fingerprint>,
}

impl Repo {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Repo {
            path: path.into(),
            seen: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the current store and remembers its fingerprint.
    pub fn load(&mut self) -> Result<Store> {
        // Fingerprint first: if the file changes mid-load we reload again later
        // rather than missing the change.
        let fingerprint = self.fingerprint();
        let store = Store::load(&self.path)
            .with_context(|| format!("loading store at {}", self.path.display()))?;
        self.seen = fingerprint;
        Ok(store)
    }

    /// True if the file differs from what the last [`Repo::load`] saw.
    pub fn changed(&self) -> bool {
        self.fingerprint() != self.seen
    }

    /// Load → apply `f` → save. Nothing is written if `f` fails.
    pub fn with_store<T>(
        &mut self,
        f: impl FnOnce(&mut Store) -> agentodo_core::Result<T>,
    ) -> Result<T> {
        let mut store = Store::load(&self.path)?;
        store.set_actor(Actor::from_env(Source::Tui));
        let out = f(&mut store)?;
        store.save()?;
        Ok(out)
    }

    fn fingerprint(&self) -> Option<Fingerprint> {
        let meta = fs::metadata(&self.path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }
}
