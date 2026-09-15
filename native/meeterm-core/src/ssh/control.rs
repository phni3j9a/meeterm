//! One serialized tmux command stream and its native pane transports.
use super::*;
use std::collections::{HashSet, VecDeque};

const MAX_PANES: usize = 4096;

fn selected_pane_for_sync(
    panes: &[tmux::PaneInfo],
    requested: Option<u64>,
    strict: bool,
) -> Result<u64, FlowFailure> {
    if strict {
        return requested
            .filter(|id| panes.iter().any(|pane| pane.pane_id == *id))
            .ok_or(FlowFailure::TmuxRuntimeMissing);
    }
    Ok(requested
        .filter(|id| panes.iter().any(|pane| pane.pane_id == *id))
        .or_else(|| {
            panes
                .iter()
                .find(|pane| pane.active && pane.window_active)
                .map(|pane| pane.pane_id)
        })
        .unwrap_or(panes[0].pane_id))
}

fn strict_final_readback_required(
    strict_recovery: bool,
    strict_sync_pending: bool,
    initial: bool,
) -> bool {
    strict_recovery && strict_sync_pending && !initial
}

/// Mark a set of newly reconstructed pane transports as usable only after
/// every one has passed the generation/binding check. If one pane was revoked
/// while the batch was in flight, roll back the marks made by this attempt so
/// a caller cannot publish a partially-ready session.
fn mark_transport_ids_ready(
    shared: &ConnectionShared,
    ids: &[TerminalId],
    expected_epoch: u64,
) -> Result<(), FlowFailure> {
    let mut marked = Vec::with_capacity(ids.len());
    for id in ids {
        if !shared.current_request_epoch(expected_epoch) {
            for marked_id in marked {
                registry::detach_transport(marked_id, shared.generation);
            }
            return Err(FlowFailure::Stale);
        }
        if !registry::mark_transport_ready(*id, shared.generation) {
            for marked_id in marked {
                registry::detach_transport(marked_id, shared.generation);
            }
            return Err(FlowFailure::Stale);
        }
        marked.push(*id);
    }
    if !shared.current_request_epoch(expected_epoch) {
        for marked_id in marked {
            registry::detach_transport(marked_id, shared.generation);
        }
        return Err(FlowFailure::Stale);
    }
    Ok(())
}

enum PaneEvent {
    Input(u64, Vec<u8>),
    Resize(u64, u16, u16),
}

struct StagedCapture {
    native: u64,
    columns: u16,
    rows: u16,
    bytes: Vec<u8>,
    trailing_output: Vec<u8>,
}

struct ControlClient {
    shared: Arc<ConnectionShared>,
    session: String,
    reader: russh::ChannelReadHalf,
    writer: russh::ChannelWriteHalf<client::Msg>,
    decoder: tmux::Decoder,
    events: VecDeque<tmux::Event>,
    routes: HashMap<u64, tokio::task::JoinHandle<()>>,
    pane_sender: mpsc::Sender<PaneEvent>,
    pane_receiver: mpsc::Receiver<PaneEvent>,
    viewport: (u16, u16),
    dirty: bool,
    capturing: Option<u64>,
    capture_complete: bool,
    capture_output: Vec<u8>,
    command_number: u64,
    zoom_hooks: Option<tmux::ZoomRecoveryHookAllocation>,
    strict_recovery: bool,
    mapping: HashMap<u64, u64>,
    staged_native: Vec<u64>,
    command_epoch: Option<u64>,
    /// Epoch captured for a strict recovery actor. Routine syncs use the
    /// command/current epoch at their own start, while strict reconstruction
    /// must never adopt a newer one after an awaited capture.
    operation_epoch: u64,
    staged_captures: Vec<StagedCapture>,
    strict_sync_pending: bool,
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        for task in self.routes.values() {
            task.abort();
        }
        for native in self.staged_native.drain(..) {
            if native != self.shared.terminal_id {
                registry::detach_transport(native, self.shared.generation);
                registry::destroy_terminal(native);
            }
        }
        detach_all(&self.shared);
        // Zoom ownership is retained across a recoverable transport loss so
        // the reconnecting actor can restore the mobile layout and its hooks.
        // The outer connection lifecycle clears it only when retries stop or
        // an explicit replacement takes ownership of the generation.
    }
}

pub(super) async fn run(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::AttachingTmux);

    let exact_runtime = profile.tmux_identity.is_some() || profile.runtime.is_some();
    let initial_recovery_epoch = {
        let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        (state.recovery.phase != RecoveryPhase::None).then_some(state.operation_epoch)
    };
    let strict_recovery = initial_recovery_epoch.is_some();
    if strict_recovery
        && shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .selected_pane
            .is_none()
    {
        return Err(FlowFailure::TmuxRuntimeMissing);
    }
    let (runtime_session, startup, selected_identity) =
        if let Some(identity) = profile.tmux_identity.clone() {
            verify_selected_runtime(shared, session, &identity).await?;
            let startup = tmux::attach_command_for_session(&identity.session_id)
                .map_err(|_| FlowFailure::TmuxRuntimeMissing)?;
            (identity.session_id.clone(), startup, identity)
        } else if let Some(name) = profile.runtime.as_deref() {
            // A direct named tmux option has no native identity yet. Resolve it
            // once through the bounded list and then use the exact `$N` for this
            // lifecycle; a missing name never falls through to create/attach.
            let identities = discover(shared, session).await?;
            let identity = identities
                .into_iter()
                .find(|identity| identity.name == name)
                .ok_or(FlowFailure::TmuxRuntimeMissing)?;
            profile.tmux_identity = Some(identity.clone());
            shared.set_profile(profile.clone());
            let startup = tmux::attach_command_for_session(&identity.session_id)
                .map_err(|_| FlowFailure::TmuxRuntimeMissing)?;
            (identity.session_id.clone(), startup, identity)
        } else {
            // A tmux actor without a selected identity must not fall back to
            // the historical `new-session -A` command. Fresh/manual host
            // flows enter the picker. An automatic reconnect with no
            // verifiable binding remains a fail-closed retained-work loss.
            return Err(FlowFailure::TmuxRuntimeMissing);
        };
    let channel = await_stage(
        shared,
        session.channel_open_session(),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    let (reader, writer) = channel.split();
    let viewport = shared
        .session
        .lock()
        .map_err(|_| FlowFailure::Stale)?
        .viewport
        .unwrap_or(
            registry::terminal_dimensions(shared.terminal_id).map_err(|_| FlowFailure::Stale)?,
        );
    shared
        .session
        .lock()
        .map_err(|_| FlowFailure::Stale)?
        .viewport = Some(viewport);
    let (pane_sender, pane_receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let mut client = ControlClient {
        shared: Arc::clone(shared),
        session: runtime_session,
        reader,
        writer,
        decoder: tmux::Decoder::new(),
        events: VecDeque::new(),
        routes: HashMap::new(),
        pane_sender,
        pane_receiver,
        viewport,
        dirty: false,
        capturing: None,
        capture_complete: false,
        capture_output: Vec::new(),
        command_number: 0,
        zoom_hooks: None,
        strict_recovery,
        mapping: HashMap::new(),
        staged_native: Vec::new(),
        command_epoch: None,
        operation_epoch: initial_recovery_epoch.unwrap_or_else(|| shared.operation_epoch()),
        staged_captures: Vec::new(),
        strict_sync_pending: false,
    };
    await_stage(
        shared,
        client.writer.exec(true, startup.into_bytes()),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    // An SSH request success and tmux's startup block are separate boundaries.
    loop {
        match await_channel_message(shared, &mut client.reader).await? {
            Some(ChannelMsg::Success) => break,
            Some(ChannelMsg::Data { data }) => client.decode(&data)?,
            Some(ChannelMsg::ExtendedData { .. }) => {}
            Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close) | None => {
                return Err(FlowFailure::Tmux);
            }
            _ => {}
        }
    }
    loop {
        match client.next_event().await? {
            tmux::Event::Command(block) if !block.error => break,
            tmux::Event::Command(_) => {
                return Err(if exact_runtime {
                    FlowFailure::TmuxRuntimeMissing
                } else {
                    FlowFailure::Tmux
                });
            }
            event => client.dispatch(event)?,
        }
    }
    client.verify_attached_session(&selected_identity).await?;
    client.synchronize(true).await?;
    loop {
        // Drain every decoded event before blocking on the SSH channel again.
        while let Some(event) = client.events.pop_front() {
            client.dispatch(event)?;
        }
        if shared.recovery_requires_controller_exit(initial_recovery_epoch) {
            for task in client.routes.values() {
                task.abort();
            }
            detach_all(shared);
            return Err(FlowFailure::Transport);
        }
        if client.dirty {
            client.synchronize(false).await?;
            continue;
        }
        tokio::select! {
            biased;
            _ = shared.cancelled() => {
                client.restore_zoom().await;
                return Err(FlowFailure::Stale);
            }
            _ = shared.retry_notify.notified() => {}
            command = commands.recv() => {
                let Some(request) = command else {
                    return Err(FlowFailure::Stale);
                };
                if !shared.current_request_epoch(request.epoch) {
                    continue;
                }
                let command = request.command;
                if !matches!(&command, ControlCommand::SetTerminalVisible { visible: false })
                    && !shared.current_request_is_ready(request.epoch)
                {
                    continue;
                }
                client.command_epoch = Some(request.epoch);
                match Some(command) {
                    Some(ControlCommand::SelectPane { window_id, pane_id }) => {
                        client.select(window_id, pane_id).await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::CreateWorkspace { name }) => {
                        client.ensure_session_topology_safe().await?;
                        let existing = client
                            .shared
                            .session
                            .lock()
                            .map_err(|_| FlowFailure::Stale)?
                            .snapshot
                            .clone();
                        let command = tmux::create_workspace_command_for_session(
                            &client.session,
                            &name,
                        )
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                        if let Some(window) = client.new_window_id_since(&existing)
                            && let Some(pane) = client.first_pane_in_window(window)
                        {
                            client.select(window, pane).await?;
                            client.synchronize(false).await?;
                        }
                    }
                    Some(ControlCommand::RenameWorkspace { window_id, name }) => {
                        client.ensure_topology_safe(window_id).await?;
                        let command = tmux::rename_workspace_command_for_session(
                            &client.session,
                            window_id,
                            &name,
                        )
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::CloseWorkspace { window_id }) => {
                        client.ensure_topology_safe(window_id).await?;
                        let final_window = {
                            let state = client
                                .shared
                                .session
                                .lock()
                                .map_err(|_| FlowFailure::Stale)?;
                            state.snapshot.windows.len() == 1
                                && state.snapshot.windows[0].window_id == window_id
                        };
                        let command = tmux::close_workspace_command_for_session(
                            &client.session,
                            window_id,
                        )
                        .map_err(|_| FlowFailure::TmuxProtocol)?;
                        if final_window {
                            return client.close_last_session(command).await;
                        }
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::CreatePane { window_id }) => {
                        client.ensure_topology_safe(window_id).await?;
                        let existing = client
                            .shared
                            .session
                            .lock()
                            .map_err(|_| FlowFailure::Stale)?
                            .snapshot
                            .clone();
                        let command = tmux::create_pane_command_for_session(
                            &client.session,
                            window_id,
                        )
                        .map_err(|_| FlowFailure::TmuxProtocol)?;
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                        if let Some(pane_id) = client.new_pane_id_since(&existing, window_id) {
                            client.select(window_id, pane_id).await?;
                            client.synchronize(false).await?;
                        }
                    }
                    Some(ControlCommand::RenamePane { pane_id, name }) => {
                        let window_id = client
                            .shared
                            .session
                            .lock()
                            .map_err(|_| FlowFailure::Stale)?
                            .snapshot
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == pane_id)
                            .map(|pane| pane.window_id)
                            .ok_or(FlowFailure::Stale)?;
                        client.ensure_topology_safe(window_id).await?;
                        let command = tmux::rename_pane_command_for_session(
                            &client.session,
                            window_id,
                            pane_id,
                            &name,
                        )
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::ClosePane { pane_id }) => {
                        let (window_id, final_pane) = {
                            let state = client
                                .shared
                                .session
                                .lock()
                                .map_err(|_| FlowFailure::Stale)?;
                            let pane = state
                                .snapshot
                                .panes
                                .iter()
                                .find(|pane| pane.pane_id == pane_id)
                                .ok_or(FlowFailure::Stale)?;
                            (pane.window_id, state.snapshot.panes.len() == 1)
                        };
                        client.ensure_topology_safe(window_id).await?;
                        let command = tmux::close_pane_command_for_session(
                            &client.session,
                            window_id,
                            pane_id,
                        )
                        .map_err(|_| FlowFailure::TmuxProtocol)?;
                        if final_pane {
                            return client.close_last_session(command).await;
                        }
                        client.query(&command).await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::RefreshTerminal) => {
                        client.refresh_terminal().await?;
                        client.synchronize(false).await?;
                    }
                    Some(ControlCommand::RefreshRuntimes
                        | ControlCommand::SelectRuntime { .. }
                        | ControlCommand::CreateRuntime { .. }) => {
                        // A picker action can already be queued when the
                        // first selection reaches Ready. It belongs to the
                        // old picker state and must not tear down the newly
                        // selected runtime (double taps are discarded).
                    }
                    Some(ControlCommand::CreateGroup { .. } | ControlCommand::RenameGroup { .. }
                        | ControlCommand::CloseGroup { .. } | ControlCommand::SelectGroup { .. }) => return Err(FlowFailure::TmuxProtocol),
                    Some(ControlCommand::SetTerminalVisible { .. }) => {},
                    Some(ControlCommand::RetryRecovery)
                    | Some(ControlCommand::ConfirmRecovery { .. }) => {}
                    None => return Err(FlowFailure::Stale),
                }
                client.command_epoch = None;
            }
            event = client.pane_receiver.recv() => {
                match event {
                    Some(PaneEvent::Input(pane, bytes)) => {
                        let epoch = shared.operation_epoch();
                        if shared.current_terminal_input_is_ready(epoch)
                            && client.routes.contains_key(&pane)
                        {
                            let command = tmux::send_bytes_command_for_session(
                                &client.session,
                                pane,
                                &bytes,
                            )
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                            client.command_epoch = Some(epoch);
                            let result = client.query(&command).await;
                            client.command_epoch = None;
                            result?;
                        }
                    }
                    Some(PaneEvent::Resize(pane, columns, rows)) => {
                        let epoch = shared.operation_epoch();
                        if !shared.current_terminal_input_is_ready(epoch) {
                            continue;
                        }
                        let selected = shared.session.lock().map_err(|_| FlowFailure::Stale)?.selected_pane;
                        if selected == Some(pane) && client.viewport != (columns, rows) {
                            client.viewport = (columns, rows);
                            shared.session.lock().map_err(|_| FlowFailure::Stale)?.viewport = Some((columns, rows));
                            client.command_epoch = Some(epoch);
                            let result = async {
                                client.resize_client().await?;
                                client.synchronize(false).await
                            }
                            .await;
                            client.command_epoch = None;
                            result?;
                        }
                    }
                    None => return Err(FlowFailure::Transport),
                }
            }
            message = wait_channel_message(shared, &mut client.reader) => {
                client.channel_message(message?).await?;
            }
        }
    }
}

/// Discover only the ordinary tmux server. A missing server with the narrow
/// documented diagnostic is an empty picker section; missing binaries,
/// permissions, malformed output, and uncertain statuses remain errors.
pub(super) async fn discover(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
) -> Result<Vec<tmux::SessionIdentity>, FlowFailure> {
    let output = super::run_remote_command_with_timeout(
        shared,
        session,
        tmux::list_sessions_command().to_owned(),
        super::MAX_RUNTIME_COMMAND_OUTPUT_BYTES,
        FlowFailure::TmuxDiscoveryMalformed,
        FlowFailure::TmuxDiscoveryTimeout,
    )
    .await?;
    match output.exit_status {
        Some(0) => parse_sessions(&output.stdout),
        Some(127) if tmux::is_command_missing(output.exit_status, &output.stderr) => {
            Err(FlowFailure::TmuxDiscoveryMissing)
        }
        Some(1) if tmux::is_verified_no_server(&output.stderr) => Ok(Vec::new()),
        Some(_) if is_permission_error(&output.stderr) => Err(FlowFailure::TmuxDiscoveryPermission),
        Some(_) => Err(FlowFailure::TmuxDiscoveryMalformed),
        None => Err(FlowFailure::TmuxDiscoveryMalformed),
    }
}

fn parse_sessions(bytes: &[u8]) -> Result<Vec<tmux::SessionIdentity>, FlowFailure> {
    let mut sessions = Vec::new();
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if sessions.len() >= tmux::MAX_RUNTIME_SESSIONS {
            return Err(FlowFailure::TmuxDiscoveryMalformed);
        }
        let identity =
            tmux::parse_session_line(line).map_err(|_| FlowFailure::TmuxDiscoveryMalformed)?;
        if !ids.insert(identity.session_id.clone()) || !names.insert(identity.name.clone()) {
            return Err(FlowFailure::TmuxDiscoveryMalformed);
        }
        sessions.push(identity);
    }
    Ok(sessions)
}

fn is_permission_error(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("permission denied")
        || message.contains("access denied")
        || message.contains("cannot connect")
}

pub(super) async fn create_runtime(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    name: &str,
) -> Result<tmux::SessionIdentity, FlowFailure> {
    let command =
        tmux::create_session_command(name).map_err(|_| FlowFailure::TmuxRuntimeUnknown)?;
    let output = super::run_remote_command_with_timeout(
        shared,
        session,
        command,
        super::MAX_RUNTIME_COMMAND_OUTPUT_BYTES,
        FlowFailure::TmuxRuntimeUnknown,
        FlowFailure::TmuxRuntimeUnknown,
    )
    .await?;
    let identity = match output.exit_status {
        Some(0) => {
            let sessions =
                parse_sessions(&output.stdout).map_err(|_| FlowFailure::TmuxRuntimeUnknown)?;
            if sessions.len() != 1 {
                return Err(FlowFailure::TmuxRuntimeUnknown);
            }
            let identity = sessions.into_iter().next().expect("one session parsed");
            if identity.name != name {
                return Err(FlowFailure::TmuxRuntimeUnknown);
            }
            identity
        }
        Some(_) if tmux::is_session_collision(&output.stderr) => {
            return Err(FlowFailure::TmuxRuntimeCollision);
        }
        Some(_) => return Err(FlowFailure::TmuxRuntimeUnknown),
        None => return Err(FlowFailure::TmuxRuntimeUnknown),
    };
    let listed = match discover(shared, session).await {
        Ok(listed) => listed,
        Err(FlowFailure::Stale) => return Err(FlowFailure::Stale),
        Err(_) => return Err(FlowFailure::TmuxRuntimeUnknown),
    };
    if listed.iter().any(|candidate| candidate == &identity) {
        Ok(identity)
    } else {
        Err(FlowFailure::TmuxRuntimeUnknown)
    }
}

async fn verify_selected_runtime(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    expected: &tmux::SessionIdentity,
) -> Result<(), FlowFailure> {
    let listed = match discover(shared, session).await {
        Ok(listed) => listed,
        Err(
            FlowFailure::TmuxDiscoveryMissing
            | FlowFailure::TmuxDiscoveryPermission
            | FlowFailure::TmuxDiscoveryTimeout
            | FlowFailure::TmuxDiscoveryMalformed,
        ) => return Err(FlowFailure::TmuxRuntimeMissing),
        Err(failure) => return Err(failure),
    };
    if listed.iter().any(|candidate| candidate == expected) {
        Ok(())
    } else {
        Err(FlowFailure::TmuxRuntimeMissing)
    }
}

fn topology_is_safe(
    topologies: &[tmux::WindowTopology],
    selected_session: &str,
    window_id: u64,
) -> bool {
    let mut observed = false;
    for topology in topologies {
        let selected =
            topology.session_id == selected_session || topology.session_name == selected_session;
        if selected && topology.window_id == window_id {
            observed = true;
            if topology.linked_sessions > 1 {
                return false;
            }
        }
    }
    observed
}

fn topology_session_is_safe(topologies: &[tmux::WindowTopology], selected_session: &str) -> bool {
    let mut observed = false;
    for topology in topologies {
        if topology.session_id == selected_session || topology.session_name == selected_session {
            observed = true;
            if topology.linked_sessions > 1 {
                return false;
            }
        }
    }
    observed
}

impl ControlClient {
    fn decode(&mut self, bytes: &[u8]) -> Result<(), FlowFailure> {
        self.events.extend(
            self.decoder
                .feed(bytes)
                .map_err(|_| FlowFailure::TmuxProtocol)?,
        );
        Ok(())
    }

    async fn channel_message(&mut self, message: Option<ChannelMsg>) -> Result<(), FlowFailure> {
        match message {
            Some(ChannelMsg::Data { data }) => self.decode(&data),
            // stderr is diagnostic text, never Control Mode or terminal input.
            Some(ChannelMsg::ExtendedData { .. }) => Ok(()),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                self.decoder
                    .finish()
                    .map_err(|_| FlowFailure::TmuxProtocol)?;
                Err(FlowFailure::RemoteClosed)
            }
            Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                Err(FlowFailure::Tmux)
            }
            _ => Ok(()),
        }
    }

    async fn next_event(&mut self) -> Result<tmux::Event, FlowFailure> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            let message = await_channel_message(&self.shared, &mut self.reader).await?;
            self.channel_message(message).await?;
        }
    }

    fn dispatch(&mut self, event: tmux::Event) -> Result<(), FlowFailure> {
        if self.shared.is_cancelled()
            || self
                .shared
                .session
                .lock()
                .map_err(|_| FlowFailure::Stale)?
                .generation
                != self.shared.generation
        {
            return Err(FlowFailure::Stale);
        }
        match event {
            tmux::Event::Output { pane_id, bytes } => {
                if self.capturing == Some(pane_id) {
                    if self.capture_complete {
                        if self.capture_output.len().saturating_add(bytes.len()) > 32 * 1024 * 1024
                        {
                            return Err(FlowFailure::TmuxProtocol);
                        }
                        self.capture_output.extend(bytes);
                    }
                    return Ok(());
                }
                let id = self.mapping.get(&pane_id).copied();
                // Output for a newly discovered pane is included in its first
                // capture; it must never be applied to the selected old pane.
                if let Some(id) = id
                    && self.routes.contains_key(&pane_id)
                    && !registry::feed_remote(id, self.shared.generation, &bytes)
                {
                    return Err(FlowFailure::Transport);
                }
            }
            tmux::Event::Notification { name, .. } => {
                if name == "exit" {
                    return Err(FlowFailure::RemoteClosed);
                }
                if matches!(
                    name.as_str(),
                    "window-add"
                        | "window-close"
                        | "window-renamed"
                        | "window-pane-changed"
                        | "layout-change"
                        | "session-window-changed"
                        | "session-changed"
                        | "sessions-changed"
                ) {
                    self.dirty = true;
                }
            }
            tmux::Event::Command(block) if block.error => return Err(FlowFailure::Tmux),
            _ => {}
        }
        Ok(())
    }

    async fn query(&mut self, command: &str) -> Result<Vec<tmux::CommandBlock>, FlowFailure> {
        let expected_epoch = self.command_epoch.unwrap_or_else(|| {
            self.strict_recovery
                .then_some(self.operation_epoch)
                .unwrap_or_else(|| self.shared.operation_epoch())
        });
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        self.command_number += 1;
        // tmux emits one block per command, including commands nested in an
        // if-shell. A trailing sentinel delimits the whole request, so a
        // resize/selection response cannot be mistaken for a topology result.
        let marker = format!(
            "MEETERM_DONE_{}_{}",
            self.shared.generation, self.command_number
        );
        let request = format!("{command} ; display-message -p '{marker}'\n");
        await_stage(
            &self.shared,
            self.writer.data_bytes(request.into_bytes()),
            SSH_STAGE_TIMEOUT,
            FlowFailure::Transport,
        )
        .await?;
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        let deadline = tokio::time::Instant::now() + SSH_STAGE_TIMEOUT;
        let mut blocks = Vec::new();
        let mut reply_bytes = 0usize;
        loop {
            let event = tokio::time::timeout_at(deadline, self.next_event())
                .await
                .map_err(|_| FlowFailure::Tmux)??;
            match event {
                tmux::Event::Command(block) => {
                    if block.lines.len() == 1 && block.lines[0] == marker.as_bytes() {
                        return Ok(blocks);
                    }
                    if block.error {
                        // Mutations update the Rust snapshot only after a
                        // complete tmux response and synchronization. Keep
                        // the last coherent topology when tmux rejects a
                        // command; the outer flow exposes the failure state
                        // instead of showing an optimistic item that never
                        // existed remotely.
                        return Err(FlowFailure::Tmux);
                    }
                    reply_bytes =
                        reply_bytes.saturating_add(block.lines.iter().map(Vec::len).sum::<usize>());
                    if reply_bytes > 32 * 1024 * 1024 || blocks.len() >= 4096 {
                        return Err(FlowFailure::TmuxProtocol);
                    }
                    blocks.push(block);
                    if self.capturing.is_some() {
                        self.capture_complete = true;
                    }
                }
                event => self.dispatch(event)?,
            }
        }
    }

    /// Re-check the selected session after the attach command has been
    /// accepted. The discovery/preflight command used a separate SSH channel,
    /// so only this same Control Mode stream closes the list-to-attach race.
    /// A cancellation remains a stale lifecycle result; every other failure is
    /// deliberately reported as a missing runtime so the caller returns to
    /// the picker instead of accepting an uncertain server epoch.
    async fn verify_attached_session(
        &mut self,
        expected: &tmux::SessionIdentity,
    ) -> Result<(), FlowFailure> {
        let reply = match self.query(tmux::attached_session_epoch_command()).await {
            Ok(reply) => reply,
            Err(FlowFailure::Stale) => return Err(FlowFailure::Stale),
            Err(_) => return Err(FlowFailure::TmuxRuntimeMissing),
        };
        let Some(block) = (reply.len() == 1).then(|| &reply[0]) else {
            return Err(FlowFailure::TmuxRuntimeMissing);
        };
        let Some(line) = (block.lines.len() == 1).then(|| block.lines[0].as_slice()) else {
            return Err(FlowFailure::TmuxRuntimeMissing);
        };
        let observed =
            tmux::parse_session_epoch_line(line).map_err(|_| FlowFailure::TmuxRuntimeMissing)?;
        observed
            .matches(expected)
            .then_some(())
            .ok_or(FlowFailure::TmuxRuntimeMissing)
    }

    async fn resize_client(&mut self) -> Result<(), FlowFailure> {
        // Control clients do not display a status bar: verify actual pane size
        // from topology after setting the native viewport.
        self.query(&tmux::refresh_client_command(
            self.viewport.0,
            self.viewport.1,
        ))
        .await?;
        Ok(())
    }

    /// Check linked-window/session topology immediately before a mutation, in
    /// this same serialized Control Mode actor. A missing or malformed
    /// observation fails closed; the mutation is never replaced with
    /// `unlink-window` or another weaker operation.
    async fn ensure_topology_safe(&mut self, window_id: u64) -> Result<(), FlowFailure> {
        let lines = self.read_topology().await?;
        topology_is_safe(&lines, &self.session, window_id)
            .then_some(())
            .ok_or(FlowFailure::TmuxTopologyUnsafe)
    }

    async fn ensure_session_topology_safe(&mut self) -> Result<(), FlowFailure> {
        let lines = self.read_topology().await?;
        topology_session_is_safe(&lines, &self.session)
            .then_some(())
            .ok_or(FlowFailure::TmuxTopologyUnsafe)
    }

    async fn read_topology(&mut self) -> Result<Vec<tmux::WindowTopology>, FlowFailure> {
        let reply = self.query(tmux::list_topology_command()).await?;
        if reply.len() != 1 {
            return Err(FlowFailure::TmuxTopologyUnsafe);
        }
        reply
            .first()
            .ok_or(FlowFailure::TmuxTopologyUnsafe)?
            .lines
            .iter()
            .map(|line| tmux::parse_topology_line(line))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| FlowFailure::TmuxTopologyUnsafe)
    }

    async fn refresh_terminal(&mut self) -> Result<(), FlowFailure> {
        let pane = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .selected_pane;
        let Some(pane) = pane else {
            return Ok(());
        };
        if self.routes.contains_key(&pane) {
            // capture() reconstructs the native Term from tmux's current
            // screen, including alternate-screen/TUI mode and cursor state.
            // It also replays output that arrived while the capture command
            // was in flight, so an explicit redraw cannot lose keystrokes.
            self.capture(pane).await?;
        }
        Ok(())
    }

    /// Apply an explicit final-pane/window close without allowing the normal
    /// transport retry policy to recreate the managed session. tmux may close
    /// the Control Mode channel as soon as the last window disappears, so no
    /// response-bearing query can be required here. The user action itself is
    /// the terminal lifecycle boundary; cancellation makes run_connection
    /// finish as Disconnected even if the remote channel closes immediately.
    async fn close_last_session(&mut self, command: String) -> Result<(), FlowFailure> {
        if let Some(epoch) = self.command_epoch
            && !self.shared.current_request_is_ready(epoch)
        {
            return Err(FlowFailure::Stale);
        }
        let request = format!("{command}\n");
        await_stage(
            &self.shared,
            self.writer.data_bytes(request.into_bytes()),
            SSH_STAGE_TIMEOUT,
            FlowFailure::Transport,
        )
        .await?;
        // Writing to russh only queues channel data. Wait for tmux to accept
        // the destructive command and close its Control Mode client before
        // cancellation can tear down SSH and leave the session alive. The
        // final pane/window normally yields `%exit` followed by EOF/close;
        // an explicit command error or a missing close acknowledgement fails
        // instead of reporting a false Disconnected state.
        tokio::time::timeout(SSH_STAGE_TIMEOUT, async {
            loop {
                match self.next_event().await {
                    Ok(tmux::Event::Command(block)) if block.error => {
                        return Err(FlowFailure::Tmux);
                    }
                    Ok(tmux::Event::Notification { name, .. }) if name == "exit" => return Ok(()),
                    Ok(event) => match self.dispatch(event) {
                        Err(FlowFailure::RemoteClosed) => return Ok(()),
                        result => result?,
                    },
                    Err(FlowFailure::RemoteClosed) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|_| FlowFailure::Tmux)??;
        self.shared.cancel();
        Err(FlowFailure::Stale)
    }

    fn new_window_id_since(&self, before: &SessionSnapshot) -> Option<u64> {
        let state = self.shared.session.lock().ok()?;
        state
            .snapshot
            .windows
            .iter()
            .find(|window| {
                !before
                    .windows
                    .iter()
                    .any(|old| old.window_id == window.window_id)
            })
            .map(|window| window.window_id)
    }

    fn first_pane_in_window(&self, window_id: u64) -> Option<u64> {
        self.shared
            .session
            .lock()
            .ok()?
            .snapshot
            .windows
            .iter()
            .find(|window| window.window_id == window_id)
            .and_then(|window| {
                window
                    .panes
                    .iter()
                    .find(|pane| pane.active)
                    .or_else(|| window.panes.first())
            })
            .map(|pane| pane.pane_id)
    }

    fn new_pane_id_since(&self, before: &SessionSnapshot, window_id: u64) -> Option<u64> {
        let state = self.shared.session.lock().ok()?;
        state
            .snapshot
            .panes
            .iter()
            .find(|pane| {
                pane.window_id == window_id
                    && !before.panes.iter().any(|old| old.pane_id == pane.pane_id)
            })
            .map(|pane| pane.pane_id)
    }

    async fn select(&mut self, window: u64, pane: u64) -> Result<(), FlowFailure> {
        self.select_impl(window, pane, true).await
    }

    async fn select_without_publish(&mut self, window: u64, pane: u64) -> Result<(), FlowFailure> {
        self.select_impl(window, pane, false).await
    }

    async fn select_impl(
        &mut self,
        window: u64,
        pane: u64,
        publish: bool,
    ) -> Result<(), FlowFailure> {
        let previous = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .meeterm_zoomed_pane;
        // Return an earlier meeterm-owned window to its ordinary layout before
        // inspecting the new target.  This ordering matters when the target
        // is the same pane: the zoom we just remove must not be mistaken for
        // a desktop zoom that meeterm should preserve.
        if let Some(previous) = previous {
            let command = tmux::restore_layout_command_for_session(&self.session, previous)
                .map_err(|_| FlowFailure::TmuxProtocol)?;
            self.query(&command).await?;
        }

        // A desktop user may already have zoomed this window.  The latest
        // topology snapshot records that state for every window.  If the
        // previous pane was meeterm-owned in this same window, the restore
        // above has just cleared that zoom and the stale snapshot must not
        // make us treat it as desktop-owned.
        let zoomed = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            let zoomed = state
                .snapshot
                .windows
                .iter()
                .find(|candidate| candidate.window_id == window)
                .is_some_and(|candidate| candidate.zoomed);
            let previous_same_window = previous.is_some_and(|previous| {
                state
                    .snapshot
                    .panes
                    .iter()
                    .find(|candidate| candidate.pane_id == previous)
                    .is_some_and(|candidate| candidate.window_id == window)
            });
            zoomed && !previous_same_window
        };
        if zoomed {
            // tmux unzooms a window when selecting a different pane inside
            // the zoomed window.  Its normal idempotent selection command
            // immediately re-zooms the new target, so the desktop zoom state
            // survives the mobile tab change even though meeterm does not own
            // the cleanup.
            let mut transition = vec![
                tmux::select_pane_command_for_session(&self.session, None, window, pane)
                    .map_err(|_| FlowFailure::TmuxProtocol)?,
            ];
            if let Some(allocation) = self.zoom_hooks.take() {
                transition.push(
                    tmux::remove_zoom_recovery_hooks_command_for_session(&self.session, allocation)
                        .map_err(|_| FlowFailure::TmuxProtocol)?,
                );
            }
            self.query(&transition.join(" ; ")).await?;
        } else {
            let allocation = if let Some(allocation) = self.zoom_hooks {
                allocation
            } else {
                let target = tmux::session_target_for_hooks(&self.session)
                    .map_err(|_| FlowFailure::TmuxProtocol)?;
                let reply = self.query(&format!("show-hooks -t {target}:")).await?;
                let hooks = reply
                    .into_iter()
                    .flat_map(|b| b.lines)
                    .collect::<Vec<_>>()
                    .join(&b'\n');
                let allocation =
                    tmux::choose_zoom_recovery_hook(&hooks).ok_or(FlowFailure::Tmux)?;
                self.zoom_hooks = Some(allocation);
                allocation
            };
            // Install recovery before applying zoom. Existing indexed user
            // hooks remain intact; only our allocated pair is updated on tab
            // selection.
            let transition = [
                tmux::install_zoom_recovery_hooks_command_for_session(
                    &self.session,
                    allocation,
                    pane,
                )
                .map_err(|_| FlowFailure::TmuxProtocol)?,
                tmux::select_pane_command_for_session(&self.session, None, window, pane)
                    .map_err(|_| FlowFailure::TmuxProtocol)?,
            ]
            .join(" ; ");
            self.query(&transition).await?;
        }
        if publish {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            state.selected_pane = Some(pane);
            state.meeterm_zoomed = !zoomed;
            state.meeterm_zoomed_pane = (!zoomed).then_some(pane);
            mark_selected(&mut state.snapshot, pane);
        }
        Ok(())
    }

    async fn restore_zoom(&mut self) {
        let pane = self
            .shared
            .session
            .lock()
            .ok()
            .and_then(|s| s.meeterm_zoomed.then_some(s.meeterm_zoomed_pane).flatten());
        if let Some(pane) = pane {
            // Cancellation rejects all normal commands. This bounded best
            // effort cleanup is the only write permitted after cancellation.
            let cleanup = if let Some(allocation) = self.zoom_hooks {
                tmux::cleanup_zoom_recovery_hooks_command_for_session(
                    &self.session,
                    allocation,
                    pane,
                )
                .unwrap_or_default()
            } else {
                tmux::restore_layout_command_for_session(&self.session, pane).unwrap_or_default()
            };
            let command = format!("{cleanup}\n");
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                self.writer.data_bytes(command.into_bytes()),
            )
            .await;
            // A disconnected actor must not leave stale ownership behind for
            // the next reconnect.  The generation check prevents an older
            // cancellation from clearing ownership established by a newer
            // Control Mode actor using the same terminal ID.
            if let Ok(mut state) = self.shared.session.lock()
                && state.generation == self.shared.generation
                && state.meeterm_zoomed
                && state.meeterm_zoomed_pane == Some(pane)
            {
                state.meeterm_zoomed = false;
                state.meeterm_zoomed_pane = None;
            }
        }
    }

    fn attach(&mut self, pane: u64, id: u64, size: (u16, u16)) -> Result<(), FlowFailure> {
        let (input, mut receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        let (resize, mut sizes) = watch::channel(size);
        registry::prepare_pane_transport(id, self.shared.generation, size, input, resize)
            .map_err(|_| FlowFailure::Stale)?;
        let sender = self.pane_sender.clone();
        let shared = Arc::clone(&self.shared);
        let task = tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    _ = shared.cancelled() => break,
                    input = receiver.recv() => match input { Some(bytes) => PaneEvent::Input(pane, bytes), None => break },
                    resize = sizes.changed() => {
                        if resize.is_err() { break; }
                        let (cols, rows) = *sizes.borrow_and_update();
                        PaneEvent::Resize(pane, cols, rows)
                    }
                };
                tokio::select! {
                    _ = shared.cancelled() => break,
                    sent = sender.send(event) => if sent.is_err() { break; },
                }
            }
        });
        if let Some(old) = self.routes.insert(pane, task) {
            old.abort();
        }
        Ok(())
    }

    async fn synchronize(&mut self, initial: bool) -> Result<(), FlowFailure> {
        let expected_epoch = self.command_epoch.unwrap_or_else(|| {
            self.strict_recovery
                .then_some(self.operation_epoch)
                .unwrap_or_else(|| self.shared.operation_epoch())
        });
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        let strict_final_readback =
            strict_final_readback_required(self.strict_recovery, self.strict_sync_pending, initial);
        if strict_final_readback {
            // Captures collected before the recovery selection mutation are
            // only probes. The final dirty readback must be the sole display
            // source that can reach the retained Term.
            self.staged_captures.clear();
        }
        // Routine refreshes keep the live transport usable. Reporting a new
        // connection phase here would make the UI unmount its terminal view.
        if initial {
            self.shared.set_state(ConnectionState::Synchronizing);
        }
        self.dirty = false;
        let windows_command = tmux::list_windows_command_for_session(&self.session)
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        let panes_command = tmux::list_panes_command_for_session(&self.session)
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        let windows_reply = self.query(&windows_command).await?;
        let panes_reply = self.query(&panes_command).await?;
        let windows = windows_reply
            .first()
            .ok_or(FlowFailure::TmuxProtocol)?
            .lines
            .iter()
            .map(|l| tmux::parse_window_line(l))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        let panes = panes_reply
            .first()
            .ok_or(FlowFailure::TmuxProtocol)?
            .lines
            .iter()
            .map(|l| tmux::parse_pane_line(l))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        if panes.is_empty() || panes.len() > MAX_PANES {
            return Err(FlowFailure::TmuxProtocol);
        }
        let (old, mut mapping) = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            (
                state.snapshot.clone(),
                if self.strict_recovery && self.strict_sync_pending {
                    self.mapping.clone()
                } else {
                    state.pane_terminals.clone()
                },
            )
        };
        self.mapping = mapping.clone();
        let ids = panes.iter().map(|p| p.pane_id).collect::<HashSet<_>>();
        let stale = mapping
            .keys()
            .filter(|id| !ids.contains(id))
            .copied()
            .collect::<Vec<_>>();
        let selected = {
            let requested = self
                .shared
                .session
                .lock()
                .map_err(|_| FlowFailure::Stale)?
                .selected_pane;
            selected_pane_for_sync(&panes, requested, self.strict_recovery)?
        };
        // In strict recovery, prove that the original pane is still present
        // before any client resize or selection mutation. A replacement pane
        // must never receive the old owner's geometry.
        if initial {
            if self.strict_recovery && !panes.iter().any(|pane| pane.pane_id == selected) {
                return Err(FlowFailure::TmuxRuntimeMissing);
            }
            self.resize_client().await?;
            if !self.shared.current_request_epoch(expected_epoch) {
                return Err(FlowFailure::Stale);
            }
        }
        let mut capture = Vec::new();
        let mut newly_attached = Vec::new();
        for pane in &panes {
            let id = match mapping.get(&pane.pane_id).copied() {
                Some(id) => id,
                None => {
                    let id = if mapping.is_empty() && old.panes.is_empty() {
                        self.shared.terminal_id
                    } else {
                        let id = registry::create_terminal(pane.columns, pane.rows)
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                        self.staged_native.push(id);
                        id
                    };
                    mapping.insert(pane.pane_id, id);
                    id
                }
            };
            if !self.routes.contains_key(&pane.pane_id) {
                if !self.strict_recovery {
                    self.attach(pane.pane_id, id, (pane.columns, pane.rows))?;
                    newly_attached.push(id);
                }
                capture.push(pane.pane_id);
            } else if strict_final_readback {
                // Reconcile every pane after the selection/zoom mutation;
                // this is the final authoritative frame set.
                capture.push(pane.pane_id);
            } else if old
                .panes
                .iter()
                .find(|p| p.pane_id == pane.pane_id)
                .is_some_and(|p| p.columns != pane.columns || p.rows != pane.rows)
            {
                capture.push(pane.pane_id);
            }
        }
        self.mapping = mapping.clone();
        let names = windows
            .iter()
            .map(|w| (w.window_id, w.name.clone()))
            .collect::<HashMap<_, _>>();
        let flat = panes
            .iter()
            .map(|pane| PaneSnapshot {
                window_id: pane.window_id,
                pane_id: pane.pane_id,
                terminal_id: mapping[&pane.pane_id],
                window_name: names.get(&pane.window_id).cloned().unwrap_or_default(),
                active: pane.active,
                selected: pane.pane_id == selected,
                index: pane.index,
                columns: pane.columns,
                rows: pane.rows,
                title: pane.title.clone(),
                pane_name: pane.pane_name.clone(),
            })
            .collect::<Vec<_>>();
        for pane in capture {
            if !self.shared.current_request_epoch(expected_epoch) {
                return Err(FlowFailure::Stale);
            }
            self.capture(pane).await?;
        }
        if initial {
            let pane = panes.iter().find(|p| p.pane_id == selected).unwrap();
            if self.strict_recovery {
                self.select_without_publish(pane.window_id, selected)
                    .await?;
            } else {
                self.select(pane.window_id, selected).await?;
            }
            self.dirty = true; // selection/zoom sizes are read back before input readiness
            if self.strict_recovery {
                // Do not expose topology, selection, native capture, or
                // stale-terminal cleanup until the dirty final readback below
                // has completed successfully.
                self.strict_sync_pending = true;
                return Ok(());
            }
        }
        let stale_native = stale
            .iter()
            .filter_map(|pane| mapping.remove(pane))
            .collect::<Vec<_>>();
        self.mapping = mapping.clone();
        if strict_final_readback {
            // Rebind every retained/native pane to this actor generation only
            // after the final topology readback has succeeded. The operation
            // is local and keeps the existing Term/cells intact; no public
            // topology or readiness is changed until the commit below.
            for native in mapping.values().copied() {
                registry::begin_remote(native, self.shared.generation)
                    .map_err(|_| FlowFailure::Stale)?;
            }
            for pane in &panes {
                self.attach(
                    pane.pane_id,
                    mapping[&pane.pane_id],
                    (pane.columns, pane.rows),
                )?;
            }
            let ids = mapping.values().copied().collect::<Vec<_>>();
            let staged_captures = std::mem::take(&mut self.staged_captures);
            let shared = Arc::clone(&self.shared);
            let commit = self
                .shared
                .commit_ready_at_epoch_result(expected_epoch, |state| {
                    // This closure runs only after the expected epoch has been
                    // checked and while the session lock is held. The registry
                    // batch keeps every pane Attached while it preflights and
                    // replays all captures, then marks every transport Ready
                    // only after the last replay succeeds. VT replies emitted
                    // by capture replay therefore hit the closed gate and are
                    // deliberately discarded; no stale query is buffered.
                    apply_staged_captures_locked(&shared, &staged_captures)?;
                    apply_topology_state(state, &mapping, &windows, &panes, &flat, selected);
                    Ok(())
                });
            if let Err(failure) = commit {
                for id in ids {
                    registry::detach_transport(id, self.shared.generation);
                }
                return Err(failure);
            }
            self.strict_sync_pending = false;
            self.strict_recovery = false;
            for pane in stale {
                if let Some(task) = self.routes.remove(&pane) {
                    task.abort();
                }
            }
            for id in stale_native {
                registry::detach_transport(id, self.shared.generation);
                if id != self.shared.terminal_id {
                    registry::destroy_terminal(id);
                }
            }
            self.staged_native.clear();
            return Ok(());
        }
        self.commit_topology(&mapping, &windows, &panes, &flat, selected, expected_epoch)?;
        for pane in stale.iter().copied() {
            if let Some(task) = self.routes.remove(&pane) {
                task.abort();
            }
        }
        for id in stale_native.iter().copied() {
            registry::detach_transport(id, self.shared.generation);
            if id != self.shared.terminal_id {
                registry::destroy_terminal(id);
            }
        }
        if !initial && (!self.shared.has_been_ready() || self.strict_recovery) {
            let ids = self
                .shared
                .session
                .lock()
                .map_err(|_| FlowFailure::Stale)?
                .pane_terminals
                .values()
                .copied()
                .collect::<Vec<_>>();
            mark_transport_ids_ready(&self.shared, &ids, expected_epoch)?;
            if !self.shared.mark_ready_at_epoch(expected_epoch) {
                for id in ids {
                    registry::detach_transport(id, self.shared.generation);
                }
                return Err(FlowFailure::Stale);
            }
            self.staged_native.clear();
            self.strict_recovery = false;
        } else if !initial {
            // A routine topology refresh can discover a new pane after the
            // session is already Ready. Its authoritative capture is the
            // readiness proof for that pane; existing panes keep their live
            // transport gates untouched.
            mark_transport_ids_ready(&self.shared, &newly_attached, expected_epoch)?;
            self.staged_native.clear();
            self.shared.refresh_terminal_input_ready();
        }
        Ok(())
    }

    fn commit_topology(
        &self,
        mapping: &HashMap<u64, u64>,
        windows: &[tmux::WindowInfo],
        panes: &[tmux::PaneInfo],
        flat: &[PaneSnapshot],
        selected: u64,
        expected_epoch: u64,
    ) -> Result<(), FlowFailure> {
        let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        if self.shared.is_cancelled()
            || state.generation != self.shared.generation
            || state.operation_epoch != expected_epoch
        {
            return Err(FlowFailure::Stale);
        }
        apply_topology_state(&mut state, mapping, windows, panes, flat, selected);
        Ok(())
    }
}

fn apply_topology_state(
    state: &mut SessionState,
    mapping: &HashMap<u64, u64>,
    windows: &[tmux::WindowInfo],
    panes: &[tmux::PaneInfo],
    flat: &[PaneSnapshot],
    selected: u64,
) {
    if state.meeterm_zoomed_pane.is_some_and(|owned| {
        !panes
            .iter()
            .any(|pane| pane.pane_id == owned && pane.zoomed)
    }) {
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_pane = None;
    }
    state.pane_terminals = mapping.clone();
    state.selected_pane = Some(selected);
    state.snapshot = SessionSnapshot {
        windows: windows
            .iter()
            .map(|window| WindowSnapshot {
                window_id: window.window_id,
                name: window.name.clone(),
                panes: flat
                    .iter()
                    .filter(|pane| pane.window_id == window.window_id)
                    .cloned()
                    .collect(),
                selected: flat
                    .iter()
                    .any(|pane| pane.window_id == window.window_id && pane.selected),
                zoomed: panes
                    .iter()
                    .any(|pane| pane.window_id == window.window_id && pane.zoomed),
            })
            .collect(),
        panes: flat.to_vec(),
        selected_pane: Some(selected),
    };
}

/// Apply capture records only from inside the strict final commit. The
/// registry-level transaction resolves and locks every target in deterministic
/// order, validates all generations/bindings/dimensions first, applies every
/// Term, and changes all Attached gates to Ready last. Thus an error is
/// pre-apply and cannot leave one pane with newer cells/history than another.
fn apply_staged_captures_locked(
    shared: &ConnectionShared,
    captures: &[StagedCapture],
) -> Result<(), FlowFailure> {
    let captures = captures
        .iter()
        .map(|capture| registry::ScreenCapture {
            terminal_id: capture.native,
            columns: capture.columns,
            rows: capture.rows,
            bytes: &capture.bytes,
            trailing_output: &capture.trailing_output,
        })
        .collect::<Vec<_>>();
    registry::restore_strict_capture_batch(shared.generation, &captures)
        .map_err(|_| FlowFailure::Stale)
}

impl ControlClient {
    async fn capture(&mut self, pane: u64) -> Result<(), FlowFailure> {
        self.capturing = Some(pane);
        self.capture_complete = false;
        self.capture_output.clear();
        let command = tmux::capture_pane_command_for_session(&self.session, pane)
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        let reply = self.query(&command).await?;
        if reply.len() != 2 {
            return Err(FlowFailure::TmuxProtocol);
        }
        let metadata = reply[1].lines.first().ok_or(FlowFailure::TmuxProtocol)?;
        let fields = metadata
            .split(|b| *b == b',')
            .map(|s| {
                std::str::from_utf8(s)
                    .ok()
                    .and_then(|s| s.parse::<u16>().ok())
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(FlowFailure::TmuxProtocol)?;
        if fields.len() != 12
            || fields[0] < 2
            || fields[0] > 4096
            || fields[1] == 0
            || fields[1] > 4096
            || fields[2] >= fields[0]
            || fields[3] >= fields[1]
            || fields[4..].iter().any(|flag| *flag > 1)
        {
            return Err(FlowFailure::TmuxProtocol);
        }
        let mut bytes = Vec::new();
        if fields[4] != 0 {
            bytes.extend_from_slice(b"\x1b[?1049h");
        }
        for (index, line) in reply[0].lines.iter().enumerate() {
            if index != 0 {
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.extend(tmux::decode_capture(line).map_err(|_| FlowFailure::TmuxProtocol)?);
        }
        bytes.extend_from_slice(
            format!(
                "\x1b[4{}\x1b[?6{}\x1b[?7{}\x1b[{};{}H\x1b[?25{}\x1b[?1{}\x1b[?2004{}{}",
                if fields[9] != 0 { 'h' } else { 'l' },
                if fields[10] != 0 { 'h' } else { 'l' },
                if fields[11] != 0 { 'h' } else { 'l' },
                fields[3] + 1,
                fields[2] + 1,
                if fields[5] != 0 { 'h' } else { 'l' },
                if fields[6] != 0 { 'h' } else { 'l' },
                if fields[8] != 0 { 'h' } else { 'l' },
                if fields[7] != 0 { "\x1b=" } else { "\x1b>" }
            )
            .as_bytes(),
        );
        let id = self.mapping.get(&pane).copied().ok_or(FlowFailure::Stale)?;
        let trailing_output = std::mem::take(&mut self.capture_output);
        self.capturing = None;
        if self.strict_recovery {
            self.staged_captures.push(StagedCapture {
                native: id,
                columns: fields[0],
                rows: fields[1],
                bytes,
                trailing_output,
            });
        } else {
            registry::restore_screen(id, self.shared.generation, fields[0], fields[1], &bytes)
                .map_err(|_| FlowFailure::Stale)?;
            // Output delivered after the capture response is newer than that
            // snapshot. Replay it exactly once instead of dropping it during
            // the following metadata/sentinel responses.
            if !registry::feed_remote(id, self.shared.generation, &trailing_output) {
                return Err(FlowFailure::Transport);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::grid::Dimensions;

    #[test]
    fn topology_safety_ignores_unlinked_nonselected_session() {
        let topologies = vec![
            tmux::WindowTopology {
                session_id: "$1".to_owned(),
                session_name: "selected".to_owned(),
                window_id: 7,
                linked_sessions: 1,
            },
            tmux::WindowTopology {
                session_id: "$2".to_owned(),
                session_name: "other".to_owned(),
                window_id: 8,
                linked_sessions: 1,
            },
        ];
        assert!(topology_is_safe(&topologies, "$1", 7));
        assert!(!topology_is_safe(&topologies, "$1", 8));
    }

    #[test]
    fn topology_safety_rejects_linked_selected_window_and_missing_target() {
        let linked = [tmux::WindowTopology {
            session_id: "$1".to_owned(),
            session_name: "selected".to_owned(),
            window_id: 7,
            linked_sessions: 2,
        }];
        assert!(!topology_is_safe(&linked, "$1", 7));
        assert!(!topology_is_safe(&linked, "$1", 9));
    }

    #[test]
    fn topology_safety_rejects_shared_session_for_new_workspace() {
        let linked = [
            tmux::WindowTopology {
                session_id: "$1".to_owned(),
                session_name: "selected".to_owned(),
                window_id: 7,
                linked_sessions: 2,
            },
            tmux::WindowTopology {
                session_id: "$1".to_owned(),
                session_name: "selected".to_owned(),
                window_id: 8,
                linked_sessions: 2,
            },
        ];
        assert!(!topology_session_is_safe(&linked, "$1"));
        assert!(!topology_session_is_safe(
            &[tmux::WindowTopology {
                session_id: "$2".to_owned(),
                session_name: "other".to_owned(),
                window_id: 7,
                linked_sessions: 1,
            }],
            "$1"
        ));
    }

    fn pane(id: u64, active: bool, window_active: bool) -> tmux::PaneInfo {
        tmux::PaneInfo {
            window_id: 1,
            pane_id: id,
            index: 0,
            active,
            columns: 80,
            rows: 24,
            pane_name: format!("pane-{id}"),
            title: format!("pane-{id}"),
            zoomed: false,
            window_active,
        }
    }

    #[test]
    fn strict_recovery_never_falls_back_to_another_tmux_pane() {
        let panes = vec![pane(17, true, true), pane(23, false, false)];
        assert!(matches!(
            selected_pane_for_sync(&panes, Some(23), true),
            Ok(23)
        ));
        assert!(matches!(
            selected_pane_for_sync(&panes, Some(99), true),
            Err(FlowFailure::TmuxRuntimeMissing)
        ));
        assert!(matches!(
            selected_pane_for_sync(&panes, Some(99), false),
            Ok(17)
        ));
    }

    #[test]
    fn strict_recovery_stages_until_the_final_dirty_readback() {
        assert!(!strict_final_readback_required(true, true, true));
        assert!(strict_final_readback_required(true, true, false));
        assert!(!strict_final_readback_required(false, true, false));
        assert!(!strict_final_readback_required(true, false, false));
    }

    #[test]
    fn strict_final_epoch_abort_keeps_native_term_before_commit() {
        let owner = registry::create_terminal(80, 24).expect("strict atomic owner terminal");
        let generation = 77_003;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-atomic.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-atomic-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("strict atomic session")
            .generation = generation;
        registry::begin_remote(owner, generation).expect("remote owner");
        registry::restore_screen(owner, generation, 80, 24, b"committed-screen")
            .expect("committed capture");
        let before = registry::snapshot(owner).expect("committed native snapshot");
        let expected_epoch = shared.operation_epoch();
        let prepared = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let callback_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_prepared = Arc::clone(&prepared);
        let worker_release = Arc::clone(&release);
        let worker_called = Arc::clone(&callback_called);
        let worker = std::thread::spawn(move || {
            let capture = StagedCapture {
                native: owner,
                columns: 80,
                rows: 24,
                bytes: b"stale-screen".to_vec(),
                trailing_output: Vec::new(),
            };
            // The final capture is completely prepared, but it has not been
            // allowed to enter the native Term commit yet.
            worker_prepared.wait();
            worker_release.wait();
            let callback_shared = Arc::clone(&worker_shared);
            worker_shared.commit_ready_at_epoch_result(expected_epoch, |state| {
                worker_called.store(true, std::sync::atomic::Ordering::Release);
                apply_staged_captures_locked(&callback_shared, &[capture])?;
                state.snapshot = SessionSnapshot::default();
                Ok(())
            })
        });

        prepared.wait();
        // Invalidate the epoch after capture preparation but before the
        // transaction lock is entered. The callback must not run, and the
        // old Term must retain both its visible cells and its history.
        assert!(shared.begin_recovery("transport", 1).is_some());
        release.wait();
        assert!(matches!(
            worker.join().expect("strict atomic worker"),
            Err(FlowFailure::Stale)
        ));
        assert!(!callback_called.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            registry::snapshot(owner).expect("native snapshot after stale commit"),
            before
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn strict_capture_replay_drops_vt_replies_until_ready_commit() {
        let first = registry::create_terminal(20, 4).expect("first strict pane");
        let second = registry::create_terminal(20, 4).expect("second strict pane");
        let generation = 77_004;
        let shared = Arc::new(ConnectionShared::new(
            first,
            generation,
            "tmux-replay.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-replay-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("strict replay session")
            .generation = generation;

        let (first_input, mut first_receiver) = mpsc::channel(8);
        let (first_resize, _first_sizes) = watch::channel((20, 4));
        registry::prepare_pane_transport(first, generation, (20, 4), first_input, first_resize)
            .expect("first attached transport");
        let (second_input, mut second_receiver) = mpsc::channel(8);
        let (second_resize, _second_sizes) = watch::channel((20, 4));
        registry::prepare_pane_transport(second, generation, (20, 4), second_input, second_resize)
            .expect("second attached transport");

        let captures = vec![
            StagedCapture {
                native: first,
                columns: 20,
                rows: 4,
                bytes: b"first-capture\r\n\x1b[6n".to_vec(),
                trailing_output: b"\x1b[c".to_vec(),
            },
            StagedCapture {
                native: second,
                columns: 20,
                rows: 4,
                bytes: b"second-capture\r\n\x1b[6n".to_vec(),
                trailing_output: b"\x1b[c".to_vec(),
            },
        ];
        let expected_epoch = shared
            .begin_recovery("strict-replay", 1)
            .expect("recovery epoch");

        // The callback runs before commit_ready_at_epoch_result publishes the
        // session Ready state. Both DSR/DA replies generated by capture and
        // trailing replay must therefore be absent from the input channels.
        let result = shared.commit_ready_at_epoch_result(expected_epoch, |state| {
            apply_staged_captures_locked(&shared, &captures)?;
            assert!(first_receiver.try_recv().is_err());
            assert!(second_receiver.try_recv().is_err());
            assert!(!state.runtime_operations_ready);
            Ok(())
        });
        assert!(result.is_ok());
        assert_eq!(
            shared.info.lock().expect("replay connection info").state,
            ConnectionState::Ready
        );

        // A query generated by a new live frame after the commit is the only
        // reply allowed to reach the newly Ready transport.
        assert!(registry::feed_remote(first, generation, b"\x1b[6n"));
        assert_eq!(
            first_receiver.try_recv().expect("post-commit DSR reply"),
            b"\x1b[2;1R"
        );
        assert!(second_receiver.try_recv().is_err());

        registry::destroy_terminal(second);
        registry::destroy_terminal(first);
    }

    #[test]
    fn strict_capture_batch_preflights_second_pane_before_any_term_changes() {
        let first = registry::create_terminal(20, 4).expect("first atomic pane");
        let second = registry::create_terminal(20, 4).expect("second atomic pane");
        let generation = 77_005;
        let stale_generation = generation + 1;
        let shared = Arc::new(ConnectionShared::new(
            first,
            generation,
            "tmux-batch.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-batch-known-hosts"),
        ));
        {
            let mut state = shared.session.lock().expect("batch session");
            state.generation = generation;
            state.pane_terminals.insert(101, first);
            state.pane_terminals.insert(102, second);
            state.selected_pane = Some(101);
        }

        let (first_input, mut first_receiver) = mpsc::channel(8);
        let (first_resize, _first_sizes) = watch::channel((20, 4));
        registry::prepare_pane_transport(first, generation, (20, 4), first_input, first_resize)
            .expect("first attached transport");
        let (second_input, mut second_receiver) = mpsc::channel(8);
        let (second_resize, _second_sizes) = watch::channel((20, 4));
        // Inject a real per-terminal generation mismatch at the second pane.
        // The shared session still expects `generation`, while the second
        // Terminal is attached to a newer actor generation.
        registry::prepare_pane_transport(
            second,
            stale_generation,
            (20, 4),
            second_input,
            second_resize,
        )
        .expect("stale second transport");

        registry::restore_screen(
            first,
            generation,
            20,
            4,
            b"first-old-01\r\nfirst-old-02\r\nfirst-old-03\r\nfirst-old-04\r\nfirst-old-05\r\nfirst-old-06",
        )
        .expect("first baseline capture");
        registry::restore_screen(
            second,
            stale_generation,
            20,
            4,
            b"second-old-01\r\nsecond-old-02\r\nsecond-old-03\r\nsecond-old-04\r\nsecond-old-05\r\nsecond-old-06",
        )
        .expect("second baseline capture");
        let native_state = |id| {
            registry::with_terminal_for_test(id, |terminal| {
                (
                    terminal.snapshot().expect("native snapshot"),
                    terminal.content_revision(),
                    terminal.term().grid().history_size(),
                    terminal.term().grid().display_offset(),
                )
            })
            .expect("native state")
        };
        let first_before = native_state(first);
        let second_before = native_state(second);
        let expected_epoch = shared
            .begin_recovery("strict-batch", 1)
            .expect("batch recovery epoch");
        let (topology_before, mapping_before, selected_before, phase_before) = {
            let state = shared.session.lock().expect("batch state before");
            (
                state.snapshot.clone(),
                state.pane_terminals.clone(),
                state.selected_pane,
                state.recovery.phase,
            )
        };
        let info_before = shared.info.lock().expect("batch info before").state;

        let captures = vec![
            StagedCapture {
                native: first,
                columns: 20,
                rows: 4,
                bytes: b"first-new\r\n\x1b[6n".to_vec(),
                trailing_output: Vec::new(),
            },
            StagedCapture {
                native: second,
                columns: 20,
                rows: 4,
                bytes: b"second-new\r\n\x1b[6n".to_vec(),
                trailing_output: Vec::new(),
            },
        ];
        let result = shared.commit_ready_at_epoch_result(expected_epoch, |_state| {
            apply_staged_captures_locked(&shared, &captures)
        });
        assert!(matches!(result, Err(FlowFailure::Stale)));

        // The second pane's generation failure was found during the all-pane
        // preflight. Neither Term has new cells, history, or revision, and no
        // capture-generated reply was queued. Session topology and Ready state
        // are equally untouched because the callback returned before applying.
        assert_eq!(native_state(first), first_before);
        assert_eq!(native_state(second), second_before);
        assert!(first_receiver.try_recv().is_err());
        assert!(second_receiver.try_recv().is_err());
        {
            let state = shared.session.lock().expect("batch state after");
            assert_eq!(state.snapshot, topology_before);
            assert_eq!(state.pane_terminals, mapping_before);
            assert_eq!(state.selected_pane, selected_before);
            assert_eq!(state.recovery.phase, phase_before);
        }
        assert_eq!(
            shared.info.lock().expect("batch info after").state,
            info_before
        );
        assert!(registry::send_bytes(first, b"must-stay-attached").is_err());
        assert!(registry::send_bytes(second, b"must-stay-attached").is_err());

        registry::destroy_terminal(second);
        registry::destroy_terminal(first);
    }

    #[test]
    fn routine_sync_marks_a_new_pane_without_disturbing_existing_input() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let existing = registry::create_terminal(80, 24).expect("existing pane terminal");
        let created = registry::create_terminal(80, 24).expect("new pane terminal");
        let generation = 77_001;
        let shared = ConnectionShared::new(
            owner,
            generation,
            "tmux-test.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-test-known-hosts"),
        );
        shared.session.lock().expect("session state").generation = generation;

        let (existing_input, mut existing_receiver) = mpsc::channel(4);
        let (existing_resize, _existing_sizes) = watch::channel((80, 24));
        registry::prepare_pane_transport(
            existing,
            generation,
            (80, 24),
            existing_input,
            existing_resize,
        )
        .expect("existing transport");
        assert!(registry::mark_transport_ready(existing, generation));
        registry::send_bytes(existing, b"existing-marker").expect("existing input");
        assert_eq!(
            existing_receiver.try_recv().expect("existing marker"),
            b"existing-marker"
        );

        let (created_input, mut created_receiver) = mpsc::channel(4);
        let (created_resize, _created_sizes) = watch::channel((80, 24));
        registry::prepare_pane_transport(
            created,
            generation,
            (80, 24),
            created_input,
            created_resize,
        )
        .expect("new pane transport");
        assert!(
            registry::send_bytes(created, b"before-ready").is_err(),
            "an attached pane cannot accept input before its capture marker"
        );

        let expected_epoch = shared.operation_epoch();
        assert!(matches!(
            mark_transport_ids_ready(&shared, &[created], expected_epoch),
            Ok(())
        ));
        registry::send_bytes(created, b"new-marker").expect("new pane input");
        assert_eq!(
            created_receiver.try_recv().expect("new pane marker"),
            b"new-marker"
        );
        // The helper only marks the newly attached list; the already live pane
        // remains able to carry its marker through the same backend sync.
        registry::send_bytes(existing, b"existing-again").expect("existing input again");
        assert_eq!(
            existing_receiver.try_recv().expect("existing marker again"),
            b"existing-again"
        );

        registry::destroy_terminal(created);
        registry::destroy_terminal(existing);
        registry::destroy_terminal(owner);
    }

    #[test]
    fn a_revoked_pane_rolls_back_partial_ready_marks() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let first = registry::create_terminal(80, 24).expect("first pane terminal");
        let second = registry::create_terminal(80, 24).expect("second pane terminal");
        let generation = 77_002;
        let shared = ConnectionShared::new(
            owner,
            generation,
            "tmux-test.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-test-known-hosts"),
        );
        shared.session.lock().expect("session state").generation = generation;
        for id in [first, second] {
            let (input, _receiver) = mpsc::channel(2);
            let (resize, _sizes) = watch::channel((80, 24));
            registry::prepare_pane_transport(id, generation, (80, 24), input, resize)
                .expect("prepared pane");
        }
        // Inject loss at the second pane boundary. The first mark must be
        // rolled back, so the caller cannot publish a partially-ready batch.
        registry::detach_transport(second, generation);
        let result = mark_transport_ids_ready(&shared, &[first, second], shared.operation_epoch());
        assert!(matches!(result, Err(FlowFailure::Stale)));
        assert!(!registry::mark_transport_ready(first, generation));
        assert!(!registry::mark_transport_ready(second, generation));

        registry::destroy_terminal(second);
        registry::destroy_terminal(first);
        registry::destroy_terminal(owner);
    }
}
