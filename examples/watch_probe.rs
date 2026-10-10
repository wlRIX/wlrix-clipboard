// SPDX-License-Identifier: GPL-3.0-or-later
//! Log every clipboard change, to see what a program actually does when it copies.
//!
//! Never reads a selection and never sets one, so running it changes nothing: it prints each
//! offer's types and each `selection` event, with the time since start, until killed. A daemon's
//! own selections show up with `application/x-wlrix-clipboard` among their types.
//!
//! ```sh
//! cargo run --example watch_probe
//! ```

use std::time::Instant;

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
};

struct Probe {
    start: Instant,
}

impl Probe {
    fn stamp(&self) -> String {
        format!("{:>9.3}s", self.start.elapsed().as_secs_f64())
    }
}

fn main() {
    let connection = Connection::connect_to_env().expect("no compositor");
    let (globals, mut queue) = registry_queue_init::<Probe>(&connection).expect("no registry");
    let qh = queue.handle();
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("the compositor has no wlr-data-control");
    let seat: WlSeat = globals.bind(&qh, 1..=8, ()).expect("no seat");
    let _device = manager.get_data_device(&seat, &qh, ());

    let mut probe = Probe {
        start: Instant::now(),
    };
    loop {
        queue
            .blocking_dispatch(&mut probe)
            .expect("lost the compositor");
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                println!("{} offer {}", probe.stamp(), id.id().protocol_id());
            }
            zwlr_data_control_device_v1::Event::Selection { id } => match id {
                Some(offer) => println!(
                    "{} selection = offer {}",
                    probe.stamp(),
                    offer.id().protocol_id()
                ),
                None => println!("{} selection = EMPTY", probe.stamp()),
            },
            // Destroyed at once: only the clipboard is of interest here.
            zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                offer.destroy();
            }
            zwlr_data_control_device_v1::Event::Finished => {
                println!("{} device finished", probe.stamp());
                std::process::exit(0);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Probe, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            println!(
                "{}   offer {} type {mime_type}",
                probe.stamp(),
                offer.id().protocol_id()
            );
        }
    }
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
    ZwlrDataControlSourceV1,
);
