// SPDX-License-Identifier: GPL-3.0-or-later
//! The history on disk.
//!
//! ```text
//! ~/.local/share/wlrix/clipboard/      mode 0700
//!     history.toml                     the index, and every text entry
//!     blobs/<id>.png                   one file per image entry
//! ```
//!
//! Text lives in the index, because it is small and the popup wants all of it at once to search.
//! Images live beside it as files of their own, named for their type, so the popup can read one
//! directly by path and nothing megabytes long ever crosses the bus.
//!
//! Everything is written owner-only and replaced by rename, never rewritten in place: what is in
//! here is whatever the user copied, and a crash halfway through a save must cost the last
//! change, not the whole history.
//!
//! With persistence off, the same layout goes under `$XDG_RUNTIME_DIR` and is emptied at startup,
//! so the popup reads images the same way in both cases.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::history::{Content, Entry, History};

const INDEX: &str = "history.toml";
const BLOBS: &str = "blobs";

/// The index as written. Kept apart from [`History`] so the file format is spelled out in one
/// place and the history's own types stay free of serde.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    next_id: u64,
    /// 0 for none: TOML has no null.
    #[serde(default)]
    active: u64,
    /// Newest first.
    #[serde(default)]
    entry: Vec<IndexEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexEntry {
    id: u64,
    created: i64,
    #[serde(default)]
    starred: bool,
    /// Set for a text entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Set for an image entry: its type. The bytes are in `blobs/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    /// The image's digest, in hex: TOML integers are signed, and a digest is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open the history directory, creating it owner-only. `wipe` empties it first, for a history
    /// that is not meant to outlive the session it was made in.
    pub fn open(dir: PathBuf, wipe: bool) -> std::io::Result<Self> {
        if wipe {
            match std::fs::remove_dir_all(&dir) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir.join(BLOBS))?;
        Ok(Self { dir })
    }

    /// Read the history back. A missing index is an empty history; an unreadable one is
    /// reported and treated the same, rather than costing the user their clipboard.
    ///
    /// Image entries whose blob has gone are dropped, and blobs no entry names are deleted, so
    /// the two halves agree again after anything went wrong between them.
    pub fn load(&self, max_entries: usize) -> History {
        let index = match std::fs::read_to_string(self.dir.join(INDEX)) {
            Ok(text) => toml::from_str::<Index>(&text).unwrap_or_else(|err| {
                eprintln!(
                    "wlrix-clipboard: {} is not readable, starting afresh: {err}",
                    self.dir.join(INDEX).display()
                );
                Index::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Index::default(),
            Err(err) => {
                eprintln!("wlrix-clipboard: could not read the history: {err}");
                Index::default()
            }
        };

        let entries: Vec<Entry> = index
            .entry
            .into_iter()
            .filter_map(|saved| {
                let content = match (saved.text, saved.image) {
                    (Some(text), None) => Content::Text(text),
                    (None, Some(mime)) => Content::Image {
                        mime,
                        digest: saved
                            .digest
                            .and_then(|hex| u64::from_str_radix(&hex, 16).ok())?,
                        size: saved.size?,
                    },
                    _ => return None,
                };
                let entry = Entry {
                    id: saved.id,
                    content,
                    created: saved.created,
                    starred: saved.starred,
                };
                match self.blob_path(&entry) {
                    Some(path) if !path.exists() => None,
                    _ => Some(entry),
                }
            })
            .collect();

        let active = (index.active != 0).then_some(index.active);
        let (history, evicted) = History::restore(entries, index.next_id, active, max_entries);
        for entry in &evicted {
            self.remove_blob(entry);
        }
        self.remove_orphans(&history);
        history
    }

    /// Write the index.
    pub fn save(&self, history: &History) -> std::io::Result<()> {
        let index = Index {
            next_id: history.next_id(),
            active: history.active().unwrap_or(0),
            entry: history
                .entries()
                .iter()
                .map(|entry| {
                    let mut saved = IndexEntry {
                        id: entry.id,
                        created: entry.created,
                        starred: entry.starred,
                        text: None,
                        image: None,
                        digest: None,
                        size: None,
                    };
                    match &entry.content {
                        Content::Text(text) => saved.text = Some(text.clone()),
                        Content::Image { mime, digest, size } => {
                            saved.image = Some(mime.clone());
                            saved.digest = Some(format!("{digest:016x}"));
                            saved.size = Some(*size);
                        }
                    }
                    saved
                })
                .collect(),
        };
        let text = toml::to_string(&index).map_err(std::io::Error::other)?;
        write_atomically(&self.dir.join(INDEX), text.as_bytes())
    }

    /// Where an image entry's bytes are. `None` for a text entry.
    pub fn blob_path(&self, entry: &Entry) -> Option<PathBuf> {
        match &entry.content {
            Content::Text(_) => None,
            Content::Image { mime, .. } => Some(self.dir.join(BLOBS).join(format!(
                "{}.{}",
                entry.id,
                crate::mime::extension(mime)
            ))),
        }
    }

    pub fn write_blob(&self, entry: &Entry, bytes: &[u8]) -> std::io::Result<()> {
        let path = self
            .blob_path(entry)
            .ok_or_else(|| std::io::Error::other("a text entry has no blob"))?;
        write_atomically(&path, bytes)
    }

    pub fn read_blob(&self, entry: &Entry) -> std::io::Result<Vec<u8>> {
        let path = self
            .blob_path(entry)
            .ok_or_else(|| std::io::Error::other("a text entry has no blob"))?;
        std::fs::read(path)
    }

    /// Delete an entry's blob, if it has one.
    pub fn remove_blob(&self, entry: &Entry) {
        if let Some(path) = self.blob_path(entry)
            && let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "wlrix-clipboard: could not remove {}: {err}",
                path.display()
            );
        }
    }

    /// Delete every blob the history does not name.
    fn remove_orphans(&self, history: &History) {
        let wanted: Vec<PathBuf> = history
            .entries()
            .iter()
            .filter_map(|entry| self.blob_path(entry))
            .collect();
        let Ok(dir) = std::fs::read_dir(self.dir.join(BLOBS)) else {
            return;
        };
        for file in dir.flatten() {
            let path = file.path();
            if !wanted.contains(&path) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Write `bytes` to `path` by way of a temporary file beside it, owner-only.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file: File = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fresh directory under the system temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wlrix-clipboard-test-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_history_round_trips() {
        let scratch = Scratch::new("round-trip");
        let store = Store::open(scratch.0.clone(), false).unwrap();

        let mut history = History::new(10);
        let text = history.add(Content::Text("line one\nline \"two\"".to_owned()), 1);
        history.set_starred(text.id, true);
        let png = b"\x89PNG not really";
        let image = history.add(Content::image("image/png", png), 2);
        let entry = history.get(image.id).unwrap().clone();
        store.write_blob(&entry, png).unwrap();
        store.save(&history).unwrap();

        let loaded = store.load(10);
        assert_eq!(loaded.entries(), history.entries());
        assert_eq!(loaded.active(), Some(image.id));
        assert_eq!(loaded.next_id(), history.next_id());
        assert_eq!(store.read_blob(&entry).unwrap(), png);
    }

    #[test]
    fn everything_is_owner_only() {
        let scratch = Scratch::new("modes");
        let store = Store::open(scratch.0.clone(), false).unwrap();
        let mut history = History::new(10);
        let image = history.add(Content::image("image/png", b"x"), 1);
        let entry = history.get(image.id).unwrap().clone();
        store.write_blob(&entry, b"x").unwrap();
        store.save(&history).unwrap();

        assert_eq!(mode(&scratch.0), 0o700);
        assert_eq!(mode(&scratch.0.join(INDEX)), 0o600);
        assert_eq!(mode(&store.blob_path(&entry).unwrap()), 0o600);
    }

    #[test]
    fn an_image_whose_blob_is_gone_is_dropped() {
        let scratch = Scratch::new("lost-blob");
        let store = Store::open(scratch.0.clone(), false).unwrap();
        let mut history = History::new(10);
        history.add(Content::image("image/png", b"x"), 1);
        history.add(Content::Text("kept".to_owned()), 2);
        store.save(&history).unwrap();

        let loaded = store.load(10);
        assert_eq!(loaded.entries().len(), 1);
        assert!(matches!(loaded.entries()[0].content, Content::Text(_)));
    }

    #[test]
    fn stray_blobs_are_cleaned_up() {
        let scratch = Scratch::new("orphans");
        let store = Store::open(scratch.0.clone(), false).unwrap();
        let stray = scratch.0.join(BLOBS).join("99.png");
        std::fs::write(&stray, b"x").unwrap();
        store.load(10);
        assert!(!stray.exists());
    }

    #[test]
    fn a_session_history_starts_empty() {
        let scratch = Scratch::new("wipe");
        let store = Store::open(scratch.0.clone(), false).unwrap();
        let mut history = History::new(10);
        history.add(Content::Text("gone at logout".to_owned()), 1);
        store.save(&history).unwrap();

        let store = Store::open(scratch.0.clone(), true).unwrap();
        assert!(store.load(10).entries().is_empty());
    }

    #[test]
    fn a_damaged_index_is_an_empty_history() {
        let scratch = Scratch::new("damaged");
        let store = Store::open(scratch.0.clone(), false).unwrap();
        std::fs::write(scratch.0.join(INDEX), "this is not [toml").unwrap();
        assert!(store.load(10).entries().is_empty());
    }
}
