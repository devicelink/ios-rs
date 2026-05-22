use anyhow::{Context, Result};

use crate::cmd::resolve_device;

pub fn pair(udid: Option<&str>) -> Result<()> {
    let device = resolve_device(udid)?;
    ios_rs::lockdown::pairing::pair(device.device_id, &device.serial)
        .context("pairing")?;
    println!("paired successfully — pair record saved to usbmuxd");
    Ok(())
}

pub fn unpair(udid: Option<&str>) -> Result<()> {
    let device = resolve_device(udid)?;
    ios_rs::lockdown::pairing::unpair(device.device_id, &device.serial)
        .context("unpair")?;
    println!("unpaired — pair record deleted");
    Ok(())
}
