// SPDX-License-Identifier: GPL-3.0-or-later
//! Put something on the clipboard and hold it, to drive the daemon without `wl-clipboard`.
//!
//! Stays running as the clipboard's owner until somebody else takes the clipboard, or until it
//! is killed -- which is the "program that copied it exited" case `keep_clipboard_full` is for.
//!
//! ```sh
//! cargo run --example copy_probe -- "some text"
//! cargo run --example copy_probe -- --secret "hunter2"     # as a password manager would
//! cargo run --example copy_probe -- --image shot.png       # offered as image/png
//! ```

use std::os::fd::OwnedFd;

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

struct Probe {
    bytes: Vec<u8>,
    done: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (types, bytes): (Vec<&str>, Vec<u8>) = match args.as_slice() {
        [flag, path] if flag == "--image" => (
            vec!["image/png"],
            std::fs::read(path).expect("read the image"),
        ),
        [flag, text] if flag == "--secret" => (
            vec!["text/plain;charset=utf-8", "x-kde-passwordManagerHint"],
            text.clone().into_bytes(),
        ),
        [text] => (
            vec!["text/plain;charset=utf-8", "text/plain"],
            text.clone().into_bytes(),
        ),
        _ => {
            eprintln!("usage: copy_probe [--secret] <text> | --image <file.png>");
            std::process::exit(2);
        }
    };

    let connection = Connection::connect_to_env().expect("no compositor");
    let (globals, mut queue) = registry_queue_init::<Probe>(&connection).expect("no registry");
    let qh = queue.handle();
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("the compositor has no wlr-data-control");
    let seat: WlSeat = globals.bind(&qh, 1..=8, ()).expect("no seat");
    let device = manager.get_data_device(&seat, &qh, ());
    let source = manager.create_data_source(&qh, ());
    for mime in &types {
        source.offer((*mime).to_owned());
    }
    device.set_selection(Some(&source));

    let mut probe = Probe { bytes, done: false };
    queue.roundtrip(&mut probe).expect("lost the compositor");
    println!("holding the clipboard as {}", types.join(", "));
    while !probe.done {
        queue
            .blocking_dispatch(&mut probe)
            .expect("lost the compositor");
    }
    println!("somebody else took the clipboard");
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                // The hint's value, as KeePassXC sends it; everything else gets the payload.
                let bytes = if mime_type == "x-kde-passwordManagerHint" {
                    b"secret".to_vec()
                } else {
                    probe.bytes.clone()
                };
                write(fd, &bytes);
                println!("answered a paste as {mime_type}");
            }
            zwlr_data_control_source_v1::Event::Cancelled => probe.done = true,
            _ => {}
        }
    }
}

fn write(fd: OwnedFd, bytes: &[u8]) {
    use std::io::Write;
    let _ = std::fs::File::from(fd).write_all(bytes);
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Probe {
    fn event(
        _probe: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        _event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(Probe, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

macro_rules! ignore_events {
    ($($ty:ty $(: $data:ty)?),* $(,)?) => {$(
        impl Dispatch<$ty, ignore_events!(@data $($data)?)> for Probe {
            fn event(
                _probe: &mut Self,
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
    ZwlrDataControlOfferV1,
);
