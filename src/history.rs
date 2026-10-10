// SPDX-License-Identifier: GPL-3.0-or-later
//! The history itself: what was copied, in what order, and which entries are starred.
//!
//! Pure bookkeeping, with no Wayland, no bus and no files, so that every rule below is tested
//! without any of them. Callers act on what these methods hand back -- an evicted image's blob is
//! [`crate::store`]'s to delete, an activated entry is [`crate::daemon`]'s to put on the
//! clipboard.
//!
//! # The rules
//!
//! - **Newest first.** Copying something, or choosing it from the popup, moves it to the top.
//! - **No duplicates.** Copying what is already in the history moves that entry to the top
//!   rather than adding a second one -- otherwise copying the same word five times would push
//!   four useful entries out.
//! - **Stars are kept.** Only unstarred entries count toward the limit, so starring something is
//!   a promise that it stays until it is deleted by hand. Clearing the history leaves them too.

/// What an entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// The bytes themselves are not kept here: an image can be megabytes, and it is only read
    /// back when it is put on the clipboard again. `digest` and `size` are what tell two images
    /// apart without opening either.
    Image {
        mime: String,
        digest: u64,
        size: u64,
    },
}

impl Content {
    /// An image entry for `bytes`, as offered under `mime`.
    pub fn image(mime: &str, bytes: &[u8]) -> Self {
        Content::Image {
            mime: mime.to_owned(),
            digest: digest(bytes),
            size: bytes.len() as u64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Never reused, so a popup holding a stale list cannot act on the wrong entry. Starts at 1;
    /// 0 is what the bus uses for "none".
    pub id: u64,
    pub content: Content,
    /// When it was last copied, in seconds since the epoch.
    pub created: i64,
    pub starred: bool,
}

/// What [`History::add`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Added {
    pub id: u64,
    /// `false` when the content was already in the history and the existing entry was moved.
    pub new: bool,
    /// Entries pushed out by this one.
    pub evicted: Vec<Entry>,
}

/// Why an edit was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    NoSuchEntry,
    NotText,
    Empty,
}

#[derive(Debug)]
pub struct History {
    /// Newest first.
    entries: Vec<Entry>,
    next_id: u64,
    /// The entry on the clipboard right now, as far as this program knows. `None` when the
    /// clipboard holds something that is not in the history -- a password, an empty selection,
    /// or an entry that has since been deleted.
    active: Option<u64>,
    max_entries: usize,
}

impl History {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            active: None,
            max_entries,
        }
    }

    /// Rebuild a history from what [`crate::store`] read back. `entries` is newest first.
    ///
    /// The id counter is carried rather than recomputed from the entries, so that an id deleted
    /// just before logout is not handed out again just after login.
    pub fn restore(
        entries: Vec<Entry>,
        next_id: u64,
        active: Option<u64>,
        max_entries: usize,
    ) -> (Self, Vec<Entry>) {
        let next_id = entries
            .iter()
            .map(|entry| entry.id + 1)
            .max()
            .unwrap_or(1)
            .max(next_id);
        let active = active.filter(|id| entries.iter().any(|entry| entry.id == *id));
        let mut history = Self {
            entries,
            next_id,
            active,
            max_entries,
        };
        // A history saved under a larger limit is trimmed to this one now, rather than at the
        // next copy.
        let evicted = history.evict();
        (history, evicted)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn active(&self) -> Option<u64> {
        self.active
    }

    /// The id the next new entry will get. Saved, so ids are not reused across logins.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Say that the clipboard holds something the history does not.
    pub fn deactivate(&mut self) {
        self.active = None;
    }

    /// Record something just copied, and make it the active entry.
    pub fn add(&mut self, content: Content, now: i64) -> Added {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.content == content)
        {
            let mut entry = self.entries.remove(index);
            entry.created = now;
            let id = entry.id;
            self.entries.insert(0, entry);
            self.active = Some(id);
            return Added {
                id,
                new: false,
                evicted: Vec::new(),
            };
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            0,
            Entry {
                id,
                content,
                created: now,
                starred: false,
            },
        );
        self.active = Some(id);
        Added {
            id,
            new: true,
            evicted: self.evict(),
        }
    }

    /// Move an entry to the top and make it the active one. `false` when there is no such entry.
    pub fn activate(&mut self, id: u64) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
        self.active = Some(id);
        true
    }

    /// Replace a text entry's text. Returns the entries the edit made redundant: editing one entry
    /// into a copy of another merges the two, keeping the edited one, starred if either was.
    pub fn set_text(&mut self, id: u64, text: String) -> Result<Vec<Entry>, EditError> {
        if text.is_empty() {
            return Err(EditError::Empty);
        }
        let index = self.index(id).ok_or(EditError::NoSuchEntry)?;
        if !matches!(self.entries[index].content, Content::Text(_)) {
            return Err(EditError::NotText);
        }
        let content = Content::Text(text);
        let mut merged = Vec::new();
        while let Some(other) = self
            .entries
            .iter()
            .position(|entry| entry.id != id && entry.content == content)
        {
            let other = self.entries.remove(other);
            if self.active == Some(other.id) {
                self.active = Some(id);
            }
            merged.push(other);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .expect("found above, and only other entries were removed");
        entry.content = content;
        entry.starred |= merged.iter().any(|other| other.starred);
        Ok(merged)
    }

    /// Delete one entry, starred or not.
    pub fn remove(&mut self, id: u64) -> Option<Entry> {
        let index = self.index(id)?;
        if self.active == Some(id) {
            self.active = None;
        }
        Some(self.entries.remove(index))
    }

    /// Star or unstar an entry. Returns `None` when there is no such entry, otherwise whatever
    /// unstarring it pushed out -- which can be the entry itself, if it was the oldest.
    pub fn set_starred(&mut self, id: u64, starred: bool) -> Option<Vec<Entry>> {
        let index = self.index(id)?;
        self.entries[index].starred = starred;
        Some(self.evict())
    }

    /// Delete every unstarred entry.
    pub fn clear(&mut self) -> Vec<Entry> {
        let (kept, cleared): (Vec<_>, Vec<_>) =
            self.entries.drain(..).partition(|entry| entry.starred);
        self.entries = kept;
        if let Some(active) = self.active
            && self.index(active).is_none()
        {
            self.active = None;
        }
        cleared
    }

    /// Change the limit, as a config reload does. Returns what no longer fits.
    pub fn set_max_entries(&mut self, max_entries: usize) -> Vec<Entry> {
        self.max_entries = max_entries;
        self.evict()
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// Drop the oldest unstarred entries until the rest fit.
    fn evict(&mut self) -> Vec<Entry> {
        let mut evicted = Vec::new();
        let mut unstarred = self.entries.iter().filter(|entry| !entry.starred).count();
        while unstarred > self.max_entries {
            let oldest = self
                .entries
                .iter()
                .rposition(|entry| !entry.starred)
                .expect("counted above");
            let entry = self.entries.remove(oldest);
            if self.active == Some(entry.id) {
                self.active = None;
            }
            evicted.push(entry);
            unstarred -= 1;
        }
        evicted
    }
}

/// A stable 64-bit digest of an image, for telling two apart.
///
/// FNV-1a rather than `std`'s hasher, whose output is allowed to change between Rust releases --
/// and these are written to disk, so a rebuilt daemon must still recognize yesterday's
/// screenshot. Collisions are not a security matter here; the size is compared too.
pub fn digest(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Content {
        Content::Text(text.to_owned())
    }

    fn texts(history: &History) -> Vec<String> {
        history
            .entries()
            .iter()
            .map(|entry| match &entry.content {
                Content::Text(text) => text.clone(),
                Content::Image { mime, .. } => mime.clone(),
            })
            .collect()
    }

    #[test]
    fn newest_comes_first_and_is_active() {
        let mut history = History::new(10);
        history.add(text("one"), 1);
        let two = history.add(text("two"), 2);
        assert_eq!(texts(&history), ["two", "one"]);
        assert_eq!(history.active(), Some(two.id));
    }

    #[test]
    fn copying_the_same_thing_again_moves_it_up() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        history.add(text("two"), 2);
        let again = history.add(text("one"), 3);
        assert!(!again.new);
        assert_eq!(again.id, one.id, "the existing entry is kept, not replaced");
        assert_eq!(texts(&history), ["one", "two"]);
        assert_eq!(history.get(one.id).unwrap().created, 3);
    }

    #[test]
    fn images_are_told_apart_by_their_bytes() {
        let mut history = History::new(10);
        let a = history.add(Content::image("image/png", b"aaaa"), 1);
        let b = history.add(Content::image("image/png", b"bbbb"), 2);
        let a_again = history.add(Content::image("image/png", b"aaaa"), 3);
        assert_ne!(a.id, b.id);
        assert_eq!(a_again.id, a.id);
        assert_eq!(history.entries().len(), 2);
    }

    #[test]
    fn the_oldest_unstarred_entry_goes_first() {
        let mut history = History::new(2);
        let one = history.add(text("one"), 1);
        history.add(text("two"), 2);
        let three = history.add(text("three"), 3);
        assert_eq!(three.evicted.len(), 1);
        assert_eq!(three.evicted[0].id, one.id);
        assert_eq!(texts(&history), ["three", "two"]);
    }

    #[test]
    fn starred_entries_are_never_evicted_and_do_not_count() {
        let mut history = History::new(2);
        let one = history.add(text("one"), 1);
        history.set_starred(one.id, true);
        history.add(text("two"), 2);
        history.add(text("three"), 3);
        // Two unstarred plus the starred one: nothing has been pushed out yet.
        assert_eq!(texts(&history), ["three", "two", "one"]);
        let four = history.add(text("four"), 4);
        assert_eq!(texts(&history), ["four", "three", "one"]);
        assert_eq!(texts_of(&four.evicted), ["two"]);
    }

    fn texts_of(entries: &[Entry]) -> Vec<String> {
        let mut history = History::new(usize::MAX);
        history.entries = entries.to_vec();
        texts(&history)
    }

    #[test]
    fn unstarring_can_push_an_entry_out() {
        let mut history = History::new(1);
        let one = history.add(text("one"), 1);
        history.set_starred(one.id, true);
        history.add(text("two"), 2);
        let evicted = history.set_starred(one.id, false).unwrap();
        assert_eq!(texts_of(&evicted), ["one"]);
        assert_eq!(texts(&history), ["two"]);
    }

    #[test]
    fn clearing_keeps_the_stars() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        history.set_starred(one.id, true);
        history.add(text("two"), 2);
        let cleared = history.clear();
        assert_eq!(texts_of(&cleared), ["two"]);
        assert_eq!(texts(&history), ["one"]);
        assert_eq!(history.active(), None, "the active entry was cleared");
    }

    #[test]
    fn activating_moves_an_entry_up() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        history.add(text("two"), 2);
        assert!(history.activate(one.id));
        assert_eq!(texts(&history), ["one", "two"]);
        assert_eq!(history.active(), Some(one.id));
        assert!(!history.activate(999));
    }

    #[test]
    fn editing_replaces_the_text_in_place() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        history.add(text("two"), 2);
        assert_eq!(history.set_text(one.id, "uno".to_owned()), Ok(Vec::new()));
        assert_eq!(texts(&history), ["two", "uno"]);
    }

    #[test]
    fn editing_into_a_duplicate_merges_the_two() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        let two = history.add(text("two"), 2);
        history.set_starred(two.id, true);
        let merged = history.set_text(one.id, "two".to_owned()).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, two.id);
        let kept = history.get(one.id).unwrap();
        assert!(kept.starred, "the star survives the merge");
        assert_eq!(history.active(), Some(one.id), "and so does being active");
    }

    #[test]
    fn edits_that_make_no_sense_are_refused() {
        let mut history = History::new(10);
        let image = history.add(Content::image("image/png", b"x"), 1);
        let one = history.add(text("one"), 2);
        assert_eq!(
            history.set_text(image.id, "x".to_owned()),
            Err(EditError::NotText)
        );
        assert_eq!(
            history.set_text(one.id, String::new()),
            Err(EditError::Empty)
        );
        assert_eq!(
            history.set_text(999, "x".to_owned()),
            Err(EditError::NoSuchEntry)
        );
    }

    #[test]
    fn removing_the_active_entry_leaves_nothing_active() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        assert!(history.remove(one.id).is_some());
        assert_eq!(history.active(), None);
        assert!(history.remove(one.id).is_none());
    }

    #[test]
    fn ids_are_never_reused() {
        let mut history = History::new(10);
        let one = history.add(text("one"), 1);
        history.remove(one.id);
        let two = history.add(text("two"), 2);
        assert!(two.id > one.id);

        // Not even across a restore that has lost the deleted entry.
        let (restored, _) = History::restore(
            history.entries().to_vec(),
            history.next_id(),
            history.active(),
            10,
        );
        assert!(restored.next_id() > two.id);
    }

    #[test]
    fn a_restore_under_a_smaller_limit_trims() {
        let mut history = History::new(10);
        for n in 0..5 {
            history.add(text(&n.to_string()), n);
        }
        let (restored, evicted) =
            History::restore(history.entries().to_vec(), history.next_id(), None, 3);
        assert_eq!(texts(&restored), ["4", "3", "2"]);
        assert_eq!(evicted.len(), 2);
    }

    #[test]
    fn the_digest_is_stable() {
        // Written to disk, so it must not change between builds. These are FNV-1a's published
        // test vectors.
        assert_eq!(digest(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
