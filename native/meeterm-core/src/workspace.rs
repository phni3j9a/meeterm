//! Backend-independent, low-frequency control-plane metadata.
//!
//! IDs are opaque to the UI and are resolved only inside the owning SSH
//! connection/runtime. Native terminal handles have a separate lifetime.
use serde::Serialize;

/// The bounded recovery budget shared by the native actor and its JSON
/// control contract.  Keeping the value here avoids making the low-frequency
/// workspace model depend on the SSH module's private constants.
pub const DEFAULT_RECOVERY_MAX_ATTEMPTS: u32 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Tmux,
    Herdr,
}

impl Backend {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tmux" => Some(Self::Tmux),
            "herdr" => Some(Self::Herdr),
            _ => None,
        }
    }
}

/// The low-frequency state of one backend's runtime list.  This is a control
/// plane model only: it contains display data and opaque native candidate IDs,
/// never a tmux server identity, Herdr socket, session directory, or
/// executable path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeSectionState {
    #[default]
    Loading,
    Success,
    Empty,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeState {
    #[default]
    Running,
    Stopped,
    Unknown,
}

/// A recovery phase is deliberately separate from [`crate::ssh::ConnectionState`].
/// The latter is a fixed C ABI snapshot; this enum is part of the append-only
/// workspace JSON contract and can describe a retained screen while the SSH
/// actor is being rebuilt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryPhase {
    #[default]
    None,
    Reconnecting,
    AwaitingConfirmation,
    Resynchronizing,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySnapshot {
    pub phase: RecoveryPhase,
    /// A stable, sanitized snake_case reason.  Human-facing copy belongs to
    /// the application layer; native never forwards remote diagnostics here.
    pub reason: String,
    pub attempt: u32,
    pub max_attempts: u32,
    /// Opaque, bounded, native-scoped confirmation material.  It is empty
    /// outside `awaitingConfirmation` and is never a remote path or ID.
    pub confirmation_token: String,
}

impl Default for RecoverySnapshot {
    fn default() -> Self {
        Self {
            phase: RecoveryPhase::None,
            reason: String::new(),
            attempt: 0,
            max_attempts: DEFAULT_RECOVERY_MAX_ATTEMPTS,
            confirmation_token: String::new(),
        }
    }
}

/// Low-frequency lifecycle controls serialized beside the authoritative
/// workspace topology.  `operation_epoch` is a decimal string because the
/// value must remain exact when consumed by JavaScript on 64-bit platforms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeControlSnapshot {
    pub operation_epoch: String,
    pub has_retained_work: bool,
    pub runtime_operations_ready: bool,
    pub terminal_input_ready: bool,
    pub recovery: RecoverySnapshot,
}

impl Default for RuntimeControlSnapshot {
    fn default() -> Self {
        Self {
            operation_epoch: "0".to_owned(),
            has_retained_work: false,
            runtime_operations_ready: false,
            terminal_input_ready: false,
            recovery: RecoverySnapshot::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCandidate {
    /// A native-owned ID. It is scoped to one connection generation and one
    /// discovery revision and is not a remote session/socket identity.
    pub id: String,
    pub backend: Backend,
    pub name: String,
    pub state: RuntimeState,
    pub selectable: bool,
    pub suggested: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSection {
    pub state: RuntimeSectionState,
    pub candidates: Vec<RuntimeCandidate>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDiscoverySnapshot {
    pub connection_generation: u64,
    pub discovery_revision: u64,
    pub tmux: RuntimeSection,
    pub herdr: RuntimeSection,
}

impl RuntimeDiscoverySnapshot {
    pub(crate) fn loading(generation: u64, revision: u64) -> Self {
        Self {
            connection_generation: generation,
            discovery_revision: revision,
            tmux: RuntimeSection {
                state: RuntimeSectionState::Loading,
                ..RuntimeSection::default()
            },
            herdr: RuntimeSection {
                state: RuntimeSectionState::Loading,
                ..RuntimeSection::default()
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub control: RuntimeControlSnapshot,
    pub backend: Backend,
    pub runtime: String,
    pub groups_supported: bool,
    pub workspaces: Vec<Workspace>,
    pub groups: Vec<TerminalGroup>,
    pub terminals: Vec<Terminal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Blocked,
    Done,
    Working,
    Idle,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub agent_status: Option<AgentStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalGroup {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub selected: bool,
    pub agent_status: Option<AgentStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    pub id: String,
    pub workspace_id: String,
    pub group_id: String,
    pub terminal_id: String,
    pub name: String,
    pub active: bool,
    pub selected: bool,
    pub agent: Option<Agent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Agent {
    pub name: String,
    pub status: AgentStatus,
}

impl RuntimeSnapshot {
    pub(crate) fn tmux_for_runtime(snapshot: &crate::tmux::SessionSnapshot, runtime: &str) -> Self {
        Self {
            control: RuntimeControlSnapshot::default(),
            backend: Backend::Tmux,
            runtime: runtime.to_owned(),
            groups_supported: false,
            workspaces: snapshot
                .windows
                .iter()
                .map(|window| Workspace {
                    id: format!("@{}", window.window_id),
                    name: window.name.clone(),
                    agent_status: None,
                })
                .collect(),
            // A virtual group never creates or rearranges anything in tmux.
            groups: snapshot
                .windows
                .iter()
                .map(|window| TerminalGroup {
                    id: format!("@{}", window.window_id),
                    workspace_id: format!("@{}", window.window_id),
                    name: String::new(),
                    selected: window.selected,
                    agent_status: None,
                })
                .collect(),
            terminals: snapshot
                .panes
                .iter()
                .map(|pane| Terminal {
                    id: format!("%{}", pane.pane_id),
                    workspace_id: format!("@{}", pane.window_id),
                    group_id: format!("@{}", pane.window_id),
                    terminal_id: format!("native:{}", pane.terminal_id),
                    name: pane.pane_name.clone(),
                    active: pane.active,
                    selected: pane.selected,
                    agent: None,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{PaneSnapshot, SessionSnapshot, WindowSnapshot};

    #[test]
    fn agent_status_uses_one_lowercase_wire_vocabulary() {
        let values = [
            (AgentStatus::Blocked, "blocked"),
            (AgentStatus::Done, "done"),
            (AgentStatus::Working, "working"),
            (AgentStatus::Idle, "idle"),
            (AgentStatus::Unknown, "unknown"),
        ];
        for (status, encoded) in values {
            assert_eq!(serde_json::to_value(status).unwrap(), encoded);
        }

        let agent = Agent {
            name: "Codex".to_owned(),
            status: AgentStatus::Done,
        };
        assert_eq!(
            serde_json::to_value(agent).unwrap(),
            serde_json::json!({"name":"Codex", "status":"done"})
        );
    }

    #[test]
    fn common_workspace_and_group_snapshots_have_nullable_agent_status() {
        let snapshot = RuntimeSnapshot {
            workspaces: vec![Workspace {
                id: "workspace".to_owned(),
                name: "Workspace".to_owned(),
                agent_status: Some(AgentStatus::Blocked),
            }],
            groups: vec![TerminalGroup {
                id: "group".to_owned(),
                workspace_id: "workspace".to_owned(),
                name: "Group".to_owned(),
                selected: true,
                agent_status: None,
            }],
            ..RuntimeSnapshot::default()
        };
        let encoded = serde_json::to_value(snapshot).unwrap();
        assert_eq!(encoded["workspaces"][0]["agentStatus"], "blocked");
        assert_eq!(
            encoded["groups"][0],
            serde_json::json!({
                "id": "group",
                "workspaceId": "workspace",
                "name": "Group",
                "selected": true,
                "agentStatus": null
            })
        );
    }

    #[test]
    fn recovery_control_uses_the_stable_camel_case_wire_contract() {
        let snapshot = RuntimeSnapshot {
            control: RuntimeControlSnapshot {
                operation_epoch: "18446744073709551615".to_owned(),
                has_retained_work: true,
                runtime_operations_ready: false,
                terminal_input_ready: false,
                recovery: RecoverySnapshot {
                    phase: RecoveryPhase::AwaitingConfirmation,
                    reason: "runtime_identity_uncertain".to_owned(),
                    attempt: 2,
                    max_attempts: DEFAULT_RECOVERY_MAX_ATTEMPTS,
                    confirmation_token: "opaque-token".to_owned(),
                },
            },
            ..RuntimeSnapshot::default()
        };
        let encoded = serde_json::to_value(snapshot).unwrap();
        assert_eq!(
            encoded["control"],
            serde_json::json!({
                "operationEpoch": "18446744073709551615",
                "hasRetainedWork": true,
                "runtimeOperationsReady": false,
                "terminalInputReady": false,
                "recovery": {
                    "phase": "awaitingConfirmation",
                    "reason": "runtime_identity_uncertain",
                    "attempt": 2,
                    "maxAttempts": DEFAULT_RECOVERY_MAX_ATTEMPTS,
                    "confirmationToken": "opaque-token"
                }
            })
        );
    }

    #[test]
    fn tmux_projection_keeps_remote_layout_and_native_identity_distinct() {
        let pane = PaneSnapshot {
            window_id: 7,
            pane_id: 12,
            terminal_id: 82,
            window_name: "work".into(),
            pane_name: "shell".into(),
            title: "shell".into(),
            active: true,
            selected: true,
            index: 0,
            columns: 40,
            rows: 20,
        };
        let source = SessionSnapshot {
            selected_pane: Some(12),
            panes: vec![pane.clone()],
            windows: vec![WindowSnapshot {
                window_id: 7,
                name: "work".into(),
                panes: vec![pane],
                selected: true,
                zoomed: false,
            }],
        };
        let original = source.clone();
        let result = RuntimeSnapshot::tmux_for_runtime(&source, crate::tmux::SESSION_NAME);
        assert_eq!(source, original);
        assert_eq!(result.runtime, "meeterm");
        assert_eq!(
            RuntimeSnapshot::tmux_for_runtime(&source, "dev-日本語").runtime,
            "dev-日本語"
        );
        assert!(!result.groups_supported);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].workspace_id, result.workspaces[0].id);
        assert_eq!(result.terminals[0].group_id, result.groups[0].id);
        assert_eq!(result.terminals[0].id, "%12");
        assert_eq!(result.terminals[0].terminal_id, "native:82");
        assert!(result.terminals[0].agent.is_none());
        assert!(result.workspaces[0].agent_status.is_none());
        assert!(result.groups[0].agent_status.is_none());
    }
}
