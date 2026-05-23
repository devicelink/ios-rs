use std::io::IsTerminal;

#[derive(Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OutputMode {
    #[default]
    Auto,
    Text,
    Json,
}

impl OutputMode {
    pub fn is_json(self) -> bool {
        match self {
            OutputMode::Json => true,
            OutputMode::Text => false,
            OutputMode::Auto => !std::io::stdout().is_terminal(),
        }
    }
}

pub fn print_json<T: serde::Serialize>(val: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(val)?);
    Ok(())
}

/// Standard envelope for action-only commands (pair, reboot, erase, …).
#[derive(serde::Serialize)]
pub struct ActionResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ActionResult {
    pub fn ok() -> Self { Self { ok: true, message: None } }
    pub fn with_msg(msg: impl Into<String>) -> Self { Self { ok: true, message: Some(msg.into()) } }
}

pub fn plist_to_json(v: &plist::Value) -> serde_json::Value {
    match v {
        plist::Value::String(s)      => serde_json::Value::String(s.clone()),
        plist::Value::Boolean(b)     => serde_json::Value::Bool(*b),
        plist::Value::Integer(i)     => {
            if let Some(n) = i.as_signed() { serde_json::json!(n) }
            else { serde_json::json!(i.as_unsigned().unwrap_or(0)) }
        }
        plist::Value::Real(f)        => serde_json::json!(f),
        plist::Value::Data(b)        => serde_json::Value::String(
            b.iter().map(|x| format!("{x:02x}")).collect()
        ),
        plist::Value::Array(arr)     => serde_json::Value::Array(arr.iter().map(plist_to_json).collect()),
        plist::Value::Dictionary(d)  => {
            let map: serde_json::Map<_, _> = d.iter()
                .map(|(k, v)| (k.clone(), plist_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        plist::Value::Date(d)        => serde_json::Value::String(format!("{d:?}")),
        _                            => serde_json::Value::Null,
    }
}
