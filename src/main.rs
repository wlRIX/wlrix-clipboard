// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX clipboard history daemon.
//!
//! Remembers what was copied and puts it back on request. Started by `wlrix-session`; the popup
//! that shows the history is `wlrix-clipboard-history`. See [`wlrix_clipboard::daemon`].

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut rest = args.iter();
    if let Some(argument) = rest.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "wlrix-clipboard {}\n\n\
                     The clipboard history for the wlRIX desktop. Records what is copied and\n\
                     puts it back when it is chosen in wlrix-clipboard-history. Started by\n\
                     wlrix-session; needs a running compositor with wlr-data-control and a\n\
                     session D-Bus.\n\n\
                     Usage: wlrix-clipboard [options]\n\n\
                     Options:\n  \
                       --check-config <path>  say whether that file would be accepted, exit\n  \
                       -h, --help             this message\n  \
                       -V, --version          print the version\n\n\
                     Settings live in ~/.config/wlrix/clipboard.toml, or\n\
                     /etc/wlrix/clipboard.toml. The history is kept in\n\
                     ~/.local/share/wlrix/clipboard.",
                    env!("CARGO_PKG_VERSION")
                );
                return;
            }
            "--version" | "-V" => {
                println!("wlrix-clipboard {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            // Answers a question about a file rather than doing anything with it, so it needs no
            // compositor and no bus. `wlrix-settings-daemon` runs this against a candidate file
            // before renaming it into place.
            "--check-config" => {
                let Some(path) = rest.next() else {
                    eprintln!("wlrix-clipboard: --check-config needs a path");
                    std::process::exit(2);
                };
                if let Err(why) = wlrix_clipboard::config::check(std::path::Path::new(path)) {
                    eprintln!("{why}");
                    std::process::exit(1);
                }
                return;
            }
            other => {
                eprintln!("wlrix-clipboard: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    if let Err(err) = wlrix_clipboard::daemon::run() {
        eprintln!("wlrix-clipboard: {err}");
        std::process::exit(1);
    }
}
