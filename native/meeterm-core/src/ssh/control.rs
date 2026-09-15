//! One serialized tmux command stream and its native pane transports.
use super::*;
use std::collections::{HashSet, VecDeque};

const MAX_PANES: usize = 4096;

enum PaneEvent {
    Input(u64, Vec<u8>),
    Resize(u64, u16, u16),
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
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        for task in self.routes.values() {
            task.abort();
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
    commands: &mut mpsc::Receiver<ControlCommand>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::AttachingTmux);

    let exact_runtime = profile.tmux_identity.is_some() || profile.runtime.is_some();
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
            // flows enter the picker, and an automatic reconnect with no
            // verifiable binding is handled as a local runtime loss there.
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
            command = commands.recv() => {
                match command {
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
                    None => return Err(FlowFailure::Stale),
                }
            }
            event = client.pane_receiver.recv() => {
                match event {
                    Some(PaneEvent::Input(pane, bytes)) => {
                        if client.routes.contains_key(&pane) {
                            let command = tmux::send_bytes_command_for_session(
                                &client.session,
                                pane,
                                &bytes,
                            )
                            .map_err(|_| FlowFailure::TmuxProtocol)?;
                            client.query(&command).await?;
                        }
                    }
                    Some(PaneEvent::Resize(pane, columns, rows)) => {
                        let selected = shared.session.lock().map_err(|_| FlowFailure::Stale)?.selected_pane;
                        if selected == Some(pane) && client.viewport != (columns, rows) {
                            client.viewport = (columns, rows);
                            shared.session.lock().map_err(|_| FlowFailure::Stale)?.viewport = Some((columns, rows));
                            client.resize_client().await?;
                            client.synchronize(false).await?;
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
                let id = self
                    .shared
                    .session
                    .lock()
                    .map_err(|_| FlowFailure::Stale)?
                    .pane_terminals
                    .get(&pane_id)
                    .copied();
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
        let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        state.selected_pane = Some(pane);
        state.meeterm_zoomed = !zoomed;
        state.meeterm_zoomed_pane = (!zoomed).then_some(pane);
        mark_selected(&mut state.snapshot, pane);
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
        // Routine refreshes keep the live transport usable. Reporting a new
        // connection phase here would make the UI unmount its terminal view.
        if initial {
            self.shared.set_state(ConnectionState::Synchronizing);
        }
        self.dirty = false;
        if initial {
            self.resize_client().await?;
        }
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
        let old = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .snapshot
            .clone();
        let mut mapping = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .pane_terminals
            .clone();
        let ids = panes.iter().map(|p| p.pane_id).collect::<HashSet<_>>();
        let stale = mapping
            .keys()
            .filter(|id| !ids.contains(id))
            .copied()
            .collect::<Vec<_>>();
        for pane in stale {
            if let Some(task) = self.routes.remove(&pane) {
                task.abort();
            }
            if let Some(id) = mapping.remove(&pane) {
                registry::detach_transport(id, self.shared.generation);
                if id != self.shared.terminal_id {
                    registry::destroy_terminal(id);
                }
            }
        }
        let selected = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .selected_pane
            .filter(|id| ids.contains(id))
            .or_else(|| {
                panes
                    .iter()
                    .find(|p| p.active && p.window_active)
                    .map(|p| p.pane_id)
            })
            .unwrap_or(panes[0].pane_id);
        let mut capture = Vec::new();
        for pane in &panes {
            let id = match mapping.get(&pane.pane_id).copied() {
                Some(id) => id,
                None => {
                    let id = if mapping.is_empty() && old.panes.is_empty() {
                        self.shared.terminal_id
                    } else {
                        registry::create_terminal(pane.columns, pane.rows)
                            .map_err(|_| FlowFailure::TmuxProtocol)?
                    };
                    mapping.insert(pane.pane_id, id);
                    id
                }
            };
            if !self.routes.contains_key(&pane.pane_id) {
                self.attach(pane.pane_id, id, (pane.columns, pane.rows))?;
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
        {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            if self.shared.is_cancelled() || state.generation != self.shared.generation {
                return Err(FlowFailure::Stale);
            }
            if state.meeterm_zoomed_pane.is_some_and(|owned| {
                !panes
                    .iter()
                    .any(|pane| pane.pane_id == owned && pane.zoomed)
            }) {
                state.meeterm_zoomed = false;
                state.meeterm_zoomed_pane = None;
            }
            state.pane_terminals = mapping;
            state.selected_pane = Some(selected);
            state.snapshot = SessionSnapshot {
                windows: windows
                    .iter()
                    .map(|w| WindowSnapshot {
                        window_id: w.window_id,
                        name: w.name.clone(),
                        panes: flat
                            .iter()
                            .filter(|p| p.window_id == w.window_id)
                            .cloned()
                            .collect(),
                        selected: flat
                            .iter()
                            .any(|p| p.window_id == w.window_id && p.selected),
                        zoomed: panes.iter().any(|p| p.window_id == w.window_id && p.zoomed),
                    })
                    .collect(),
                panes: flat,
                selected_pane: Some(selected),
            };
        }
        for pane in capture {
            self.capture(pane).await?;
        }
        if initial {
            let pane = panes.iter().find(|p| p.pane_id == selected).unwrap();
            self.select(pane.window_id, selected).await?;
            self.dirty = true; // selection/zoom sizes are read back before input readiness
        }
        for id in self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .pane_terminals
            .values()
        {
            registry::mark_transport_ready(*id, self.shared.generation);
        }
        self.shared.set_state(ConnectionState::Ready);
        Ok(())
    }

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
        let id = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .pane_terminals[&pane];
        registry::restore_screen(id, self.shared.generation, fields[0], fields[1], &bytes)
            .map_err(|_| FlowFailure::Stale)?;
        self.capturing = None;
        // Output delivered after the capture response is newer than that
        // snapshot. Replay it exactly once instead of dropping it during the
        // following metadata/sentinel responses.
        if !registry::feed_remote(id, self.shared.generation, &self.capture_output) {
            return Err(FlowFailure::Transport);
        }
        self.capture_output.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
