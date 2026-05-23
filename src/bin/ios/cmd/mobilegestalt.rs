use anyhow::{Context, Result};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{plist_to_json, print_json, OutputMode};

#[derive(serde::Serialize)]
struct GestaltResult {
    key:   String,
    value: serde_json::Value,
}

pub fn query(udid: Option<&str>, mode: ConnectionMode, key: &str, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    // MobileGestalt keys are exposed via lockdownd root-domain GetValue.
    let val = session.lockdown()
        .get_value(None, key)
        .with_context(|| format!("MobileGestalt key {key:?}"))?;

    if output.is_json() {
        let result = GestaltResult {
            key:   key.to_owned(),
            value: plist_to_json(&val),
        };
        return print_json(&result);
    }

    print_value(&val);
    Ok(())
}

fn print_value(val: &plist::Value) {
    match val {
        plist::Value::String(s)  => println!("{s}"),
        plist::Value::Integer(i) => println!("{i}"),
        plist::Value::Real(f)    => println!("{f}"),
        plist::Value::Boolean(b) => println!("{b}"),
        plist::Value::Data(b)    => println!("{}", hex(b)),
        _ => {
            let mut buf = Vec::new();
            let _ = plist::to_writer_xml(&mut buf, val);
            print!("{}", String::from_utf8_lossy(&buf));
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join("")
}
