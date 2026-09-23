//! Existing Herdr public API and direct terminal control over one SSH session.
//! Each API request has its own forwarded Unix socket. Input requests are
//! awaited in order; only the selected direct controller may enqueue input.
use super::*;
use crate::herdr as wire;
use crate::input::{KeyCode, Modifiers, encode_key, encode_text};
use crate::terminal::SemanticInput;
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};

const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
static NEXT_REMOTE_HANDLE: AtomicU64 = AtomicU64::new(1_000_000);
static NEXT_RECOVERY_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub(super) struct Metadata {
    pub(super) snapshot: RuntimeSnapshot,
    ids: HashMap<(u8, String), u64>,
    workspaces: HashMap<u64, String>,
    groups: HashMap<u64, String>,
    pub(super) panes: HashMap<u64, RemotePane>,
    /// Herdr 0.9.0 pane/tab close may cascade from these parents when its
    /// confirm_close setting is disabled. No per-close no-cascade flag exists.
    linked_worktree_parents: HashSet<u64>,
    /// The selected group is independent of terminal selection so an empty
    /// group remains selectable across snapshot rebuilds.
    selected_groups: HashMap<u64, u64>,
    active_group: Option<u64>,
}

#[derive(Clone)]
pub(super) struct RemotePane {
    pub(super) pane_id: String,
    pub(super) terminal_id: String,
    pub(super) workspace: u64,
    pub(super) group: u64,
}

impl Metadata {
    fn id(&mut self, kind: u8, remote: &str) -> u64 {
        *self
            .ids
            .entry((kind, remote.to_owned()))
            .or_insert_with(|| NEXT_REMOTE_HANDLE.fetch_add(1, Ordering::Relaxed))
    }

    pub(super) fn select(&mut self, pane: u64) {
        let group = self.panes.get(&pane).map(|p| (p.workspace, p.group));
        if let Some((workspace, group)) = group {
            self.selected_groups.insert(workspace, group);
            self.active_group = Some(group);
        }
        for terminal in &mut self.snapshot.terminals {
            terminal.selected = terminal.id == pane.to_string();
        }
        self.refresh_group_selection();
    }

    /// Select a group independently of its panes. This clears terminal
    /// selection so an empty group remains represented as the selected group.
    pub(super) fn select_group(&mut self, group: u64) {
        let Some(workspace) = self
            .snapshot
            .groups
            .iter()
            .find(|item| item.id == group.to_string())
            .and_then(|item| item.workspace_id.parse::<u64>().ok())
        else {
            return;
        };
        self.selected_groups.insert(workspace, group);
        self.active_group = Some(group);
        for terminal in &mut self.snapshot.terminals {
            terminal.selected = false;
        }
        self.refresh_group_selection();
    }

    fn refresh_group_selection(&mut self) {
        for item in &mut self.snapshot.groups {
            let selected = item
                .workspace_id
                .parse::<u64>()
                .ok()
                .and_then(|workspace| self.selected_groups.get(&workspace))
                .is_some_and(|group| *group == item.id.parse::<u64>().unwrap_or(0));
            item.selected = selected;
        }
    }

    pub(super) fn active_group(&self) -> Option<u64> {
        self.active_group
    }
}

fn choose_selected_pane(
    metadata: &Metadata,
    flat: &[PaneSnapshot],
    old_selected: Option<u64>,
    old_target: Option<&RemotePane>,
    preferred: Option<u64>,
    selected_group: Option<u64>,
) -> Option<u64> {
    old_selected
        .filter(|id| metadata.panes.contains_key(id))
        .or_else(|| {
            old_target
                .filter(|old| selected_group.is_none_or(|group| group == old.group))
                .and_then(|old| {
                    flat.iter()
                        .find(|pane| metadata.panes[&pane.pane_id].group == old.group)
                        .map(|pane| pane.pane_id)
                })
        })
        .or_else(|| {
            old_target
                .filter(|old| selected_group.is_none_or(|group| group == old.group))
                .and_then(|old| {
                    flat.iter()
                        .find(|pane| pane.window_id == old.workspace)
                        .map(|pane| pane.pane_id)
                })
        })
        .or_else(|| {
            preferred.filter(|pane| {
                selected_group.is_none_or(|group| {
                    metadata
                        .panes
                        .get(pane)
                        .is_some_and(|remote| remote.group == group)
                })
            })
        })
        .or_else(|| {
            selected_group.and_then(|group| {
                flat.iter()
                    .find(|pane| metadata.panes[&pane.pane_id].group == group)
                    .map(|pane| pane.pane_id)
            })
        })
        // A focused or locally selected empty group is a valid state. Do not
        // silently select a pane from another group.
        .or_else(|| {
            selected_group
                .is_none()
                .then(|| flat.first().map(|pane| pane.pane_id))
                .flatten()
        })
}

fn choose_selected_group(
    groups: &[u64],
    local: Option<u64>,
    legacy_flag: Option<u64>,
    focused: Option<u64>,
) -> Option<u64> {
    local
        .filter(|group| groups.contains(group))
        .or_else(|| legacy_flag.filter(|group| groups.contains(group)))
        .or_else(|| focused.filter(|group| groups.contains(group)))
        .or_else(|| groups.first().copied())
}

struct JsonChannel {
    reader: russh::ChannelReadHalf,
    writer: russh::ChannelWriteHalf<client::Msg>,
    decoder: wire::NdjsonDecoder,
    values: VecDeque<Value>,
}

impl JsonChannel {
    fn new(channel: russh::Channel<client::Msg>) -> Self {
        let (reader, writer) = channel.split();
        Self {
            reader,
            writer,
            decoder: wire::NdjsonDecoder::with_default_limit(),
            values: VecDeque::new(),
        }
    }

    async fn send(&self, value: Value) -> Result<(), FlowFailure> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| FlowFailure::HerdrProtocol)?;
        if bytes.len() > MAX_JSON_BYTES {
            return Err(FlowFailure::HerdrProtocol);
        }
        bytes.push(b'\n');
        tokio::time::timeout(SSH_STAGE_TIMEOUT, self.writer.data_bytes(bytes))
            .await
            .map_err(|_| FlowFailure::Transport)?
            .map_err(|_| FlowFailure::Transport)
    }

    async fn request(&self, id: &str, method: &str, params: &Value) -> Result<(), FlowFailure> {
        let bytes = wire::request(id, method, params).map_err(|_| FlowFailure::HerdrProtocol)?;
        if bytes.len() > MAX_JSON_BYTES {
            return Err(FlowFailure::HerdrProtocol);
        }
        tokio::time::timeout(SSH_STAGE_TIMEOUT, self.writer.data_bytes(bytes))
            .await
            .map_err(|_| FlowFailure::Transport)?
            .map_err(|_| FlowFailure::Transport)
    }

    async fn next(&mut self) -> Result<Value, FlowFailure> {
        loop {
            if let Some(value) = self.values.pop_front() {
                return Ok(value);
            }
            match self.reader.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let values = self
                        .decoder
                        .push(&data)
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    if self.values.len().saturating_add(values.len()) > 256 {
                        return Err(FlowFailure::HerdrProtocol);
                    }
                    self.values.extend(values);
                }
                // CLI diagnostics may contain names or paths. Never put them
                // in logs or the generic low-frequency failure snapshot.
                Some(ChannelMsg::ExtendedData { .. }) => {}
                Some(ChannelMsg::Failure) => return Err(FlowFailure::HerdrOperation),
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                    self.decoder
                        .finish()
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    return Err(FlowFailure::RemoteClosed);
                }
                _ => {}
            }
        }
    }

    async fn close(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), self.writer.close()).await;
    }
}

struct Controller {
    pane: u64,
    native: u64,
    epoch: u64,
    stream: JsonChannel,
    input: mpsc::Receiver<SemanticInput>,
    sizes: watch::Receiver<(u16, u16)>,
    seq: Option<u64>,
    ready: bool,
}

struct HerdrClient<'a> {
    shared: &'a Arc<ConnectionShared>,
    session: &'a client::Handle<HostKeyHandler>,
    executable: String,
    runtime: Option<String>,
    socket: String,
    subscription: JsonChannel,
    subscribed_panes: HashSet<String>,
    controller: Option<Controller>,
    viewport: (u16, u16),
    request_id: u64,
    /// During retained recovery the stable Herdr terminal is the only
    /// acceptable target.  Pane aliases are deliberately resolved again from
    /// the authoritative snapshot and are never used as identity.
    strict_terminal: Option<String>,
    /// Session operation epoch captured at the start of this actor attempt.
    /// Every awaited discovery/controller operation must remain within it.
    operation_epoch: u64,
    command_epoch: Option<u64>,
    staged_projection: Option<SnapshotProjection>,
}

impl Drop for HerdrClient<'_> {
    fn drop(&mut self) {
        self.discard_staged_projection();
        detach_all(self.shared);
    }
}

#[derive(Clone)]
pub(super) struct DiscoveredSession {
    pub(super) name: String,
    pub(super) default: bool,
    pub(super) running: bool,
    pub(super) executable: String,
}

pub(super) struct Discovery {
    pub(super) sessions: Vec<DiscoveredSession>,
    pub(super) executable: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecoveryTarget {
    Terminal(String),
    EmptyGroup(u64),
    Missing,
}

/// Classify the retained Herdr selection before recovery starts. An empty
/// selected group is a valid metadata-only target; a group that disappeared,
/// or a non-empty group whose previously selected terminal disappeared, must
/// stop rather than silently selecting an unrelated pane.
fn recovery_target(state: &SessionState) -> RecoveryTarget {
    if let Some(terminal) = state.recovery_terminal_id.clone() {
        return RecoveryTarget::Terminal(terminal);
    }
    let Some(group) = state.recovery_group_id else {
        return RecoveryTarget::Missing;
    };
    let group_exists = state
        .herdr
        .snapshot
        .groups
        .iter()
        .any(|candidate| candidate.id == group.to_string());
    let group_has_panes = state.herdr.panes.values().any(|pane| pane.group == group);
    if group_exists && !group_has_panes {
        RecoveryTarget::EmptyGroup(group)
    } else {
        RecoveryTarget::Missing
    }
}

fn is_recovery_local_failure(failure: FlowFailure) -> bool {
    matches!(
        failure,
        FlowFailure::HerdrMissing
            | FlowFailure::HerdrSessionMissing
            | FlowFailure::HerdrIncompatible
            | FlowFailure::HerdrUnsupported
            | FlowFailure::HerdrForwarding
            | FlowFailure::HerdrProtocol
            | FlowFailure::HerdrController
            | FlowFailure::HerdrOperation
            | FlowFailure::HerdrDiscoveryMissing
            | FlowFailure::HerdrDiscoveryIncompatible
            | FlowFailure::HerdrDiscoveryMalformed
            | FlowFailure::HerdrDiscoveryPermission
            | FlowFailure::HerdrDiscoveryTimeout
    )
}

fn recovery_reason_for_failure(failure: FlowFailure) -> &'static str {
    match failure {
        FlowFailure::HerdrSessionMissing => "herdr_session_missing",
        FlowFailure::HerdrController => "controller_conflict",
        FlowFailure::HerdrIncompatible
        | FlowFailure::HerdrDiscoveryIncompatible
        | FlowFailure::HerdrUnsupported => "herdr_incompatible",
        FlowFailure::HerdrProtocol
        | FlowFailure::HerdrDiscoveryMalformed
        | FlowFailure::HerdrDiscoveryPermission
        | FlowFailure::HerdrDiscoveryTimeout => "runtime_identity_uncertain",
        FlowFailure::HerdrMissing | FlowFailure::HerdrForwarding => "herdr_session_missing",
        _ => "runtime_identity_uncertain",
    }
}

fn publish_recovery_candidate(
    shared: &ConnectionShared,
    profile: &ConnectionProfile,
    candidate: &DiscoveredSession,
    expected_epoch: u64,
) -> Result<u64, FlowFailure> {
    let mut state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
    if state.generation != shared.generation
        || state.operation_epoch != expected_epoch
        || shared.is_cancelled()
    {
        return Err(FlowFailure::Stale);
    }
    let revision = state
        .runtime_discovery
        .discovery_revision
        .wrapping_add(1)
        .max(1);
    let id = format!("recovery-{}-herdr-{}", shared.generation, revision);
    let binding = RuntimeBinding::Herdr {
        name: candidate.name.clone(),
        default: candidate.default,
        executable: candidate.executable.clone(),
    };
    state.runtime_candidates.clear();
    state.runtime_candidates.insert(id.clone(), binding);
    state.runtime_discovery = RuntimeDiscoverySnapshot {
        connection_generation: shared.generation,
        discovery_revision: revision,
        tmux: RuntimeSection {
            state: RuntimeSectionState::Empty,
            ..RuntimeSection::default()
        },
        herdr: RuntimeSection {
            state: RuntimeSectionState::Success,
            candidates: vec![RuntimeCandidate {
                id,
                backend: Backend::Herdr,
                name: candidate.name.clone(),
                state: RuntimeState::Running,
                selectable: true,
                suggested: candidate.default,
                error_code: None,
                error_message: None,
            }],
            ..RuntimeSection::default()
        },
    };
    // The profile is the connection-scoped capability.  It is updated only
    // after the candidate has been verified and never exposes the executable
    // through the serialized picker rows.
    state.profile = Some(profile.clone());
    Ok(revision)
}

fn terminal_scope_digest(terminal_id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    terminal_id.hash(&mut hasher);
    hasher.finish()
}

/// Resolve and list all Herdr sessions without opening, starting, stopping,
/// or updating any runtime. The upstream response contains socket paths and
/// session directories; the caller receives only names and running state.
pub(super) async fn discover(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    expected: Option<&str>,
) -> Result<Discovery, FlowFailure> {
    discover_at_epoch(shared, session, expected, None).await
}

async fn discover_at_epoch(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    expected: Option<&str>,
    expected_epoch: Option<u64>,
) -> Result<Discovery, FlowFailure> {
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        return Err(FlowFailure::Stale);
    }
    let executable = resolve_executable(shared, session, expected, true, expected_epoch).await?;
    let command = wire::command_with_executable(&executable, None, &["session", "list", "--json"])
        .map_err(|_| FlowFailure::HerdrDiscoveryMalformed)?;
    let output = super::run_remote_command_with_timeout_at_epoch(
        shared,
        session,
        command,
        MAX_JSON_BYTES,
        FlowFailure::HerdrDiscoveryMalformed,
        FlowFailure::HerdrDiscoveryTimeout,
        expected_epoch,
    )
    .await?;
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        return Err(FlowFailure::Stale);
    }
    match output.exit_status {
        Some(0) => {
            let value: Value = serde_json::from_slice(&output.stdout)
                .map_err(|_| FlowFailure::HerdrDiscoveryMalformed)?;
            let sessions = wire::decode_session_list(&value)
                .map_err(|_| FlowFailure::HerdrDiscoveryMalformed)?
                .into_iter()
                .map(|session| DiscoveredSession {
                    name: session.name,
                    default: session.default,
                    running: session.running,
                    executable: executable.clone(),
                })
                .collect::<Vec<_>>();
            Ok(Discovery {
                sessions,
                executable,
            })
        }
        Some(127) => Err(FlowFailure::HerdrDiscoveryMissing),
        Some(78) => Err(FlowFailure::HerdrDiscoveryIncompatible),
        Some(_) if is_permission_error(&output.stderr) => {
            Err(FlowFailure::HerdrDiscoveryPermission)
        }
        Some(_) => Err(FlowFailure::HerdrDiscoveryMalformed),
        None => Err(FlowFailure::HerdrDiscoveryMalformed),
    }
}

async fn resolve_executable(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    expected: Option<&str>,
    discovery: bool,
    expected_epoch: Option<u64>,
) -> Result<String, FlowFailure> {
    let output = super::run_remote_command_with_timeout_at_epoch(
        shared,
        session,
        wire::resolver_command().to_owned(),
        64 * 1024,
        if discovery {
            FlowFailure::HerdrDiscoveryMalformed
        } else {
            FlowFailure::HerdrProtocol
        },
        if discovery {
            FlowFailure::HerdrDiscoveryTimeout
        } else {
            FlowFailure::HerdrProtocol
        },
        expected_epoch,
    )
    .await?;
    let (missing, incompatible, malformed) = if discovery {
        (
            FlowFailure::HerdrDiscoveryMissing,
            FlowFailure::HerdrDiscoveryIncompatible,
            FlowFailure::HerdrDiscoveryMalformed,
        )
    } else {
        (
            FlowFailure::HerdrMissing,
            FlowFailure::HerdrIncompatible,
            FlowFailure::HerdrProtocol,
        )
    };
    let executable = match output.exit_status {
        Some(0) => wire::parse_resolved_executable(&output.stdout).map_err(|_| malformed)?,
        Some(127) => return Err(missing),
        Some(78) => return Err(incompatible),
        Some(_) | None => return Err(malformed),
    };
    if expected.is_some_and(|expected| expected != executable) {
        // A lifecycle that already selected a binary must not silently switch
        // to another installation after reconnect.
        return Err(incompatible);
    }
    let schema_command =
        wire::api_schema_command_with_executable(&executable).map_err(|_| malformed)?;
    let schema_output = super::run_remote_command_with_timeout_at_epoch(
        shared,
        session,
        schema_command,
        512 * 1024,
        malformed,
        if discovery {
            FlowFailure::HerdrDiscoveryTimeout
        } else {
            FlowFailure::HerdrProtocol
        },
        expected_epoch,
    )
    .await?;
    if schema_output.exit_status != Some(0) {
        return Err(incompatible);
    }
    let schema: Value = serde_json::from_slice(&schema_output.stdout).map_err(|_| malformed)?;
    wire::validate_api_schema(&schema).map_err(|_| incompatible)?;
    Ok(executable)
}

fn is_permission_error(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("permission denied")
        || message.contains("access denied")
        || message.contains("operation not permitted")
}

pub(super) async fn run(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    run_impl(shared, profile, session, commands, None, None).await
}

/// Reauthenticate and rediscover a Herdr runtime without acquiring a
/// controller until the user confirms the currently discovered candidate.
/// The existing SSH actor owns this loop, so duplicate retry/confirm calls
/// cannot create parallel controllers.
pub(super) async fn recover(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    // Copy the recovery identity out of the mutex before taking any failure
    // transition. Calling `stop_recovery` while a temporary SessionState guard
    // is alive self-deadlocks on the same mutex. An empty selected group is a
    // valid metadata-only target; a group with panes but no selected stable
    // terminal means the previously selected terminal disappeared.
    let target = {
        let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        recovery_target(&state)
    };
    let (expected_terminal, empty_group) = match target {
        RecoveryTarget::Terminal(terminal) => (Some(terminal), None),
        RecoveryTarget::EmptyGroup(group) => (None, Some(group)),
        RecoveryTarget::Missing => (None, None),
    };
    if expected_terminal.is_none() && empty_group.is_none() {
        shared.stop_recovery("herdr_terminal_missing");
        return Err(FlowFailure::HerdrSessionMissing);
    }

    loop {
        let discovery_epoch = shared.operation_epoch();
        if !shared.current_request_epoch(discovery_epoch) {
            return Err(FlowFailure::Stale);
        }
        let discovery = match discover_at_epoch(
            shared,
            session,
            profile.herdr_executable.as_deref(),
            Some(discovery_epoch),
        )
        .await
        {
            Ok(discovery) => discovery,
            Err(failure) if is_recovery_local_failure(failure) => {
                shared.stop_recovery(recovery_reason_for_failure(failure));
                return Err(failure);
            }
            Err(failure) => return Err(failure),
        };
        let candidate = discovery
            .sessions
            .iter()
            .find(|candidate| {
                candidate.running
                    && ((profile.runtime.is_none() && candidate.default)
                        || (profile.runtime.as_deref() == Some(candidate.name.as_str())))
            })
            .cloned();
        let Some(candidate) = candidate else {
            shared.stop_recovery("herdr_session_missing");
            return Err(FlowFailure::HerdrSessionMissing);
        };
        if candidate.executable != discovery.executable {
            shared.stop_recovery("herdr_incompatible");
            return Err(FlowFailure::HerdrIncompatible);
        }
        profile.herdr_executable = Some(discovery.executable.clone());
        shared.set_profile(profile.clone());
        let revision = publish_recovery_candidate(shared, profile, &candidate, discovery_epoch)?;
        let token = format!(
            "herdr-recovery-{}-{}-{}-{:016x}-{}",
            shared.generation,
            discovery_epoch,
            revision,
            expected_terminal
                .as_deref()
                .map(terminal_scope_digest)
                .unwrap_or_else(|| terminal_scope_digest(&format!("group:{:?}", empty_group))),
            NEXT_RECOVERY_TOKEN.fetch_add(1, Ordering::Relaxed)
        );
        if shared
            .publish_recovery_confirmation(token)?
            .ne(&discovery_epoch)
        {
            return Err(FlowFailure::Stale);
        }

        loop {
            if shared.recovery_phase() != RecoveryPhase::AwaitingConfirmation {
                // Foreground/visibility invalidation may have happened before
                // this loop registered its waiter. The outer loop owns the
                // next discovery and confirmation token.
                break;
            }
            let request = tokio::select! {
                _ = shared.cancelled() => return Err(FlowFailure::Stale),
                _ = shared.explicit_cleanup() => return Err(FlowFailure::Stale),
                _ = shared.retry_notify.notified() => {
                    if shared.recovery_phase() != RecoveryPhase::AwaitingConfirmation {
                        break;
                    }
                    continue;
                },
                request = commands.recv() => request,
            };
            let Some(request) = request else {
                return Err(FlowFailure::Stale);
            };
            if !shared.current_request_epoch(request.epoch) {
                continue;
            }
            match request.command {
                ControlCommand::RetryRecovery => {
                    let attempt = shared
                        .session
                        .lock()
                        .map(|state| state.recovery.attempt.saturating_add(1))
                        .unwrap_or(1);
                    if shared.begin_recovery("manual_retry", attempt).is_none() {
                        return Err(FlowFailure::Stale);
                    }
                    break;
                }
                ControlCommand::ConfirmRecovery { token: submitted } => {
                    // `confirm_recovery` consumes the public token and places
                    // the same bounded value in this private slot before the
                    // request is queued.  No controller operation occurs for
                    // a stale or duplicated token.
                    if !shared.take_pending_confirmation(&submitted) {
                        shared.stop_recovery("recovery_stale");
                        return Err(FlowFailure::HerdrProtocol);
                    }
                    if shared.recovery_phase() != RecoveryPhase::Resynchronizing {
                        shared.stop_recovery("recovery_stale");
                        return Err(FlowFailure::HerdrProtocol);
                    }
                    let confirmed_epoch = request.epoch;
                    let confirmed = match discover_at_epoch(
                        shared,
                        session,
                        profile.herdr_executable.as_deref(),
                        Some(confirmed_epoch),
                    )
                    .await
                    {
                        Ok(discovery) => discovery,
                        Err(failure) if is_recovery_local_failure(failure) => {
                            shared.stop_recovery(recovery_reason_for_failure(failure));
                            return Err(failure);
                        }
                        Err(failure) => return Err(failure),
                    };
                    if !shared.current_request_epoch(confirmed_epoch) {
                        return Err(FlowFailure::Stale);
                    }
                    let Some(candidate) = confirmed.sessions.iter().find(|candidate| {
                        candidate.running
                            && ((profile.runtime.is_none() && candidate.default)
                                || (profile.runtime.as_deref() == Some(candidate.name.as_str())))
                    }) else {
                        shared.stop_recovery("herdr_session_missing");
                        return Err(FlowFailure::HerdrSessionMissing);
                    };
                    if candidate.executable != confirmed.executable {
                        shared.stop_recovery("herdr_incompatible");
                        return Err(FlowFailure::HerdrIncompatible);
                    }
                    profile.herdr_executable = Some(confirmed.executable.clone());
                    shared.set_profile(profile.clone());
                    let result = run_impl(
                        shared,
                        profile,
                        session,
                        commands,
                        expected_terminal.clone(),
                        Some(confirmed_epoch),
                    )
                    .await;
                    if let Err(failure) = result {
                        if is_recovery_local_failure(failure) {
                            shared.stop_recovery(recovery_reason_for_failure(failure));
                        }
                        return Err(failure);
                    }
                    return Ok(());
                }
                ControlCommand::SetTerminalVisible { visible: false } => {
                    // There is no controller in this phase.  The public
                    // setter already revoked the native transport.
                }
                _ => {
                    // Picker/topology/input commands cannot be replayed into
                    // a later recovery epoch.
                }
            }
        }
    }
}

async fn run_impl(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
    strict_terminal: Option<String>,
    expected_epoch: Option<u64>,
) -> Result<(), FlowFailure> {
    let operation_epoch = {
        let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        let epoch = expected_epoch.unwrap_or(state.operation_epoch);
        (state.generation == shared.generation && state.operation_epoch == epoch)
            .then_some(epoch)
            .ok_or(FlowFailure::Stale)?
    };
    let initial_recovery_epoch = {
        let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        (state.recovery.phase != RecoveryPhase::None).then_some(operation_epoch)
    };
    let executable = resolve_executable(
        shared,
        session,
        profile.herdr_executable.as_deref(),
        false,
        Some(operation_epoch),
    )
    .await?;
    if !shared.current_request_epoch(operation_epoch) {
        return Err(FlowFailure::Stale);
    }
    profile.herdr_executable = Some(executable.clone());
    shared.set_profile(profile.clone());
    shared.set_state(ConnectionState::Synchronizing);
    let status = command_output(
        shared,
        session,
        wire::command_with_executable(
            &executable,
            profile.runtime.as_deref(),
            &["status", "--json"],
        )
        .map_err(|_| FlowFailure::HerdrProtocol)?,
        operation_epoch,
    )
    .await
    .map_err(|failure| match failure {
        // A named/default server can disappear after picker discovery. Treat
        // a failed status probe as a local selection miss so the actor returns
        // to the picker instead of reporting a host-wide transport failure.
        FlowFailure::HerdrOperation => FlowFailure::HerdrSessionMissing,
        failure => failure,
    })?;
    if !shared.current_request_epoch(operation_epoch) {
        return Err(FlowFailure::Stale);
    }
    let status: Value = serde_json::from_slice(&status).map_err(|_| FlowFailure::HerdrProtocol)?;
    let server = &status["server"];
    if server["running"] != true {
        return Err(FlowFailure::HerdrSessionMissing);
    }
    if server["version"] != wire::HERDR_VERSION
        || server["protocol"] != wire::HERDR_PROTOCOL
        || status["client"]["protocol"] != wire::HERDR_PROTOCOL
        || status
            .get("schema")
            .or_else(|| server.get("schema"))
            .is_some_and(|schema| schema.as_u64() != Some(u64::from(wire::HERDR_SCHEMA)))
    {
        return Err(FlowFailure::HerdrIncompatible);
    }
    let socket = server["socket"]
        .as_str()
        .filter(|path| path.starts_with('/') && path.len() <= 4096 && !path.contains('\0'))
        .ok_or(FlowFailure::HerdrProtocol)?
        .to_owned();
    let expected_runtime = profile.runtime.as_deref().filter(|name| *name != "default");
    if server["session"].as_str() != expected_runtime
        || status["client"]["session"].as_str() != expected_runtime
    {
        return Err(FlowFailure::HerdrIncompatible);
    }
    let viewport = shared
        .session
        .lock()
        .map_err(|_| FlowFailure::Stale)?
        .viewport
        .unwrap_or(
            registry::terminal_dimensions(shared.terminal_id()).map_err(|_| FlowFailure::Stale)?,
        );
    let subscription =
        subscribe(shared, session, &socket, &HashSet::new(), operation_epoch).await?;
    let mut client = HerdrClient {
        shared,
        session,
        executable,
        runtime: profile.runtime.clone(),
        socket,
        subscription,
        subscribed_panes: HashSet::new(),
        controller: None,
        viewport,
        request_id: 1,
        strict_terminal,
        operation_epoch,
        command_epoch: None,
        staged_projection: None,
    };
    client.synchronize().await?;
    client.activate_selected().await?;
    if client.controller.is_none() && client.strict_terminal.is_none() {
        // Metadata/runtime readiness is independent from terminal visibility.
        // The picker surface is hidden until this state is published; once it
        // is visible, activate_selected performs the controller/full-frame
        // boundary and opens input.
        if !shared.mark_ready_at_epoch(operation_epoch) {
            return Err(FlowFailure::Stale);
        }
    }
    loop {
        if shared.is_cancelled() || shared.explicit_cleanup_requested() {
            let _ = client.release().await;
            return Err(FlowFailure::Stale);
        }
        if shared.recovery_requires_controller_exit(initial_recovery_epoch) {
            let _ = client.release().await;
            return Err(FlowFailure::Transport);
        }
        let (stream, sizes, input) = match client.controller.as_mut() {
            Some(controller) => (
                Some(&mut controller.stream),
                controller.ready.then_some(&mut controller.sizes),
                controller.ready.then_some(&mut controller.input),
            ),
            None => (None, None, None),
        };
        tokio::select! {
            _ = shared.cancelled() => { let _ = client.release().await; return Err(FlowFailure::Stale); },
            _ = shared.explicit_cleanup() => { let _ = client.release().await; return Err(FlowFailure::Stale); },
            _ = shared.retry_notify.notified() => {
                if shared.is_foreground() {
                    client.activate_selected().await?;
                }
            },
            frame = async {
                match stream {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match frame {
                    Ok(value) => client.frame(value)?,
                    Err(error) => { let _ = client.release().await; return Err(error); },
                }
            },
            event = client.subscription.next() => {
                let event = event?;
                if event.get("event").is_none() { return Err(FlowFailure::HerdrProtocol); }
                // One coherent re-read covers an entire queued burst of
                // structural/status events, including pane moves.
                client.subscription.values.clear();
                client.synchronize().await?;
                client.activate_selected().await?;
            },
            command = commands.recv() => {
                let Some(request) = command else { let _ = client.release().await; return Err(FlowFailure::Stale); };
                if !shared.current_request_epoch(request.epoch) {
                    continue;
                }
                let command = request.command;
                let revoke = matches!(&command, ControlCommand::SetTerminalVisible { visible: false });
                let allow_recovery_visibility = matches!(
                    &command,
                    ControlCommand::SetTerminalVisible { visible: true }
                ) && (client.strict_terminal.is_some()
                    || !shared.current_request_is_ready(request.epoch));
                if !revoke
                    && !allow_recovery_visibility
                    && !shared.current_request_is_ready(request.epoch)
                {
                    continue;
                }
                client.command_epoch = (!revoke && !allow_recovery_visibility)
                    .then_some(request.epoch);
                let result = client.command(command).await;
                client.command_epoch = None;
                result?;
            },
            size = async {
                match sizes {
                    Some(sizes) => match sizes.changed().await {
                        Ok(()) => Some(*sizes.borrow_and_update()),
                        Err(_) => None,
                    },
                    None => std::future::pending().await,
                }
            } => {
                let Some((cols, rows)) = size else {
                    // Public visibility/selection changes revoke the native
                    // binding before their queued command reaches this actor.
                    // Its resize sender can close first. Stop watching that
                    // binding while the lifecycle command releases/reacquires
                    // the controller; this is not a stale SSH connection.
                    if let Some(controller) = client.controller.as_mut() {
                        controller.ready = false;
                    }
                    continue;
                };
                let Some(controller_epoch) = client
                    .controller
                    .as_ref()
                    .map(|controller| controller.epoch)
                else {
                    continue;
                };
                if !shared.current_terminal_input_is_ready(controller_epoch) {
                    continue;
                }
                client.viewport = (cols, rows);
                client.shared.session.lock().map_err(|_| FlowFailure::Stale)?.viewport = Some((cols, rows));
                if !shared.current_request_epoch(controller_epoch) {
                    continue;
                }
                if let Some(controller) = &client.controller {
                    controller.stream.send(json!({"type":"terminal.resize", "cols":cols, "rows":rows})).await?;
                }
            },
            input = async {
                match input {
                    Some(input) => input.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                let Some(input) = input else { continue; };
                let controller_epoch = client
                    .controller
                    .as_ref()
                    .map(|controller| controller.epoch);
                if controller_epoch
                    .is_none_or(|epoch| !shared.current_terminal_input_is_ready(epoch))
                {
                    continue;
                }
                client.input(input).await?;
            },
        }
    }
}

async fn command_output(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    command: String,
    expected_epoch: u64,
) -> Result<Vec<u8>, FlowFailure> {
    if !shared.current_request_epoch(expected_epoch) {
        return Err(FlowFailure::Stale);
    }
    let mut channel = await_stage(
        shared,
        session.channel_open_session(),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    if !shared.current_request_epoch(expected_epoch) {
        let _ = channel.close().await;
        return Err(FlowFailure::Stale);
    }
    await_stage(
        shared,
        channel.exec(true, command),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    if !shared.current_request_epoch(expected_epoch) {
        let _ = channel.close().await;
        return Err(FlowFailure::Stale);
    }
    let read = async {
        let mut bytes = Vec::new();
        let mut exit_status = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    if bytes.len().saturating_add(data.len()) > MAX_JSON_BYTES {
                        return Err(FlowFailure::HerdrProtocol);
                    }
                    bytes.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus {
                    exit_status: status,
                } => exit_status = Some(status),
                ChannelMsg::Close => break,
                ChannelMsg::Failure => return Err(FlowFailure::Channel),
                _ => {}
            }
        }
        match exit_status {
            Some(0) => Ok(bytes),
            Some(127) => Err(FlowFailure::HerdrMissing),
            _ => Err(FlowFailure::HerdrOperation),
        }
    };
    let result = tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(SSH_STAGE_TIMEOUT, read) => result.map_err(|_| FlowFailure::Transport)?,
    }?;
    if !shared.current_request_epoch(expected_epoch) {
        return Err(FlowFailure::Stale);
    }
    Ok(result)
}

async fn open_api(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    socket: &str,
) -> Result<JsonChannel, FlowFailure> {
    let channel = await_stage(
        shared,
        session.channel_open_direct_streamlocal(socket),
        SSH_STAGE_TIMEOUT,
        FlowFailure::HerdrForwarding,
    )
    .await?;
    Ok(JsonChannel::new(channel))
}

fn decode_snapshot_result(value: &Value) -> Result<wire::HerdrSessionSnapshot, wire::HerdrError> {
    if value["type"] != "session_snapshot" {
        return Err(wire::HerdrError::InvalidResponse(
            "expected session_snapshot".to_owned(),
        ));
    }
    wire::decode_session_snapshot(&value["snapshot"])
}

fn snapshot_contains_stable_terminal(
    snapshot: &wire::HerdrSessionSnapshot,
    terminal_id: &str,
) -> bool {
    snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.groups.iter())
        .flat_map(|group| group.panes.iter())
        .any(|pane| pane.terminal_id == terminal_id)
}

/// Preserve the status Herdr reports for a workspace or tab as a present
/// common rollup. `Unknown` is a real upstream value; it is not the same as
/// the `None` used by the tmux projection.
fn herdr_rollup_status(status: wire::AgentStatus) -> Option<workspace::AgentStatus> {
    Some(status.into())
}

struct SnapshotProjection {
    metadata: Metadata,
    mapping: HashMap<u64, u64>,
    selected: Option<u64>,
    flat: Vec<PaneSnapshot>,
    windows: Vec<WindowSnapshot>,
    stale: Vec<u64>,
}

/// Apply one decoded Herdr snapshot to the native common model. This is kept
/// separate from the stream/lock commit so the production actor and its
/// focused projection tests exercise the same workspace/group/pane mapping.
fn project_snapshot(
    mut metadata: Metadata,
    mut mapping: HashMap<u64, u64>,
    old_selected: Option<u64>,
    snapshot: wire::HerdrSessionSnapshot,
    runtime: String,
    viewport: (u16, u16),
    generation: u64,
) -> Result<SnapshotProjection, FlowFailure> {
    let old_target = old_selected.and_then(|id| metadata.panes.get(&id)).cloned();
    let old_selected_groups = metadata.selected_groups.clone();
    let mut old_flag_groups = HashMap::new();
    for group in metadata
        .snapshot
        .groups
        .iter()
        .filter(|group| group.selected)
    {
        let (Some(workspace), Some(group)) = (
            group.workspace_id.parse::<u64>().ok(),
            group.id.parse::<u64>().ok(),
        ) else {
            continue;
        };
        old_flag_groups.entry(workspace).or_insert(group);
    }
    let old_active_group = metadata.active_group;
    let focused_workspace_id = snapshot.focused_workspace_id.clone();
    let focused_tab_id = snapshot.focused_tab_id.clone();
    let focused_pane_id = snapshot.focused_pane_id.clone();
    metadata.snapshot = RuntimeSnapshot {
        backend: Backend::Herdr,
        runtime,
        groups_supported: true,
        ..RuntimeSnapshot::default()
    };
    metadata.workspaces.clear();
    metadata.groups.clear();
    metadata.panes.clear();
    metadata.linked_worktree_parents.clear();
    metadata.selected_groups.clear();
    metadata.active_group = None;
    let mut live_ids = HashSet::new();
    let mut preferred = None;
    let mut flat = Vec::new();
    let mut windows = Vec::new();
    let mut focused_groups = HashMap::new();
    let mut worktree_members = HashMap::<String, usize>::new();
    for workspace in &snapshot.workspaces {
        if let Some(worktree) = &workspace.worktree {
            *worktree_members
                .entry(worktree.repo_key.clone())
                .or_default() += 1;
        }
    }
    for workspace in snapshot.workspaces {
        let wid = metadata.id(b'w', &workspace.workspace_id);
        if workspace.worktree.as_ref().is_some_and(|worktree| {
            !worktree.is_linked_worktree
                && worktree_members
                    .get(&worktree.repo_key)
                    .copied()
                    .unwrap_or(0)
                    > 1
        }) {
            metadata.linked_worktree_parents.insert(wid);
        }
        live_ids.insert((b'w', workspace.workspace_id.clone()));
        metadata.workspaces.insert(wid, workspace.workspace_id);
        metadata.snapshot.workspaces.push(workspace::Workspace {
            id: wid.to_string(),
            name: workspace.name.clone(),
            agent_status: herdr_rollup_status(workspace.agent_status),
        });
        for group in workspace.groups {
            let gid = metadata.id(b'g', &group.tab_id);
            if group.focused {
                focused_groups.entry(wid).or_insert(gid);
            }
            live_ids.insert((b'g', group.tab_id.clone()));
            metadata.groups.insert(gid, group.tab_id);
            metadata.snapshot.groups.push(workspace::TerminalGroup {
                id: gid.to_string(),
                workspace_id: wid.to_string(),
                name: group.name,
                selected: false,
                agent_status: herdr_rollup_status(group.agent_status),
            });
            for (index, pane) in group.panes.into_iter().enumerate() {
                let pid = metadata.id(b'p', &pane.terminal_id);
                live_ids.insert((b'p', pane.terminal_id.clone()));
                let native = if let Some(native) = mapping.get(&pid) {
                    *native
                } else {
                    let native = registry::create_terminal(viewport.0, viewport.1)
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    registry::begin_remote(native, generation).map_err(|_| FlowFailure::Stale)?;
                    mapping.insert(pid, native);
                    native
                };
                if focused_pane_id.as_deref() == Some(&pane.pane_id) {
                    preferred = Some(pid);
                }
                metadata.panes.insert(
                    pid,
                    RemotePane {
                        pane_id: pane.pane_id,
                        terminal_id: pane.terminal_id,
                        workspace: wid,
                        group: gid,
                    },
                );
                let name = pane
                    .name
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| format!("Terminal {}", index + 1));
                let agent = pane.agent_name.map(|name| workspace::Agent {
                    name,
                    status: pane.agent_status.into(),
                });
                metadata.snapshot.terminals.push(workspace::Terminal {
                    id: pid.to_string(),
                    workspace_id: wid.to_string(),
                    group_id: gid.to_string(),
                    terminal_id: format!("native:{native}"),
                    name: name.clone(),
                    active: pane.focused,
                    selected: false,
                    agent,
                });
                flat.push(PaneSnapshot {
                    window_id: wid,
                    pane_id: pid,
                    terminal_id: native,
                    window_name: workspace.name.clone(),
                    pane_name: name.clone(),
                    title: name,
                    active: pane.focused,
                    selected: false,
                    index: index as u32,
                    columns: viewport.0,
                    rows: viewport.1,
                });
            }
        }
        windows.push(WindowSnapshot {
            window_id: wid,
            name: workspace.name,
            panes: Vec::new(),
            selected: false,
            zoomed: false,
        });
    }
    let mut groups_by_workspace: HashMap<u64, Vec<u64>> = HashMap::new();
    for group in &metadata.snapshot.groups {
        let (Some(workspace), Some(group)) = (
            group.workspace_id.parse::<u64>().ok(),
            group.id.parse::<u64>().ok(),
        ) else {
            continue;
        };
        groups_by_workspace
            .entry(workspace)
            .or_default()
            .push(group);
    }
    for workspace in &metadata.snapshot.workspaces {
        let Some(workspace_id) = workspace.id.parse::<u64>().ok() else {
            continue;
        };
        let Some(groups) = groups_by_workspace.get(&workspace_id) else {
            continue;
        };
        let selected = choose_selected_group(
            groups,
            old_selected_groups.get(&workspace_id).copied(),
            old_flag_groups.get(&workspace_id).copied(),
            focused_groups.get(&workspace_id).copied(),
        );
        if let Some(group) = selected {
            metadata.selected_groups.insert(workspace_id, group);
        }
    }
    metadata.active_group = old_active_group
        .filter(|group| {
            groups_by_workspace
                .values()
                .any(|groups| groups.contains(group))
        })
        .or_else(|| {
            focused_tab_id
                .as_deref()
                .and_then(|tab| metadata.ids.get(&(b'g', tab.to_owned())).copied())
        })
        .or_else(|| {
            focused_workspace_id.as_deref().and_then(|workspace| {
                let workspace = metadata.ids.get(&(b'w', workspace.to_owned()))?;
                metadata.selected_groups.get(workspace).copied()
            })
        });
    metadata.refresh_group_selection();
    metadata.ids.retain(|key, _| live_ids.contains(key));
    let selected_group = metadata.active_group;
    let selected = choose_selected_pane(
        &metadata,
        &flat,
        old_selected,
        old_target.as_ref(),
        preferred,
        selected_group,
    );
    if let Some(selected) = selected {
        metadata.select(selected);
    }
    let stale: Vec<u64> = mapping
        .iter()
        .filter(|(id, _)| !metadata.panes.contains_key(id))
        .map(|(_, native)| *native)
        .collect();
    mapping.retain(|id, _| metadata.panes.contains_key(id));
    Ok(SnapshotProjection {
        metadata,
        mapping,
        selected,
        flat,
        windows,
        stale,
    })
}

async fn subscribe(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    socket: &str,
    panes: &HashSet<String>,
    expected_epoch: u64,
) -> Result<JsonChannel, FlowFailure> {
    if !shared.current_request_epoch(expected_epoch) {
        return Err(FlowFailure::Stale);
    }
    let mut channel = open_api(shared, session, socket).await?;
    if !shared.current_request_epoch(expected_epoch) {
        channel.close().await;
        return Err(FlowFailure::Stale);
    }
    let mut subscriptions: Vec<Value> = [
        "workspace.created",
        "workspace.updated",
        "workspace.renamed",
        "workspace.closed",
        "workspace.moved",
        "workspace.reordered",
        "tab.created",
        "tab.closed",
        "tab.renamed",
        "tab.moved",
        "pane.created",
        "pane.closed",
        "pane.updated",
        "pane.moved",
        "pane.exited",
        "pane.agent_detected",
    ]
    .into_iter()
    .map(|event| json!({"type":event}))
    .collect();
    subscriptions.extend(
        panes
            .iter()
            .map(|pane| json!({"type":"pane.agent_status_changed", "pane_id":pane})),
    );
    if !shared.current_request_epoch(expected_epoch) {
        channel.close().await;
        return Err(FlowFailure::Stale);
    }
    channel
        .request(
            "subscribe",
            "events.subscribe",
            &json!({"subscriptions":subscriptions}),
        )
        .await?;
    if !shared.current_request_epoch(expected_epoch) {
        channel.close().await;
        return Err(FlowFailure::Stale);
    }
    let response = tokio::select! {
        _ = shared.cancelled() => return Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => return Err(FlowFailure::Stale),
        response = tokio::time::timeout(SSH_STAGE_TIMEOUT, channel.next()) => response.map_err(|_| FlowFailure::Transport)??,
    };
    if !shared.current_request_epoch(expected_epoch) {
        channel.close().await;
        return Err(FlowFailure::Stale);
    }
    match wire::response("subscribe", &response).map_err(|_| FlowFailure::HerdrProtocol)? {
        wire::ApiResponse::Ok { .. } => {}
        wire::ApiResponse::Error { .. } => return Err(FlowFailure::HerdrUnsupported),
    }
    Ok(channel)
}

impl HerdrClient<'_> {
    fn request_epoch(&self) -> u64 {
        self.command_epoch.unwrap_or(self.operation_epoch)
    }

    async fn synchronize(&mut self) -> Result<(), FlowFailure> {
        // Every snapshot applied below has an acknowledged subscription for
        // exactly its pane set. Bound topology churn; reconnect can obtain a
        // new coherent baseline rather than silently dropping status events.
        for _ in 0..8 {
            let result = self.request("session.snapshot", json!({})).await?;
            let snapshot =
                decode_snapshot_result(&result).map_err(|_| FlowFailure::HerdrProtocol)?;
            if snapshot.version != wire::HERDR_VERSION || snapshot.protocol != wire::HERDR_PROTOCOL
            {
                return Err(FlowFailure::HerdrIncompatible);
            }
            let pane_ids: HashSet<String> = snapshot
                .workspaces
                .iter()
                .flat_map(|w| &w.groups)
                .flat_map(|g| &g.panes)
                .map(|p| p.pane_id.clone())
                .collect();
            if pane_ids == self.subscribed_panes {
                return self.apply_snapshot(snapshot);
            }
            let subscription = subscribe(
                self.shared,
                self.session,
                &self.socket,
                &pane_ids,
                self.request_epoch(),
            )
            .await?;
            self.subscription.close().await;
            self.subscription = subscription;
            self.subscribed_panes = pane_ids;
        }
        Err(FlowFailure::Transport)
    }

    fn apply_snapshot(&mut self, snapshot: wire::HerdrSessionSnapshot) -> Result<(), FlowFailure> {
        if let Some(expected) = self.strict_terminal.as_deref()
            && !snapshot_contains_stable_terminal(&snapshot, expected)
        {
            return Err(FlowFailure::HerdrSessionMissing);
        }
        let (metadata, mapping, old_selected) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            (
                state.herdr.clone(),
                state.pane_terminals.clone(),
                state.selected_pane,
            )
        };
        let SnapshotProjection {
            mut metadata,
            mapping,
            mut selected,
            mut flat,
            mut windows,
            stale,
        } = project_snapshot(
            metadata,
            mapping,
            old_selected,
            snapshot,
            self.runtime.clone().unwrap_or_else(|| "default".to_owned()),
            self.viewport,
            self.shared.generation,
        )?;
        {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if self.shared.is_cancelled() || state.generation != self.shared.generation {
                return Err(FlowFailure::Stale);
            }
            // A serialized selection command may have committed a newer
            // intent while this snapshot was being prepared. Preserve that
            // actor-owned selection at the commit boundary instead of
            // overwriting it with the snapshot's older clone.
            if state.selected_pane != old_selected {
                selected = state
                    .selected_pane
                    .filter(|id| metadata.panes.contains_key(id));
                for (workspace, group) in &state.herdr.selected_groups {
                    if metadata.groups.contains_key(group) {
                        metadata.selected_groups.insert(*workspace, *group);
                    }
                }
                if let Some(group) = state
                    .herdr
                    .active_group
                    .filter(|group| metadata.groups.contains_key(group))
                {
                    metadata.active_group = Some(group);
                }
                if let Some(selected) = selected {
                    metadata.select(selected);
                } else {
                    for terminal in &mut metadata.snapshot.terminals {
                        terminal.selected = false;
                    }
                    metadata.refresh_group_selection();
                }
            }
            if let Some(expected) = self.strict_terminal.as_deref() {
                selected = Some(
                    metadata
                        .panes
                        .iter()
                        .find(|(_, pane)| pane.terminal_id == expected)
                        .map(|(id, _)| *id)
                        .ok_or(FlowFailure::HerdrSessionMissing)?,
                );
                metadata.select(selected.expect("strict Herdr selection is present"));
            }
            for pane in &mut flat {
                pane.selected = Some(pane.pane_id) == selected;
            }
            for window in &mut windows {
                window.panes = flat
                    .iter()
                    .filter(|pane| pane.window_id == window.window_id)
                    .cloned()
                    .collect();
                window.selected = window.panes.iter().any(|pane| pane.selected);
            }
        }
        let projection = SnapshotProjection {
            metadata,
            mapping,
            selected,
            flat,
            windows,
            stale,
        };
        if self.strict_terminal.is_some() {
            // Recovery uses a two-phase commit: the new aliases/topology and
            // any newly allocated native terminals remain private to this
            // actor until a fresh complete frame has been restored.
            self.discard_staged_projection();
            self.staged_projection = Some(projection);
            return Ok(());
        }
        let expected_epoch = self.request_epoch();
        self.commit_projection(&projection, expected_epoch)?;
        self.cleanup_projection_stale(&projection);
        Ok(())
    }

    fn discard_staged_projection(&mut self) {
        let Some(projection) = self.staged_projection.take() else {
            return;
        };
        let committed = self
            .shared
            .session
            .lock()
            .map(|state| {
                state
                    .pane_terminals
                    .values()
                    .copied()
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        for native in projection.mapping.values().copied() {
            if native != self.shared.terminal_id() && !committed.contains(&native) {
                registry::detach_transport(native, self.shared.generation);
                registry::destroy_terminal(native);
            }
        }
    }

    fn commit_projection(
        &self,
        projection: &SnapshotProjection,
        expected_epoch: u64,
    ) -> Result<(), FlowFailure> {
        let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        if self.shared.is_cancelled()
            || state.generation != self.shared.generation
            || state.operation_epoch != expected_epoch
        {
            return Err(FlowFailure::Stale);
        }
        apply_projection_state(&mut state, projection);
        drop(state);
        Ok(())
    }
}

fn apply_projection_state(state: &mut SessionState, projection: &SnapshotProjection) {
    state.herdr = projection.metadata.clone();
    state.pane_terminals = projection.mapping.clone();
    state.selected_pane = projection.selected;
    state.snapshot = SessionSnapshot {
        windows: projection.windows.clone(),
        panes: projection.flat.clone(),
        selected_pane: projection.selected,
    };
}

impl HerdrClient<'_> {
    fn cleanup_projection_stale(&self, projection: &SnapshotProjection) {
        for native in &projection.stale {
            registry::detach_transport(*native, self.shared.generation);
            if *native != self.shared.terminal_id() {
                registry::destroy_terminal(*native);
            }
        }
    }

    /// Commit a pane selection only after the serialized actor has accepted
    /// the request. The payload is the source of truth for both the local
    /// projection and the controller target; this deliberately does not look
    /// up a replacement selection from a shared field that a later request
    /// could have changed.
    async fn select_pane_payload(
        &mut self,
        window_id: u64,
        pane_id: u64,
    ) -> Result<(), FlowFailure> {
        let expected_epoch = self.request_epoch();
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if state.operation_epoch != expected_epoch {
                return Err(FlowFailure::Stale);
            }
            if state
                .herdr
                .panes
                .get(&pane_id)
                .is_none_or(|pane| pane.workspace != window_id)
                || !state
                    .snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == pane_id && pane.window_id == window_id)
                || !state.pane_terminals.contains_key(&pane_id)
            {
                return Err(FlowFailure::HerdrOperation);
            }
            state.selected_pane = Some(pane_id);
            state.herdr.select(pane_id);
            mark_selected(&mut state.snapshot, pane_id);
            state.terminal_input_ready = false;
        }
        self.release().await?;
        self.activate_pane(pane_id).await
    }

    async fn command(&mut self, command: ControlCommand) -> Result<(), FlowFailure> {
        match command {
            ControlCommand::SetTerminalVisible { visible } => {
                if !visible {
                    self.release().await?;
                    return Ok(());
                }
                return self.activate_selected().await;
            }
            ControlCommand::SelectPane { window_id, pane_id } => {
                return self.select_pane_payload(window_id, pane_id).await;
            }
            ControlCommand::SelectGroup { group_id } => {
                {
                    let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
                    if !state.herdr.groups.contains_key(&group_id) {
                        return Err(FlowFailure::HerdrOperation);
                    }
                    let id = state
                        .herdr
                        .snapshot
                        .terminals
                        .iter()
                        .find(|terminal| {
                            terminal.group_id == group_id.to_string() && terminal.active
                        })
                        .or_else(|| {
                            state
                                .herdr
                                .snapshot
                                .terminals
                                .iter()
                                .find(|terminal| terminal.group_id == group_id.to_string())
                        })
                        .and_then(|terminal| terminal.id.parse().ok());
                    state.selected_pane = id;
                    state.herdr.select_group(group_id);
                    if let Some(id) = id {
                        state.herdr.select(id);
                        mark_selected(&mut state.snapshot, id);
                    } else {
                        state.snapshot.selected_pane = None;
                        for pane in &mut state.snapshot.panes {
                            pane.selected = false;
                        }
                        for window in &mut state.snapshot.windows {
                            window.selected = false;
                            for pane in &mut window.panes {
                                pane.selected = false;
                            }
                        }
                    }
                }
                return self.activate_selected().await;
            }
            ControlCommand::RefreshTerminal => {
                self.release().await?;
                self.synchronize().await?;
                return self.activate_selected().await;
            }
            ControlCommand::RefreshRuntimes
            | ControlCommand::SelectRuntime { .. }
            | ControlCommand::CreateRuntime { .. } => {
                // A picker action can already be queued when the first
                // selection reaches Ready. It belongs to the old picker
                // state and must not tear down the selected runtime.
                return Ok(());
            }
            ControlCommand::PromoteRuntimeBrowse {
                token,
                source_owner,
                provisional_owner,
            } => {
                if super::promote_runtime_browse(
                    token,
                    source_owner,
                    provisional_owner,
                    Arc::clone(self.shared),
                )
                .is_err()
                {
                    super::mark_runtime_browse_failed(
                        token,
                        "runtime_browse_promotion_failed",
                        "The selected runtime could not be promoted safely.".to_owned(),
                    );
                    self.shared
                        .invalidate_explicitly("runtime_browse_promotion_failed");
                    return Err(FlowFailure::Stale);
                }
                if let Some(controller) = self.controller.as_mut()
                    && controller.native == provisional_owner
                {
                    controller.native = source_owner;
                }
                return Ok(());
            }
            ControlCommand::RetryRecovery | ControlCommand::ConfirmRecovery { .. } => {
                // Recovery commands are consumed by the native recovery
                // coordinator before a selected backend actor is started.
                return Ok(());
            }
            _ => {}
        }
        if matches!(
            command,
            ControlCommand::CloseGroup { .. } | ControlCommand::ClosePane { .. }
        ) {
            // Closing a last pane/tab can implicitly close a worktree group.
            // Re-read its current membership at the explicit action boundary,
            // including workspace-created events still queued for this actor.
            self.synchronize().await?;
        }
        let (method, params) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            let workspace = |id| {
                state
                    .herdr
                    .workspaces
                    .get(&id)
                    .cloned()
                    .ok_or(FlowFailure::HerdrOperation)
            };
            let group = |id| {
                state
                    .herdr
                    .groups
                    .get(&id)
                    .cloned()
                    .ok_or(FlowFailure::HerdrOperation)
            };
            let pane = |id| {
                state
                    .herdr
                    .panes
                    .get(&id)
                    .cloned()
                    .ok_or(FlowFailure::HerdrOperation)
            };
            match command {
                ControlCommand::RefreshRuntimes
                | ControlCommand::SelectRuntime { .. }
                | ControlCommand::CreateRuntime { .. }
                | ControlCommand::PromoteRuntimeBrowse { .. }
                | ControlCommand::SelectPane { .. }
                | ControlCommand::SelectGroup { .. }
                | ControlCommand::RefreshTerminal
                | ControlCommand::SetTerminalVisible { .. } => unreachable!(),
                ControlCommand::RetryRecovery | ControlCommand::ConfirmRecovery { .. } => {
                    unreachable!()
                }
                ControlCommand::CreateWorkspace { name } => {
                    ("workspace.create", json!({"label":name, "focus":false}))
                }
                ControlCommand::RenameWorkspace { window_id, name } => (
                    "workspace.rename",
                    json!({"workspace_id":workspace(window_id)?,"label":name}),
                ),
                ControlCommand::CloseWorkspace { window_id } => (
                    "workspace.close",
                    json!({"workspace_id":workspace(window_id)?,"close_group":false}),
                ),
                ControlCommand::CreateGroup { window_id, name } => (
                    "tab.create",
                    json!({"workspace_id":workspace(window_id)?,"label":name,"focus":false}),
                ),
                ControlCommand::RenameGroup { group_id, name } => (
                    "tab.rename",
                    json!({"tab_id":group(group_id)?,"label":name}),
                ),
                ControlCommand::CloseGroup { group_id } => {
                    let workspace = state
                        .herdr
                        .snapshot
                        .groups
                        .iter()
                        .find(|item| item.id == group_id.to_string())
                        .and_then(|item| item.workspace_id.parse::<u64>().ok())
                        .ok_or(FlowFailure::HerdrOperation)?;
                    if state.herdr.linked_worktree_parents.contains(&workspace) {
                        return Err(FlowFailure::HerdrWorkspaceGroup);
                    }
                    ("tab.close", json!({"tab_id":group(group_id)?}))
                }
                ControlCommand::CreatePane { window_id } => {
                    let group_id = state
                        .herdr
                        .selected_groups
                        .get(&window_id)
                        .copied()
                        .ok_or(FlowFailure::HerdrOperation)?;
                    let target =
                        state
                            .selected_pane
                            .and_then(|id| state.herdr.panes.get(&id))
                            .filter(|pane| pane.workspace == window_id && pane.group == group_id)
                            .or_else(|| {
                                state.herdr.panes.values().find(|pane| {
                                    pane.workspace == window_id && pane.group == group_id
                                })
                            })
                            .ok_or(FlowFailure::HerdrOperation)?;
                    (
                        "pane.split",
                        json!({"target_pane_id":target.pane_id,"direction":"right","focus":false}),
                    )
                }
                ControlCommand::RenamePane { pane_id, name } => (
                    "pane.rename",
                    json!({"pane_id":pane(pane_id)?.pane_id,"label":name}),
                ),
                ControlCommand::ClosePane { pane_id } => {
                    let pane = pane(pane_id)?;
                    if state
                        .herdr
                        .linked_worktree_parents
                        .contains(&pane.workspace)
                    {
                        return Err(FlowFailure::HerdrWorkspaceGroup);
                    }
                    ("pane.close", json!({"pane_id":pane.pane_id}))
                }
            }
        };
        // Mutations have a definite place after previously acknowledged input.
        // A selected remote process may exit during the request; release its
        // controller first so its EOF cannot masquerade as a transport fault.
        let release =
            method.ends_with(".close") || method.ends_with(".create") || method == "pane.split";
        if release {
            self.release().await?;
        }
        let result = self.request(method, params).await?;
        let created = result
            .get("root_pane")
            .or_else(|| {
                if method == "pane.split" {
                    result.get("pane")
                } else {
                    None
                }
            })
            .and_then(|pane| pane["terminal_id"].as_str())
            .map(str::to_owned);
        self.synchronize().await?;
        if let Some(remote_id) = created {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if let Some(id) = state
                .herdr
                .panes
                .iter()
                .find(|(_, pane)| pane.terminal_id == remote_id)
                .map(|(id, _)| *id)
            {
                state.selected_pane = Some(id);
                state.herdr.select(id);
                mark_selected(&mut state.snapshot, id);
            }
        }
        self.activate_selected().await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, FlowFailure> {
        let expected_epoch = self.request_epoch();
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        self.request_id = self
            .request_id
            .checked_add(1)
            .ok_or(FlowFailure::HerdrProtocol)?;
        let id = format!("meeterm-{}", self.request_id);
        let mut channel = open_api(self.shared, self.session, &self.socket).await?;
        // Opening a direct stream is an awaited boundary. A foreground loss,
        // confirmation invalidation, or newer command epoch during that wait
        // must prevent the request bytes from being sent on this channel.
        if !self.shared.current_request_epoch(expected_epoch) {
            channel.close().await;
            return Err(FlowFailure::Stale);
        }
        channel.request(&id, method, &params).await?;
        let response = tokio::select! {
            _ = self.shared.cancelled() => return Err(FlowFailure::Stale),
            _ = self.shared.explicit_cleanup() => return Err(FlowFailure::Stale),
            response = tokio::time::timeout(SSH_STAGE_TIMEOUT, channel.next()) => response.map_err(|_| FlowFailure::Transport)??,
        };
        // The response wait is another awaited boundary. The API response may
        // already be buffered while a foreground/recovery notification bumps
        // the session epoch; do not let that old response commit a mutation
        // or be reported as the result of a newer operation.
        if !self.shared.current_request_epoch(expected_epoch) {
            channel.close().await;
            return Err(FlowFailure::Stale);
        }
        channel.close().await;
        match wire::response(&id, &response).map_err(|_| FlowFailure::HerdrProtocol)? {
            wire::ApiResponse::Ok { result, .. } => Ok(result),
            wire::ApiResponse::Error { code, .. } => Err(api_failure(&code)),
        }
    }

    async fn release(&mut self) -> Result<(), FlowFailure> {
        let Some(mut controller) = self.controller.take() else {
            return Ok(());
        };
        let controller_epoch = controller.epoch;
        registry::detach_transport(controller.native, self.shared.generation);
        // Drop queued input immediately. Do not reacquire until the existing
        // CLI confirms that Herdr has removed this direct controller's lease.
        controller.input.close();
        // A release is a remote mutation too. A normal epoch invalidation
        // closes the stream locally, but an explicit shutdown is the ordered
        // handoff boundary: no newer generation is installed until this old
        // actor finishes, so release its controller before SSH teardown.
        let explicit_shutdown = self.shared.explicit_cleanup_requested();
        if !self.shared.current_request_epoch(controller_epoch) && !explicit_shutdown {
            controller.stream.close().await;
            return Ok(());
        }
        let released = tokio::time::timeout(Duration::from_secs(3), async {
            if !self.shared.current_request_epoch(controller_epoch)
                && !self.shared.explicit_cleanup_requested()
            {
                return Ok(());
            }
            controller
                .stream
                .send(json!({"type":"terminal.release"}))
                .await?;
            loop {
                match controller.stream.next().await {
                    Ok(value) if value["type"] == "terminal.closed" => return Ok(()),
                    Ok(_) => {}
                    Err(FlowFailure::RemoteClosed) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|_| FlowFailure::Transport)
        .and_then(|result| result);
        controller.stream.close().await;
        released
    }

    async fn activate_selected(&mut self) -> Result<(), FlowFailure> {
        self.activate(None).await
    }

    async fn activate_pane(&mut self, pane_id: u64) -> Result<(), FlowFailure> {
        self.activate(Some(pane_id)).await
    }

    /// Finish a foreground wake for the live controller without opening a new
    /// Herdr lease. The transition guard closes the race with a concurrent
    /// background call; the native terminal lock has already been released
    /// before the session readiness refresh below.
    fn resume_controller_on_foreground(&self) {
        let _transition = self.shared.foreground_transition_lock();
        if !self.shared.is_foreground() {
            return;
        }
        if let Some(controller) = self.controller.as_ref()
            && controller.ready
        {
            registry::resume_transport(controller.native, self.shared.generation);
        }
        self.shared.refresh_terminal_input_ready();
    }

    async fn activate(&mut self, requested_pane: Option<u64>) -> Result<(), FlowFailure> {
        let operation_epoch = self.request_epoch();
        if !self.shared.current_request_epoch(operation_epoch) {
            return Err(FlowFailure::Stale);
        }
        let (selected, remote, native, visible) = {
            if let Some(requested_pane) = requested_pane {
                let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
                if state.selected_pane != Some(requested_pane) {
                    return Err(FlowFailure::Stale);
                }
                let remote = state
                    .herdr
                    .panes
                    .get(&requested_pane)
                    .cloned()
                    .ok_or(FlowFailure::HerdrOperation)?;
                let native = state
                    .pane_terminals
                    .get(&requested_pane)
                    .copied()
                    .ok_or(FlowFailure::HerdrOperation)?;
                (
                    Some(requested_pane),
                    Some(remote),
                    Some(native),
                    state.terminal_visible,
                )
            } else if let Some(projection) = self.staged_projection.as_ref() {
                let visible = self
                    .shared
                    .session
                    .lock()
                    .map_err(|_| FlowFailure::Stale)?
                    .terminal_visible;
                let selected = projection.selected;
                let remote = selected
                    .and_then(|id| projection.metadata.panes.get(&id))
                    .cloned();
                let native = selected.and_then(|id| projection.mapping.get(&id)).copied();
                (selected, remote, native, visible)
            } else {
                let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
                let visible = state.terminal_visible;
                let selected = state.selected_pane.filter(|_| visible);
                let remote = selected.and_then(|id| state.herdr.panes.get(&id)).cloned();
                let native = selected
                    .and_then(|id| state.pane_terminals.get(&id))
                    .copied();
                (selected, remote, native, visible)
            }
        };
        if !visible {
            self.release().await?;
            return Ok(());
        }
        if self
            .controller
            .as_ref()
            .is_some_and(|controller| Some(controller.pane) == selected)
        {
            // A healthy Herdr controller stays attached while hidden; only
            // the live actor's foreground wake re-arms its native gate. If
            // the first frame arrived while hidden, `ready` is already true
            // but the Suspended gate remains closed until this point.
            self.resume_controller_on_foreground();
            return Ok(());
        }
        self.release().await?;
        if !self.shared.current_request_epoch(operation_epoch) {
            return Err(FlowFailure::Stale);
        }
        let (Some(selected), Some(remote), Some(native)) = (selected, remote, native) else {
            if !self.shared.mark_ready_at_epoch(operation_epoch) {
                return Err(FlowFailure::Stale);
            }
            return Ok(());
        };
        if !self.shared.is_foreground() {
            return Ok(());
        }
        let (cols, rows) = self.viewport;
        let command = wire::command_with_executable(
            &self.executable,
            self.runtime.as_deref(),
            &[
                "terminal",
                "session",
                "control",
                &remote.terminal_id,
                "--cols",
                &cols.to_string(),
                "--rows",
                &rows.to_string(),
            ],
        )
        .map_err(|_| FlowFailure::HerdrProtocol)?;
        let channel = await_stage(
            self.shared,
            self.session.channel_open_session(),
            SSH_STAGE_TIMEOUT,
            FlowFailure::Channel,
        )
        .await?;
        if !self.shared.current_request_epoch(operation_epoch) {
            return Err(FlowFailure::Stale);
        }
        channel
            .exec(true, command)
            .await
            .map_err(|_| FlowFailure::Channel)?;
        if !self.shared.current_request_epoch(operation_epoch) {
            return Err(FlowFailure::Stale);
        }
        let stream = JsonChannel::new(channel);
        let (input, receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        let (resize, sizes) = watch::channel(self.viewport);
        let terminal = registry::shared_terminal(native).map_err(|_| FlowFailure::Stale)?;
        {
            let mut terminal = terminal.lock().map_err(|_| FlowFailure::Stale)?;
            terminal
                .begin_remote(self.shared.generation)
                .map_err(|_| FlowFailure::Stale)?;
            terminal
                .resize_from_remote(cols, rows)
                .map_err(|_| FlowFailure::HerdrProtocol)?;
            terminal
                .attach_semantic_transport(self.shared.generation, input, resize)
                .map_err(|_| FlowFailure::Stale)?;
        }
        self.shared.suspend_terminal_if_background(native);
        self.controller = Some(Controller {
            pane: selected,
            native,
            epoch: operation_epoch,
            stream,
            input: receiver,
            sizes,
            seq: None,
            ready: false,
        });
        // No user input is enabled before the first complete remote display.
        // A competing direct controller fails here, with no --takeover.
        let frame = tokio::select! {
            _ = self.shared.cancelled() => return Err(FlowFailure::Stale),
            _ = self.shared.explicit_cleanup() => return Err(FlowFailure::Stale),
            frame = tokio::time::timeout(SSH_STAGE_TIMEOUT, self.controller.as_mut().unwrap().stream.next()) => {
                frame.map_err(|_| FlowFailure::Transport)??
            },
        };
        self.frame(frame)
    }

    fn frame(&mut self, value: Value) -> Result<(), FlowFailure> {
        // Validate the controller epoch and current target before decoding the
        // record. The strict first-frame path below rechecks the same target
        // while holding the session lock in the Ready transaction; no native
        // Term mutation occurs in this preliminary stage.
        let (controller_epoch, controller_pane, controller_native, controller_ready, old_seq) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            let controller = self.controller.as_ref().ok_or(FlowFailure::Stale)?;
            if self.shared.is_cancelled()
                || state.generation != self.shared.generation
                || state.operation_epoch != controller.epoch
            {
                return Err(FlowFailure::Stale);
            }
            if (!state.terminal_visible && self.strict_terminal.is_none())
                || state.selected_pane != Some(controller.pane)
            {
                return Ok(());
            }
            (
                controller.epoch,
                controller.pane,
                controller.native,
                controller.ready,
                controller.seq,
            )
        };
        let frame =
            match wire::decode_terminal_record(&value).map_err(|_| FlowFailure::HerdrProtocol)? {
                wire::TerminalRecord::Frame(frame) => frame,
                wire::TerminalRecord::Closed { reason } => {
                    if let Some(controller) = self.controller.as_mut() {
                        controller.ready = false;
                        controller.input.close();
                        registry::detach_transport(controller.native, self.shared.generation);
                    }
                    return Err(
                        if reason.as_deref().is_some_and(|reason| {
                            reason.contains("taken over")
                                || reason.contains("already has an attached client")
                        }) {
                            FlowFailure::HerdrController
                        } else {
                            FlowFailure::RemoteClosed
                        },
                    );
                }
                _ => return Err(FlowFailure::HerdrProtocol),
            };
        let wire::TerminalFrame {
            seq,
            width: cols,
            height: rows,
            full,
            bytes,
        } = frame;
        if old_seq.is_some_and(|old| seq <= old) || (!full && old_seq.is_none()) {
            return Err(FlowFailure::HerdrProtocol);
        }

        if self.strict_terminal.is_some() && !controller_ready {
            if !full {
                return Err(FlowFailure::HerdrProtocol);
            }
            let projection = self
                .staged_projection
                .as_ref()
                .ok_or(FlowFailure::HerdrProtocol)?;
            let stale = projection.stale.clone();
            let commit = self
                .shared
                .commit_ready_at_epoch_result(controller_epoch, |state| {
                    // `commit_ready_at_epoch_result` owns the `info -> session`
                    // boundary. Recheck visibility/selection inside it so a
                    // queued hide or selection cannot let this prepared frame
                    // become a controller target without a fresh command.
                    if state.selected_pane != Some(controller_pane) || !state.terminal_visible {
                        return Err(FlowFailure::Stale);
                    }
                    // The registry helper keeps its map and selected Terminal
                    // lock through preflight, Term replacement, and transport
                    // Ready. It has no fallible step after the native apply;
                    // projection and session Ready are committed immediately
                    // in this same callback.
                    registry::restore_remote_display_and_ready(
                        controller_native,
                        self.shared.generation,
                        cols,
                        rows,
                        &bytes,
                    )
                    .map_err(|_| FlowFailure::HerdrProtocol)?;
                    apply_projection_state(state, projection);
                    Ok(())
                });
            if let Err(failure) = commit {
                registry::detach_transport(controller_native, self.shared.generation);
                return Err(failure);
            }

            let controller = self.controller.as_mut().ok_or(FlowFailure::Stale)?;
            controller.seq = Some(seq);
            controller.ready = true;
            for native in stale {
                registry::detach_transport(native, self.shared.generation);
                if native != self.shared.terminal_id() {
                    registry::destroy_terminal(native);
                }
            }
            self.staged_projection = None;
            self.strict_terminal = None;
            // The first full frame can race a foreground transition. Let the
            // live actor finish the native rearm even if the notify edge was
            // consumed while this strict commit was in progress.
            self.resume_controller_on_foreground();
            return Ok(());
        }

        let terminal =
            registry::shared_terminal(controller_native).map_err(|_| FlowFailure::Stale)?;
        let mut terminal = terminal.lock().map_err(|_| FlowFailure::Stale)?;
        if full {
            terminal
                .restore_remote_display(self.shared.generation, cols, rows, &bytes)
                .map_err(|_| FlowFailure::HerdrProtocol)?;
        } else {
            terminal
                .resize_from_remote(cols, rows)
                .map_err(|_| FlowFailure::HerdrProtocol)?;
            if !terminal.feed_remote(self.shared.generation, &bytes) {
                return Err(FlowFailure::Transport);
            }
        }
        let controller = self.controller.as_mut().ok_or(FlowFailure::Stale)?;
        controller.seq = Some(seq);
        let became_ready = if !controller.ready {
            controller.ready = terminal.mark_transport_ready(self.shared.generation);
            if !controller.ready {
                // A queued hide/show edge can revoke the binding while the
                // desired pane stays the same. Its command will reacquire.
                return Ok(());
            }
            true
        } else {
            false
        };
        let controller_native = controller.native;
        drop(terminal);
        if became_ready && !self.shared.mark_ready_at_epoch(controller_epoch) {
            registry::detach_transport(controller_native, self.shared.generation);
            return Err(FlowFailure::Stale);
        }
        // If foreground returned while this frame was being decoded, the
        // actor may have missed the edge-triggered notify while it was not in
        // the select loop. Recheck the live controller here so a valid first
        // frame still completes the Suspended -> Ready wake safely.
        self.resume_controller_on_foreground();
        Ok(())
    }

    async fn input_request(
        &mut self,
        epoch: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, FlowFailure> {
        if !self.shared.current_terminal_input_is_ready(epoch) {
            return Err(FlowFailure::Stale);
        }
        self.command_epoch = Some(epoch);
        let result = self.request(method, params).await;
        self.command_epoch = None;
        result
    }

    async fn input(&mut self, input: SemanticInput) -> Result<(), FlowFailure> {
        let Some(controller) = self
            .controller
            .as_ref()
            .filter(|controller| controller.ready)
        else {
            return Ok(());
        };
        if !self
            .shared
            .current_terminal_input_is_ready(controller.epoch)
        {
            return Ok(());
        }
        let pane = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if state.selected_pane != Some(controller.pane)
                || !state.terminal_visible
                || !self.shared.is_foreground()
            {
                return Ok(());
            }
            state
                .herdr
                .panes
                .get(&controller.pane)
                .ok_or(FlowFailure::HerdrOperation)?
                .pane_id
                .clone()
        };
        let epoch = controller.epoch;
        if let SemanticInput::Scroll(lines) = input {
            if lines != 0 {
                if !self.shared.current_terminal_input_is_ready(epoch) {
                    return Ok(());
                }
                controller.stream.send(json!({"type":"terminal.scroll", "direction":if lines>0 {"up"} else {"down"},
                    "lines":lines.unsigned_abs().min(u16::MAX as u32)})).await?;
            }
            return Ok(());
        }
        // The public send APIs encode against the remote PTY's modes, but
        // unlike direct terminal.input they do not reset remote scrollback.
        self.input_request(
            epoch,
            "pane.scroll",
            json!({"pane_id":pane,"offset_from_bottom":0}),
        )
        .await?;
        {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if self.shared.is_cancelled()
                || !self.shared.is_foreground()
                || !state.terminal_visible
                || self
                    .controller
                    .as_ref()
                    .is_none_or(|controller| state.selected_pane != Some(controller.pane))
            {
                return Ok(());
            }
        }
        if !self.shared.current_terminal_input_is_ready(epoch) {
            return Ok(());
        }
        match input {
            SemanticInput::Scroll(_) => unreachable!(),
            SemanticInput::Paste(text) => {
                self.input_request(
                    epoch,
                    "pane.send_input",
                    json!({"pane_id":pane,"text":text}),
                )
                .await?;
            }
            SemanticInput::Text(text, modifiers)
                if !modifiers.contains(Modifiers::CTRL) && !modifiers.contains(Modifiers::ALT) =>
            {
                self.input_request(epoch, "pane.send_text", json!({"pane_id":pane,"text":text}))
                    .await?;
            }
            SemanticInput::Text(text, modifiers) => {
                let keys = text
                    .chars()
                    .map(|character| character_key(character, modifiers))
                    .collect::<Option<Vec<_>>>();
                if let Some(keys) = keys {
                    self.input_request(
                        epoch,
                        "pane.send_keys",
                        json!({"pane_id":pane,"keys":keys}),
                    )
                    .await?;
                } else {
                    let text = String::from_utf8(encode_text(&text, modifiers))
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    self.input_request(
                        epoch,
                        "pane.send_text",
                        json!({"pane_id":pane,"text":text}),
                    )
                    .await?;
                }
            }
            SemanticInput::Key(key, modifiers) => {
                if let Some(key) = semantic_key(key, modifiers) {
                    self.input_request(
                        epoch,
                        "pane.send_keys",
                        json!({"pane_id":pane,"keys":[key]}),
                    )
                    .await?;
                } else {
                    // Herdr 0.9.0's public key-name parser has no navigation
                    // names for Home/End/Insert/Delete/PageUp/PageDown. Keep
                    // their existing xterm byte form explicit and ordered.
                    let text = String::from_utf8(encode_key(key, modifiers, false))
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    self.input_request(
                        epoch,
                        "pane.send_text",
                        json!({"pane_id":pane,"text":text}),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

fn api_failure(code: &str) -> FlowFailure {
    match code {
        "workspace_group_close_required" | "confirmation_required" => {
            FlowFailure::HerdrWorkspaceGroup
        }
        "unsupported_method" | "method_not_found" | "unsupported_in_app_mode" => {
            FlowFailure::HerdrUnsupported
        }
        _ => FlowFailure::HerdrOperation,
    }
}

fn modifiers_prefix(modifiers: Modifiers) -> String {
    let mut prefix = String::new();
    if modifiers.contains(Modifiers::CTRL) {
        prefix.push_str("ctrl+");
    }
    if modifiers.contains(Modifiers::ALT) {
        prefix.push_str("alt+");
    }
    if modifiers.contains(Modifiers::SHIFT) {
        prefix.push_str("shift+");
    }
    prefix
}

fn semantic_key(key: KeyCode, modifiers: Modifiers) -> Option<String> {
    let name = match key {
        KeyCode::Escape => "escape",
        KeyCode::Tab => "tab",
        KeyCode::Enter => "enter",
        KeyCode::Backspace => "backspace",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Interrupt => return Some(format!("{}ctrl+c", modifiers_prefix(modifiers))),
        KeyCode::F1 => "f1",
        KeyCode::F2 => "f2",
        KeyCode::F3 => "f3",
        KeyCode::F4 => "f4",
        KeyCode::F5 => "f5",
        KeyCode::F6 => "f6",
        KeyCode::F7 => "f7",
        KeyCode::F8 => "f8",
        KeyCode::F9 => "f9",
        KeyCode::F10 => "f10",
        KeyCode::F11 => "f11",
        KeyCode::F12 => "f12",
        _ => return None,
    };
    Some(format!("{}{name}", modifiers_prefix(modifiers)))
}

fn character_key(character: char, modifiers: Modifiers) -> Option<String> {
    let name = match character {
        '+' => "plus".to_owned(),
        ' ' => "space".to_owned(),
        '\n' | '\r' => "enter".to_owned(),
        '\t' => "tab".to_owned(),
        character if character.is_control() || character.is_whitespace() => return None,
        character => character.to_string(),
    };
    // Native committed text already has its casing/IME result applied.
    let modifiers = Modifiers::from_bits_retain(modifiers.bits() & !Modifiers::SHIFT.bits());
    Some(format!("{}{name}", modifiers_prefix(modifiers)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u64, workspace: u64, selected: bool) -> workspace::TerminalGroup {
        workspace::TerminalGroup {
            id: id.to_string(),
            workspace_id: workspace.to_string(),
            name: format!("group-{id}"),
            selected,
            agent_status: None,
        }
    }

    fn terminal(id: u64, workspace: u64, group: u64, selected: bool) -> workspace::Terminal {
        workspace::Terminal {
            id: id.to_string(),
            workspace_id: workspace.to_string(),
            group_id: group.to_string(),
            terminal_id: format!("native:{id}"),
            name: format!("terminal-{id}"),
            active: true,
            selected,
            agent: None,
        }
    }

    fn metadata_with_groups() -> Metadata {
        let mut metadata = Metadata {
            snapshot: RuntimeSnapshot {
                control: RuntimeControlSnapshot::default(),
                backend: Backend::Herdr,
                runtime: "default".to_owned(),
                groups_supported: true,
                workspaces: vec![
                    workspace::Workspace {
                        id: "100".to_owned(),
                        name: "workspace-100".to_owned(),
                        agent_status: None,
                    },
                    workspace::Workspace {
                        id: "200".to_owned(),
                        name: "workspace-200".to_owned(),
                        agent_status: None,
                    },
                ],
                groups: vec![
                    group(1, 100, true),
                    group(2, 100, false),
                    group(3, 200, true),
                ],
                terminals: vec![terminal(10, 100, 1, true)],
            },
            ..Metadata::default()
        };
        metadata.workspaces.insert(100, "workspace-100".to_owned());
        metadata.workspaces.insert(200, "workspace-200".to_owned());
        metadata.groups.insert(1, "tab-1".to_owned());
        metadata.groups.insert(2, "tab-2".to_owned());
        metadata.groups.insert(3, "tab-3".to_owned());
        metadata.panes.insert(
            10,
            RemotePane {
                pane_id: "pane-10".to_owned(),
                terminal_id: "terminal-10".to_owned(),
                workspace: 100,
                group: 1,
            },
        );
        metadata.selected_groups.insert(100, 1);
        metadata.selected_groups.insert(200, 3);
        metadata.active_group = Some(1);
        metadata
    }

    #[test]
    fn select_group_keeps_empty_group_and_one_selection_per_workspace() {
        let mut metadata = metadata_with_groups();

        metadata.select_group(2);

        assert_eq!(metadata.selected_groups.get(&100), Some(&2));
        assert_eq!(metadata.active_group, Some(2));
        assert_eq!(
            metadata
                .snapshot
                .groups
                .iter()
                .filter(|group| group.workspace_id == "100" && group.selected)
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2"]
        );
        assert_eq!(
            metadata
                .snapshot
                .groups
                .iter()
                .filter(|group| group.workspace_id == "200" && group.selected)
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            vec!["3"]
        );
        assert!(
            metadata
                .snapshot
                .terminals
                .iter()
                .all(|terminal| !terminal.selected)
        );
    }

    #[test]
    fn selected_group_precedes_conflicting_focused_flags() {
        assert_eq!(
            choose_selected_group(&[1, 2], Some(1), Some(2), Some(2)),
            Some(1)
        );
        assert_eq!(
            choose_selected_group(&[1, 2], None, Some(1), Some(2)),
            Some(1)
        );
        assert_eq!(choose_selected_group(&[1, 2], None, None, Some(2)), Some(2));
        assert_eq!(choose_selected_group(&[1, 2], None, None, None), Some(1));
    }

    #[test]
    fn empty_selected_group_does_not_fall_back_to_another_group_pane() {
        let mut metadata = metadata_with_groups();
        metadata.panes.insert(
            11,
            RemotePane {
                pane_id: "pane-11".to_owned(),
                terminal_id: "terminal-11".to_owned(),
                workspace: 100,
                group: 1,
            },
        );
        let pane = PaneSnapshot {
            window_id: 100,
            pane_id: 11,
            terminal_id: 111,
            window_name: "workspace-100".to_owned(),
            pane_name: "terminal-11".to_owned(),
            title: "terminal-11".to_owned(),
            active: true,
            selected: false,
            index: 0,
            columns: 80,
            rows: 24,
        };
        let old_target = RemotePane {
            pane_id: "old-pane".to_owned(),
            terminal_id: "old-terminal".to_owned(),
            workspace: 100,
            group: 1,
        };

        assert_eq!(
            choose_selected_pane(
                &metadata,
                std::slice::from_ref(&pane),
                None,
                Some(&old_target),
                Some(11),
                Some(2),
            ),
            None
        );

        metadata.panes.insert(
            12,
            RemotePane {
                pane_id: "pane-12".to_owned(),
                terminal_id: "terminal-12".to_owned(),
                workspace: 100,
                group: 2,
            },
        );
        let selected_pane = PaneSnapshot {
            pane_id: 12,
            ..pane
        };
        assert_eq!(
            choose_selected_pane(
                &metadata,
                std::slice::from_ref(&selected_pane),
                Some(12),
                None,
                Some(11),
                Some(2),
            ),
            Some(12)
        );
    }

    #[test]
    fn recovery_target_distinguishes_empty_group_from_disappeared_terminal() {
        let mut state = SessionState {
            recovery_group_id: Some(2),
            ..SessionState::default()
        };
        state.herdr.snapshot.groups = vec![group(2, 100, true)];
        assert_eq!(recovery_target(&state), RecoveryTarget::EmptyGroup(2));

        // The same remembered group is no longer an empty selected group once
        // its group record disappeared from the authoritative snapshot.
        state.herdr.snapshot.groups.clear();
        assert_eq!(recovery_target(&state), RecoveryTarget::Missing);

        // A non-empty group with no remembered stable terminal is the
        // disappeared-terminal case; recovery must not choose another pane.
        state.herdr.snapshot.groups = vec![group(1, 100, true)];
        state.recovery_group_id = Some(1);
        state.herdr.panes.insert(
            10,
            RemotePane {
                pane_id: "pane-10".to_owned(),
                terminal_id: "terminal-10".to_owned(),
                workspace: 100,
                group: 1,
            },
        );
        assert_eq!(recovery_target(&state), RecoveryTarget::Missing);

        state.recovery_terminal_id = Some("terminal-10".to_owned());
        assert_eq!(
            recovery_target(&state),
            RecoveryTarget::Terminal("terminal-10".to_owned())
        );
    }

    #[test]
    fn confirmed_epoch_is_rechecked_at_discovery_controller_and_mutation_boundaries() {
        for boundary in ["confirm-discovery", "controller-open", "mutation-open"] {
            let owner = registry::create_terminal(80, 24).expect("epoch test terminal");
            let generation = 78_000 + u64::from(owner as u32);
            let shared = Arc::new(ConnectionShared::new(
                owner,
                generation,
                "epoch-herdr.example".to_owned(),
                22,
                std::path::PathBuf::from("/tmp/epoch-herdr-known-hosts"),
            ));
            shared
                .session
                .lock()
                .expect("epoch session state")
                .generation = generation;
            let accepted_epoch = shared.operation_epoch();
            let opened = Arc::new(std::sync::Barrier::new(2));
            let release = Arc::new(std::sync::Barrier::new(2));
            let stale_send = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_shared = Arc::clone(&shared);
            let worker_opened = Arc::clone(&opened);
            let worker_release = Arc::clone(&release);
            let worker_sent = Arc::clone(&stale_send);
            let handle = std::thread::spawn(move || {
                // The channel/API open has completed, but the request/exec is
                // still unsent. This is the exact await boundary guarded by
                // the fixed confirmation epoch in production.
                worker_opened.wait();
                worker_release.wait();
                if worker_shared.current_request_epoch(accepted_epoch) {
                    worker_sent.store(true, std::sync::atomic::Ordering::Release);
                }
            });
            opened.wait();
            assert!(shared.begin_recovery("transport", 1).is_some());
            release.wait();
            handle.join().expect("epoch boundary worker");
            assert!(
                !stale_send.load(std::sync::atomic::Ordering::Acquire),
                "{boundary} must reject the accepted epoch after invalidation"
            );
            registry::destroy_terminal(owner);
        }
    }

    #[test]
    fn herdr_rollups_preserve_upstream_values_independently() {
        let workspace_status = herdr_rollup_status(wire::AgentStatus::Blocked);
        let group_status = herdr_rollup_status(wire::AgentStatus::Working);

        assert_eq!(workspace_status, Some(workspace::AgentStatus::Blocked));
        assert_eq!(group_status, Some(workspace::AgentStatus::Working));
        assert_ne!(workspace_status, group_status);
        assert_eq!(
            herdr_rollup_status(wire::AgentStatus::Unknown),
            Some(workspace::AgentStatus::Unknown)
        );
    }

    #[test]
    fn strict_recovery_resolves_a_moved_stable_terminal_without_alias_fallback() {
        let mut metadata = metadata_with_groups();
        metadata.ids.insert((b'p', "terminal-10".to_owned()), 10);
        let moved_pane = wire::HerdrPane {
            pane_id: "pane-moved".to_owned(),
            terminal_id: "terminal-10".to_owned(),
            workspace_id: "workspace-moved".to_owned(),
            tab_id: "tab-moved".to_owned(),
            name: Some("moved".to_owned()),
            focused: true,
            agent_name: None,
            agent_status: wire::AgentStatus::Idle,
        };
        let moved = wire::HerdrSessionSnapshot {
            version: wire::HERDR_VERSION.to_owned(),
            protocol: wire::HERDR_PROTOCOL,
            focused_workspace_id: Some("workspace-moved".to_owned()),
            focused_tab_id: Some("tab-moved".to_owned()),
            focused_pane_id: Some("pane-moved".to_owned()),
            workspaces: vec![wire::HerdrWorkspace {
                workspace_id: "workspace-moved".to_owned(),
                number: 1,
                name: "Moved workspace".to_owned(),
                focused: true,
                agent_status: wire::AgentStatus::Working,
                groups: vec![wire::HerdrGroup {
                    tab_id: "tab-moved".to_owned(),
                    workspace_id: "workspace-moved".to_owned(),
                    number: 1,
                    name: "Moved tab".to_owned(),
                    focused: true,
                    agent_status: wire::AgentStatus::Idle,
                    panes: vec![moved_pane],
                }],
                worktree: None,
            }],
        };
        assert!(snapshot_contains_stable_terminal(&moved, "terminal-10"));
        assert!(!snapshot_contains_stable_terminal(&moved, "terminal-gone"));
        let projection = project_snapshot(
            metadata,
            HashMap::from([(10, 12_345)]),
            Some(10),
            moved,
            "default".to_owned(),
            (80, 24),
            42_424,
        )
        .ok()
        .expect("moved stable terminal projection");
        assert_eq!(projection.selected, Some(10));
        assert_eq!(projection.mapping.get(&10), Some(&12_345));
        let moved_remote = projection.metadata.panes.get(&10).expect("moved pane");
        assert_ne!(moved_remote.workspace, 100);
        assert_ne!(moved_remote.group, 1);
        assert_eq!(moved_remote.terminal_id, "terminal-10");
    }

    struct RegistryCleanup {
        generation: u64,
        ids: Vec<u64>,
    }

    impl Drop for RegistryCleanup {
        fn drop(&mut self) {
            for id in self.ids.drain(..) {
                registry::detach_transport(id, self.generation);
                registry::destroy_terminal(id);
            }
        }
    }

    struct StrictFrameFixture {
        owner: u64,
        generation: u64,
        shared: Arc<ConnectionShared>,
        input_receiver: mpsc::Receiver<SemanticInput>,
    }

    impl StrictFrameFixture {
        fn reattach_semantic(&mut self) {
            registry::begin_remote(self.owner, self.generation)
                .expect("strict-frame remote binding");
            let (input, receiver) = mpsc::channel(8);
            let (resize, _sizes) = watch::channel((80, 4));
            registry::with_terminal_for_test(self.owner, |terminal| {
                terminal
                    .attach_semantic_transport(self.generation, input, resize)
                    .expect("strict-frame semantic binding");
            })
            .expect("strict-frame terminal");
            self.input_receiver = receiver;
        }
    }

    impl Drop for StrictFrameFixture {
        fn drop(&mut self) {
            registry::detach_transport(self.owner, self.generation);
            registry::destroy_terminal(self.owner);
        }
    }

    fn strict_frame_projection(owner: u64, label: &str) -> SnapshotProjection {
        let workspace_name = format!("workspace-{label}");
        let group_name = format!("group-{label}");
        let terminal_name = format!("terminal-{label}");
        let mut metadata = Metadata {
            snapshot: RuntimeSnapshot {
                control: RuntimeControlSnapshot::default(),
                backend: Backend::Herdr,
                runtime: "default".to_owned(),
                groups_supported: true,
                workspaces: vec![workspace::Workspace {
                    id: "100".to_owned(),
                    name: workspace_name.clone(),
                    agent_status: None,
                }],
                groups: vec![workspace::TerminalGroup {
                    id: "200".to_owned(),
                    workspace_id: "100".to_owned(),
                    name: group_name.clone(),
                    selected: true,
                    agent_status: None,
                }],
                terminals: vec![workspace::Terminal {
                    id: owner.to_string(),
                    workspace_id: "100".to_owned(),
                    group_id: "200".to_owned(),
                    terminal_id: "stable-terminal".to_owned(),
                    name: terminal_name.clone(),
                    active: true,
                    selected: true,
                    agent: None,
                }],
            },
            ..Metadata::default()
        };
        metadata.workspaces.insert(100, workspace_name.clone());
        metadata.groups.insert(200, group_name.clone());
        metadata.panes.insert(
            owner,
            RemotePane {
                pane_id: "pane-alias".to_owned(),
                terminal_id: "stable-terminal".to_owned(),
                workspace: 100,
                group: 200,
            },
        );
        metadata.selected_groups.insert(100, 200);
        metadata.active_group = Some(200);

        let pane = PaneSnapshot {
            window_id: 100,
            pane_id: owner,
            terminal_id: owner,
            window_name: workspace_name,
            active: true,
            selected: true,
            index: 0,
            columns: 80,
            rows: 4,
            pane_name: terminal_name.clone(),
            title: terminal_name,
        };
        SnapshotProjection {
            metadata,
            mapping: HashMap::from([(owner, owner)]),
            selected: Some(owner),
            flat: vec![pane.clone()],
            windows: vec![WindowSnapshot {
                window_id: 100,
                name: pane.window_name.clone(),
                panes: vec![pane],
                selected: true,
                zoomed: false,
            }],
            stale: Vec::new(),
        }
    }

    fn strict_frame_fixture() -> StrictFrameFixture {
        let owner = registry::create_terminal(80, 4).expect("strict-frame terminal");
        let generation = 93_000 + owner;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "strict-frame-herdr.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/strict-frame-herdr-known-hosts"),
        ));
        let old = strict_frame_projection(owner, "old");
        {
            let mut state = shared.session.lock().expect("strict-frame session state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "strict-frame-herdr.example".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: std::path::PathBuf::from("/tmp/strict-frame-herdr-known-hosts"),
                backend: Backend::Herdr,
                runtime: Some("default".to_owned()),
            });
            state.viewport = Some((80, 4));
            state.herdr = old.metadata.clone();
            state.pane_terminals = old.mapping.clone();
            state.selected_pane = old.selected;
            state.snapshot = SessionSnapshot {
                windows: old.windows.clone(),
                panes: old.flat.clone(),
                selected_pane: old.selected,
            };
            state.runtime_operations_ready = true;
            state.terminal_input_ready = false;
        }

        registry::begin_remote(owner, generation).expect("strict-frame initial remote mode");
        let (input, input_receiver) = mpsc::channel(8);
        let (resize, _sizes) = watch::channel((80, 4));
        registry::with_terminal_for_test(owner, |terminal| {
            terminal
                .attach_semantic_transport(generation, input, resize)
                .expect("strict-frame initial semantic binding");
        })
        .expect("strict-frame initial terminal");
        registry::restore_remote_display_and_ready(owner, generation, 80, 4, b"old-frame\r\n")
            .expect("strict-frame retained frame");
        shared
            .session
            .lock()
            .expect("strict-frame ready state")
            .terminal_input_ready = true;

        StrictFrameFixture {
            owner,
            generation,
            shared,
            input_receiver,
        }
    }

    fn strict_full_frame_value(bytes: &[u8]) -> Value {
        use base64::Engine as _;

        json!({
            "type": "terminal.frame",
            "encoding": "ansi",
            "seq": 1,
            "width": 80,
            "height": 4,
            "full": true,
            "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    #[test]
    fn strict_first_full_frame_barrier_rejects_stale_epoch_before_native_apply() {
        let mut fixture = strict_frame_fixture();
        let (old_snapshot, old_herdr, old_mapping, old_selected) = {
            let state = fixture
                .shared
                .session
                .lock()
                .expect("strict-frame retained state");
            (
                state.snapshot.clone(),
                state.herdr.snapshot.clone(),
                state.pane_terminals.clone(),
                state.selected_pane,
            )
        };
        let old_native = registry::snapshot(fixture.owner).expect("strict-frame native snapshot");
        let old_revision =
            registry::terminal_revision(fixture.owner).expect("strict-frame native revision");
        let expected_epoch = fixture
            .shared
            .begin_recovery("strict_frame", 1)
            .expect("strict-frame recovery epoch");
        registry::detach_transport(fixture.owner, fixture.generation);
        fixture.reattach_semantic();

        // Decode and stage the complete frame before the barrier. The commit
        // callback below is the only place allowed to mutate Term/projection.
        let wire::TerminalRecord::Frame(frame) =
            wire::decode_terminal_record(&strict_full_frame_value(b"new-frame\r\n\x1b[6n"))
                .expect("strict-frame decode")
        else {
            panic!("strict-frame decoder returned a non-frame record");
        };
        let projection = strict_frame_projection(fixture.owner, "new");
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_shared = Arc::clone(&fixture.shared);
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let owner = fixture.owner;
        let generation = fixture.generation;
        let worker = std::thread::spawn(move || {
            worker_entered.wait();
            worker_release.wait();
            worker_shared.commit_ready_at_epoch_result(expected_epoch, move |state| {
                registry::restore_remote_display_and_ready(
                    owner,
                    generation,
                    frame.width,
                    frame.height,
                    &frame.bytes,
                )
                .map_err(|_| FlowFailure::HerdrProtocol)?;
                apply_projection_state(state, &projection);
                Ok(())
            })
        });

        entered.wait();
        // This models Change/Disconnect after decode/prepare but before the
        // native apply callback. The stale callback must never run.
        fixture.shared.invalidate_explicitly("runtime_changed");
        registry::detach_transport(fixture.owner, fixture.generation);
        release.wait();
        assert!(matches!(
            worker.join().expect("strict-frame stale worker"),
            Err(FlowFailure::Stale)
        ));

        let state = fixture
            .shared
            .session
            .lock()
            .expect("strict-frame stale state");
        assert_eq!(state.snapshot, old_snapshot);
        assert_eq!(state.herdr.snapshot, old_herdr);
        assert_eq!(state.pane_terminals, old_mapping);
        assert_eq!(state.selected_pane, old_selected);
        assert!(!state.runtime_operations_ready);
        assert!(!state.terminal_input_ready);
        assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
        drop(state);
        assert!(!registry::transport_ready(
            fixture.owner,
            fixture.generation
        ));
        assert_eq!(
            registry::snapshot(fixture.owner).expect("strict-frame old native snapshot"),
            old_native
        );
        assert_eq!(
            registry::terminal_revision(fixture.owner).expect("strict-frame old native revision"),
            old_revision
        );
    }

    #[test]
    fn strict_first_full_frame_commit_publishes_projection_ready_and_fresh_input() {
        let mut fixture = strict_frame_fixture();
        let old_native = registry::snapshot(fixture.owner).expect("strict-frame old snapshot");
        let old_revision =
            registry::terminal_revision(fixture.owner).expect("strict-frame old revision");
        let expected_epoch = fixture
            .shared
            .begin_recovery("strict_frame", 1)
            .expect("strict-frame success epoch");
        registry::detach_transport(fixture.owner, fixture.generation);
        fixture.reattach_semantic();

        let wire::TerminalRecord::Frame(frame) =
            wire::decode_terminal_record(&strict_full_frame_value(b"new-frame\r\n\x1b[6n"))
                .expect("strict-frame success decode")
        else {
            panic!("strict-frame decoder returned a non-frame record");
        };
        let projection = strict_frame_projection(fixture.owner, "new");
        let expected_metadata = projection.metadata.snapshot.clone();
        let expected_flat = projection.flat.clone();
        let expected_windows = projection.windows.clone();
        let expected_mapping = projection.mapping.clone();
        let result = fixture
            .shared
            .commit_ready_at_epoch_result(expected_epoch, move |state| {
                registry::restore_remote_display_and_ready(
                    fixture.owner,
                    fixture.generation,
                    frame.width,
                    frame.height,
                    &frame.bytes,
                )
                .map_err(|_| FlowFailure::HerdrProtocol)?;
                apply_projection_state(state, &projection);
                Ok(())
            });
        assert!(
            result.is_ok(),
            "strict-frame success commit returned an error"
        );

        let state = fixture
            .shared
            .session
            .lock()
            .expect("strict-frame success state");
        assert_eq!(state.snapshot.panes, expected_flat);
        assert_eq!(state.snapshot.windows, expected_windows);
        assert_eq!(state.pane_terminals, expected_mapping);
        assert_eq!(state.selected_pane, Some(fixture.owner));
        assert_eq!(state.herdr.snapshot, expected_metadata);
        assert_eq!(state.recovery.phase, RecoveryPhase::None);
        assert!(state.runtime_operations_ready);
        assert!(state.terminal_input_ready);
        drop(state);
        assert_eq!(
            fixture
                .shared
                .snapshot()
                .expect("strict-frame connection state")
                .state,
            ConnectionState::Ready as u32
        );
        assert!(registry::transport_ready(fixture.owner, fixture.generation));
        assert_ne!(
            registry::snapshot(fixture.owner).expect("strict-frame new snapshot"),
            old_native
        );
        assert!(
            registry::terminal_revision(fixture.owner).expect("strict-frame new revision")
                > old_revision
        );

        // The frame contained a device-status query. It was applied while
        // the semantic gate was Attached, so no local VT reply was sent.
        assert!(fixture.input_receiver.try_recv().is_err());
        let fresh_epoch =
            registry::operation_epoch(fixture.owner).expect("strict-frame input epoch");
        assert!(
            fixture
                .shared
                .current_terminal_input_is_ready(fixture.shared.operation_epoch())
        );
        registry::commit_utf8_at_epoch(fixture.owner, fresh_epoch, b"fresh")
            .expect("strict-frame fresh input");
        assert!(matches!(
            fixture.input_receiver.try_recv().expect("strict-frame input record"),
            SemanticInput::Text(text, modifiers)
                if text == "fresh" && modifiers == Modifiers::NONE
        ));
    }

    fn herdr_pane(
        pane_id: &str,
        terminal_id: &str,
        tab_id: &str,
        name: Option<&str>,
        agent_name: Option<&str>,
        status: wire::AgentStatus,
        focused: bool,
    ) -> wire::HerdrPane {
        wire::HerdrPane {
            pane_id: pane_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
            workspace_id: "workspace-main".to_owned(),
            tab_id: tab_id.to_owned(),
            name: name.map(str::to_owned),
            focused,
            agent_name: agent_name.map(str::to_owned),
            agent_status: status,
        }
    }

    #[test]
    fn apply_snapshot_projection_keeps_distinct_workspace_group_and_pane_statuses() {
        let generation = 42_424;
        let snapshot = wire::HerdrSessionSnapshot {
            version: wire::HERDR_VERSION.to_owned(),
            protocol: wire::HERDR_PROTOCOL,
            focused_workspace_id: Some("workspace-main".to_owned()),
            focused_tab_id: Some("tab-live".to_owned()),
            focused_pane_id: Some("pane-agent".to_owned()),
            workspaces: vec![
                wire::HerdrWorkspace {
                    workspace_id: "workspace-empty".to_owned(),
                    number: 1,
                    name: "Empty workspace".to_owned(),
                    focused: false,
                    agent_status: wire::AgentStatus::Unknown,
                    groups: Vec::new(),
                    worktree: None,
                },
                wire::HerdrWorkspace {
                    workspace_id: "workspace-main".to_owned(),
                    number: 2,
                    name: "Main workspace".to_owned(),
                    focused: true,
                    agent_status: wire::AgentStatus::Working,
                    groups: vec![
                        wire::HerdrGroup {
                            tab_id: "tab-empty".to_owned(),
                            workspace_id: "workspace-main".to_owned(),
                            number: 1,
                            name: "Empty tab".to_owned(),
                            focused: false,
                            agent_status: wire::AgentStatus::Done,
                            panes: Vec::new(),
                        },
                        wire::HerdrGroup {
                            tab_id: "tab-live".to_owned(),
                            workspace_id: "workspace-main".to_owned(),
                            number: 2,
                            name: "Live tab".to_owned(),
                            focused: true,
                            agent_status: wire::AgentStatus::Blocked,
                            panes: vec![
                                herdr_pane(
                                    "pane-agentless",
                                    "terminal-agentless",
                                    "tab-live",
                                    None,
                                    None,
                                    wire::AgentStatus::Unknown,
                                    false,
                                ),
                                herdr_pane(
                                    "pane-agent",
                                    "terminal-agent",
                                    "tab-live",
                                    Some("Agent pane"),
                                    Some("Agent"),
                                    wire::AgentStatus::Idle,
                                    true,
                                ),
                            ],
                        },
                    ],
                    worktree: None,
                },
            ],
        };
        let projection = project_snapshot(
            Metadata::default(),
            HashMap::new(),
            None,
            snapshot,
            "default".to_owned(),
            (80, 24),
            generation,
        )
        .unwrap_or_else(|_| panic!("production snapshot projection"));
        let _cleanup = RegistryCleanup {
            generation,
            ids: projection.mapping.values().copied().collect(),
        };
        let projected = &projection.metadata.snapshot;

        let empty_workspace = projected
            .workspaces
            .iter()
            .find(|workspace| workspace.name == "Empty workspace")
            .expect("empty workspace projection");
        assert_eq!(
            empty_workspace.agent_status,
            Some(workspace::AgentStatus::Unknown)
        );
        let main_workspace = projected
            .workspaces
            .iter()
            .find(|workspace| workspace.name == "Main workspace")
            .expect("main workspace projection");
        assert_eq!(
            main_workspace.agent_status,
            Some(workspace::AgentStatus::Working)
        );

        let empty_tab = projected
            .groups
            .iter()
            .find(|group| group.name == "Empty tab")
            .expect("empty tab projection");
        assert_eq!(empty_tab.agent_status, Some(workspace::AgentStatus::Done));
        let live_tab = projected
            .groups
            .iter()
            .find(|group| group.name == "Live tab")
            .expect("live tab projection");
        assert_eq!(live_tab.agent_status, Some(workspace::AgentStatus::Blocked));

        let agentless = projected
            .terminals
            .iter()
            .find(|terminal| terminal.name == "Terminal 1")
            .expect("agentless pane projection");
        assert!(agentless.agent.is_none());
        assert_eq!(agentless.group_id, live_tab.id);
        let agent = projected
            .terminals
            .iter()
            .find(|terminal| terminal.name == "Agent pane")
            .expect("agent pane projection");
        let agent_status = agent.agent.as_ref().expect("agent metadata");
        assert_eq!(agent_status.name, "Agent");
        assert_eq!(agent_status.status, workspace::AgentStatus::Idle);
        assert_eq!(projection.selected, Some(agent.id.parse().unwrap()));
        assert_eq!(projection.flat.len(), 2);
        assert_eq!(projected.backend, Backend::Herdr);
        assert_eq!(projected.runtime, "default");
    }
}
