use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionType {
    Usb,
    Network,
    Unknown(String),
}

impl std::fmt::Display for ConnectionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionType::Usb => write!(f, "USB"),
            ConnectionType::Network => write!(f, "Network"),
            ConnectionType::Unknown(s) => write!(f, "{s}"),
        }
    }
}

impl From<&str> for ConnectionType {
    fn from(s: &str) -> Self {
        match s {
            "USB" => ConnectionType::Usb,
            "Network" => ConnectionType::Network,
            other => ConnectionType::Unknown(other.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Device {
    pub device_id: u32,
    pub serial: String,
    pub connection_type: ConnectionType,
    pub product_id: u16,
    pub location_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultCode {
    Ok,
    BadCommand,
    BadDevice,
    ConnRefused,
    NoSuchService,
    BadVersion,
    Unknown(u32),
}

impl std::fmt::Display for ResultCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResultCode::Ok => write!(f, "OK"),
            ResultCode::BadCommand => write!(f, "bad command"),
            ResultCode::BadDevice => write!(f, "bad device"),
            ResultCode::ConnRefused => write!(f, "connection refused"),
            ResultCode::NoSuchService => write!(f, "no such service"),
            ResultCode::BadVersion => write!(f, "bad version"),
            ResultCode::Unknown(n) => write!(f, "unknown error {n}"),
        }
    }
}

impl From<u32> for ResultCode {
    fn from(n: u32) -> Self {
        match n {
            0 => ResultCode::Ok,
            1 => ResultCode::BadCommand,
            2 => ResultCode::BadDevice,
            3 => ResultCode::ConnRefused,
            4 => ResultCode::NoSuchService,
            6 => ResultCode::BadVersion,
            n => ResultCode::Unknown(n),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    DeviceList(Vec<Device>),
    DeviceAttached(Device),
    DeviceDetached {
        device_id: u32,
    },
    /// usbmux tunnel is now open — stop using the codec, use the raw socket
    Connected {
        tag: u32,
    },
    RequestOk {
        tag: u32,
    },
    RequestFailed {
        tag: u32,
        code: ResultCode,
    },
    Buid(String),
    PairRecord {
        udid: String,
        record: Vec<u8>,
    },
}

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("plist error: {0}")]
    Plist(#[from] plist::Error),
    #[error("frame too short: got {got}, need {need}")]
    FrameTooShort { got: usize, need: usize },
    #[error("unexpected message type in response: {0}")]
    UnexpectedMessage(String),
}
