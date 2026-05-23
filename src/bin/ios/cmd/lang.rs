use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use plist::Value;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

const DOMAIN: &str = "com.apple.international";

#[derive(serde::Serialize)]
struct LangInfo {
    language: Option<String>,
    locale:   Option<String>,
}

pub fn run(
    udid:        Option<&str>,
    set_lang:    Option<&str>,
    set_locale:  Option<&str>,
    mode:        ConnectionMode,
    output:      OutputMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let ld = session.lockdown();

    if set_lang.is_none() && set_locale.is_none() {
        // Display current language and locale
        let lang   = ld.get_value(Some(DOMAIN), "Language").ok()
            .and_then(|v| if let Value::String(s) = v { Some(s) } else { None });
        let locale = ld.get_value(Some(DOMAIN), "Locale").ok()
            .and_then(|v| if let Value::String(s) = v { Some(s) } else { None });

        if output.is_json() {
            return print_json(&LangInfo { language: lang, locale });
        }

        let mut table = Table::new();
        table.load_preset(UTF8_FULL_CONDENSED);
        table.set_header(["Setting", "Value"]);
        table.add_row(["Language", lang.as_deref().unwrap_or("-")]);
        table.add_row(["Locale",   locale.as_deref().unwrap_or("-")]);
        println!("{table}");
        return Ok(());
    }

    if let Some(lang) = set_lang {
        ld.set_value(Some(DOMAIN), "Language", Value::String(lang.into()))?;
        if !output.is_json() {
            println!("Language set to '{lang}' — device will restart SpringBoard.");
        }
    }
    if let Some(locale) = set_locale {
        ld.set_value(Some(DOMAIN), "Locale", Value::String(locale.into()))?;
        if !output.is_json() {
            println!("Locale set to '{locale}'.");
        }
    }

    if output.is_json() {
        print_json(&ActionResult::ok())?;
    }

    Ok(())
}
