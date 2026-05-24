use anyhow::Result;
use ios_rs::usbmux::{Connection, Event};

use crate::cmd::output::OutputMode;

#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WatchEvent {
    Attached {
        udid: String,
        connection: String,
        product_id: u16,
    },
    Detached {
        device_id: u32,
    },
}

pub fn run(output: OutputMode) -> Result<()> {
    let conn = Connection::open()?;
    let mut listener = conn.listen()?;
    if !output.is_json() {
        eprintln!("Watching for device events (Ctrl-C to stop)…");
    }
    loop {
        match listener.next()? {
            Event::DeviceAttached(d) => {
                if output.is_json() {
                    let event = WatchEvent::Attached {
                        udid: d.serial.clone(),
                        connection: d.connection_type.to_string(),
                        product_id: d.product_id,
                    };
                    println!("{}", serde_json::to_string(&event)?);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                } else {
                    println!(
                        "+ {} [{}] product={:#06x}",
                        d.serial, d.connection_type, d.product_id
                    );
                }
            }
            Event::DeviceDetached { device_id } => {
                if output.is_json() {
                    let event = WatchEvent::Detached { device_id };
                    println!("{}", serde_json::to_string(&event)?);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                } else {
                    println!("- device_id={device_id}");
                }
            }
            _ => {}
        }
    }
}
