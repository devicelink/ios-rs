//! Device diagnostics and control via `com.apple.mobile.diagnostics_relay`.
//!
//! On iOS 17.4+ use the RSD shim; on older iOS connect via lockdownd directly.
//!
//! # Quick start
//!
//! ```ignore
//! let mut diag = DiagnosticsClient::connect(&mut session)?;
//! let battery  = diag.battery()?;
//! println!("{}%  charging={}", battery.capacity_pct, battery.is_charging);
//! diag.restart()?;
//! ```
use std::io::{Read, Write};

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;
use plist::Value;

const SERVICE: &str = "com.apple.mobile.diagnostics_relay";

// ── public types ──────────────────────────────────────────────────────────────

/// Battery and charging state.
#[derive(Debug, Clone)]
pub struct BatteryInfo {
    /// State of charge as a percentage (0–100).
    pub capacity_pct: u64,
    /// Voltage in millivolts.
    pub voltage_mv: u64,
    /// Number of charge cycles.
    pub cycle_count: u64,
    /// Design capacity in mAh.
    pub design_capacity: u64,
    /// Full charge capacity in mAh.
    pub full_capacity: u64,
    pub is_charging: bool,
    pub external_connected: bool,
    pub fully_charged: bool,
}

// ── client ────────────────────────────────────────────────────────────────────

pub struct DiagnosticsClient {
    stream: MuxSocket,
}

impl DiagnosticsClient {
    /// Connect via lockdownd.
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(DiagnosticsClient {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Build from a pre-connected stream (e.g. RSD shim).
    pub fn from_stream(stream: MuxSocket) -> Self {
        DiagnosticsClient { stream }
    }

    // ── device control ────────────────────────────────────────────────────────

    /// Reboot the device.
    pub fn restart(&mut self) -> Result<(), Error> {
        self.request("Restart")
    }

    /// Power off the device.
    pub fn shutdown(&mut self) -> Result<(), Error> {
        self.request("Shutdown")
    }

    /// Put the device to sleep.
    pub fn sleep(&mut self) -> Result<(), Error> {
        self.request("Sleep")
    }

    // ── diagnostics queries ───────────────────────────────────────────────────

    /// Return the raw full diagnostics dictionary from the device.
    pub fn all(&mut self) -> Result<plist::Dictionary, Error> {
        self.query("All")
    }

    /// Return battery / gas-gauge information.
    pub fn battery(&mut self) -> Result<BatteryInfo, Error> {
        let diag = self.query("GasGauge")?;
        let gg = diag
            .get("GasGauge")
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| Error::Afc("diagnostics: no GasGauge in response".into()))?;

        Ok(BatteryInfo {
            capacity_pct: gg_u64(gg, "BatteryCurrentCapacity"),
            voltage_mv: gg_u64(gg, "BatteryVoltage"),
            cycle_count: gg_u64(gg, "CycleCount"),
            design_capacity: gg_u64(gg, "DesignCapacity"),
            full_capacity: gg_u64(gg, "FullChargeCapacity"),
            is_charging: gg_bool(gg, "BatteryIsCharging"),
            external_connected: gg_bool(gg, "ExternalConnected"),
            fully_charged: gg_bool(gg, "FullyCharged"),
        })
    }

    // ── internals ─────────────────────────────────────────────────────────────

    /// Send a control request (Restart / Shutdown / Sleep) and read the ack.
    fn request(&mut self, req: &str) -> Result<(), Error> {
        let mut d = plist::Dictionary::new();
        d.insert("Request".into(), Value::String(req.into()));
        self.send(&Value::Dictionary(d))?;
        let resp = self.recv()?;
        let status = resp
            .as_dictionary()
            .and_then(|d| d.get("Status"))
            .and_then(|v| v.as_string())
            .unwrap_or("");
        if status != "Success" && !status.is_empty() {
            return Err(Error::Afc(format!("diagnostics {req}: status {status:?}")));
        }
        Ok(())
    }

    /// Send a diagnostics query and return the inner `Diagnostics` dict.
    fn query(&mut self, req: &str) -> Result<plist::Dictionary, Error> {
        let mut d = plist::Dictionary::new();
        d.insert("Request".into(), Value::String(req.into()));
        self.send(&Value::Dictionary(d))?;
        let resp = self.recv()?;
        if let Some(err) = resp
            .as_dictionary()
            .and_then(|d| d.get("Error"))
            .and_then(|v| v.as_string())
        {
            return Err(Error::Afc(format!("diagnostics {req}: {err}")));
        }
        resp.as_dictionary()
            .and_then(|d| d.get("Diagnostics"))
            .and_then(|v| v.as_dictionary())
            .cloned()
            .ok_or_else(|| Error::Afc(format!("diagnostics {req}: no Diagnostics in response")))
    }

    fn send(&mut self, value: &Value) -> Result<(), Error> {
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, value)?;
        let len = body.len() as u32;
        self.stream.write_all(&len.to_be_bytes())?;
        self.stream.write_all(&body)?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Value, Error> {
        let mut len_buf = [0u8; 4];
        read_exact(&mut self.stream, &mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 4 * 1024 * 1024 {
            return Err(Error::Afc(format!("diagnostics: implausible length {len}")));
        }
        let mut body = vec![0u8; len];
        read_exact(&mut self.stream, &mut body)?;
        Ok(plist::from_bytes(&body)?)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn gg_u64(d: &plist::Dictionary, key: &str) -> u64 {
    d.get(key)
        .and_then(|v| {
            v.as_unsigned_integer()
                .or_else(|| v.as_signed_integer().map(|i| i as u64))
        })
        .unwrap_or(0)
}

fn gg_bool(d: &plist::Dictionary, key: &str) -> bool {
    d.get(key).and_then(|v| v.as_boolean()).unwrap_or(false)
}

fn read_exact(s: &mut MuxSocket, buf: &mut [u8]) -> Result<(), Error> {
    let mut done = 0;
    while done < buf.len() {
        let n = s.read(&mut buf[done..])?;
        if n == 0 {
            return Err(Error::Closed);
        }
        done += n;
    }
    Ok(())
}
