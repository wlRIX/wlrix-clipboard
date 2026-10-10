# wlrix-clipboard

The clipboard history for the wlRIX desktop. Remembers what was copied — text and images — and puts any of it back on
request.

- **Language:** Rust
- **License:** GPL-3.0-or-later
- **Reference:** KDE's Klipper

It is a daemon with no window. The popup that shows the history is `wlrix-clipboard-history`, in
[`wlrix-apps`](../wlrix-apps); `Super+V` opens it, as does **Desktop ▸ Clipboard History** in the Toolchest. The popup
talks to this program over the session bus and never touches the clipboard itself.

## What it does

- **Records every copy** of plain text or an image, newest first. Copying something that is already there moves it to
  the top rather than adding it twice.
- **Puts an entry back** when it is chosen in the popup, by becoming the clipboard's owner and answering pastes for it.
- **Keeps the clipboard full.** On Wayland the clipboard belongs to the program that copied, and goes empty when that
  program exits — copy from a terminal, close it, and there is nothing to paste. When that happens this program puts
  the entry back. At login the newest entry from last time is put on the clipboard.
- **Keeps stars.** Starred entries do not count toward `max_entries`, are never evicted, and survive **Clear history**.
- **Skips passwords.** A copy a password manager marks with `x-kde-passwordManagerHint` (KeePassXC does) is never
  recorded.

The **primary selection** — highlighted text, pasted with the middle button — is not recorded. Highlighting something
is not copying it.

When an entry is put back, plain text is offered as plain text and an image in the type it came in. The rich half of a
copy (a browser's HTML, a spreadsheet's own format) is not kept.

## Using it

`wlrix-session` starts it. By hand:

```sh
cargo run
```

It needs a compositor with `zwlr_data_control_manager_v1` — `wlrix-compositor` has it, and refuses it to sandboxed
clients — and a session D-Bus. A second copy exits at once on the bus name.

## Configuration

`~/.config/wlrix/clipboard.toml`, falling back to `/etc/wlrix/clipboard.toml`. Every key is optional;
[`data/clipboard.toml`](data/clipboard.toml) lists them with their defaults.

| Key                   | Default    | Meaning                                                          |
|-----------------------|------------|------------------------------------------------------------------|
| `max_entries`         | `50`       | unstarred entries kept                                           |
| `persist`             | `true`     | keep the history across logins (read once, at startup)          |
| `keep_clipboard_full` | `true`     | put the last entry back when the clipboard goes empty            |
| `max_entry_bytes`     | `16777216` | the largest copy recorded                                        |

Unknown keys are an error. `wlrix-clipboard --check-config <path>` says whether a file would be accepted, and `SIGHUP`
re-reads it; `wlrix-settings-daemon` does both.

## Where the history is

```text
~/.local/share/wlrix/clipboard/      mode 0700
    history.toml                     the index, and every text entry
    blobs/<id>.png                   one file per image entry
```

Written owner-only, and replaced by rename rather than rewritten in place. With `persist = false` the same layout is
kept under `$XDG_RUNTIME_DIR/wlrix-clipboard/` instead, and emptied at startup.

## The bus

```text
bus name   com.wlrix.Clipboard
object     /com/wlrix/Clipboard
interface  com.wlrix.Clipboard1

GetHistory() -> a(tsssbx)   id, kind ("text" | "image"), text, image path, starred, created
GetActive()  -> t           the entry on the clipboard, or 0
Activate(t)                 put an entry on the clipboard
SetText(t, s)               edit a text entry
Remove(t)
SetStarred(t, b)
Clear()                     delete every unstarred entry
signal HistoryChanged()
```

Images are sent by path, not by content; the popup reads the file.

## Testing it

The history rules, the type choice, the store and the config are tested without a compositor:

```console
$ cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check
```

The rest needs a compositor. `wl-clipboard` is not everywhere, so two probes stand in for it. Run them, and the daemon,
against a nested `wlrix-compositor` so the real clipboard is left alone:

```console
$ export WAYLAND_DISPLAY=wayland-2 XDG_DATA_HOME=/tmp/clip-test
$ cargo run &
$ cargo run --example copy_probe -- "text 1"
$ cargo run --example copy_probe -- --secret hunter2     # not recorded
$ cargo run --example copy_probe -- --image shot.png
$ cargo run --example paste_probe                        # what is on the clipboard now
$ busctl --user call com.wlrix.Clipboard /com/wlrix/Clipboard com.wlrix.Clipboard1 GetHistory
```

Killing a `copy_probe` is the "program that copied it exited" case: `paste_probe` should still find the entry.
`copy_probe --clear-first` copies the way Edge does from an InPrivate window — empty the clipboard, then set it — which
the daemon must not answer by putting the previous entry back over the new one. `watch_probe` logs every clipboard
change without reading or setting anything, which is how that case was found; it is safe to run against a live session.

## Build

```sh
cargo build
just release && sudo just install
```
