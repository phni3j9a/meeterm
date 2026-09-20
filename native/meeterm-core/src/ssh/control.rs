//! One serialized tmux command stream and its native pane transports.
use super::*;
use std::collections::{HashSet, VecDeque};

const MAX_PANES: usize = 4096;
const EXPLICIT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControllerLoopExit {
    Cancellation,
    Recovery,
}

/// Decide which lifecycle boundary owns the controller's next exit.
/// Explicit cancellation is checked before recovery so the bounded
/// zoom/hook cleanup remains available when `invalidate_explicitly` has
/// already published a stopped phase but the cancellation wake has not yet
/// been consumed.
fn controller_loop_exit(
    shared: &ConnectionShared,
    initial_recovery_epoch: Option<u64>,
) -> Option<ControllerLoopExit> {
    if shared.is_cancelled() {
        Some(ControllerLoopExit::Cancellation)
    } else if shared.recovery_requires_controller_exit(initial_recovery_epoch) {
        // Re-read the explicit intent after the session-lock-backed recovery
        // check. `invalidate_explicitly` publishes both under that lock, so
        // this ordering closes the interval where the actor could otherwise
        // observe Stopped and a stale cancellation flag together.
        if shared.explicit_cleanup_requested() || shared.is_cancelled() {
            Some(ControllerLoopExit::Cancellation)
        } else {
            Some(ControllerLoopExit::Recovery)
        }
    } else if shared.explicit_cleanup_requested() {
        // Runtime handoff publishes its explicit intent before the actor is
        // necessarily cancelled. Keep the cleanup path selected even if the
        // recovery-state change has not become visible in this iteration.
        Some(ControllerLoopExit::Cancellation)
    } else {
        None
    }
}

/// The established controller loop is wrapped in one async finalization
/// boundary. Keep this decision separate from the loop result so every
/// `?`/inner-operation exit gets the same explicit cleanup treatment.
fn controller_loop_cleanup_needed(shared: &ConnectionShared, remote_session_closed: bool) -> bool {
    shared.explicit_cleanup_requested()
        && !shared.is_cancelled()
        && !remote_session_closed
        && shared
            .session
            .lock()
            .map(|state| state.generation == shared.generation)
            .unwrap_or(false)
}

struct StagedCapture {
    native: u64,
    columns: u16,
    rows: u16,
    bytes: Vec<u8>,
    trailing_output: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingZoomCleanup {
    pane: u64,
    hooks: Option<tmux::ZoomRecoveryHookAllocation>,
}

/// The zoom observation captured from the same fresh pane readback as an
/// initial selection.  It must not be replaced by the shared snapshot until
/// that synchronization pass has committed its topology.  The relationship
/// to a prior meeterm-owned pane is captured at the same boundary so a strict
/// recovery can distinguish its own zoom from a desktop-preexisting zoom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectionObservation {
    window_id: u64,
    pane_id: u64,
    window_zoomed: bool,
    previous_owned_same_window: bool,
}

fn fresh_selection_observation(
    pane: &tmux::PaneInfo,
    panes: &[tmux::PaneInfo],
    previous_owned_pane: Option<u64>,
) -> SelectionObservation {
    SelectionObservation {
        window_id: pane.window_id,
        pane_id: pane.pane_id,
        window_zoomed: pane.zoomed,
        previous_owned_same_window: previous_owned_pane.is_some_and(|previous| {
            panes
                .iter()
                .find(|candidate| candidate.pane_id == previous)
                .is_some_and(|candidate| candidate.window_id == pane.window_id)
        }),
    }
}

/// Prefer a fresh synchronization observation whenever one is available.
/// A mismatched observation is treated as zoomed (desktop-owned) rather than
/// falling back to stale shared state, so an observation boundary failure can
/// never grant meeterm cleanup ownership.
fn selection_is_zoomed(
    observation: Option<SelectionObservation>,
    window: u64,
    pane: u64,
    shared_zoomed: bool,
    shared_previous_same_window: bool,
) -> bool {
    match observation {
        Some(observation) if observation.window_id == window && observation.pane_id == pane => {
            observation.window_zoomed && !observation.previous_owned_same_window
        }
        Some(_) => true,
        None => shared_zoomed && !shared_previous_same_window,
    }
}

/// Record the cleanup target before the first await that can allow the remote
/// mutation to execute. Callers provide this only for a meeterm-owned restore
/// or for a target whose window was just proven not to be pre-zoomed. Keeping
/// the intent before writer completion closes the remote-apply/actor-resume
/// race without claiming a pre-existing desktop zoom.
fn pending_zoom_cleanup_before_send(
    current: Option<PendingZoomCleanup>,
    intent: Option<PendingZoomCleanup>,
) -> Option<PendingZoomCleanup> {
    intent.or(current)
}

/// Finalize the actor-private cleanup target at the writer result boundary. A
/// writer rejection leaves the already-recorded pre-send intent in place: an
/// idempotent cleanup is safer than missing a mutation that tmux applied before
/// the future reported its failure. The explicit outcome is a focused unit
/// seam for the asynchronous race.
fn pending_zoom_cleanup_after_send(
    current: Option<PendingZoomCleanup>,
    intent: Option<PendingZoomCleanup>,
    writer_accepted: bool,
) -> Option<PendingZoomCleanup> {
    if writer_accepted {
        intent.or(current)
    } else {
        current
    }
}

fn select_zoom_cleanup_target(
    pending: Option<PendingZoomCleanup>,
    shared: Option<PendingZoomCleanup>,
) -> Option<PendingZoomCleanup> {
    pending.or(shared)
}

/// A strict recovery selection is only transferable to shared ownership when
/// the candidate came from `select_without_publish` and the final readback
/// still shows that exact pane's window zoomed. A pre-existing desktop zoom
/// has no candidate and therefore cannot cross this boundary.
fn publish_strict_zoom_ownership(
    state: &mut SessionState,
    candidate: Option<PendingZoomCleanup>,
    panes: &[tmux::PaneInfo],
    selected: u64,
) {
    let Some(candidate) = candidate.filter(|candidate| {
        candidate.pane == selected
            && panes
                .iter()
                .any(|pane| pane.pane_id == selected && pane.zoomed)
    }) else {
        return;
    };
    state.meeterm_zoomed = true;
    state.meeterm_zoomed_pane = Some(candidate.pane);
}

fn publish_selection_state(state: &mut SessionState, pane: u64, zoomed: bool) {
    state.selected_pane = Some(pane);
    state.meeterm_zoomed = !zoomed;
    state.meeterm_zoomed_pane = (!zoomed).then_some(pane);
    mark_selected(&mut state.snapshot, pane);
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
    /// Actor-private cleanup intent is published before the zoom mutation is
    /// awaited by the SSH writer. It covers the interval in which tmux may
    /// apply the bytes before the writer future resumes or a response/epoch
    /// recheck can publish shared ownership.
    pending_zoom_cleanup: Option<PendingZoomCleanup>,
    /// The final selection intent from `select_without_publish`. It is kept
    /// separate from general cleanup ownership because a strict recovery can
    /// first restore an older owned pane and then observe an unrelated desktop
    /// zoom; only the final selection candidate may become shared ownership.
    pending_zoom_ownership: Option<PendingZoomCleanup>,
    /// A successful final-pane/window close destroys the tmux session. The
    /// finalizer must not send zoom cleanup to that already-closed session if
    /// an explicit lifecycle request races with the close acknowledgement.
    remote_session_closed: bool,
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
        pending_zoom_cleanup: None,
        pending_zoom_ownership: None,
        remote_session_closed: false,
    };
    let flow_result = async {
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
            match controller_loop_exit(shared, initial_recovery_epoch) {
                Some(ControllerLoopExit::Cancellation) => return Err(FlowFailure::Stale),
                Some(ControllerLoopExit::Recovery) => {
                    for task in client.routes.values() {
                        task.abort();
                    }
                    detach_all(shared);
                    return Err(FlowFailure::Transport);
                }
                None => {}
            }
            if client.dirty {
                client.synchronize(false).await?;
                continue;
            }
            tokio::select! {
                biased;
                _ = shared.cancelled() => return Err(FlowFailure::Stale),
                _ = shared.explicit_cleanup() => return Err(FlowFailure::Stale),
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
    .await;
    if controller_loop_cleanup_needed(shared, client.remote_session_closed) {
        client.restore_zoom().await;
    }
    flow_result
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

    /// Read Control Mode events for the final cleanup acknowledgement. The
    /// normal event path observes the shared lifecycle gates and stops on an
    /// explicit request; cleanup is the one bounded operation that must keep
    /// reading while hard cancellation is still deferred.
    async fn next_event_for_cleanup(&mut self) -> Result<tmux::Event, FlowFailure> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            let message = self.reader.wait().await;
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
        self.query_with_zoom_cleanup(command, None).await
    }

    /// Send one Control Mode command and optionally retain actor-private zoom
    /// cleanup authority. The intent is recorded before awaiting
    /// `data_bytes`, because tmux can apply the command before that future
    /// completes. A response may still become stale immediately afterward, so
    /// the accepted result is rechecked below.
    async fn query_with_zoom_cleanup(
        &mut self,
        command: &str,
        pending_zoom_cleanup: Option<PendingZoomCleanup>,
    ) -> Result<Vec<tmux::CommandBlock>, FlowFailure> {
        let expected_epoch = self.command_epoch.unwrap_or_else(|| {
            if self.strict_recovery {
                self.operation_epoch
            } else {
                self.shared.operation_epoch()
            }
        });
        if !self.shared.current_request_epoch(expected_epoch) {
            return Err(FlowFailure::Stale);
        }
        if let Some(intent) = pending_zoom_cleanup {
            // This is deliberately opt-in. The selection path passes an intent
            // only after proving that the target window was not pre-zoomed;
            // the pre-existing desktop-zoom path calls `query` with `None`.
            self.pending_zoom_cleanup =
                pending_zoom_cleanup_before_send(self.pending_zoom_cleanup, Some(intent));
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
        self.pending_zoom_cleanup =
            pending_zoom_cleanup_after_send(self.pending_zoom_cleanup, pending_zoom_cleanup, true);
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
        self.remote_session_closed = true;
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
        self.select_impl(window, pane, true, None).await
    }

    async fn select_with_observation(
        &mut self,
        window: u64,
        pane: u64,
        observation: SelectionObservation,
    ) -> Result<(), FlowFailure> {
        self.select_impl(window, pane, true, Some(observation))
            .await
    }

    async fn select_without_publish(
        &mut self,
        window: u64,
        pane: u64,
        observation: SelectionObservation,
    ) -> Result<(), FlowFailure> {
        self.select_impl(window, pane, false, Some(observation))
            .await
    }

    async fn select_impl(
        &mut self,
        window: u64,
        pane: u64,
        publish: bool,
        observation: Option<SelectionObservation>,
    ) -> Result<(), FlowFailure> {
        if !publish {
            // A strict recovery has one final selection candidate. Reset any
            // stale candidate before proving this selection's zoom state so a
            // pre-existing desktop zoom cannot inherit old ownership.
            self.pending_zoom_ownership = None;
        }
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
            self.query_with_zoom_cleanup(
                &command,
                Some(PendingZoomCleanup {
                    pane: previous,
                    hooks: self.zoom_hooks,
                }),
            )
            .await?;
        }

        // A desktop user may already have zoomed this window.  Routine
        // post-Ready selections use the committed topology snapshot, while
        // initial/strict selections use the typed observation captured from
        // this synchronization pass.  If the previous pane was meeterm-owned
        // in this same window, the restore above has just cleared that zoom;
        // the observation carries that exception without publishing partial
        // topology early.
        let (shared_zoomed, shared_previous_same_window) = {
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
            (zoomed, previous_same_window)
        };
        let zoomed = selection_is_zoomed(
            observation,
            window,
            pane,
            shared_zoomed,
            shared_previous_same_window,
        );
        if zoomed {
            // tmux unzooms a window when selecting a different pane inside
            // the zoomed window.  Its normal idempotent selection command
            // immediately re-zooms the new target, so the desktop zoom state
            // survives the mobile tab change even though meeterm does not own
            // the cleanup.
            let allocation = self.zoom_hooks;
            let mut transition = vec![
                tmux::select_pane_command_for_session(&self.session, None, window, pane)
                    .map_err(|_| FlowFailure::TmuxProtocol)?,
            ];
            if let Some(allocation) = allocation {
                transition.push(
                    tmux::remove_zoom_recovery_hooks_command_for_session(&self.session, allocation)
                        .map_err(|_| FlowFailure::TmuxProtocol)?,
                );
            }
            self.query(&transition.join(" ; ")).await?;
            self.zoom_hooks = None;
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
            let ownership = PendingZoomCleanup {
                pane,
                hooks: Some(allocation),
            };
            if !publish {
                // `zoomed == false` above is the proof that this selection is
                // not inheriting a desktop-owned zoom. Keep the candidate
                // until the strict final readback/Ready commit transfers it
                // to SessionState.
                self.pending_zoom_ownership = Some(ownership);
            }
            self.query_with_zoom_cleanup(&transition, Some(ownership))
                .await?;
        }
        if publish {
            let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            publish_selection_state(&mut state, pane, zoomed);
            self.pending_zoom_cleanup = None;
            self.pending_zoom_ownership = None;
        }
        Ok(())
    }

    async fn restore_zoom(&mut self) {
        let shared_owned = self.shared.session.lock().ok().and_then(|s| {
            if s.generation != self.shared.generation {
                return None;
            }
            s.meeterm_zoomed
                .then_some(s.meeterm_zoomed_pane)
                .flatten()
                .map(|pane| PendingZoomCleanup {
                    pane,
                    hooks: self.zoom_hooks,
                })
        });
        let cleanup = select_zoom_cleanup_target(self.pending_zoom_cleanup.take(), shared_owned);
        if let Some(cleanup) = cleanup {
            // Explicit shutdown has revoked normal operation gates, but hard
            // cancellation is intentionally deferred until this bounded
            // cleanup has finished. The trailing marker is an acknowledgement
            // from tmux itself, not merely russh accepting bytes into its
            // channel queue.
            let command = if let Some(allocation) = cleanup.hooks.or(self.zoom_hooks) {
                tmux::cleanup_zoom_recovery_hooks_command_for_session(
                    &self.session,
                    allocation,
                    cleanup.pane,
                )
                .unwrap_or_default()
            } else {
                tmux::restore_layout_command_for_session(&self.session, cleanup.pane)
                    .unwrap_or_default()
            };
            self.command_number = self.command_number.saturating_add(1);
            let marker = format!(
                "MEETERM_CLEANUP_DONE_{}_{}",
                self.shared.generation, self.command_number
            );
            let request = format!("{command} ; display-message -p '{marker}'\n");
            let deadline = tokio::time::Instant::now() + EXPLICIT_CLEANUP_TIMEOUT;
            let sent =
                tokio::time::timeout_at(deadline, self.writer.data_bytes(request.into_bytes()))
                    .await;
            if matches!(sent, Ok(Ok(_))) {
                let _ = tokio::time::timeout_at(deadline, async {
                    loop {
                        let event = self.next_event_for_cleanup().await?;
                        match event {
                            tmux::Event::Command(block) if block.error => {
                                return Err(FlowFailure::Tmux);
                            }
                            tmux::Event::Command(block)
                                if block.lines.len() == 1
                                    && block.lines[0] == marker.as_bytes() =>
                            {
                                return Ok(());
                            }
                            tmux::Event::Command(_) => {}
                            // Input/output/notifications that were already
                            // buffered belong to the old presentation. The
                            // explicit boundary revoked their native gates;
                            // consume them only to reach the cleanup marker.
                            tmux::Event::Output { .. } | tmux::Event::Notification { .. } => {}
                        }
                    }
                })
                .await;
            }
            // A disconnected actor must not leave stale ownership behind for
            // the next reconnect.  The generation check prevents an older
            // cancellation from clearing ownership established by a newer
            // Control Mode actor using the same terminal ID.
            if let Ok(mut state) = self.shared.session.lock()
                && state.generation == self.shared.generation
                && state.meeterm_zoomed
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
        self.shared.suspend_terminal_if_background(id);
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
            if self.strict_recovery {
                self.operation_epoch
            } else {
                self.shared.operation_epoch()
            }
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
        // `panes` is the authoritative readback for this synchronization
        // pass.  Capture the selection's zoom state before any topology is
        // committed so initial/manual reconnects do not consult an empty or
        // stale shared snapshot.  Only a still-present prior owned pane in
        // the same freshly read window can make a strict recovery eligible to
        // restore and re-publish ownership.
        let initial_selection_observation = if initial {
            let pane = panes
                .iter()
                .find(|pane| pane.pane_id == selected)
                .ok_or(FlowFailure::TmuxRuntimeMissing)?;
            let previous_owned_pane = self
                .shared
                .session
                .lock()
                .map_err(|_| FlowFailure::Stale)?
                .meeterm_zoomed_pane;
            Some(fresh_selection_observation(
                pane,
                &panes,
                previous_owned_pane,
            ))
        } else {
            None
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
            let observation = initial_selection_observation
                .expect("initial selection must have a fresh observation");
            if self.strict_recovery {
                self.select_without_publish(pane.window_id, selected, observation)
                    .await?;
            } else {
                self.select_with_observation(pane.window_id, selected, observation)
                    .await?;
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
            let strict_zoom_ownership = self.pending_zoom_ownership;
            let shared = Arc::clone(&self.shared);
            let commit = self
                .shared
                .commit_ready_at_epoch_result(expected_epoch, |state| {
                    // This closure runs only after the expected epoch has been
                    // checked and while the session lock is held. The registry
                    // batch keeps every pane Attached/Suspended while it
                    // preflights and replays all captures, then marks Attached
                    // transports Ready only after the last replay succeeds.
                    // VT replies emitted by capture replay therefore hit the
                    // closed gate and are deliberately discarded; no stale
                    // query is buffered.
                    apply_staged_captures_locked(&shared, &staged_captures)?;
                    apply_topology_state(state, &mapping, &windows, &panes, &flat, selected);
                    // This is part of the same lock/epoch transaction as the
                    // topology and Ready publication. If the callback is
                    // stale, the candidate remains actor-private for an
                    // explicit finalizer instead of being lost.
                    publish_strict_zoom_ownership(state, strict_zoom_ownership, &panes, selected);
                    Ok(())
                });
            if let Err(failure) = commit {
                for id in ids {
                    registry::detach_transport(id, self.shared.generation);
                }
                return Err(failure);
            }
            // Shared ownership is now authoritative. Only after the commit
            // succeeds may the actor drop its duplicate cleanup source.
            self.pending_zoom_cleanup = None;
            self.pending_zoom_ownership = None;
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
        for pane in &stale {
            if let Some(task) = self.routes.remove(pane) {
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
/// Term, and changes Attached gates to Ready last. Suspended gates remain
/// closed until foreground resumes them. Thus an error is pre-apply and
/// cannot leave one pane with newer cells/history than another.
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

    fn zoomed_pane(id: u64, active: bool, window_active: bool) -> tmux::PaneInfo {
        let mut pane = pane(id, active, window_active);
        pane.zoomed = true;
        pane
    }

    #[test]
    fn initial_selection_uses_fresh_zoom_with_empty_shared_snapshot() {
        let target = zoomed_pane(17, true, true);
        let observation = fresh_selection_observation(&target, std::slice::from_ref(&target), None);

        // The shared snapshot is still empty during the initial selection,
        // but the same synchronization pass already proved that the window
        // was zoomed by the desktop.
        assert!(selection_is_zoomed(Some(observation), 1, 17, false, false));
        let mut state = SessionState::default();
        publish_selection_state(&mut state, 17, true);
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
    }

    #[test]
    fn strict_initial_selection_only_reuses_proven_same_window_ownership() {
        let target = zoomed_pane(17, true, true);
        let previous = pane(23, false, false);
        let panes = vec![target.clone(), previous.clone()];
        let same_window = fresh_selection_observation(&target, &panes, Some(previous.pane_id));

        // A retained owner in the same freshly read window is the one
        // permitted exception: restore then re-zoom may retain ownership.
        assert!(same_window.previous_owned_same_window);
        assert!(!selection_is_zoomed(Some(same_window), 1, 17, false, false));
        let mut state = SessionState::default();
        publish_selection_state(&mut state, 17, false);
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, Some(17));

        // A missing prior pane or a prior pane in another window does not
        // prove that the fresh zoom belongs to meeterm.
        let missing = fresh_selection_observation(&target, std::slice::from_ref(&target), Some(23));
        assert!(!missing.previous_owned_same_window);
        assert!(selection_is_zoomed(Some(missing), 1, 17, false, false));

        let mut other_window = previous;
        other_window.window_id = 2;
        let other_window_panes = vec![target.clone(), other_window];
        let other_window_observation =
            fresh_selection_observation(&target, &other_window_panes, Some(23));
        assert!(!other_window_observation.previous_owned_same_window);
        assert!(selection_is_zoomed(
            Some(other_window_observation),
            1,
            17,
            false,
            false
        ));
    }

    #[test]
    fn routine_selection_keeps_preexisting_desktop_zoom_unowned_for_cleanup() {
        let mut state = SessionState::default();
        state.snapshot.windows.push(tmux::WindowSnapshot {
            window_id: 1,
            name: "desktop".to_owned(),
            panes: Vec::new(),
            selected: true,
            zoomed: true,
        });
        let shared_zoomed = state.snapshot.windows[0].zoomed;
        let zoomed = selection_is_zoomed(None, 1, 23, shared_zoomed, false);

        // A later pane selection uses the committed snapshot and must not
        // manufacture a cleanup target for a desktop-owned zoom.
        assert!(zoomed);
        publish_selection_state(&mut state, 23, zoomed);
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
        assert_eq!(select_zoom_cleanup_target(None, None), None);
    }

    #[test]
    fn fresh_zoom_readback_cannot_claim_against_stale_shared_snapshot() {
        let target = zoomed_pane(17, true, true);
        let observation = fresh_selection_observation(&target, std::slice::from_ref(&target), None);

        // Every contradictory stale shared combination is ignored while the
        // fresh observation is present. In particular, shared `zoomed=false`
        // must not turn a fresh desktop zoom into meeterm ownership.
        for (shared_zoomed, shared_previous_same_window) in
            [(false, false), (false, true), (true, false)]
        {
            assert!(selection_is_zoomed(
                Some(observation),
                1,
                17,
                shared_zoomed,
                shared_previous_same_window
            ));
        }
        let mut state = SessionState::default();
        publish_selection_state(&mut state, 17, true);
        assert!(!state.meeterm_zoomed);
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
    fn zoom_cleanup_ownership_requires_writer_acceptance_and_prefers_pending_target() {
        let allocation = tmux::ZoomRecoveryHookAllocation { index: 1_003 };
        let previous = PendingZoomCleanup {
            pane: 17,
            hooks: Some(allocation),
        };
        let target = PendingZoomCleanup {
            pane: 23,
            hooks: Some(allocation),
        };

        // The remote mutation may be applied while data_bytes is still
        // pending and before an explicit disconnect reaches the actor. The
        // pre-send intent is therefore already sufficient for finalization.
        let before_writer_completion = pending_zoom_cleanup_before_send(None, Some(target));
        assert_eq!(before_writer_completion, Some(target));
        assert_eq!(
            pending_zoom_cleanup_after_send(before_writer_completion, Some(target), false,),
            Some(target)
        );

        // A previously owned pane is replaced by the newest mutation intent,
        // while no intent remains when the target was pre-zoomed by desktop.
        assert_eq!(
            pending_zoom_cleanup_before_send(Some(previous), Some(target)),
            Some(target)
        );
        assert_eq!(pending_zoom_cleanup_before_send(None, None), None);

        // If a caller had not recorded a pre-send intent, the post-send
        // helper remains conservative: rejection does not invent ownership,
        // and a previous owned-pane unzoom remains the safe fallback.
        assert_eq!(
            pending_zoom_cleanup_after_send(Some(previous), Some(target), false),
            Some(previous)
        );
        // Once the new zoom bytes are accepted, the target is retained even
        // if its response/epoch publication later returns Stale.
        assert_eq!(
            pending_zoom_cleanup_after_send(Some(previous), Some(target), true),
            Some(target)
        );
        // A pre-existing desktop zoom supplies no intent and is never claimed,
        // even if its ordinary query is accepted.
        assert_eq!(pending_zoom_cleanup_after_send(None, None, true), None);
        assert_eq!(
            select_zoom_cleanup_target(Some(target), Some(previous)),
            Some(target)
        );
    }

    #[test]
    fn explicit_cancellation_wins_before_recovery_exit_even_before_cancel_wake() {
        let owner = registry::create_terminal(80, 24).expect("explicit cleanup owner terminal");
        let generation = 77_006;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-explicit-cleanup.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-explicit-cleanup-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("explicit cleanup session")
            .generation = generation;

        // Model the exact gap in the public disconnect path: invalidation
        // publishes Stopped before cancel() publishes the wake. The loop
        // must retain the cancellation cleanup branch in that interval.
        shared.invalidate_explicitly("explicit_disconnect");
        assert!(!shared.is_cancelled());
        assert_eq!(
            controller_loop_exit(&shared, None),
            Some(ControllerLoopExit::Cancellation)
        );
        assert!(controller_loop_cleanup_needed(&shared, false));

        shared.cancel();
        assert_eq!(
            controller_loop_exit(&shared, None),
            Some(ControllerLoopExit::Cancellation)
        );
        assert!(!controller_loop_cleanup_needed(&shared, false));
        shared
            .session
            .lock()
            .expect("replacement generation session")
            .generation = generation + 1;
        assert!(!controller_loop_cleanup_needed(&shared, false));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn explicit_cleanup_finalizes_after_inner_operation_escape() {
        let owner = registry::create_terminal(80, 24).expect("inner escape owner terminal");
        let generation = 77_008;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-inner-escape.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-inner-escape-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("inner escape session")
            .generation = generation;
        shared.invalidate_explicitly("explicit_disconnect");

        // Model an awaited synchronize/query/dispatch operation returning
        // through `?`: finalization is evaluated after the inner result, not
        // only by the top-of-loop classifier.
        let inner_result: Result<(), FlowFailure> = (|| {
            Err(FlowFailure::Stale)?;
            Ok(())
        })();
        assert!(matches!(inner_result, Err(FlowFailure::Stale)));
        assert!(controller_loop_cleanup_needed(&shared, false));
        assert!(!controller_loop_cleanup_needed(&shared, true));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn explicit_cleanup_is_blocked_after_generation_replacement_or_force_cancel() {
        let owner = registry::create_terminal(80, 24).expect("generation cleanup owner terminal");
        let generation = 77_009;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-generation-cleanup.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-generation-cleanup-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("generation cleanup session")
            .generation = generation;
        shared.invalidate_explicitly("explicit_disconnect");
        assert!(controller_loop_cleanup_needed(&shared, false));

        shared
            .session
            .lock()
            .expect("replaced generation cleanup session")
            .generation = generation + 1;
        assert!(!controller_loop_cleanup_needed(&shared, false));

        shared.cancel();
        assert!(!controller_loop_cleanup_needed(&shared, false));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn recovery_exit_remains_transport_owned_without_explicit_cancellation() {
        let owner = registry::create_terminal(80, 24).expect("recovery exit owner terminal");
        let generation = 77_007;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-recovery-exit.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-recovery-exit-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("recovery exit session")
            .generation = generation;
        shared
            .begin_recovery("transport", 1)
            .expect("recovery should begin");

        assert!(!shared.is_cancelled());
        assert_eq!(
            controller_loop_exit(&shared, None),
            Some(ControllerLoopExit::Recovery)
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn strict_recovery_stages_until_the_final_dirty_readback() {
        assert!(!strict_final_readback_required(true, true, true));
        assert!(strict_final_readback_required(true, true, false));
        assert!(!strict_final_readback_required(false, true, false));
        assert!(!strict_final_readback_required(true, false, false));
    }

    #[test]
    fn strict_recovery_ready_commit_publishes_meeterm_zoom_ownership() {
        let owner = registry::create_terminal(80, 24).expect("strict ownership owner terminal");
        let generation = 77_010;
        let shared = ConnectionShared::new(
            owner,
            generation,
            "tmux-strict-ownership.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-strict-ownership-known-hosts"),
        );
        shared
            .session
            .lock()
            .expect("strict ownership session")
            .generation = generation;
        let expected_epoch = shared
            .begin_recovery("strict_ownership", 1)
            .expect("strict ownership recovery epoch");
        let candidate = PendingZoomCleanup {
            pane: 17,
            hooks: Some(tmux::ZoomRecoveryHookAllocation { index: 1_010 }),
        };
        let final_panes = [zoomed_pane(17, true, true)];
        let result = shared.commit_ready_at_epoch_result(expected_epoch, |state| {
            // The ownership transfer is inside the same transaction as the
            // final readback and Ready publication.
            publish_strict_zoom_ownership(state, Some(candidate), &final_panes, 17);
            Ok(())
        });
        assert!(result.is_ok());
        let state = shared
            .session
            .lock()
            .expect("strict ownership committed state");
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, Some(17));
        drop(state);
        assert_eq!(
            shared.info.lock().expect("strict ownership info").state,
            ConnectionState::Ready
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn recovered_meeterm_zoom_ownership_survives_same_pane_selection() {
        let mut state = SessionState::default();
        let candidate = PendingZoomCleanup {
            pane: 17,
            hooks: Some(tmux::ZoomRecoveryHookAllocation { index: 1_011 }),
        };
        let final_panes = [zoomed_pane(17, true, true)];
        publish_strict_zoom_ownership(&mut state, Some(candidate), &final_panes, 17);
        assert_eq!(state.meeterm_zoomed_pane, Some(17));

        // A same-pane reselect restores the owned layout and applies the
        // meeterm zoom again before its normal publish block. That block must
        // retain shared ownership rather than treating the zoom as desktop
        // owned.
        let previous = state.meeterm_zoomed_pane;
        assert_eq!(previous, Some(17));
        publish_selection_state(&mut state, 17, false);
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, previous);
    }

    #[test]
    fn strict_recovery_keeps_preexisting_desktop_zoom_unowned() {
        let mut state = SessionState::default();
        let final_panes = [zoomed_pane(17, true, true)];

        // Fresh/manual and recovery paths pass no candidate when the final
        // readback observes a zoom that was already present on the desktop.
        publish_strict_zoom_ownership(&mut state, None, &final_panes, 17);
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
    }

    #[test]
    fn stale_strict_commit_keeps_actor_intent_without_shared_zoom_ownership() {
        let owner = registry::create_terminal(80, 24).expect("stale strict owner terminal");
        let generation = 77_012;
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "tmux-stale-strict.example".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/tmux-stale-strict-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("stale strict session")
            .generation = generation;
        let expected_epoch = shared
            .begin_recovery("strict_stale", 1)
            .expect("stale strict recovery epoch");
        let candidate = PendingZoomCleanup {
            pane: 17,
            hooks: Some(tmux::ZoomRecoveryHookAllocation { index: 1_012 }),
        };
        let actor_intent = pending_zoom_cleanup_before_send(None, Some(candidate));
        shared.invalidate_explicitly("explicit_disconnect");
        let final_panes = [zoomed_pane(17, true, true)];
        let result = shared.commit_ready_at_epoch_result(expected_epoch, |state| {
            publish_strict_zoom_ownership(state, actor_intent, &final_panes, 17);
            Ok(())
        });
        assert!(matches!(result, Err(FlowFailure::Stale)));
        let state = shared.session.lock().expect("stale strict committed state");
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
        drop(state);
        assert_eq!(actor_intent, Some(candidate));
        assert!(controller_loop_cleanup_needed(&shared, false));
        registry::destroy_terminal(owner);
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
