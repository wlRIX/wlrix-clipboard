// SPDX-License-Identifier: GPL-3.0-or-later
//! `com.wlrix.Clipboard`: the history, for the popup to read and drive.
//!
//! ```text
//! bus name   com.wlrix.Clipboard
//! object     /com/wlrix/Clipboard
//! interface  com.wlrix.Clipboard1
//!
//! GetHistory() -> a(tsssbx)   id, kind ("text" | "image"), text, image path, starred, created
//! GetActive()  -> t           the entry on the clipboard, or 0
//! Activate(t)                 put an entry on the clipboard
//! SetText(t, s)               edit a text entry
//! Remove(t)
//! SetStarred(t, b)
//! Clear()                     delete every unstarred entry
//! signal HistoryChanged()
//! ```
//!
//! The history is newest first. An image's bytes are not sent: its path is, and the popup reads
//! the file -- a screenshot can be megabytes, and a list of thumbnails should not cost a copy of
//! each through the bus daemon.
//!
//! # Threads
//!
//! calloop is synchronous, so the bus is kept apart from it rather than reconciled, the shape
//! `wlrix-tray` and `wlrix-idle` use. Reads are answered on zbus's own thread from a
//! [`Snapshot`] the main loop refreshes after every change. Writes are checked against the same
//! snapshot, so a call naming an entry that does not exist fails at once, and then handed to the
//! loop as a [`Request`], because acting on one can mean setting the clipboard -- which only the
//! loop's Wayland connection can do. `HistoryChanged` follows once the loop has acted, which is
//! what the popup refreshes on.

use std::sync::{Arc, Mutex};

use zbus::fdo;

pub const BUS_NAME: &str = "com.wlrix.Clipboard";
pub const OBJECT_PATH: &str = "/com/wlrix/Clipboard";
pub const INTERFACE: &str = "com.wlrix.Clipboard1";

/// One entry, as `GetHistory` sends it: `(id, kind, text, image path, starred, created)`.
pub type Row = (u64, String, String, String, bool, i64);

/// The history as the bus sees it, refreshed by the main loop after every change.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    /// 0 for none.
    pub active: u64,
}

impl Snapshot {
    fn row(&self, id: u64) -> Option<&Row> {
        self.rows.iter().find(|row| row.0 == id)
    }
}

/// What the bus asks the main loop to do.
#[derive(Debug)]
pub enum Request {
    Activate(u64),
    SetText(u64, String),
    Remove(u64),
    SetStarred(u64, bool),
    Clear,
}

struct Service {
    snapshot: Arc<Mutex<Snapshot>>,
    sender: calloop::channel::Sender<Request>,
}

impl Service {
    fn snapshot(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.snapshot.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Fail with a useful message when `id` is not in the history.
    fn existing(&self, id: u64) -> fdo::Result<Row> {
        self.snapshot()
            .row(id)
            .cloned()
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("no entry {id} in the history")))
    }

    fn send(&self, request: Request) -> fdo::Result<()> {
        self.sender
            .send(request)
            .map_err(|_| fdo::Error::Failed("the clipboard history is shutting down".to_owned()))
    }
}

#[zbus::interface(name = "com.wlrix.Clipboard1")]
impl Service {
    #[zbus(out_args("history"))]
    fn get_history(&self) -> Vec<Row> {
        self.snapshot().rows.clone()
    }

    #[zbus(out_args("id"))]
    fn get_active(&self) -> u64 {
        self.snapshot().active
    }

    fn activate(&self, id: u64) -> fdo::Result<()> {
        self.existing(id)?;
        self.send(Request::Activate(id))
    }

    fn set_text(&self, id: u64, text: String) -> fdo::Result<()> {
        let row = self.existing(id)?;
        if row.1 != "text" {
            return Err(fdo::Error::InvalidArgs(format!(
                "entry {id} is an image, not text"
            )));
        }
        if text.is_empty() {
            return Err(fdo::Error::InvalidArgs(
                "an entry cannot be edited down to nothing; remove it instead".to_owned(),
            ));
        }
        self.send(Request::SetText(id, text))
    }

    fn remove(&self, id: u64) -> fdo::Result<()> {
        self.existing(id)?;
        self.send(Request::Remove(id))
    }

    fn set_starred(&self, id: u64, starred: bool) -> fdo::Result<()> {
        self.existing(id)?;
        self.send(Request::SetStarred(id, starred))
    }

    fn clear(&self) -> fdo::Result<()> {
        self.send(Request::Clear)
    }

    #[zbus(signal)]
    async fn history_changed(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Serve the history on the session bus.
///
/// Fails when the name is already owned, which means another copy of this program is running:
/// two of them would each record every copy, and each would put its own entry back the moment
/// the other's owner went away.
pub fn start(
    snapshot: Arc<Mutex<Snapshot>>,
    sender: calloop::channel::Sender<Request>,
) -> Result<zbus::blocking::Connection, String> {
    let connection = zbus::blocking::connection::Builder::session()
        .map_err(|err| format!("no session bus: {err}"))?
        // Served before the name is requested, so a popup that calls the instant the name
        // appears never finds an empty connection behind it.
        .serve_at(OBJECT_PATH, Service { snapshot, sender })
        .map_err(|err| format!("could not serve {OBJECT_PATH}: {err}"))?
        .build()
        .map_err(|err| format!("could not connect to the session bus: {err}"))?;
    let reply = connection
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .map_err(|err| match err {
            // zbus reports `DoNotQueue`'s refusal as an error rather than a reply.
            zbus::Error::NameTaken => already_running(),
            err => format!("could not request {BUS_NAME}: {err}"),
        })?;
    if !matches!(reply, zbus::fdo::RequestNameReply::PrimaryOwner) {
        return Err(already_running());
    }
    Ok(connection)
}

fn already_running() -> String {
    format!("{BUS_NAME} is already owned -- another wlrix-clipboard is running")
}

/// Tell the popup the history changed.
///
/// Emitted through the connection rather than a `SignalEmitter`, because the blocking API has no
/// way to await one from the synchronous main loop.
pub fn announce_change(connection: &zbus::blocking::Connection) {
    if let Err(err) =
        connection.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "HistoryChanged", &())
    {
        eprintln!("wlrix-clipboard: could not announce a change: {err}");
    }
}
