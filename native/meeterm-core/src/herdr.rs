//! Bounded, public Herdr JSON/NDJSON wire handling.
//!
//! This module deliberately contains only wire and model code. It does not
//! open sockets, start a Herdr process, or own ordering/generation state. The
//! connection actor is responsible for those concerns and can use the parsed
//! stable terminal_id together with the current pane_id alias.

use std::collections::{HashMap, HashSet};
use std::fmt;

use base64::Engine;
use serde_json::{Map, Value};

/// The direct terminal CLI inherits this limit from Herdr's normal wire frame
/// limit. The JSON envelope is allowed a little more room by
/// DEFAULT_MAX_NDJSON_LINE_BYTES for base64 and metadata.
pub(crate) const MAX_TERMINAL_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_NDJSON_LINE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SNAPSHOT_ENTITIES: usize = 4096;
pub(crate) const MAX_SNAPSHOT_ID_BYTES: usize = 512;
pub(crate) const MAX_SNAPSHOT_NAME_BYTES: usize = 4096;
pub(crate) const MAX_TERMINAL_DIMENSION: u16 = 4096;

/// A decoding or command-construction failure in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HerdrError {
    InvalidArgument(String),
    InvalidJson(String),
    LineTooLong { max: usize },
    IncompleteLine { bytes: usize },
    InvalidRecord(String),
    InvalidSnapshot(String),
    InvalidFrame(String),
    InvalidResponse(String),
    InvalidCommand(String),
    Base64(String),
}

impl fmt::Display for HerdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) => write!(f, "invalid argument: {message}"),
            Self::InvalidJson(message) => write!(f, "invalid JSON: {message}"),
            Self::LineTooLong { max } => write!(f, "NDJSON line exceeds {max} bytes"),
            Self::IncompleteLine { bytes } => {
                write!(f, "NDJSON input ended with an incomplete {bytes}-byte line")
            }
            Self::InvalidRecord(message) => write!(f, "invalid Herdr record: {message}"),
            Self::InvalidSnapshot(message) => {
                write!(f, "invalid Herdr session snapshot: {message}")
            }
            Self::InvalidFrame(message) => write!(f, "invalid Herdr terminal frame: {message}"),
            Self::InvalidResponse(message) => {
                write!(f, "invalid Herdr API response: {message}")
            }
            Self::InvalidCommand(message) => write!(f, "invalid Herdr command: {message}"),
            Self::Base64(message) => {
                write!(f, "invalid Herdr terminal frame base64: {message}")
            }
        }
    }
}

impl std::error::Error for HerdrError {}

/// Incremental newline-delimited JSON decoder.
///
/// A caller should stop using the decoder after an error. push accepts
/// arbitrary chunks, including a chunk that ends in the middle of a UTF-8 or
/// JSON value. Blank lines are ignored; every non-blank line must be valid
/// JSON. The line limit applies before parsing, so malformed or hostile input
/// cannot grow the pending buffer without bound.
#[derive(Debug, Clone)]
pub(crate) struct NdjsonDecoder {
    pending: Vec<u8>,
    max_line_bytes: usize,
}

impl NdjsonDecoder {
    pub(crate) fn new(max_line_bytes: usize) -> Result<Self, HerdrError> {
        if max_line_bytes == 0 {
            return Err(HerdrError::InvalidArgument(
                "NDJSON line limit must be greater than zero".to_owned(),
            ));
        }
        Ok(Self {
            pending: Vec::new(),
            max_line_bytes,
        })
    }

    pub(crate) fn with_default_limit() -> Self {
        Self::new(DEFAULT_MAX_NDJSON_LINE_BYTES).expect("default NDJSON limit is non-zero")
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, HerdrError> {
        let mut values = Vec::new();
        let mut start = 0;

        while let Some(relative_end) = bytes[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + relative_end;
            self.append_segment(&bytes[start..end])?;
            if let Some(value) = self.take_line()? {
                values.push(value);
            }
            start = end + 1;
        }

        self.append_segment(&bytes[start..])?;
        Ok(values)
    }

    /// Finish a stream and reject a final unterminated non-empty line.
    pub(crate) fn finish(&mut self) -> Result<Vec<Value>, HerdrError> {
        if self.pending.is_empty() || self.pending.iter().all(u8::is_ascii_whitespace) {
            self.pending.clear();
            return Ok(Vec::new());
        }
        Err(HerdrError::IncompleteLine {
            bytes: self.pending.len(),
        })
    }

    fn append_segment(&mut self, segment: &[u8]) -> Result<(), HerdrError> {
        let new_len =
            self.pending
                .len()
                .checked_add(segment.len())
                .ok_or(HerdrError::LineTooLong {
                    max: self.max_line_bytes,
                })?;
        if new_len > self.max_line_bytes {
            return Err(HerdrError::LineTooLong {
                max: self.max_line_bytes,
            });
        }
        self.pending.extend_from_slice(segment);
        Ok(())
    }

    fn take_line(&mut self) -> Result<Option<Value>, HerdrError> {
        while self.pending.last() == Some(&b'\r') {
            self.pending.pop();
        }
        if self.pending.iter().all(u8::is_ascii_whitespace) {
            self.pending.clear();
            return Ok(None);
        }
        let value = serde_json::from_slice(&self.pending)
            .map_err(|error| HerdrError::InvalidJson(error.to_string()))?;
        self.pending.clear();
        Ok(Some(value))
    }
}

/// Status values from the public snapshot schema. Unknown future values are
/// intentionally represented as Unknown so a newer server remains usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HerdrSessionSnapshot {
    pub(crate) version: String,
    pub(crate) protocol: u32,
    pub(crate) focused_workspace_id: Option<String>,
    pub(crate) focused_tab_id: Option<String>,
    pub(crate) focused_pane_id: Option<String>,
    pub(crate) workspaces: Vec<HerdrWorkspace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HerdrWorkspace {
    pub(crate) workspace_id: String,
    pub(crate) number: usize,
    pub(crate) name: String,
    pub(crate) focused: bool,
    pub(crate) agent_status: AgentStatus,
    pub(crate) groups: Vec<HerdrGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HerdrGroup {
    pub(crate) tab_id: String,
    pub(crate) workspace_id: String,
    pub(crate) number: usize,
    pub(crate) name: String,
    pub(crate) focused: bool,
    pub(crate) agent_status: AgentStatus,
    pub(crate) panes: Vec<HerdrPane>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HerdrPane {
    /// Herdr's current mutable pane alias.
    pub(crate) pane_id: String,
    /// Stable terminal identity used for terminal state.
    pub(crate) terminal_id: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
    pub(crate) name: Option<String>,
    pub(crate) focused: bool,
    pub(crate) agent_name: Option<String>,
    pub(crate) agent_status: AgentStatus,
}

/// Decode the public v0.9.0 flat snapshot into a hierarchy while retaining
/// empty workspaces and tabs. Counter/layout fields are intentionally not
/// used as identity; explicit references and IDs are authoritative.
pub(crate) fn decode_session_snapshot(value: &Value) -> Result<HerdrSessionSnapshot, HerdrError> {
    let root = as_object(value, "snapshot")?;
    let version =
        bounded_required_string(root, "version", "snapshot", MAX_SNAPSHOT_NAME_BYTES)?.to_owned();
    let protocol = required_u64(root, "protocol", "snapshot")?
        .try_into()
        .map_err(|_| HerdrError::InvalidSnapshot("protocol does not fit in u32".to_owned()))?;
    let focused_workspace_id = optional_id(root, "focused_workspace_id", "snapshot")?;
    let focused_tab_id = optional_id(root, "focused_tab_id", "snapshot")?;
    let focused_pane_id = optional_id(root, "focused_pane_id", "snapshot")?;

    let workspace_values = required_array(root, "workspaces", "snapshot")?;
    let tab_values = required_array(root, "tabs", "snapshot")?;
    let pane_values = required_array(root, "panes", "snapshot")?;
    for (kind, values) in [
        ("workspaces", workspace_values),
        ("tabs", tab_values),
        ("panes", pane_values),
    ] {
        if values.len() > MAX_SNAPSHOT_ENTITIES {
            return Err(HerdrError::InvalidSnapshot(format!(
                "{kind} exceeds {MAX_SNAPSHOT_ENTITIES} entries"
            )));
        }
    }

    let mut workspaces = Vec::with_capacity(workspace_values.len());
    let mut workspace_indexes = HashMap::with_capacity(workspace_values.len());
    for (index, value) in workspace_values.iter().enumerate() {
        let object = as_object(value, "workspace")?;
        let workspace_id = required_id(object, "workspace_id", "workspace")?;
        if workspace_indexes
            .insert(workspace_id.clone(), index)
            .is_some()
        {
            return Err(HerdrError::InvalidSnapshot(format!(
                "duplicate workspace_id {workspace_id:?}"
            )));
        }
        let workspace = HerdrWorkspace {
            workspace_id,
            number: required_usize(object, "number", "workspace")?,
            name: bounded_required_string(object, "label", "workspace", MAX_SNAPSHOT_NAME_BYTES)?
                .to_owned(),
            focused: required_bool(object, "focused", "workspace")?,
            agent_status: optional_status(object, "agent_status", "workspace")?,
            groups: Vec::new(),
        };
        // This remains present even for an empty workspace; validate its wire
        // type without using it as a hierarchy edge.
        let _ =
            bounded_required_string(object, "active_tab_id", "workspace", MAX_SNAPSHOT_ID_BYTES)?;
        workspaces.push(workspace);
    }

    let mut group_indexes = HashMap::with_capacity(tab_values.len());
    for value in tab_values {
        let object = as_object(value, "tab")?;
        let tab_id = required_id(object, "tab_id", "tab")?;
        if group_indexes.contains_key(&tab_id) {
            return Err(HerdrError::InvalidSnapshot(format!(
                "duplicate tab_id {tab_id:?}"
            )));
        }
        let workspace_id = required_id(object, "workspace_id", "tab")?;
        let workspace_index = *workspace_indexes.get(&workspace_id).ok_or_else(|| {
            HerdrError::InvalidSnapshot(format!(
                "tab {tab_id:?} references missing workspace {workspace_id:?}"
            ))
        })?;
        let group_index = workspaces[workspace_index].groups.len();
        workspaces[workspace_index].groups.push(HerdrGroup {
            tab_id: tab_id.clone(),
            workspace_id: workspace_id.clone(),
            number: required_usize(object, "number", "tab")?,
            name: bounded_required_string(object, "label", "tab", MAX_SNAPSHOT_NAME_BYTES)?
                .to_owned(),
            focused: required_bool(object, "focused", "tab")?,
            agent_status: optional_status(object, "agent_status", "tab")?,
            panes: Vec::new(),
        });
        group_indexes.insert(tab_id, (workspace_index, group_index));
    }

    let mut terminal_ids = HashSet::with_capacity(pane_values.len());
    let mut pane_ids = HashSet::with_capacity(pane_values.len());
    for value in pane_values {
        let object = as_object(value, "pane")?;
        let pane_id = required_id(object, "pane_id", "pane")?;
        let terminal_id = required_id(object, "terminal_id", "pane")?;
        if !pane_ids.insert(pane_id.clone()) {
            return Err(HerdrError::InvalidSnapshot(format!(
                "duplicate pane_id {pane_id:?}"
            )));
        }
        if !terminal_ids.insert(terminal_id.clone()) {
            return Err(HerdrError::InvalidSnapshot(format!(
                "duplicate terminal_id {terminal_id:?}"
            )));
        }
        let workspace_id = required_id(object, "workspace_id", "pane")?;
        let tab_id = required_id(object, "tab_id", "pane")?;
        let (workspace_index, group_index) = *group_indexes.get(&tab_id).ok_or_else(|| {
            HerdrError::InvalidSnapshot(format!(
                "pane {pane_id:?} references missing tab {tab_id:?}"
            ))
        })?;
        if workspaces[workspace_index].workspace_id != workspace_id {
            return Err(HerdrError::InvalidSnapshot(format!(
                "pane {pane_id:?} workspace does not match its tab"
            )));
        }
        workspaces[workspace_index].groups[group_index]
            .panes
            .push(HerdrPane {
                pane_id,
                terminal_id,
                workspace_id,
                tab_id,
                name: optional_bounded_string(object, "label", "pane", MAX_SNAPSHOT_NAME_BYTES)?,
                focused: required_bool(object, "focused", "pane")?,
                agent_name: optional_bounded_string(
                    object,
                    "agent",
                    "pane",
                    MAX_SNAPSHOT_NAME_BYTES,
                )?,
                agent_status: optional_status(object, "agent_status", "pane")?,
            });
    }

    validate_focus_id(
        focused_workspace_id.as_deref(),
        |id| workspace_indexes.contains_key(id),
        "focused_workspace_id",
    )?;
    validate_focus_id(
        focused_tab_id.as_deref(),
        |id| group_indexes.contains_key(id),
        "focused_tab_id",
    )?;
    validate_focus_id(
        focused_pane_id.as_deref(),
        |id| pane_ids.contains(id),
        "focused_pane_id",
    )?;

    Ok(HerdrSessionSnapshot {
        version,
        protocol,
        focused_workspace_id,
        focused_tab_id,
        focused_pane_id,
        workspaces,
    })
}

fn validate_focus_id<F>(id: Option<&str>, contains: F, field: &str) -> Result<(), HerdrError>
where
    F: FnOnce(&str) -> bool,
{
    if let Some(id) = id
        && !contains(id)
    {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{field} references missing id {id:?}"
        )));
    }
    Ok(())
}

/// A frame emitted by Herdr's public terminal JSON client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalFrame {
    pub(crate) seq: u64,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) full: bool,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TerminalRecord {
    Frame(TerminalFrame),
    Closed {
        reason: Option<String>,
    },
    Error {
        code: Option<String>,
        message: String,
    },
    Ready {
        info: Value,
    },
    Info {
        info: Value,
    },
    Other(Value),
}

/// Decode one public terminal JSON record. Sequence ordering and generation
/// checks intentionally remain outside this pure decoder.
pub(crate) fn decode_terminal_record(value: &Value) -> Result<TerminalRecord, HerdrError> {
    let object = as_object(value, "terminal record")?;
    let record_type = required_string(object, "type", "terminal record")?;
    match record_type {
        "terminal.frame" => Ok(TerminalRecord::Frame(decode_terminal_frame(object)?)),
        "terminal.closed" => Ok(TerminalRecord::Closed {
            reason: optional_bounded_string(
                object,
                "reason",
                "terminal.closed",
                MAX_SNAPSHOT_NAME_BYTES,
            )?,
        }),
        "error" | "terminal.error" => decode_terminal_error(object),
        "ready" | "terminal.ready" => Ok(TerminalRecord::Ready {
            info: value.clone(),
        }),
        "info" => Ok(TerminalRecord::Info {
            info: value.clone(),
        }),
        _ => Ok(TerminalRecord::Other(value.clone())),
    }
}

fn decode_terminal_frame(object: &Map<String, Value>) -> Result<TerminalFrame, HerdrError> {
    let encoding = required_string(object, "encoding", "terminal.frame")?;
    if encoding != "ansi" {
        return Err(HerdrError::InvalidFrame(format!(
            "unsupported encoding {encoding:?}"
        )));
    }
    let seq = required_u64(object, "seq", "terminal.frame")?;
    let width = required_u64(object, "width", "terminal.frame")?
        .try_into()
        .map_err(|_| HerdrError::InvalidFrame("width does not fit in u16".to_owned()))?;
    let height = required_u64(object, "height", "terminal.frame")?
        .try_into()
        .map_err(|_| HerdrError::InvalidFrame("height does not fit in u16".to_owned()))?;
    if width == 0 || height == 0 {
        return Err(HerdrError::InvalidFrame(
            "width and height must be greater than zero".to_owned(),
        ));
    }
    if width > MAX_TERMINAL_DIMENSION || height > MAX_TERMINAL_DIMENSION {
        return Err(HerdrError::InvalidFrame(format!(
            "width and height must not exceed {MAX_TERMINAL_DIMENSION}"
        )));
    }
    let full = required_bool(object, "full", "terminal.frame")?;
    let encoded = required_string(object, "bytes", "terminal.frame")?;
    let max_encoded = (MAX_TERMINAL_FRAME_BYTES.saturating_add(2) / 3).saturating_mul(4);
    if encoded.len() > max_encoded {
        return Err(HerdrError::InvalidFrame(format!(
            "base64 payload exceeds {MAX_TERMINAL_FRAME_BYTES} decoded bytes"
        )));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| HerdrError::Base64(error.to_string()))?;
    if bytes.len() > MAX_TERMINAL_FRAME_BYTES {
        return Err(HerdrError::InvalidFrame(format!(
            "decoded payload exceeds {MAX_TERMINAL_FRAME_BYTES} bytes"
        )));
    }
    Ok(TerminalFrame {
        seq,
        width,
        height,
        full,
        bytes,
    })
}

fn decode_terminal_error(object: &Map<String, Value>) -> Result<TerminalRecord, HerdrError> {
    let nested = object.get("error").and_then(Value::as_object);
    let code = match nested.and_then(|value| value.get("code")) {
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| HerdrError::InvalidRecord("error.code must be a string".to_owned()))?
                .to_owned(),
        ),
        None => optional_bounded_string(object, "code", "error", MAX_SNAPSHOT_ID_BYTES)?,
    };
    let message = match nested.and_then(|value| value.get("message")) {
        Some(value) => value
            .as_str()
            .ok_or_else(|| HerdrError::InvalidRecord("error.message must be a string".to_owned()))?
            .to_owned(),
        None => {
            bounded_required_string(object, "message", "error", MAX_SNAPSHOT_NAME_BYTES)?.to_owned()
        }
    };
    if code
        .as_deref()
        .is_some_and(|value| value.len() > MAX_SNAPSHOT_ID_BYTES)
        || message.len() > MAX_SNAPSHOT_NAME_BYTES
    {
        return Err(HerdrError::InvalidRecord(
            "error code or message exceeds its size limit".to_owned(),
        ));
    }
    Ok(TerminalRecord::Error { code, message })
}

/// The public API response shape used by Herdr's JSON API.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApiResponse {
    Ok {
        id: String,
        result: Value,
    },
    Error {
        id: String,
        code: String,
        message: String,
    },
}

/// Construct one newline-terminated public API request.
pub(crate) fn request(id: &str, method: &str, params: &Value) -> Result<Vec<u8>, HerdrError> {
    validate_wire_token(id, "request id")?;
    validate_wire_token(method, "request method")?;
    let value = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    let mut bytes =
        serde_json::to_vec(&value).map_err(|error| HerdrError::InvalidJson(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Validate a response against the request ID and preserve either the result
/// or the public code/message error without accepting an ambiguous shape.
pub(crate) fn response(expected_id: &str, value: &Value) -> Result<ApiResponse, HerdrError> {
    validate_wire_token(expected_id, "expected response id")?;
    let object = as_object(value, "response")?;
    let id = required_string(object, "id", "response")?.to_owned();
    if id.len() > MAX_SNAPSHOT_ID_BYTES {
        return Err(HerdrError::InvalidResponse(format!(
            "response id exceeds {MAX_SNAPSHOT_ID_BYTES} bytes"
        )));
    }
    if id != expected_id {
        return Err(HerdrError::InvalidResponse(format!(
            "response id {id:?} does not match request id {expected_id:?}"
        )));
    }
    match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(ApiResponse::Ok {
            id,
            result: result.clone(),
        }),
        (None, Some(error)) => {
            let error = as_object(error, "response.error")?;
            Ok(ApiResponse::Error {
                id,
                code: bounded_required_string(
                    error,
                    "code",
                    "response.error",
                    MAX_SNAPSHOT_ID_BYTES,
                )?
                .to_owned(),
                message: bounded_required_string(
                    error,
                    "message",
                    "response.error",
                    MAX_SNAPSHOT_NAME_BYTES,
                )?
                .to_owned(),
            })
        }
        (Some(_), Some(_)) => Err(HerdrError::InvalidResponse(
            "response cannot contain both result and error".to_owned(),
        )),
        (None, None) => Err(HerdrError::InvalidResponse(
            "response must contain result or error".to_owned(),
        )),
    }
}

/// Quote one remote shell argument using the single-quote convention used by
/// the official Herdr remote launcher. Control characters and NUL are rejected
/// before quoting rather than being passed through a shell.
pub(crate) fn shell_quote(value: &str) -> Result<String, HerdrError> {
    if value.chars().any(char::is_control) {
        return Err(HerdrError::InvalidCommand(
            "shell argument contains a control character or NUL".to_owned(),
        ));
    }
    if value.len() > MAX_SNAPSHOT_NAME_BYTES {
        return Err(HerdrError::InvalidCommand(format!(
            "shell argument exceeds {MAX_SNAPSHOT_NAME_BYTES} bytes"
        )));
    }
    if value.is_empty() {
        return Ok("''".to_owned());
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(&byte))
    {
        return Ok(value.to_owned());
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

/// Build a remote Herdr command. The session selector is always explicit so
/// inherited HERDR_SESSION or socket environment cannot redirect the command.
pub(crate) fn command(runtime: Option<&str>, args: &[&str]) -> Result<String, HerdrError> {
    let session = runtime.unwrap_or("default");
    if session.is_empty() {
        return Err(HerdrError::InvalidCommand(
            "Herdr session name cannot be empty".to_owned(),
        ));
    }
    let mut parts = vec![
        "herdr".to_owned(),
        "--session".to_owned(),
        shell_quote(session)?,
    ];
    parts.extend(
        args.iter()
            .map(|arg| shell_quote(arg))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(parts.join(" "))
}

fn as_object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, HerdrError> {
    value
        .as_object()
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context} must be an object")))
}

fn required_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a Vec<Value>, HerdrError> {
    let value = object
        .get(key)
        .ok_or_else(|| HerdrError::InvalidSnapshot(format!("{context}.{key} is missing")))?;
    value
        .as_array()
        .ok_or_else(|| HerdrError::InvalidSnapshot(format!("{context}.{key} must be an array")))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a str, HerdrError> {
    object
        .get(key)
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} is missing")))?
        .as_str()
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} must be a string")))
}

fn bounded_required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
    max_bytes: usize,
) -> Result<&'a str, HerdrError> {
    let value = required_string(object, key, context)?;
    if value.len() > max_bytes {
        return Err(HerdrError::InvalidRecord(format!(
            "{context}.{key} exceeds {max_bytes} bytes"
        )));
    }
    Ok(value)
}

fn required_u64(object: &Map<String, Value>, key: &str, context: &str) -> Result<u64, HerdrError> {
    object
        .get(key)
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} is missing")))?
        .as_u64()
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} must be an integer")))
}

fn required_usize(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<usize, HerdrError> {
    required_u64(object, key, context)?
        .try_into()
        .map_err(|_| HerdrError::InvalidSnapshot(format!("{context}.{key} does not fit in usize")))
}

fn required_bool(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<bool, HerdrError> {
    object
        .get(key)
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} is missing")))?
        .as_bool()
        .ok_or_else(|| HerdrError::InvalidRecord(format!("{context}.{key} must be a boolean")))
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<String>, HerdrError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(HerdrError::InvalidRecord(format!(
            "{context}.{key} must be a string or null"
        ))),
    }
}

fn optional_bounded_string(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
    max_bytes: usize,
) -> Result<Option<String>, HerdrError> {
    let value = optional_string(object, key, context)?;
    if value
        .as_deref()
        .is_some_and(|value| value.len() > max_bytes)
    {
        return Err(HerdrError::InvalidRecord(format!(
            "{context}.{key} exceeds {max_bytes} bytes"
        )));
    }
    Ok(value)
}

fn required_id(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<String, HerdrError> {
    let value = required_string(object, key, context)?;
    if value.is_empty() {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{context}.{key} cannot be empty"
        )));
    }
    if value.len() > MAX_SNAPSHOT_ID_BYTES {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{context}.{key} exceeds {MAX_SNAPSHOT_ID_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{context}.{key} contains a control character"
        )));
    }
    Ok(value.to_owned())
}

fn optional_id(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<String>, HerdrError> {
    let value = optional_string(object, key, context)?;
    if value.as_deref().is_some_and(str::is_empty) {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{context}.{key} cannot be empty"
        )));
    }
    if value
        .as_deref()
        .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(HerdrError::InvalidSnapshot(format!(
            "{context}.{key} contains a control character"
        )));
    }
    Ok(value)
}

fn optional_status(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<AgentStatus, HerdrError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(AgentStatus::Unknown),
        Some(Value::String(value)) => Ok(match value.as_str() {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }),
        Some(_) => Err(HerdrError::InvalidRecord(format!(
            "{context}.{key} must be a string or null"
        ))),
    }
}

fn validate_wire_token(value: &str, context: &str) -> Result<(), HerdrError> {
    if value.chars().any(char::is_control) {
        return Err(HerdrError::InvalidArgument(format!(
            "{context} contains a control character or NUL"
        )));
    }
    if value.len() > MAX_SNAPSHOT_ID_BYTES {
        return Err(HerdrError::InvalidArgument(format!(
            "{context} exceeds {MAX_SNAPSHOT_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ndjson_handles_split_lines_and_crlf() {
        let mut decoder = NdjsonDecoder::new(128).unwrap();
        assert!(decoder.push(b"{\"type\":\"ready\"}\r").unwrap().is_empty());
        let values = decoder.push(b"\n{\"type\":\"info\"}\n").unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["type"], "ready");
        assert_eq!(values[1]["type"], "info");
        assert!(decoder.finish().unwrap().is_empty());
    }

    #[test]
    fn ndjson_rejects_malformed_and_oversized_lines() {
        let mut malformed = NdjsonDecoder::new(64).unwrap();
        assert!(matches!(
            malformed.push(br#"{"#),
            Ok(values) if values.is_empty()
        ));
        assert!(malformed.finish().is_err());

        let mut invalid = NdjsonDecoder::new(64).unwrap();
        assert!(matches!(
            invalid.push(b"not-json\n"),
            Err(HerdrError::InvalidJson(_))
        ));

        let mut oversized = NdjsonDecoder::new(4).unwrap();
        assert!(matches!(
            oversized.push(b"12345"),
            Err(HerdrError::LineTooLong { max: 4 })
        ));
    }

    #[test]
    fn terminal_frame_decodes_and_records_lifecycle_messages() {
        let frame = json!({
            "type": "terminal.frame",
            "seq": 7,
            "encoding": "ansi",
            "width": 80,
            "height": 24,
            "full": true,
            "bytes": "G1sybg=="
        });
        let TerminalRecord::Frame(frame) = decode_terminal_record(&frame).unwrap() else {
            panic!("expected frame")
        };
        assert_eq!(frame.bytes, b"\x1b[2n");
        assert_eq!(frame.width, 80);

        let closed = json!({"type":"terminal.closed","reason":"server stopped"});
        assert!(matches!(
            decode_terminal_record(&closed).unwrap(),
            TerminalRecord::Closed { reason: Some(reason) } if reason == "server stopped"
        ));
        let error = json!({"type":"error","error":{"code":"busy","message":"taken"}});
        assert!(matches!(
            decode_terminal_record(&error).unwrap(),
            TerminalRecord::Error { code: Some(code), message } if code == "busy" && message == "taken"
        ));
        assert!(decode_terminal_record(&json!({"type":"terminal.frame","seq":0})).is_err());
    }

    #[test]
    fn snapshot_preserves_empty_hierarchy_and_stable_terminal_identity() {
        let snapshot = json!({
            "version":"0.9.0",
            "protocol":1,
            "focused_workspace_id":"ws-1",
            "focused_tab_id":"tab-1",
            "focused_pane_id":"pane-current",
            "workspaces":[
                {"workspace_id":"ws-empty","number":1,"label":"Empty","focused":false,"pane_count":0,"tab_count":0,"active_tab_id":"","agent_status":"idle"},
                {"workspace_id":"ws-1","number":2,"label":"Project","focused":true,"pane_count":1,"tab_count":2,"active_tab_id":"tab-1","agent_status":"working"}
            ],
            "tabs":[
                {"tab_id":"tab-empty","workspace_id":"ws-1","number":1,"label":"No panes","focused":false,"pane_count":0,"agent_status":"done"},
                {"tab_id":"tab-1","workspace_id":"ws-1","number":2,"label":"Shell","focused":true,"pane_count":1,"agent_status":"future_status"}
            ],
            "panes":[
                {"pane_id":"pane-current","terminal_id":"terminal-stable","workspace_id":"ws-1","tab_id":"tab-1","focused":true,"label":"main","agent":"Claude","agent_status":"blocked"}
            ],
            "layouts":[],
            "agents":[]
        });
        let decoded = decode_session_snapshot(&snapshot).unwrap();
        assert_eq!(decoded.workspaces.len(), 2);
        assert!(decoded.workspaces[0].groups.is_empty());
        assert_eq!(decoded.workspaces[1].groups.len(), 2);
        assert!(decoded.workspaces[1].groups[0].panes.is_empty());
        let pane = &decoded.workspaces[1].groups[1].panes[0];
        assert_eq!(pane.terminal_id, "terminal-stable");
        assert_eq!(pane.pane_id, "pane-current");
        assert_eq!(pane.agent_name.as_deref(), Some("Claude"));
        assert_eq!(pane.agent_status, AgentStatus::Blocked);
        assert_eq!(
            decoded.workspaces[1].groups[1].agent_status,
            AgentStatus::Unknown
        );
    }

    #[test]
    fn snapshot_rejects_alias_or_hierarchy_mismatch() {
        let snapshot = json!({
            "version":"0.9.0",
            "protocol":1,
            "workspaces":[
                {"workspace_id":"ws","number":1,"label":"W","focused":false,"active_tab_id":"tab","agent_status":"idle"}
            ],
            "tabs":[
                {"tab_id":"tab","workspace_id":"ws","number":1,"label":"T","focused":false,"agent_status":"idle"}
            ],
            "panes":[
                {"pane_id":"pane","terminal_id":"term","workspace_id":"ws","tab_id":"tab","focused":false,"agent_status":"idle"},
                {"pane_id":"pane-2","terminal_id":"term","workspace_id":"ws","tab_id":"tab","focused":false,"agent_status":"idle"}
            ]
        });
        assert!(decode_session_snapshot(&snapshot).is_err());
    }

    #[test]
    fn snapshot_and_frame_limits_are_bounded() {
        let mut too_many = json!({
            "version":"0.9.0",
            "protocol":1,
            "workspaces":[],
            "tabs":[],
            "panes":[]
        });
        too_many["workspaces"] = Value::Array(
            (0..=MAX_SNAPSHOT_ENTITIES)
                .map(|index| {
                    json!({
                        "workspace_id": format!("ws-{index}"),
                        "number": index,
                        "label": "workspace",
                        "focused": false,
                        "active_tab_id": "",
                        "agent_status": "idle"
                    })
                })
                .collect(),
        );
        assert!(decode_session_snapshot(&too_many).is_err());

        let mut too_long_name = json!({
            "version":"0.9.0",
            "protocol":1,
            "workspaces":[{
                "workspace_id":"ws",
                "number":1,
                "label":"workspace",
                "focused":false,
                "active_tab_id":"",
                "agent_status":"idle"
            }],
            "tabs":[],
            "panes":[]
        });
        too_long_name["workspaces"][0]["label"] =
            Value::String("x".repeat(MAX_SNAPSHOT_NAME_BYTES + 1));
        assert!(decode_session_snapshot(&too_long_name).is_err());

        let too_wide = json!({
            "type":"terminal.frame",
            "seq":1,
            "encoding":"ansi",
            "width": MAX_TERMINAL_DIMENSION as u64 + 1,
            "height": 24,
            "full": true,
            "bytes":""
        });
        assert!(decode_terminal_record(&too_wide).is_err());
    }

    #[test]
    fn api_request_and_response_validate_id_and_shape() {
        let bytes = request("r1", "session.snapshot", &json!({})).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["method"],
            "session.snapshot"
        );
        let ok = response("r1", &json!({"id":"r1","result":{"type":"ok"}})).unwrap();
        assert!(matches!(ok, ApiResponse::Ok { .. }));
        assert!(response("r1", &json!({"id":"other","result":{}})).is_err());
        assert!(response("r1", &json!({"id":"r1","result":{},"error":{}})).is_err());
        assert!(request("r\n1", "ping", &json!({})).is_err());
    }

    #[test]
    fn shell_command_quotes_injection_and_selects_default_explicitly() {
        assert_eq!(shell_quote("plain-name").unwrap(), "plain-name");
        assert_eq!(shell_quote("a; rm -rf /").unwrap(), "'a; rm -rf /'");
        assert!(shell_quote("bad\nname").is_err());
        assert_eq!(
            command(None, &["session.snapshot", "a; rm -rf /"]).unwrap(),
            "herdr --session default session.snapshot 'a; rm -rf /'"
        );
        assert_eq!(
            command(Some("work"), &["status", "--json"]).unwrap(),
            "herdr --session work status --json"
        );
    }
}
