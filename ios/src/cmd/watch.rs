use anyhow::Result;
use ios_rs::usbmux::{Connection, Event};

pub fn run() -> Result<()> {
    let conn = Connection::open()?;
    let mut listener = conn.listen()?;
    eprintln!("Watching for device events (Ctrl-C to stop)…");
    loop {
        match listener.next()? {
            Event::DeviceAttached(d) => {
                println!("+ {} [{}] product={:#06x}",
                    d.serial, d.connection_type, d.product_id);
            }
            Event::DeviceDetached { device_id } => {
                println!("- device_id={device_id}");
            }
            _ => {}
        }
    }
}
