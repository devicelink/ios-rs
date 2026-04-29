use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use plist::Value;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

const DOMAIN: &str = "com.apple.international";

pub fn run(
    udid:        Option<&str>,
    set_lang:    Option<&str>,
    set_locale:  Option<&str>,
    mode:        ConnectionMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let ld = session.lockdown();

    if set_lang.is_none() && set_locale.is_none() {
        // Display current language and locale
        let lang   = ld.get_value(Some(DOMAIN), "Language")?;
        let locale = ld.get_value(Some(DOMAIN), "Locale")?;

        let mut table = Table::new();
        table.load_preset(UTF8_FULL_CONDENSED);
        table.set_header(["Setting", "Value"]);
        table.add_row(["Language", lang.as_string().unwrap_or("-")]);
        table.add_row(["Locale",   locale.as_string().unwrap_or("-")]);
        println!("{table}");
        return Ok(());
    }

    if let Some(lang) = set_lang {
        ld.set_value(Some(DOMAIN), "Language", Value::String(lang.into()))?;
        println!("Language set to '{lang}' — device will restart SpringBoard.");
    }
    if let Some(locale) = set_locale {
        ld.set_value(Some(DOMAIN), "Locale", Value::String(locale.into()))?;
        println!("Locale set to '{locale}'.");
    }

    Ok(())
}
