#!/usr/bin/env just --justfile
name := 'wlrix-clipboard'

rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
bin-dir := base-dir / 'bin'

bin-src := 'target' / 'release' / name
bin-dst := bin-dir / name

default:
  @just --list

release:
  cargo build --release

lint:
  cargo clippy --all-targets -- -D warnings

test:
  cargo test

# Install the daemon.
#
# One file. There is deliberately no `.desktop` entry: the Toolchest builds its menu by scanning
# them, and the daemon is not something a person starts -- `wlrix-session` starts it, and a second
# copy started by hand would fail on the bus name and exit. The popup a person *does* start is
# `wlrix-clipboard-history`, installed with the other apps.
#
# Deliberately does not build: this is normally run as root, and building as root leaves a target
# directory nobody can write to afterwards.
#
#     just release && sudo just install
[doc("Install the daemon (build first; run as root)")]
install:
  #!/usr/bin/env bash
  set -euo pipefail
  if [ ! -x '{{bin-src}}' ]; then
    echo "no release build -- run 'just release' first" >&2
    exit 1
  fi
  install -Dm0755 '{{bin-src}}' '{{bin-dst}}'
  echo "installed {{bin-dst}}"
  echo
  echo "wlrix-session starts this by name off PATH. Settings go in"
  echo "~/.config/wlrix/clipboard.toml; see data/clipboard.toml for every key."

[doc("Remove what install put down")]
uninstall:
  #!/usr/bin/env bash
  set -euo pipefail
  rm -f '{{bin-dst}}'
  echo "removed {{bin-dst}}"

clean:
  cargo clean
