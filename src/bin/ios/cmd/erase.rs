use std::io::{self, Write};

use anyhow::{bail, Context, Result};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

pub fn run(udid: Option<&str>, mode: ConnectionMode, yes: bool, output: OutputMode) -> Result<()> {
    if !yes {
        eprint!("This will erase ALL data on the device. Type YES to confirm: ");
        io::stderr().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if line.trim() != "YES" {
            bail!("aborted");
        }
    }

    let mut session = open_session(udid, mode)?;
    let ld = session.lockdown();

    let mut stream = ld
        .connect_service("com.apple.mobile.obliteration")
        .context("connect obliteration service")?;

    let mut req = plist::Dictionary::new();
    req.insert(
        "Request".into(),
        plist::Value::String("ObliterateDevice".into()),
    );

    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &plist::Value::Dictionary(req))?;
    let len = body.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;

    if output.is_json() {
        print_json(&ActionResult::with_msg("erase initiated"))?;
    } else {
        eprintln!("erase initiated — device will reset to factory defaults");
    }
    Ok(())
}
