// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX's clipboard history.
//!
//! A daemon with no window. It watches the clipboard through wlr-data-control, keeps what was
//! copied -- text and images -- and puts any of it back on request. The popup that shows the
//! history is `wlrix-clipboard-history`, in `wlrix-apps`; it talks to this program over
//! `com.wlrix.Clipboard` on the session bus and never touches the clipboard itself.
//!
//! [`daemon`] is the Wayland side and the event loop, [`dbus`] the bus. Between them sit the
//! parts that are neither, and are therefore testable without a compositor or a session:
//! [`history`] (the rules about what is kept), [`mime`] (which type of a selection is recorded)
//! and [`store`] (the history on disk).

pub mod config;
pub mod daemon;
pub mod dbus;
pub mod history;
pub mod mime;
pub mod pidfile;
pub mod signals;
pub mod store;
pub mod xdg;
