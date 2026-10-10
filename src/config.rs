// SPDX-License-Identifier: GPL-3.0-or-later
//! The hand-edited settings file.
//!
//! ```toml
//! # ~/.config/wlrix/clipboard.toml
//! max_entries = 50             # unstarred entries kept; starred ones never count
//! persist = true               # keep the history across logins
//! keep_clipboard_full = true   # when the program that copied exits, put the last entry back
//! max_entry_bytes = 16777216   # anything bigger is not recorded
//! ```
//!
//! Read from the user's config directory first, then `/etc/wlrix`; the first file found wins
//! outright rather than merging, as everywhere else in wlRIX. Unknown keys are an error, for the
//! same reason the compositor rejects them -- a silently ignored typo in a config file is a bad
//! afternoon.

use serde::Deserialize;

/// What `max_entries` means when the file does not say. KDE's default, and enough to reach back
/// past a morning's copying without the popup becoming something to scroll through.
const DEFAULT_MAX_ENTRIES: usize = 50;
/// What `max_entry_bytes` means when the file does not say. Comfortably a full-screen 4K
/// screenshot as PNG; a copied video file is not something a history is for.
const DEFAULT_MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
/// The fewest entries the history will keep, whatever the file says. Zero would make the
/// daemon a clipboard that forgets everything the moment it is copied -- including the entry it
/// is meant to be keeping on the clipboard.
const MIN_ENTRIES: usize = 1;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How many unstarred entries to keep. Starred entries are kept as well as these, never
    /// instead of them.
    #[serde(default)]
    max_entries: Option<usize>,
    /// Whether the history is written somewhere that outlives the session. Defaults to yes.
    ///
    /// Read once, at startup: a reload that turned it off would have to decide whether to delete
    /// what is already on disk, and that is not a decision a `SIGHUP` should make.
    #[serde(default)]
    persist: Option<bool>,
    /// Whether an empty clipboard is refilled from the history. Defaults to yes.
    ///
    /// Wayland's clipboard belongs to the program that set it, and goes empty when that program
    /// exits -- copy from a terminal, close it, and there is nothing to paste. With this on, the
    /// daemon steps in and offers the newest entry again.
    #[serde(default)]
    keep_clipboard_full: Option<bool>,
    /// The largest selection recorded, in bytes. A bigger one is left alone: still on the
    /// clipboard, just not in the history.
    #[serde(default)]
    max_entry_bytes: Option<usize>,
}

impl Config {
    pub fn max_entries(&self) -> usize {
        self.max_entries
            .unwrap_or(DEFAULT_MAX_ENTRIES)
            .max(MIN_ENTRIES)
    }

    pub fn persist(&self) -> bool {
        self.persist.unwrap_or(true)
    }

    pub fn keep_clipboard_full(&self) -> bool {
        self.keep_clipboard_full.unwrap_or(true)
    }

    pub fn max_entry_bytes(&self) -> usize {
        self.max_entry_bytes.unwrap_or(DEFAULT_MAX_ENTRY_BYTES)
    }

    /// Load the first config file that exists. No file at all is not an error -- the defaults are
    /// a working history.
    pub fn load() -> Self {
        for path in crate::xdg::config_paths() {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                // Not-found is the ordinary case; only real errors are worth a line.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    eprintln!("wlrix-clipboard: could not read {}: {err}", path.display());
                    continue;
                }
            };
            match toml::from_str::<Self>(&text) {
                Ok(config) => return config,
                Err(err) => {
                    // Loud, then carry on with defaults: a broken config should cost the user
                    // their settings, not their history.
                    eprintln!("wlrix-clipboard: {} is not valid: {err}", path.display());
                    return Self::default();
                }
            }
        }
        Self::default()
    }
}

/// Parse a candidate config file, for `--check-config`.
///
/// `wlrix-settings-daemon` writes a temporary file and runs this against it before renaming it
/// into place, so a settings app cannot produce a file this program would refuse. Deliberately
/// not [`Config::load`], which reports and carries on with defaults -- right at startup, wrong
/// here, where the question *is* whether the file is acceptable.
pub fn check(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    toml::from_str::<Config>(&text)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        toml::from_str(text).expect("the test config should parse")
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        let config = config("");
        assert_eq!(config.max_entries(), DEFAULT_MAX_ENTRIES);
        assert!(config.persist());
        assert!(config.keep_clipboard_full());
        assert_eq!(config.max_entry_bytes(), DEFAULT_MAX_ENTRY_BYTES);
    }

    #[test]
    fn a_full_file_is_read() {
        let config = config(
            "max_entries = 10\n\
             persist = false\n\
             keep_clipboard_full = false\n\
             max_entry_bytes = 1024\n",
        );
        assert_eq!(config.max_entries(), 10);
        assert!(!config.persist());
        assert!(!config.keep_clipboard_full());
        assert_eq!(config.max_entry_bytes(), 1024);
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        assert!(toml::from_str::<Config>("max_entires = 10\n").is_err());
    }

    #[test]
    fn a_history_of_nothing_is_raised_to_one() {
        assert_eq!(config("max_entries = 0\n").max_entries(), MIN_ENTRIES);
    }
}
