//! JSON-RPC message contracts and instance-scoped runtime environment.
//!
//! Every message that crosses a process or UI boundary is serialised through
//! the types below, so the wire format stays stable regardless of which crate
//! produces or consumes it.

use std::path::PathBuf;

/// A request sent to the runtime. `params` defaults to `null` when omitted.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RpcRequest {
    /// Correlation id echoed back by the matching [`RpcResponse`].
    pub id: String,
    /// Dot-separated method name, e.g. `buffer.append`.
    pub method: String,
    /// Optional positional or named parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// The reply to an [`RpcRequest`]. Exactly one of `result` / `error` is set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RpcResponse {
    /// Correlation id copied from the originating [`RpcRequest`].
    pub id: String,
    /// Fast success discriminator for consumers that do not inspect `result`.
    pub ok: bool,
    /// Payload on success; omitted from the wire when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error code / message on failure; omitted from the wire when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    /// Successful reply carrying `result`.
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// Failed reply carrying only a message, no structured detail.
    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }

    /// Failed reply whose `result` slot carries structured detail
    /// (for example a validation report or a remediation hint).
    pub fn err_with_detail(
        id: impl Into<String>,
        error: impl Into<String>,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: Some(detail),
            error: Some(error.into()),
        }
    }
}

/// A topic-tagged notification pushed from the runtime to subscribers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RpcEvent {
    /// Topic name, e.g. `buffer.changed`.
    pub topic: String,
    /// Event body.
    pub payload: serde_json::Value,
}

/// Instance-scoped runtime environment resolved from process variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// Root directory for per-instance state (databases, buffers, logs).
    pub data_dir: PathBuf,
    /// Identifier that isolates this instance from sibling instances.
    pub instance_id: String,
}

impl RuntimeEnv {
    /// Resolves the environment from the real process variables.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Resolves the environment through an injectable lookup so that tests
    /// never depend on the ambient process state.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        Self {
            data_dir: resolve_data_dir(&lookup),
            instance_id: resolve_instance_id(&lookup),
        }
    }

    /// Name of the named-mutex used to detect a duplicate instance.
    pub fn mutex_name(&self) -> String {
        format!("Local\\syki-{}", self.instance_id)
    }

    /// Per-instance WebView2 user-data folder.
    pub fn webview_user_data_dir(&self) -> PathBuf {
        self.data_dir.join("webview2")
    }
}

/// `SYKI_DATA_DIR` wins when it is non-blank; otherwise the roaming app-data
/// folder is used, with a relative directory as the last-resort fallback.
fn resolve_data_dir<F: Fn(&str) -> Option<String>>(lookup: &F) -> PathBuf {
    if let Some(raw) = lookup("SYKI_DATA_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(appdata) = lookup("APPDATA") {
        let trimmed = appdata.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("syki-sok");
        }
    }
    PathBuf::from("syki-sok")
}

/// `SYKI_INSTANCE_ID` wins when it is non-blank; otherwise the primary
/// instance name is used.
fn resolve_instance_id<F: Fn(&str) -> Option<String>>(lookup: &F) -> String {
    if let Some(raw) = lookup("SYKI_INSTANCE_ID") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "syki-sok-main".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A lookup backed by a fixed map, so tests stay hermetic.
    fn lookup_from(entries: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn rpc_request_round_trip_preserves_fields() {
        let req = RpcRequest {
            id: "r1".into(),
            method: "buffer.append".into(),
            params: json!({ "text": "hello", "lines": 2 }),
        };
        let wire = serde_json::to_string(&req).expect("request serialises");
        let back: RpcRequest = serde_json::from_str(&wire).expect("request deserialises");
        assert_eq!(req, back);
    }

    #[test]
    fn rpc_request_params_default_to_null() {
        let back: RpcRequest =
            serde_json::from_str(r#"{"id":"r2","method":"ui.focus"}"#).expect("minimal request");
        assert_eq!(back.params, serde_json::Value::Null);
    }

    #[test]
    fn rpc_response_ok_round_trip_omits_error() {
        let resp = RpcResponse::ok("r1", json!({ "count": 3 }));
        let wire = serde_json::to_string(&resp).expect("response serialises");
        assert!(!wire.contains("\"error\""));
        let back: RpcResponse = serde_json::from_str(&wire).expect("response deserialises");
        assert_eq!(resp, back);
        assert!(back.ok);
    }

    #[test]
    fn rpc_response_err_round_trip_omits_result() {
        let resp = RpcResponse::err("r2", "not_found");
        let wire = serde_json::to_string(&resp).expect("response serialises");
        assert!(!wire.contains("\"result\""));
        let back: RpcResponse = serde_json::from_str(&wire).expect("response deserialises");
        assert_eq!(resp, back);
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("not_found"));
    }

    #[test]
    fn rpc_response_err_with_detail_keeps_structured_result() {
        let detail = json!({ "hint": "run `syki-cli config init`" });
        let resp = RpcResponse::err_with_detail("r3", "invalid_state", detail.clone());
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("invalid_state"));
        assert_eq!(resp.result, Some(detail));

        let wire = serde_json::to_string(&resp).expect("detail response serialises");
        let back: RpcResponse =
            serde_json::from_str(&wire).expect("detail response deserialises");
        assert_eq!(resp, back);
    }

    #[test]
    fn rpc_event_round_trip_preserves_fields() {
        let ev = RpcEvent {
            topic: "buffer.changed".into(),
            payload: json!({ "id": 42, "dirty": true }),
        };
        let wire = serde_json::to_string(&ev).expect("event serialises");
        let back: RpcEvent = serde_json::from_str(&wire).expect("event deserialises");
        assert_eq!(ev, back);
    }

    #[test]
    fn runtime_env_prefers_explicit_overrides() {
        let env = RuntimeEnv::from_lookup(lookup_from(&[
            ("SYKI_DATA_DIR", "  D:\\custom\\syki  "),
            ("APPDATA", "C:\\Users\\someone\\AppData\\Roaming"),
            ("SYKI_INSTANCE_ID", " worker-7 "),
        ]));
        assert_eq!(env.data_dir, PathBuf::from("D:\\custom\\syki"));
        assert_eq!(env.instance_id, "worker-7");
        assert_eq!(env.mutex_name(), "Local\\syki-worker-7");
        assert_eq!(
            env.webview_user_data_dir(),
            PathBuf::from("D:\\custom\\syki").join("webview2")
        );
    }

    #[test]
    fn runtime_env_falls_back_to_appdata_and_primary_instance() {
        let env = RuntimeEnv::from_lookup(lookup_from(&[(
            "APPDATA",
            "C:\\Users\\someone\\AppData\\Roaming",
        )]));
        assert_eq!(
            env.data_dir,
            PathBuf::from("C:\\Users\\someone\\AppData\\Roaming").join("syki-sok")
        );
        assert_eq!(env.instance_id, "syki-sok-main");
        assert_eq!(env.mutex_name(), "Local\\syki-syki-sok-main");
        assert_eq!(
            env.webview_user_data_dir(),
            env.data_dir.join("webview2")
        );
    }

    #[test]
    fn runtime_env_blank_values_are_ignored() {
        let env = RuntimeEnv::from_lookup(lookup_from(&[
            ("SYKI_DATA_DIR", "   "),
            ("SYKI_INSTANCE_ID", ""),
            ("APPDATA", " "),
        ]));
        assert_eq!(env.data_dir, PathBuf::from("syki-sok"));
        assert_eq!(env.instance_id, "syki-sok-main");
    }

    #[test]
    fn runtime_env_without_any_variable_uses_relative_defaults() {
        let env = RuntimeEnv::from_lookup(|_| None);
        assert_eq!(env.data_dir, PathBuf::from("syki-sok"));
        assert_eq!(env.instance_id, "syki-sok-main");
        assert_eq!(env.mutex_name(), "Local\\syki-syki-sok-main");
    }
}
