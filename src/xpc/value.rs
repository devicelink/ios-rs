use std::collections::HashMap;

/// Typed XPC value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int64(i64),
    Uint64(u64),
    Double(f64),
    /// Raw bytes (plist `<data>` equivalent)
    Data(Vec<u8>),
    String(String),
    Uuid([u8; 16]),
    Array(Vec<Value>),
    Dictionary(HashMap<String, Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self { Some(s) } else { None }
    }
    pub fn as_dict(&self) -> Option<&HashMap<String, Value>> {
        if let Value::Dictionary(d) = self { Some(d) } else { None }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        if let Value::Array(a) = self { Some(a) } else { None }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Uint64(n) => Some(*n),
            Value::Int64(n)  => Some(*n as u64),
            _                => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int64(n)  => Some(*n),
            Value::Uint64(n) => Some(*n as i64),
            _                => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(*b) } else { None }
    }
}

impl From<bool>   for Value { fn from(v: bool)   -> Self { Value::Bool(v) } }
impl From<i64>    for Value { fn from(v: i64)    -> Self { Value::Int64(v) } }
impl From<u64>    for Value { fn from(v: u64)    -> Self { Value::Uint64(v) } }
impl From<f64>    for Value { fn from(v: f64)    -> Self { Value::Double(v) } }
impl From<String> for Value { fn from(v: String) -> Self { Value::String(v) } }
impl From<&str>   for Value { fn from(v: &str)   -> Self { Value::String(v.to_owned()) } }
impl From<Vec<u8>> for Value { fn from(v: Vec<u8>) -> Self { Value::Data(v) } }

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null            => write!(f, "null"),
            Value::Bool(b)         => write!(f, "{b}"),
            Value::Int64(n)        => write!(f, "{n}"),
            Value::Uint64(n)       => write!(f, "{n}"),
            Value::Double(d)       => write!(f, "{d}"),
            Value::Data(b)         => write!(f, "<{} bytes>", b.len()),
            Value::String(s)       => write!(f, "{s}"),
            Value::Uuid(u)         => {
                let h: String = u.iter().map(|b| format!("{b:02x}")).collect();
                write!(f, "{h}")
            }
            Value::Array(a)        => write!(f, "[{} items]", a.len()),
            Value::Dictionary(d)   => write!(f, "{{{} keys}}", d.len()),
        }
    }
}
