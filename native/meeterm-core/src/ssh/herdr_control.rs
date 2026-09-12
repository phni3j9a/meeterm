//! Existing Herdr public API and direct terminal control over one SSH session.
//! Each API request has its own forwarded Unix socket. Input requests are
//! awaited in order; only the selected direct controller may enqueue input.
use super::*;
use crate::herdr as wire;
use crate::input::{KeyCode, Modifiers, encode_key, encode_text};
use crate::terminal::SemanticInput;
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};

const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
static NEXT_REMOTE_HANDLE: AtomicU64 = AtomicU64::new(1_000_000);

#[derive(Clone, Default)]
pub(super) struct Metadata {
    pub(super) snapshot: RuntimeSnapshot,
    ids: HashMap<(u8, String), u64>,
    workspaces: HashMap<u64, String>,
    groups: HashMap<u64, String>,
    panes: HashMap<u64, RemotePane>,
    /// The selected group is independent of terminal selection so an empty
    /// group remains selectable across snapshot rebuilds.
    selected_groups: HashMap<u64, u64>,
    active_group: Option<u64>,
}

#[derive(Clone)]
struct RemotePane {
    pane_id: String,
    terminal_id: String,
    workspace: u64,
    group: u64,
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
    stream: JsonChannel,
    input: mpsc::Receiver<SemanticInput>,
    sizes: watch::Receiver<(u16, u16)>,
    seq: Option<u64>,
    ready: bool,
}

struct HerdrClient<'a> {
    shared: &'a Arc<ConnectionShared>,
    session: &'a client::Handle<HostKeyHandler>,
    runtime: Option<String>,
    socket: String,
    subscription: JsonChannel,
    subscribed_panes: HashSet<String>,
    controller: Option<Controller>,
    viewport: (u16, u16),
    request_id: u64,
}

impl Drop for HerdrClient<'_> {
    fn drop(&mut self) {
        detach_all(self.shared);
    }
}

pub(super) async fn run(
    shared: &Arc<ConnectionShared>,
    profile: &ConnectionProfile,
    session: &client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlCommand>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::Synchronizing);
    let status = command_output(
        shared,
        session,
        wire::command(profile.runtime.as_deref(), &["status", "--json"])
            .map_err(|_| FlowFailure::HerdrProtocol)?,
    )
    .await?;
    let status: Value = serde_json::from_slice(&status).map_err(|_| FlowFailure::HerdrProtocol)?;
    let server = &status["server"];
    if server["running"] != true {
        return Err(FlowFailure::HerdrSessionMissing);
    }
    if server["protocol"] != 22 || status["client"]["protocol"] != 22 {
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
            registry::terminal_dimensions(shared.terminal_id).map_err(|_| FlowFailure::Stale)?,
        );
    let subscription = subscribe(shared, session, &socket, &HashSet::new()).await?;
    let mut client = HerdrClient {
        shared,
        session,
        runtime: profile.runtime.clone(),
        socket,
        subscription,
        subscribed_panes: HashSet::new(),
        controller: None,
        viewport,
        request_id: 1,
    };
    client.synchronize().await?;
    client.activate_selected().await?;
    if client.controller.is_none() {
        shared.set_state(ConnectionState::Ready);
    }
    loop {
        if shared.is_cancelled() {
            let _ = client.release().await;
            return Err(FlowFailure::Stale);
        }
        if !shared.is_foreground() {
            let _ = client.release().await;
            return Err(FlowFailure::Transport);
        }
        let (stream, sizes, input) = match client.controller.as_mut() {
            Some(controller) => (
                Some(&mut controller.stream),
                Some(&mut controller.sizes),
                controller.ready.then_some(&mut controller.input),
            ),
            None => (None, None, None),
        };
        tokio::select! {
            _ = shared.cancelled() => { let _ = client.release().await; return Err(FlowFailure::Stale); },
            _ = shared.retry_notify.notified() => {},
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
                let Some(command) = command else { let _ = client.release().await; return Err(FlowFailure::Stale); };
                client.command(command).await?;
            },
            size = async {
                match sizes {
                    Some(sizes) => { sizes.changed().await.map_err(|_| FlowFailure::Stale)?; Ok(*sizes.borrow_and_update()) },
                    None => std::future::pending().await,
                }
            } => {
                let (cols, rows) = size?;
                client.viewport = (cols, rows);
                client.shared.session.lock().map_err(|_| FlowFailure::Stale)?.viewport = Some((cols, rows));
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
                client.input(input).await?;
            },
        }
    }
}

async fn command_output(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    command: String,
) -> Result<Vec<u8>, FlowFailure> {
    let mut channel = await_stage(
        shared,
        session.channel_open_session(),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    await_stage(
        shared,
        channel.exec(true, command),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
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
    tokio::select! { _ = shared.cancelled() => Err(FlowFailure::Stale), result = tokio::time::timeout(SSH_STAGE_TIMEOUT, read) => result.map_err(|_| FlowFailure::Transport)? }
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

async fn subscribe(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    socket: &str,
    panes: &HashSet<String>,
) -> Result<JsonChannel, FlowFailure> {
    let mut channel = open_api(shared, session, socket).await?;
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
    channel
        .request(
            "subscribe",
            "events.subscribe",
            &json!({"subscriptions":subscriptions}),
        )
        .await?;
    let response = tokio::select! { _ = shared.cancelled() => return Err(FlowFailure::Stale), response = tokio::time::timeout(SSH_STAGE_TIMEOUT, channel.next()) => response.map_err(|_| FlowFailure::Transport)?? };
    match wire::response("subscribe", &response).map_err(|_| FlowFailure::HerdrProtocol)? {
        wire::ApiResponse::Ok { .. } => {}
        wire::ApiResponse::Error { .. } => return Err(FlowFailure::HerdrUnsupported),
    }
    Ok(channel)
}

impl HerdrClient<'_> {
    async fn synchronize(&mut self) -> Result<(), FlowFailure> {
        // Every snapshot applied below has an acknowledged subscription for
        // exactly its pane set. Bound topology churn; reconnect can obtain a
        // new coherent baseline rather than silently dropping status events.
        for _ in 0..8 {
            let result = self.request("session.snapshot", json!({})).await?;
            let snapshot =
                decode_snapshot_result(&result).map_err(|_| FlowFailure::HerdrProtocol)?;
            if snapshot.protocol != 22 {
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
            let subscription =
                subscribe(self.shared, self.session, &self.socket, &pane_ids).await?;
            self.subscription.close().await;
            self.subscription = subscription;
            self.subscribed_panes = pane_ids;
        }
        Err(FlowFailure::Transport)
    }

    fn apply_snapshot(&mut self, snapshot: wire::HerdrSessionSnapshot) -> Result<(), FlowFailure> {
        let (mut metadata, mut mapping, old_selected) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            (
                state.herdr.clone(),
                state.pane_terminals.clone(),
                state.selected_pane,
            )
        };
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
            runtime: self.runtime.clone().unwrap_or_else(|| "default".to_owned()),
            groups_supported: true,
            ..RuntimeSnapshot::default()
        };
        metadata.workspaces.clear();
        metadata.groups.clear();
        metadata.panes.clear();
        metadata.selected_groups.clear();
        metadata.active_group = None;
        let mut live_ids = HashSet::new();
        let mut preferred = None;
        let mut flat = Vec::new();
        let mut windows = Vec::new();
        let mut focused_groups = HashMap::new();
        for workspace in snapshot.workspaces {
            let wid = metadata.id(b'w', &workspace.workspace_id);
            live_ids.insert((b'w', workspace.workspace_id.clone()));
            metadata.workspaces.insert(wid, workspace.workspace_id);
            metadata.snapshot.workspaces.push(workspace::Workspace {
                id: wid.to_string(),
                name: workspace.name.clone(),
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
                });
                for (index, pane) in group.panes.into_iter().enumerate() {
                    let pid = metadata.id(b'p', &pane.terminal_id);
                    live_ids.insert((b'p', pane.terminal_id.clone()));
                    let native = if let Some(native) = mapping.get(&pid) {
                        *native
                    } else {
                        let native = registry::create_terminal(self.viewport.0, self.viewport.1)
                            .map_err(|_| FlowFailure::HerdrProtocol)?;
                        registry::begin_remote(native, self.shared.generation)
                            .map_err(|_| FlowFailure::Stale)?;
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
                        status: match pane.agent_status {
                            wire::AgentStatus::Idle => "idle",
                            wire::AgentStatus::Working => "working",
                            wire::AgentStatus::Blocked => "blocked",
                            wire::AgentStatus::Done => "done",
                            wire::AgentStatus::Unknown => "unknown",
                        }
                        .to_owned(),
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
                        columns: self.viewport.0,
                        rows: self.viewport.1,
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
        let mut selected = choose_selected_pane(
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
        {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if self.shared.is_cancelled() || state.generation != self.shared.generation {
                return Err(FlowFailure::Stale);
            }
            // Public select_pane updates the desired state synchronously while
            // this actor is waiting for a snapshot. Preserve that newer intent
            // at the commit boundary instead of overwriting it with the
            // snapshot's older clone.
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
            state.herdr = metadata;
            state.pane_terminals = mapping;
            state.selected_pane = selected;
            state.snapshot = SessionSnapshot {
                windows,
                panes: flat,
                selected_pane: selected,
            };
        }
        for native in stale {
            registry::detach_transport(native, self.shared.generation);
            registry::destroy_terminal(native);
        }
        Ok(())
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
            ControlCommand::SelectPane { .. } => {
                self.release().await?;
                return self.activate_selected().await;
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
            _ => {}
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
                ControlCommand::SelectPane { .. }
                | ControlCommand::SelectGroup { .. }
                | ControlCommand::RefreshTerminal
                | ControlCommand::SetTerminalVisible { .. } => unreachable!(),
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
                    ("pane.close", json!({"pane_id":pane(pane_id)?.pane_id}))
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
        self.request_id = self
            .request_id
            .checked_add(1)
            .ok_or(FlowFailure::HerdrProtocol)?;
        let id = format!("meeterm-{}", self.request_id);
        let mut channel = open_api(self.shared, self.session, &self.socket).await?;
        channel.request(&id, method, &params).await?;
        let response = tokio::select! {
            _ = self.shared.cancelled() => return Err(FlowFailure::Stale),
            response = tokio::time::timeout(SSH_STAGE_TIMEOUT, channel.next()) => response.map_err(|_| FlowFailure::Transport)??,
        };
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
        registry::detach_transport(controller.native, self.shared.generation);
        // Drop queued input immediately. Do not reacquire until the existing
        // CLI confirms that Herdr has removed this direct controller's lease.
        controller.input.close();
        let released = tokio::time::timeout(Duration::from_secs(3), async {
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
        let (selected, remote, native) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            let selected = state.selected_pane.filter(|_| state.terminal_visible);
            let remote = selected.and_then(|id| state.herdr.panes.get(&id)).cloned();
            let native = selected
                .and_then(|id| state.pane_terminals.get(&id))
                .copied();
            (selected, remote, native)
        };
        if self
            .controller
            .as_ref()
            .is_some_and(|controller| Some(controller.pane) == selected)
        {
            return Ok(());
        }
        self.release().await?;
        let (Some(selected), Some(remote), Some(native)) = (selected, remote, native) else {
            self.shared.set_state(ConnectionState::Ready);
            return Ok(());
        };
        if !self.shared.is_foreground() {
            return Ok(());
        }
        let (cols, rows) = self.viewport;
        let command = wire::command(
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
        channel
            .exec(true, command)
            .await
            .map_err(|_| FlowFailure::Channel)?;
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
        self.controller = Some(Controller {
            pane: selected,
            native,
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
            frame = tokio::time::timeout(SSH_STAGE_TIMEOUT, self.controller.as_mut().unwrap().stream.next()) => {
                frame.map_err(|_| FlowFailure::Transport)??
            },
        };
        self.frame(frame)
    }

    fn frame(&mut self, value: Value) -> Result<(), FlowFailure> {
        // Selection/visibility changes synchronously revoke native input.
        // Keep the same lock until this frame is applied so it cannot rearm
        // a controller the UI has just hidden or replaced.
        let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        if !state.terminal_visible
            || self
                .controller
                .as_ref()
                .is_none_or(|controller| state.selected_pane != Some(controller.pane))
        {
            return Ok(());
        }
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
        let controller = self.controller.as_mut().ok_or(FlowFailure::Stale)?;
        let wire::TerminalFrame {
            seq,
            width: cols,
            height: rows,
            full,
            bytes,
        } = frame;
        if controller.seq.is_some_and(|old| seq <= old) || (!full && controller.seq.is_none()) {
            return Err(FlowFailure::HerdrProtocol);
        }
        let terminal =
            registry::shared_terminal(controller.native).map_err(|_| FlowFailure::Stale)?;
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
        controller.seq = Some(seq);
        if !controller.ready {
            controller.ready = terminal.mark_transport_ready(self.shared.generation);
            if !controller.ready {
                // A queued hide/show edge can revoke the binding while the
                // desired pane stays the same. Its command will reacquire.
                return Ok(());
            }
            drop(terminal);
            drop(state);
            self.shared.set_state(ConnectionState::Ready);
        }
        Ok(())
    }

    async fn input(&mut self, input: SemanticInput) -> Result<(), FlowFailure> {
        let Some(controller) = self
            .controller
            .as_ref()
            .filter(|controller| controller.ready)
        else {
            return Ok(());
        };
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
        if let SemanticInput::Scroll(lines) = input {
            if lines != 0 {
                controller.stream.send(json!({"type":"terminal.scroll", "direction":if lines>0 {"up"} else {"down"},
                    "lines":lines.unsigned_abs().min(u16::MAX as u32)})).await?;
            }
            return Ok(());
        }
        // The public send APIs encode against the remote PTY's modes, but
        // unlike direct terminal.input they do not reset remote scrollback.
        self.request(
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
        match input {
            SemanticInput::Scroll(_) => unreachable!(),
            SemanticInput::Paste(text) => {
                self.request("pane.send_input", json!({"pane_id":pane,"text":text}))
                    .await?;
            }
            SemanticInput::Text(text, modifiers)
                if !modifiers.contains(Modifiers::CTRL) && !modifiers.contains(Modifiers::ALT) =>
            {
                self.request("pane.send_text", json!({"pane_id":pane,"text":text}))
                    .await?;
            }
            SemanticInput::Text(text, modifiers) => {
                let keys = text
                    .chars()
                    .map(|character| character_key(character, modifiers))
                    .collect::<Option<Vec<_>>>();
                if let Some(keys) = keys {
                    self.request("pane.send_keys", json!({"pane_id":pane,"keys":keys}))
                        .await?;
                } else {
                    let text = String::from_utf8(encode_text(&text, modifiers))
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    self.request("pane.send_text", json!({"pane_id":pane,"text":text}))
                        .await?;
                }
            }
            SemanticInput::Key(key, modifiers) => {
                if let Some(key) = semantic_key(key, modifiers) {
                    self.request("pane.send_keys", json!({"pane_id":pane,"keys":[key]}))
                        .await?;
                } else {
                    // Herdr 0.9.0's public key-name parser has no navigation
                    // names for Home/End/Insert/Delete/PageUp/PageDown. Keep
                    // their existing xterm byte form explicit and ordered.
                    let text = String::from_utf8(encode_key(key, modifiers, false))
                        .map_err(|_| FlowFailure::HerdrProtocol)?;
                    self.request("pane.send_text", json!({"pane_id":pane,"text":text}))
                        .await?;
                }
            }
        }
        Ok(())
    }
}

fn api_failure(code: &str) -> FlowFailure {
    match code {
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
                backend: Backend::Herdr,
                runtime: "default".to_owned(),
                groups_supported: true,
                workspaces: vec![
                    workspace::Workspace {
                        id: "100".to_owned(),
                        name: "workspace-100".to_owned(),
                    },
                    workspace::Workspace {
                        id: "200".to_owned(),
                        name: "workspace-200".to_owned(),
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
}
