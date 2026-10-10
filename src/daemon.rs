// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon: watching the clipboard, recording it, and putting entries back.
//!
//! # Watching
//!
//! A `zwlr_data_control_device_v1` on the seat reports every new selection as an offer and the
//! types it comes in. One type is chosen ([`crate::mime::choose`]) and read through a pipe: the
//! owner writes, closes its end, and the close is the end of the data. The read end sits on the
//! event loop as a non-blocking source with a size cap and a timeout, so a program that offers a
//! selection and then never answers -- or answers with a film -- costs one entry, not the daemon.
//!
//! # Putting back
//!
//! Choosing an entry in the popup makes this program the clipboard's owner: it creates a source
//! offering the entry, sets it as the selection, and answers every paste from it. Each answer is
//! written on a thread of its own, so a paster that reads slowly cannot stall the loop.
//!
//! The same happens unasked when the clipboard goes **empty** -- which on Wayland is what
//! happens when the program that copied something exits. With `keep_clipboard_full` on (the
//! default), the active entry is offered again, so closing the terminal something was copied
//! from no longer loses it. At startup the newest entry is offered if the clipboard is empty,
//! whatever that setting says, so the history from the last session is there to paste at once.
//!
//! # Why wlr-data-control
//!
//! `wl_data_device` would need keyboard focus and an input serial to read or set the clipboard,
//! and this program has no window to have either. Data control exists for clipboard managers and
//! needs neither. The compositor refuses it to sandboxed clients, which is the right side of
//! that trade: whoever binds it sees everything copied.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    backend::WaylandError,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::config::Config;
use crate::dbus::{self, Request, Snapshot};
use crate::history::{Content, EditError, Entry, History};
use crate::mime::{self, Choice, Reason};
use crate::store::Store;

/// How long a selection's owner gets to finish sending it. Generous: a big image from a program
/// that renders it on demand can take a while, and the only cost of waiting is that one entry
/// arrives late.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The types an offer has announced, collected as they arrive. The compositor sends the offer,
/// then its types, then the `selection` event naming it, so they are complete by the time
/// anything reads them.
#[derive(Default)]
pub struct OfferTypes(Mutex<Vec<String>>);

/// What a source this program set is offering: the bytes, and the types they are offered as.
pub struct Payload {
    bytes: Vec<u8>,
    types: Vec<String>,
}

/// A selection being read.
struct Reading {
    /// Which selection this is. A read that finishes after a newer selection has arrived is
    /// thrown away rather than recorded over it.
    generation: u64,
    pipe: RegistrationToken,
    timer: RegistrationToken,
}

pub struct Daemon {
    config: Config,
    history: History,
    store: Store,
    snapshot: Arc<Mutex<Snapshot>>,
    bus: zbus::blocking::Connection,

    connection: Connection,
    qh: QueueHandle<Daemon>,
    manager: ZwlrDataControlManagerV1,
    device: ZwlrDataControlDeviceV1,
    handle: LoopHandle<'static, Daemon>,

    /// The offer that is the clipboard right now, kept so it can be destroyed when the next one
    /// replaces it.
    selection: Option<ZwlrDataControlOfferV1>,
    reading: Option<Reading>,
    generation: u64,
    /// The source this program set, while it is still the clipboard.
    serving: Option<ZwlrDataControlSourceV1>,
    /// Whether the first `selection` event has arrived. It always does, straight after the
    /// device is created, and says what was on the clipboard before this program started.
    settled: bool,
    exit: Option<Result<(), String>>,
}

/// Run until the compositor goes away.
pub fn run() -> Result<(), String> {
    let config = Config::load();
    let dir = if config.persist() {
        crate::xdg::persistent_dir().ok_or("neither XDG_DATA_HOME nor HOME is set")?
    } else {
        crate::xdg::session_dir().ok_or("XDG_RUNTIME_DIR is not set")?
    };
    let store = Store::open(dir.clone(), !config.persist())
        .map_err(|err| format!("could not open {}: {err}", dir.display()))?;
    let history = store.load(config.max_entries());

    // The bus first: a second copy of this program fails here, before it has touched the
    // clipboard.
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let (sender, requests) = calloop::channel::channel();
    let bus = dbus::start(Arc::clone(&snapshot), sender)?;

    let connection = Connection::connect_to_env()
        .map_err(|err| format!("no Wayland compositor to connect to: {err}"))?;
    let (globals, queue) = registry_queue_init::<Daemon>(&connection)
        .map_err(|err| format!("could not read the registry: {err}"))?;
    let qh = queue.handle();
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|err| format!("the compositor has no wlr-data-control: {err}"))?;
    // The first seat. wlRIX has one, and a clipboard manager for several seats would be several
    // histories, which nothing here is shaped for.
    let seat: WlSeat = globals
        .bind(&qh, 1..=8, ())
        .map_err(|err| format!("the compositor has no seat: {err}"))?;
    let device = manager.get_data_device(&seat, &qh, ());

    let mut event_loop: EventLoop<'static, Daemon> =
        EventLoop::try_new().map_err(|err| format!("could not create the event loop: {err}"))?;
    let handle = event_loop.handle();

    let mut daemon = Daemon {
        config,
        history,
        store,
        snapshot,
        bus,
        connection: connection.clone(),
        qh,
        manager,
        device,
        handle: handle.clone(),
        selection: None,
        reading: None,
        generation: 0,
        serving: None,
        settled: false,
        exit: None,
    };
    daemon.publish(false);

    WaylandSource::new(connection.clone(), queue)
        .insert(handle.clone())
        .map_err(|err| format!("could not drive Wayland from the loop: {err}"))?;
    handle
        .insert_source(requests, |event, _, daemon: &mut Daemon| {
            if let calloop::channel::Event::Msg(request) = event {
                daemon.on_request(request);
            }
        })
        .map_err(|err| format!("could not listen to the bus: {err}"))?;

    // Re-read `clipboard.toml` on `SIGHUP`. `wlrix-settings-daemon` finds this process through
    // the pidfile below; `kill -HUP` does the same thing by hand.
    let (reload_ping, reload_source) = calloop::ping::make_ping()
        .map_err(|err| format!("could not create the reload ping: {err}"))?;
    handle
        .insert_source(reload_source, |_, _, daemon: &mut Daemon| daemon.reload())
        .map_err(|err| format!("could not watch for reloads: {err}"))?;
    crate::signals::forward_reload_to_loop(reload_ping);

    // Held until `run` returns, then its guard removes the file.
    let _pidfile = crate::pidfile::write();

    loop {
        if let Err(err) = event_loop.dispatch(None, &mut daemon) {
            return match connection.flush() {
                // The compositor going away is the ordinary end of this program's life: logging
                // out looks exactly like this.
                Err(WaylandError::Io(_)) => Ok(()),
                _ => Err(format!("event loop failed: {err}")),
            };
        }
        // Caught by hand, as in `wlrix-tray`: `calloop-wayland-source` drops protocol errors, and
        // the socket then stays readable forever, spinning the loop.
        if let Some(err) = connection.protocol_error() {
            return Err(format!("protocol error: {err}"));
        }
        if let Some(exit) = daemon.exit.take() {
            return exit;
        }
    }
}

impl Daemon {
    /// The clipboard changed hands.
    fn on_selection(&mut self, offer: Option<ZwlrDataControlOfferV1>) {
        self.cancel_read();
        if let Some(previous) = self.selection.take()
            && Some(&previous) != offer.as_ref()
        {
            previous.destroy();
        }
        let first = !self.settled;
        self.settled = true;

        let Some(offer) = offer else {
            // Empty: whoever owned the clipboard has gone. Put the active entry back, or at
            // startup the newest one.
            if first || self.config.keep_clipboard_full() {
                let entry = self
                    .history
                    .active()
                    .or_else(|| self.history.entries().first().map(|entry| entry.id));
                if let Some(id) = entry {
                    self.serve(id);
                }
            }
            return;
        };
        self.selection = Some(offer.clone());

        let types = offer
            .data::<OfferTypes>()
            .map(|types| {
                types
                    .0
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .clone()
            })
            .unwrap_or_default();
        match mime::choose(&types) {
            Choice::Text(mime) => self.read(&offer, mime, true),
            Choice::Image(mime) => self.read(&offer, mime, false),
            // Our own entry, come back round. The history already says it is active.
            Choice::Ignore(Reason::Own) => {}
            Choice::Ignore(Reason::Secret | Reason::Unsupported) => {
                // On the clipboard, but not anything in the history.
                if self.history.active().is_some() {
                    self.history.deactivate();
                    self.publish(true);
                }
            }
        }
    }

    /// Start reading the selection through `mime`.
    fn read(&mut self, offer: &ZwlrDataControlOfferV1, mime: String, text: bool) {
        let (read, write) = match rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) {
            Ok(pipe) => pipe,
            Err(err) => {
                eprintln!("wlrix-clipboard: could not make a pipe: {err}");
                return;
            }
        };
        // Only the end this process reads is non-blocking. The other goes to the selection's
        // owner, which is entitled to a plain blocking pipe to write into.
        if let Err(err) = rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK) {
            eprintln!("wlrix-clipboard: could not make the pipe non-blocking: {err}");
            return;
        }
        offer.receive(mime.clone(), write.as_fd());
        // This process's copy of the write end has to go, or the read never sees end of file.
        drop(write);
        if let Err(err) = self.connection.flush() {
            eprintln!("wlrix-clipboard: could not ask for the selection: {err}");
            return;
        }

        self.generation += 1;
        let generation = self.generation;
        let limit = self.config.max_entry_bytes();
        let mut bytes = Vec::new();
        let pipe = Generic::new(File::from(read), Interest::READ, Mode::Level);
        let pipe = self.handle.insert_source(pipe, move |_, file, daemon| {
            let mut chunk = [0u8; 64 * 1024];
            loop {
                match (&mut &**file).read(&mut chunk) {
                    Ok(0) => {
                        daemon.finish_read(generation, &mime, text, std::mem::take(&mut bytes));
                        return Ok(PostAction::Remove);
                    }
                    Ok(read) => {
                        bytes.extend_from_slice(&chunk[..read]);
                        if bytes.len() > limit {
                            eprintln!(
                                "wlrix-clipboard: not recording a selection over {limit} bytes"
                            );
                            daemon.abandon_read(generation);
                            return Ok(PostAction::Remove);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(PostAction::Continue);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(err) => {
                        eprintln!("wlrix-clipboard: could not read the selection: {err}");
                        daemon.abandon_read(generation);
                        return Ok(PostAction::Remove);
                    }
                }
            }
        });
        let timer = self.handle.insert_source(
            Timer::from_duration(READ_TIMEOUT),
            move |_, _, daemon: &mut Daemon| {
                if daemon
                    .reading
                    .as_ref()
                    .is_some_and(|reading| reading.generation == generation)
                {
                    eprintln!("wlrix-clipboard: the selection's owner never finished sending it");
                    daemon.cancel_read();
                }
                TimeoutAction::Drop
            },
        );
        match (pipe, timer) {
            (Ok(pipe), Ok(timer)) => {
                self.reading = Some(Reading {
                    generation,
                    pipe,
                    timer,
                });
            }
            (pipe, timer) => {
                eprintln!("wlrix-clipboard: could not watch the selection being read");
                for token in [pipe.ok(), timer.ok()].into_iter().flatten() {
                    self.handle.remove(token);
                }
            }
        }
    }

    /// Stop reading, from outside the pipe's own callback.
    fn cancel_read(&mut self) {
        if let Some(reading) = self.reading.take() {
            self.handle.remove(reading.pipe);
            self.handle.remove(reading.timer);
        }
    }

    /// Stop reading, from inside the pipe's callback, which removes itself.
    fn abandon_read(&mut self, generation: u64) {
        if let Some(reading) = self
            .reading
            .take_if(|reading| reading.generation == generation)
        {
            self.handle.remove(reading.timer);
        }
    }

    /// A selection has been read in full.
    fn finish_read(&mut self, generation: u64, mime: &str, text: bool, bytes: Vec<u8>) {
        let Some(reading) = self
            .reading
            .take_if(|reading| reading.generation == generation)
        else {
            return;
        };
        self.handle.remove(reading.timer);
        if bytes.is_empty() {
            return;
        }

        let (content, image) = if text {
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
            };
            (Content::Text(text), None)
        } else {
            (Content::image(mime, &bytes), Some(bytes))
        };
        let added = self.history.add(content, now());
        if added.new
            && let Some(bytes) = image
            && let Some(entry) = self.history.get(added.id).cloned()
            && let Err(err) = self.store.write_blob(&entry, &bytes)
        {
            eprintln!("wlrix-clipboard: could not keep the image: {err}");
            self.history.remove(added.id);
        }
        self.forget(&added.evicted);
        self.save();
        self.publish(true);
    }

    /// Become the clipboard's owner, offering entry `id`.
    fn serve(&mut self, id: u64) {
        let Some(entry) = self.history.get(id).cloned() else {
            return;
        };
        let payload = match &entry.content {
            Content::Text(text) => Payload {
                bytes: text.clone().into_bytes(),
                types: mime::TEXT_TYPES.map(str::to_owned).to_vec(),
            },
            Content::Image { mime, .. } => match self.store.read_blob(&entry) {
                Ok(bytes) => Payload {
                    bytes,
                    types: vec![mime.clone()],
                },
                Err(err) => {
                    eprintln!("wlrix-clipboard: could not read back entry {id}: {err}");
                    return;
                }
            },
        };
        let types = payload.types.clone();
        let source = self.manager.create_data_source(&self.qh, Arc::new(payload));
        for mime in types {
            source.offer(mime);
        }
        source.offer(mime::MARKER.to_owned());
        self.device.set_selection(Some(&source));
        if let Some(previous) = self.serving.replace(source) {
            // The compositor cancels it too, but there is no reason to wait to be told.
            previous.destroy();
        }
        let _ = self.connection.flush();
    }

    /// Act on something the popup asked for.
    fn on_request(&mut self, request: Request) {
        match request {
            Request::Activate(id) => {
                if !self.history.activate(id) {
                    return;
                }
                self.serve(id);
            }
            Request::SetText(id, text) => match self.history.set_text(id, text) {
                Ok(merged) => {
                    self.forget(&merged);
                    // Editing the entry on the clipboard edits the clipboard.
                    if self.history.active() == Some(id) {
                        self.serve(id);
                    }
                }
                Err(EditError::NoSuchEntry | EditError::NotText | EditError::Empty) => return,
            },
            Request::Remove(id) => {
                let Some(entry) = self.history.remove(id) else {
                    return;
                };
                self.forget(&[entry]);
            }
            Request::SetStarred(id, starred) => {
                let Some(evicted) = self.history.set_starred(id, starred) else {
                    return;
                };
                self.forget(&evicted);
            }
            Request::Clear => {
                let cleared = self.history.clear();
                self.forget(&cleared);
            }
        }
        self.save();
        self.publish(true);
    }

    /// Re-read the settings file.
    fn reload(&mut self) {
        self.config = Config::load();
        let evicted = self.history.set_max_entries(self.config.max_entries());
        if !evicted.is_empty() {
            self.forget(&evicted);
            self.save();
            self.publish(true);
        }
    }

    /// Delete what entries that left the history kept on disk.
    fn forget(&self, entries: &[Entry]) {
        for entry in entries {
            self.store.remove_blob(entry);
        }
    }

    fn save(&self) {
        if let Err(err) = self.store.save(&self.history) {
            eprintln!("wlrix-clipboard: could not save the history: {err}");
        }
    }

    /// Refresh what the bus answers with, and say so when `announce` is set.
    fn publish(&self, announce: bool) {
        let rows = self
            .history
            .entries()
            .iter()
            .map(|entry| {
                let (kind, text, path) = match &entry.content {
                    Content::Text(text) => ("text", text.clone(), String::new()),
                    Content::Image { .. } => (
                        "image",
                        String::new(),
                        self.store
                            .blob_path(entry)
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ),
                };
                (
                    entry.id,
                    kind.to_owned(),
                    text,
                    path,
                    entry.starred,
                    entry.created,
                )
            })
            .collect();
        *self.snapshot.lock().unwrap_or_else(|err| err.into_inner()) = Snapshot {
            rows,
            active: self.history.active().unwrap_or(0),
        };
        if announce {
            dbus::announce_change(&self.bus);
        }
    }
}

/// Seconds since the epoch.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Daemon {
    fn event(
        daemon: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::Selection { id } => daemon.on_selection(id),
            // The primary selection is not recorded -- highlighting text is not copying it --
            // so its offers are let go at once.
            zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                offer.destroy();
            }
            // The seat went away. There is nothing left to watch.
            zwlr_data_control_device_v1::Event::Finished => {
                daemon.exit = Some(Err("the seat's clipboard went away".to_owned()));
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Daemon, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, OfferTypes::default()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, OfferTypes> for Daemon {
    fn event(
        _daemon: &mut Self,
        _offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        types: &OfferTypes,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            types
                .0
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Arc<Payload>> for Daemon {
    fn event(
        daemon: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        payload: &Arc<Payload>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                if !payload.types.contains(&mime_type) {
                    // The marker, or a type never offered. Dropping the descriptor closes it,
                    // which the paster reads as an empty paste.
                    return;
                }
                let payload = Arc::clone(payload);
                let spawned = std::thread::Builder::new()
                    .name("wlrix-clipboard-send".to_owned())
                    .spawn(move || {
                        if let Err(err) = send(fd, &payload.bytes) {
                            eprintln!("wlrix-clipboard: could not answer a paste: {err}");
                        }
                    });
                if let Err(err) = spawned {
                    eprintln!("wlrix-clipboard: could not answer a paste: {err}");
                }
            }
            // Somebody else took the clipboard.
            zwlr_data_control_source_v1::Event::Cancelled => {
                if daemon.serving.as_ref() == Some(source) {
                    daemon.serving = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

/// Write a whole entry to a paster's descriptor, then close it.
///
/// Blocking, on purpose and on its own thread: the descriptor is the paster's pipe, and the
/// close at the end is the end of file it is waiting for.
fn send(fd: OwnedFd, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // Whatever flags the paster left on its end, this side writes until it is done.
    let flags = rustix::fs::fcntl_getfl(&fd)?;
    rustix::fs::fcntl_setfl(&fd, flags - rustix::fs::OFlags::NONBLOCK)?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    file.flush()
}

/// Objects with no events worth acting on, and the registry, which after `registry_queue_init`
/// only reports globals coming and going.
macro_rules! ignore_events {
    ($($ty:ty $(: $data:ty)?),* $(,)?) => {$(
        impl Dispatch<$ty, ignore_events!(@data $($data)?)> for Daemon {
            fn event(
                _daemon: &mut Self,
                _obj: &$ty,
                _event: <$ty as wayland_client::Proxy>::Event,
                _data: &ignore_events!(@data $($data)?),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
    (@data $data:ty) => { $data };
    (@data) => { () };
}
ignore_events!(
    WlRegistry: GlobalListContents,
    WlSeat,
    ZwlrDataControlManagerV1,
);
