//! Wire format of the control API: versioned JSON requests and responses, one per line.
//!
//! Request: `{"v":1,"method":"agents","params":{…},"id":…}` (`params` and `id` optional).
//! Response: `{"ok":true,"result":…,"id":…}` or `{"ok":false,"error":{"code","message"},"id":…}`,
//! with `id` echoed when the request had one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// A parsed request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

impl Request {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            method: method.into(),
            params,
            id: None,
        }
    }
}

/// Machine-readable error kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The line is not JSON, or is too long.
    ParseError,
    /// JSON, but not a request object (`v` or `method` missing or mistyped).
    InvalidRequest,
    UnsupportedVersion,
    UnknownMethod,
    InvalidParams,
    /// No agent or run matches.
    NotFound,
    /// Known method that this build cannot do yet.
    NotSupported,
    /// The app is not ready (no data yet) or not answering.
    Unavailable,
    /// The app will not do it (e.g. closing a subagent or the Queen).
    Refused,
    /// The app took too long to reply.
    Timeout,
    /// The action ran and failed (e.g. Orca refused the input).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlError {
    pub code: ErrorCode,
    pub message: String,
}

impl ControlError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Failed, message)
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = serde_json::to_value(self.code)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        write!(f, "{code}: {}", self.message)
    }
}

impl std::error::Error for ControlError {}

/// A response line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

impl Response {
    pub fn from_result(result: Result<Value, ControlError>, id: Option<Value>) -> Self {
        match result {
            Ok(value) => Self {
                ok: true,
                result: Some(value),
                error: None,
                id,
            },
            Err(error) => Self {
                ok: false,
                result: None,
                error: Some(error),
                id,
            },
        }
    }

    pub fn into_result(self) -> Result<Value, ControlError> {
        match (self.ok, self.error) {
            (true, _) => Ok(self.result.unwrap_or(Value::Null)),
            (false, Some(e)) => Err(e),
            (false, None) => Err(ControlError::failed("error response without an error")),
        }
    }
}

/// Parses a request line; the error keeps whatever `id` could be recovered.
pub fn parse_request(line: &str) -> Result<Request, (ControlError, Option<Value>)> {
    let value: Value = serde_json::from_str(line).map_err(|e| {
        (
            ControlError::new(ErrorCode::ParseError, format!("invalid JSON: {e}")),
            None,
        )
    })?;
    let Value::Object(obj) = value else {
        return Err((
            ControlError::new(ErrorCode::InvalidRequest, "request must be a JSON object"),
            None,
        ));
    };
    let id = obj.get("id").cloned();
    let invalid = |m: &str| (ControlError::new(ErrorCode::InvalidRequest, m), id.clone());
    let v = obj
        .get("v")
        .ok_or_else(|| invalid("missing \"v\" (protocol version)"))?
        .as_u64()
        .ok_or_else(|| invalid("\"v\" must be a positive integer"))?;
    if v != u64::from(PROTOCOL_VERSION) {
        return Err((
            ControlError::new(
                ErrorCode::UnsupportedVersion,
                format!("protocol version {v} not supported (this app speaks {PROTOCOL_VERSION})"),
            ),
            id,
        ));
    }
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing or non-string \"method\""))?
        .to_owned();
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    if !(params.is_null() || params.is_object()) {
        return Err((
            ControlError::invalid_params("\"params\" must be an object"),
            id,
        ));
    }
    Ok(Request {
        v: PROTOCOL_VERSION,
        method,
        params,
        id,
    })
}

/// Typed access to a request's `params` object.
pub struct Params<'a> {
    map: Option<&'a Map<String, Value>>,
}

impl<'a> Params<'a> {
    pub fn new(params: &'a Value) -> Self {
        Self {
            map: params.as_object(),
        }
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.map?.get(key).filter(|v| !v.is_null())
    }

    pub fn str(&self, key: &str) -> Result<Option<&'a str>, ControlError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(ControlError::invalid_params(format!(
                "\"{key}\" must be a string"
            ))),
        }
    }

    pub fn required_str(&self, key: &str) -> Result<&'a str, ControlError> {
        self.str(key)?
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ControlError::invalid_params(format!("missing \"{key}\"")))
    }

    pub fn bool(&self, key: &str) -> Result<Option<bool>, ControlError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(ControlError::invalid_params(format!(
                "\"{key}\" must be a boolean"
            ))),
        }
    }

    pub fn usize(&self, key: &str) -> Result<Option<usize>, ControlError> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| {
                    ControlError::invalid_params(format!(
                        "\"{key}\" must be a non-negative integer"
                    ))
                }),
        }
    }

    /// A list of strings; a single string counts as a one-element list.
    pub fn strings(&self, key: &str) -> Result<Option<Vec<String>>, ControlError> {
        let bad = || ControlError::invalid_params(format!("\"{key}\" must be a string list"));
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(vec![s.clone()])),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_owned).ok_or_else(bad))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(bad()),
        }
    }

    /// Whether `key` is present, even as `null`.
    pub fn has(&self, key: &str) -> bool {
        self.map.is_some_and(|m| m.contains_key(key))
    }
}
