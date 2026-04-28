use std::io;
use std::net::{TcpListener, TcpStream};
use std::thread;

use anyhow::Result;
use usbmux::Connection;

use super::resolve_device;

pub fn run(udid: Option<&str>, device_port: u16, listen_port: u16) -> Result<()> {
    let device = resolve_device(udid)?;

    let listener = TcpListener::bind(("127.0.0.1", listen_port))?;
    let actual_port = listener.local_addr()?.port();
    eprintln!(
        "Relaying 127.0.0.1:{actual_port} → device {} port {device_port}",
        device.serial
    );

    for incoming in listener.incoming() {
        let client = incoming?;
        let device_id = device.device_id;
        thread::spawn(move || {
            if let Err(e) = handle(client, device_id, device_port) {
                eprintln!("relay error: {e}");
            }
        });
    }
    Ok(())
}

fn handle(client: TcpStream, device_id: u32, device_port: u16) -> Result<()> {
    let mux = Connection::open()?;
    let tunnel = mux.open_tunnel(device_id, device_port)?;

    // Bidirectional copy between the local TCP client and the usbmux tunnel.
    // We need two directions concurrently — use two threads sharing clones.
    let client_read  = client.try_clone()?;
    let client_write = client;
    let tunnel_read  = tunnel.try_clone()?;
    let tunnel_write = tunnel;

    let t1 = thread::spawn(move || copy_half(client_read, tunnel_write));
    let t2 = thread::spawn(move || copy_half(tunnel_read, client_write));

    let _ = t1.join();
    let _ = t2.join();
    Ok(())
}

fn copy_half(mut src: impl io::Read, mut dst: impl io::Write) {
    let mut buf = [0u8; 16384];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() { break; }
            }
        }
    }
}
