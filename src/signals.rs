// SPDX-License-Identifier: GPL-3.0-or-later
//! Re-reading `clipboard.toml` when asked to.
//!
//! `SIGHUP` means "re-read the config", turned into a calloop [`Ping`] fired from the handler --
//! an eventfd write, which is async-signal-safe -- whose source on the event loop does the
//! actual work, where it can touch the history without racing anything. The same shape
//! `wlrix-tray`, `wlrix-idle` and `wlrix-compositor` use, kept in step by hand.
//!
//! Only `SIGHUP`. Stopping needs no handler: the history is written as it changes, so there is
//! nothing to flush on the way out, and the ordinary end of this program's life is the compositor
//! going away, which the event loop already reports.

use std::sync::OnceLock;

use calloop::ping::Ping;

/// The ping the `SIGHUP` handler fires, to reload the config on the event loop.
static RELOAD: OnceLock<Ping> = OnceLock::new();

/// Install a `SIGHUP` handler that fires `reload`, so the loop can re-read the config.
pub fn forward_reload_to_loop(reload: Ping) {
    if RELOAD.set(reload).is_err() {
        return;
    }
    // SAFETY: the handler does only async-signal-safe work -- firing the ping, which is an
    // eventfd write.
    unsafe {
        libc::signal(
            libc::SIGHUP,
            handle_reload as *const () as libc::sighandler_t,
        )
    };
}

/// Runs in signal context; may only do async-signal-safe work.
extern "C" fn handle_reload(_signal: libc::c_int) {
    if let Some(reload) = RELOAD.get() {
        reload.ping();
    }
}
