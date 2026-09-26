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

#[cfg(test)]
struct ScriptedCleanupResponse {
    blocks: Vec<Vec<Vec<u8>>>,
}

#[cfg(test)]
struct ScriptedCleanupTransport {
    responses: VecDeque<ScriptedCleanupResponse>,
    sent: Vec<Vec<u8>>,
}

#[cfg(test)]
impl ScriptedCleanupTransport {
    fn new(responses: Vec<ScriptedCleanupResponse>) -> Self {
        Self {
            responses: responses.into(),
            sent: Vec::new(),
        }
    }

    fn send(&mut self, request: Vec<u8>) -> Option<Vec<u8>> {
        self.sent.push(request.clone());
        let marker_prefix = b"display-message -p '";
        let marker_start = request
            .windows(marker_prefix.len())
            .rposition(|window| window == marker_prefix)?
            + marker_prefix.len();
        let marker_end = request.len().checked_sub(2)?;
        if marker_start >= marker_end || request.get(marker_end..request.len()) != Some(b"'\n") {
            return None;
        }
        let marker = &request[marker_start..marker_end];
        let response = self.responses.pop_front()?;
        let mut bytes = Vec::new();
        for block in response.blocks {
            bytes.extend_from_slice(b"%begin 0 0 0\n");
            for line in block {
                bytes.extend_from_slice(&line);
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(b"%end 0 0 0\n");
        }
        bytes.extend_from_slice(b"%begin 0 0 0\n");
        bytes.extend_from_slice(marker);
        bytes.extend_from_slice(b"\n%end 0 0 0\n");
        Some(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingZoomCleanup {
    window: u64,
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
    previous_owned_window: Option<u64>,
) -> SelectionObservation {
    SelectionObservation {
        window_id: pane.window_id,
        pane_id: pane.pane_id,
        window_zoomed: pane.zoomed,
        previous_owned_same_window: previous_owned_window == Some(pane.window_id)
            && panes
                .iter()
                .any(|candidate| candidate.window_id == pane.window_id),
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

fn zoom_cleanup_hook_allocation(
    cleanup: Option<PendingZoomCleanup>,
    actor_hooks: Option<tmux::ZoomRecoveryHookAllocation>,
) -> Option<tmux::ZoomRecoveryHookAllocation> {
    cleanup.and_then(|target| target.hooks).or(actor_hooks)
}

fn classify_zoom_cleanup_readback(
    mutation_confirmed: bool,
    after: Option<&[tmux::PaneInfo]>,
    window: u64,
    hooks_restored: bool,
) -> ZoomCleanupOutcome {
    if !mutation_confirmed || !hooks_restored {
        return ZoomCleanupOutcome::UnconfirmedOrFailed;
    }
    let Some(panes) = after else {
        return ZoomCleanupOutcome::UnconfirmedOrFailed;
    };
    if panes
        .iter()
        .any(|pane| pane.window_id == window && pane.zoomed)
    {
        return ZoomCleanupOutcome::UnconfirmedOrFailed;
    }
    if panes.iter().any(|pane| pane.window_id == window) {
        ZoomCleanupOutcome::RestoredConfirmed
    } else {
        ZoomCleanupOutcome::NotNeeded
    }
}

fn shared_zoom_cleanup_target(
    state: &SessionState,
    hooks: Option<tmux::ZoomRecoveryHookAllocation>,
) -> Option<PendingZoomCleanup> {
    if !state.meeterm_zoomed {
        return None;
    }
    let pane = state.meeterm_zoomed_pane?;
    let window = state.meeterm_zoomed_window.or_else(|| {
        state
            .snapshot
            .panes
            .iter()
            .find(|candidate| candidate.pane_id == pane)
            .map(|candidate| candidate.window_id)
    })?;
    Some(PendingZoomCleanup {
        window,
        pane,
        hooks,
    })
}

/// A strict recovery selection is only transferable to shared ownership when
/// the candidate came from `select_without_publish` and the final readback
/// still shows that exact pane's window zoomed. A pre-existing desktop zoom
/// has no candidate and therefore cannot cross this boundary.
fn strict_zoom_candidate_matches(
    candidate: PendingZoomCleanup,
    panes: &[tmux::PaneInfo],
    selected: u64,
) -> bool {
    candidate.pane == selected
        && panes.iter().any(|pane| {
            pane.pane_id == selected && pane.window_id == candidate.window && pane.zoomed
        })
}

/// Validate the strict recovery candidate before any staged frame/topology
/// becomes public.  A candidate is either still the exact pane/window zoom,
/// or it is authoritatively unnecessary because that original window now has
/// exactly one unzoomed pane.  Every other relation is an identity failure;
/// in particular, the same pane ID in another window is never substituted.
fn strict_zoom_commit_decision(
    candidate: Option<PendingZoomCleanup>,
    panes: &[tmux::PaneInfo],
    selected: u64,
) -> Result<Option<PendingZoomCleanup>, FlowFailure> {
    let Some(candidate) = candidate else {
        // No meeterm candidate means this is the normal desktop-owned zoom
        // path (or an ordinary unzoomed selection), not a claim of ownership.
        return Ok(None);
    };
    let selected_pane = panes
        .iter()
        .find(|pane| pane.pane_id == selected)
        .ok_or(FlowFailure::TmuxRuntimeMissing)?;
    if selected != candidate.pane || selected_pane.window_id != candidate.window {
        return Err(FlowFailure::TmuxRuntimeMissing);
    }
    if selected_pane.zoomed {
        return Ok(Some(candidate));
    }
    let pane_count = panes
        .iter()
        .filter(|pane| pane.window_id == candidate.window)
        .count();
    if pane_count == 1 {
        // The correct origin pane/window is proven, but zoom is unnecessary
        // for a one-pane window. The caller must still reconcile the saved
        // hooks before committing Ready.
        Ok(None)
    } else {
        Err(FlowFailure::TmuxRuntimeMissing)
    }
}

fn publish_strict_zoom_ownership(
    state: &mut SessionState,
    candidate: Option<PendingZoomCleanup>,
    panes: &[tmux::PaneInfo],
    selected: u64,
) {
    let Some(candidate) =
        candidate.filter(|candidate| strict_zoom_candidate_matches(*candidate, panes, selected))
    else {
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_window = None;
        state.meeterm_zoomed_pane = None;
        return;
    };
    state.meeterm_zoomed = true;
    state.meeterm_zoomed_window = Some(candidate.window);
    state.meeterm_zoomed_pane = Some(candidate.pane);
}

fn publish_selection_state(state: &mut SessionState, window: u64, pane: u64, zoomed: bool) {
    state.selected_pane = Some(pane);
    state.meeterm_zoomed = !zoomed;
    state.meeterm_zoomed_window = (!zoomed).then_some(window);
    state.meeterm_zoomed_pane = (!zoomed).then_some(pane);
    mark_selected(&mut state.snapshot, pane);
}

struct ControlClient {
    shared: Arc<ConnectionShared>,
    session: String,
    runtime_identity: tmux::SessionEpoch,
    reader: Option<russh::ChannelReadHalf>,
    writer: Option<russh::ChannelWriteHalf<client::Msg>>,
    #[cfg(test)]
    cleanup_transport: Option<ScriptedCleanupTransport>,
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
        runtime_identity: selected_identity.epoch(),
        reader: Some(reader),
        writer: Some(writer),
        #[cfg(test)]
        cleanup_transport: None,
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
        client
            .writer
            .as_mut()
            .ok_or(FlowFailure::Stale)?
            .exec(true, startup.into_bytes()),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
        )
        .await?;
    // An SSH request success and tmux's startup block are separate boundaries.
    loop {
        match await_channel_message(
            shared,
            client.reader.as_mut().ok_or(FlowFailure::Stale)?,
        )
        .await?
        {
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
    client.inherit_zoom_cleanup_record().await?;
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
                        expire_attachment_request(&request.command, AttachmentBlock::StaleOperation);
                        continue;
                    }
                    let command = request.command;
                    if !matches!(&command, ControlCommand::SetTerminalVisible { visible: false })
                        && !shared.current_request_is_ready(request.epoch)
                    {
                        expire_attachment_request(&command, AttachmentBlock::NotReady);
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
                    Some(ControlCommand::SftpUpload { attachment_id }) => {
                        launch_sftp_job(shared, session, attachment_id, SftpJob::Upload).await;
                    }
                    Some(ControlCommand::SftpRemove { attachment_id }) => {
                        launch_sftp_job(shared, session, attachment_id, SftpJob::Remove).await;
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
                message = wait_channel_message(
                    shared,
                    client.reader.as_mut().ok_or(FlowFailure::Stale)?,
                ) => {
                    client.channel_message(message?).await?;
                }
            }
        }
    }
    .await;
    let cleanup_outcome = if controller_loop_cleanup_needed(shared, client.remote_session_closed) {
        client.restore_zoom().await
    } else if shared.explicit_cleanup_requested() && client.has_zoom_cleanup_intent() {
        // The stream closed or was invalidated before a same-stream topology
        // acknowledgement could be obtained. Without an authoritative
        // vanished-target readback this is deliberately not reported as
        // NotNeeded.
        ZoomCleanupOutcome::UnconfirmedOrFailed
    } else {
        ZoomCleanupOutcome::NotNeeded
    };
    shared.record_zoom_cleanup(cleanup_outcome);
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
    fn has_zoom_cleanup_intent(&self) -> bool {
        self.pending_zoom_cleanup.is_some()
            || self.pending_zoom_ownership.is_some()
            || self.zoom_hooks.is_some()
            || self.shared.has_zoom_cleanup_intent()
    }

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
            let message = await_channel_message(
                &self.shared,
                self.reader.as_mut().ok_or(FlowFailure::Stale)?,
            )
            .await?;
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
            let message = self.reader.as_mut().ok_or(FlowFailure::Stale)?.wait().await;
            self.channel_message(message).await?;
        }
    }

    async fn send_cleanup_request(&mut self, request: Vec<u8>) -> Result<(), FlowFailure> {
        #[cfg(test)]
        if self.cleanup_transport.is_some() {
            let response = self
                .cleanup_transport
                .as_mut()
                .and_then(|transport| transport.send(request));
            let response = response.ok_or(FlowFailure::Transport)?;
            self.decode(&response)?;
            return Ok(());
        }

        tokio::time::timeout(
            EXPLICIT_CLEANUP_TIMEOUT,
            self.writer
                .as_mut()
                .ok_or(FlowFailure::Stale)?
                .data_bytes(request),
        )
        .await
        .map_err(|_| FlowFailure::Tmux)?
        .map_err(|_| FlowFailure::Transport)
    }

    /// Send a bounded cleanup/topology query on this exact Control Mode
    /// stream. Normal request epochs are revoked by explicit shutdown, so the
    /// cleanup boundary intentionally uses only the generation check and the
    /// same-stream marker acknowledgement.
    async fn cleanup_query(
        &mut self,
        command: &str,
    ) -> Result<Vec<tmux::CommandBlock>, FlowFailure> {
        if self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .generation
            != self.shared.generation
        {
            return Err(FlowFailure::Stale);
        }
        self.command_number = self.command_number.saturating_add(1);
        let marker = format!(
            "MEETERM_CLEANUP_DONE_{}_{}",
            self.shared.generation, self.command_number
        );
        let request = format!("{command} ; display-message -p '{marker}'\n");
        self.send_cleanup_request(request.into_bytes()).await?;

        let deadline = tokio::time::Instant::now() + EXPLICIT_CLEANUP_TIMEOUT;
        let mut blocks = Vec::new();
        let mut reply_bytes = 0usize;
        loop {
            let event = tokio::time::timeout_at(deadline, self.next_event_for_cleanup())
                .await
                .map_err(|_| FlowFailure::Tmux)??;
            match event {
                tmux::Event::Command(block)
                    if block.lines.len() == 1 && block.lines[0] == marker.as_bytes() =>
                {
                    return Ok(blocks);
                }
                tmux::Event::Command(block) if block.error => return Err(FlowFailure::Tmux),
                tmux::Event::Command(block) => {
                    reply_bytes =
                        reply_bytes.saturating_add(block.lines.iter().map(Vec::len).sum::<usize>());
                    if reply_bytes > 32 * 1024 * 1024 || blocks.len() >= 4096 {
                        return Err(FlowFailure::TmuxProtocol);
                    }
                    blocks.push(block);
                }
                tmux::Event::Output { .. } | tmux::Event::Notification { .. } => {}
            }
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
            if let Some(hooks) = intent.hooks
                && !self.shared.record_zoom_cleanup_intent(
                    self.runtime_identity.clone(),
                    intent.window,
                    intent.pane,
                    hooks,
                )
            {
                // A different allocation or runtime is already retained by
                // the shared lifecycle. Never overwrite it from an actor
                // that may be finishing late after a transport loss.
                return Err(FlowFailure::TmuxRuntimeMissing);
            }
            // This is deliberately opt-in. The selection path passes an intent
            // only after proving that the target window was not pre-zoomed;
            // the pre-existing desktop-zoom path calls `query` with `None`.
            self.pending_zoom_cleanup =
                pending_zoom_cleanup_before_send(self.pending_zoom_cleanup, Some(intent));
            self.shared.mark_zoom_cleanup_pending();
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
            self.writer
                .as_mut()
                .ok_or(FlowFailure::Stale)?
                .data_bytes(request.into_bytes()),
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
            self.writer
                .as_mut()
                .ok_or(FlowFailure::Stale)?
                .data_bytes(request.into_bytes()),
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
        let durable_record = self
            .shared
            .zoom_cleanup_record_for(&self.runtime_identity)?;
        let previous = {
            let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
            durable_record
                .as_ref()
                .map(|record| PendingZoomCleanup {
                    window: record.window,
                    pane: record.pane,
                    hooks: Some(record.hooks),
                })
                .or_else(|| {
                    state
                        .meeterm_zoomed
                        .then(|| {
                            state.meeterm_zoomed_window.or_else(|| {
                                state.meeterm_zoomed_pane.and_then(|owned| {
                                    state
                                        .snapshot
                                        .panes
                                        .iter()
                                        .find(|candidate| candidate.pane_id == owned)
                                        .map(|candidate| candidate.window_id)
                                })
                            })
                        })
                        .flatten()
                        .and_then(|owned_window| {
                            state
                                .meeterm_zoomed_pane
                                .map(|owned_pane| PendingZoomCleanup {
                                    window: owned_window,
                                    pane: owned_pane,
                                    hooks: self.zoom_hooks,
                                })
                        })
                })
        };
        // Return an earlier meeterm-owned window to its ordinary layout before
        // inspecting the new target.  This ordering matters when the target
        // is the same pane: the zoom we just remove must not be mistaken for
        // a desktop zoom that meeterm should preserve.
        if let Some(record) = durable_record.as_ref() {
            // A durable record is authoritative over the actor's stale pane
            // hint. Reconcile the owned window and exact hook slots before
            // acquiring any new indexed allocation.
            self.reconcile_zoom_cleanup_record(record).await?;
        } else if let Some(previous) = previous {
            let restore_target = {
                let state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
                state
                    .snapshot
                    .windows
                    .iter()
                    .any(|candidate| candidate.window_id == previous.window)
                    .then(|| {
                        tmux::restore_layout_command_for_session_window(
                            &self.session,
                            previous.window,
                        )
                        .map_err(|_| FlowFailure::TmuxProtocol)
                    })
                    .transpose()?
            };
            if let Some(command) = restore_target {
                self.query_with_zoom_cleanup(&command, Some(previous))
                    .await?;
            }
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
            let previous_same_window = previous.is_some_and(|previous| previous.window == window);
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
                    window,
                )
                .map_err(|_| FlowFailure::TmuxProtocol)?,
                tmux::select_pane_command_for_session(&self.session, None, window, pane)
                    .map_err(|_| FlowFailure::TmuxProtocol)?,
            ]
            .join(" ; ");
            let ownership = PendingZoomCleanup {
                window,
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
            publish_selection_state(&mut state, window, pane, zoomed);
            self.pending_zoom_cleanup = None;
            self.pending_zoom_ownership = None;
            self.shared.clear_zoom_cleanup_pending();
        }
        Ok(())
    }

    async fn zoom_recovery_hooks_absent(
        &mut self,
        allocation: tmux::ZoomRecoveryHookAllocation,
    ) -> Result<bool, FlowFailure> {
        let target =
            tmux::session_target_for_hooks(&self.session).map_err(|_| FlowFailure::TmuxProtocol)?;
        let blocks = self
            .cleanup_query(&format!("show-hooks -t {target}:"))
            .await?;
        let bytes = blocks
            .into_iter()
            .flat_map(|block| block.lines)
            .collect::<Vec<_>>()
            .join(&b'\n');
        tmux::zoom_recovery_hooks_absent(&bytes, allocation).map_err(|_| FlowFailure::TmuxProtocol)
    }

    async fn zoom_recovery_hooks_state(
        &mut self,
        allocation: tmux::ZoomRecoveryHookAllocation,
        window: u64,
    ) -> Result<tmux::ZoomRecoveryHookState, FlowFailure> {
        let target =
            tmux::session_target_for_hooks(&self.session).map_err(|_| FlowFailure::TmuxProtocol)?;
        let blocks = self
            .cleanup_query(&format!("show-hooks -t {target}:"))
            .await?;
        let bytes = blocks
            .into_iter()
            .flat_map(|block| block.lines)
            .collect::<Vec<_>>()
            .join(&b'\n');
        tmux::zoom_recovery_hooks_state(&bytes, allocation, &self.session, window)
            .map_err(|_| FlowFailure::TmuxProtocol)
    }

    /// Return the only command that is safe to send after hook classification.
    /// `Replaced` deliberately returns before command construction so the
    /// release path cannot accidentally unset a third-party slot.
    fn release_record_hooks_command(
        hook_state: tmux::ZoomRecoveryHookState,
        session: &str,
        allocation: tmux::ZoomRecoveryHookAllocation,
    ) -> Result<Option<String>, FlowFailure> {
        match hook_state {
            tmux::ZoomRecoveryHookState::Absent => Ok(None),
            tmux::ZoomRecoveryHookState::Owned => {
                tmux::remove_zoom_recovery_hooks_command_for_session(session, allocation)
                    .map(Some)
                    .map_err(|_| FlowFailure::TmuxProtocol)
            }
            // Never remove a slot whose body no longer proves it is ours.
            tmux::ZoomRecoveryHookState::Replaced => Err(FlowFailure::TmuxRuntimeMissing),
        }
    }

    async fn cleanup_topology(&mut self) -> Result<Vec<tmux::PaneInfo>, FlowFailure> {
        let command = tmux::list_panes_command_for_session(&self.session)
            .map_err(|_| FlowFailure::TmuxProtocol)?;
        let blocks = self.cleanup_query(&command).await?;
        parse_cleanup_panes(&blocks)
    }

    async fn release_record_hooks(
        &mut self,
        record: &ZoomCleanupRecord,
    ) -> Result<(), FlowFailure> {
        let hook_state = self
            .zoom_recovery_hooks_state(record.hooks, record.window)
            .await?;
        let Some(remove) =
            Self::release_record_hooks_command(hook_state, &self.session, record.hooks)?
        else {
            return Ok(());
        };
        self.cleanup_query(&remove).await?;
        if self.zoom_recovery_hooks_absent(record.hooks).await? {
            Ok(())
        } else {
            Err(FlowFailure::TmuxRuntimeMissing)
        }
    }

    /// Reconcile the durable record on the verified Control Mode stream. This
    /// is used both when a replacement actor inherits a record and when a
    /// same actor releases an older owned window before selecting a new one.
    /// It never follows a moved pane into another window.
    async fn reconcile_zoom_cleanup_record(
        &mut self,
        record: &ZoomCleanupRecord,
    ) -> Result<ZoomCleanupOutcome, FlowFailure> {
        let before = self.cleanup_topology().await?;
        let window_present = before.iter().any(|pane| pane.window_id == record.window);
        let window_zoomed = before
            .iter()
            .any(|pane| pane.window_id == record.window && pane.zoomed);
        if window_zoomed {
            let restore =
                tmux::restore_layout_command_for_session_window(&self.session, record.window)
                    .map_err(|_| FlowFailure::TmuxProtocol)?;
            self.cleanup_query(&restore).await?;
        }
        let after = self.cleanup_topology().await?;
        if after
            .iter()
            .any(|pane| pane.window_id == record.window && pane.zoomed)
        {
            return Err(FlowFailure::TmuxRuntimeMissing);
        }
        self.release_record_hooks(record).await?;
        if !self
            .shared
            .clear_zoom_cleanup_record(&record.runtime, record.window, record.hooks)
        {
            return Err(FlowFailure::Stale);
        }
        self.zoom_hooks = None;
        self.pending_zoom_cleanup = None;
        self.pending_zoom_ownership = None;
        self.shared.clear_owned_zoom();
        Ok(if window_present && window_zoomed {
            ZoomCleanupOutcome::RestoredConfirmed
        } else {
            ZoomCleanupOutcome::NotNeeded
        })
    }

    async fn inherit_zoom_cleanup_record(&mut self) -> Result<(), FlowFailure> {
        let Some(record) = self
            .shared
            .zoom_cleanup_record_for(&self.runtime_identity)?
        else {
            return Ok(());
        };
        match self
            .zoom_recovery_hooks_state(record.hooks, record.window)
            .await?
        {
            tmux::ZoomRecoveryHookState::Replaced => {
                // The runtime is verified, but the saved slot is now owned by
                // someone else. Keep the record and fail closed before Ready.
                Err(FlowFailure::TmuxRuntimeMissing)
            }
            tmux::ZoomRecoveryHookState::Absent => self
                .reconcile_zoom_cleanup_record(&record)
                .await
                .map(|_| ()),
            tmux::ZoomRecoveryHookState::Owned => {
                let topology = self.cleanup_topology().await?;
                if topology.iter().any(|pane| pane.window_id == record.window) {
                    self.zoom_hooks = Some(record.hooks);
                    self.pending_zoom_cleanup = Some(PendingZoomCleanup {
                        window: record.window,
                        pane: record.pane,
                        hooks: Some(record.hooks),
                    });
                    if let Ok(mut state) = self.shared.session.lock()
                        && state.generation == self.shared.generation
                    {
                        state.meeterm_zoomed = true;
                        state.meeterm_zoomed_window = Some(record.window);
                        state.meeterm_zoomed_pane = Some(record.pane);
                    }
                    Ok(())
                } else {
                    // The owned window vanished; only the exact saved hooks
                    // may be removed, then the durable record can be cleared.
                    self.reconcile_zoom_cleanup_record(&record)
                        .await
                        .map(|_| ())
                }
            }
        }
    }

    async fn remove_zoom_recovery_hooks_only(
        &mut self,
        allocation: tmux::ZoomRecoveryHookAllocation,
        window: u64,
    ) -> ZoomCleanupOutcome {
        match self.zoom_recovery_hooks_state(allocation, window).await {
            Ok(tmux::ZoomRecoveryHookState::Absent) => {
                self.zoom_hooks = None;
                return ZoomCleanupOutcome::NotNeeded;
            }
            // Never remove a slot whose body no longer proves it is ours.
            Ok(tmux::ZoomRecoveryHookState::Replaced) | Err(_) => {
                return ZoomCleanupOutcome::UnconfirmedOrFailed;
            }
            Ok(tmux::ZoomRecoveryHookState::Owned) => {}
        }
        let remove =
            match tmux::remove_zoom_recovery_hooks_command_for_session(&self.session, allocation) {
                Ok(command) => command,
                Err(_) => return ZoomCleanupOutcome::UnconfirmedOrFailed,
            };
        if self.cleanup_query(&remove).await.is_err() {
            return ZoomCleanupOutcome::UnconfirmedOrFailed;
        }
        match self.zoom_recovery_hooks_absent(allocation).await {
            Ok(true) => {
                self.zoom_hooks = None;
                ZoomCleanupOutcome::NotNeeded
            }
            Ok(false) | Err(_) => ZoomCleanupOutcome::UnconfirmedOrFailed,
        }
    }

    async fn restore_zoom(&mut self) -> ZoomCleanupOutcome {
        let durable_record = match self.shared.zoom_cleanup_record_for(&self.runtime_identity) {
            Ok(record) => record,
            Err(_) => {
                // A mismatched endpoint/runtime must not receive cleanup
                // commands. Keep the record for an explicit same-runtime
                // retry and fail closed.
                self.shared
                    .record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
                return ZoomCleanupOutcome::UnconfirmedOrFailed;
            }
        };
        if let Some(record) = durable_record.as_ref() {
            return match self.reconcile_zoom_cleanup_record(record).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    self.shared
                        .record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
                    ZoomCleanupOutcome::UnconfirmedOrFailed
                }
            };
        }
        let shared_owned = self.shared.session.lock().ok().and_then(|state| {
            (state.generation == self.shared.generation)
                .then(|| shared_zoom_cleanup_target(&state, self.zoom_hooks))
                .flatten()
        });
        let cleanup = select_zoom_cleanup_target(self.pending_zoom_cleanup.take(), shared_owned);
        let hooks = zoom_cleanup_hook_allocation(cleanup, self.zoom_hooks);
        if let Some(allocation) = hooks {
            // A pending target may carry the only copy of the allocation
            // after another lifecycle path cleared actor state. Rehydrate
            // actor-local authority before any awaited cleanup operation so
            // an unconfirmed result remains retryable.
            self.zoom_hooks = Some(allocation);
        }
        let Some(cleanup) = cleanup else {
            let outcome = if let Some(allocation) = hooks {
                // Ownership may already have been cleared after a closed
                // window or a rejected strict candidate. The hook allocation
                // remains actor-private cleanup authority, but no layout
                // target is valid anymore.
                let window = self
                    .shared
                    .session
                    .lock()
                    .ok()
                    .and_then(|state| state.meeterm_zoomed_window);
                if let Some(window) = window {
                    self.remove_zoom_recovery_hooks_only(allocation, window)
                        .await
                } else {
                    // An indexed slot cannot be classified as ours without
                    // the saved window target. Preserve it for a later
                    // same-runtime reconciliation instead of deleting a
                    // possible third-party replacement.
                    ZoomCleanupOutcome::UnconfirmedOrFailed
                }
            } else {
                ZoomCleanupOutcome::NotNeeded
            };
            if !matches!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed) {
                self.shared.clear_zoom_cleanup_pending();
            }
            return outcome;
        };

        // Classify the exact saved slots before any layout mutation. A slot
        // with a different body is third-party state and is never removed.
        let hook_state = if let Some(allocation) = hooks {
            match self
                .zoom_recovery_hooks_state(allocation, cleanup.window)
                .await
            {
                Ok(state @ tmux::ZoomRecoveryHookState::Absent)
                | Ok(state @ tmux::ZoomRecoveryHookState::Owned) => Some(state),
                Ok(tmux::ZoomRecoveryHookState::Replaced) | Err(_) => {
                    self.shared
                        .record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
                    return ZoomCleanupOutcome::UnconfirmedOrFailed;
                }
            }
        } else {
            None
        };

        // A target that disappeared is only considered NotNeeded after a
        // fresh topology read on this same Control Mode stream. Its indexed
        // hooks still need removal when they are present.
        let topology_command = match tmux::list_panes_command_for_session(&self.session) {
            Ok(command) => command,
            Err(_) => {
                return ZoomCleanupOutcome::UnconfirmedOrFailed;
            }
        };
        let before = match self.cleanup_query(&topology_command).await {
            Ok(blocks) => match parse_cleanup_panes(&blocks) {
                Ok(panes) => panes,
                Err(_) => return ZoomCleanupOutcome::UnconfirmedOrFailed,
            },
            Err(_) => return ZoomCleanupOutcome::UnconfirmedOrFailed,
        };
        let window_present = before.iter().any(|pane| pane.window_id == cleanup.window);

        if !window_present {
            // The entire owned window is gone. Remove only our exact indexed
            // hooks; no layout mutation has a valid target left.
            let outcome = if let Some(allocation) = hooks {
                self.remove_zoom_recovery_hooks_only(allocation, cleanup.window)
                    .await
            } else {
                ZoomCleanupOutcome::NotNeeded
            };
            if !matches!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed) {
                self.shared.clear_owned_zoom();
            }
            return outcome;
        }

        // The pane auxiliary may have vanished or moved windows. The owned
        // window remains the only safe restore target.
        let restore =
            match tmux::restore_layout_command_for_session_window(&self.session, cleanup.window) {
                Ok(command) => command,
                Err(_) => return ZoomCleanupOutcome::UnconfirmedOrFailed,
            };
        let mutation_confirmed = self.cleanup_query(&restore).await.is_ok();
        let after = if mutation_confirmed {
            self.cleanup_query(&topology_command)
                .await
                .ok()
                .and_then(|blocks| parse_cleanup_panes(&blocks).ok())
        } else {
            None
        };
        let hooks_restored = match (hooks, hook_state) {
            (Some(allocation), Some(tmux::ZoomRecoveryHookState::Owned)) if mutation_confirmed => {
                self.remove_zoom_recovery_hooks_only(allocation, cleanup.window)
                    .await
                    != ZoomCleanupOutcome::UnconfirmedOrFailed
            }
            (Some(_), Some(tmux::ZoomRecoveryHookState::Absent)) | (None, None) => true,
            _ => false,
        };
        let outcome = classify_zoom_cleanup_readback(
            mutation_confirmed,
            after.as_deref(),
            cleanup.window,
            hooks_restored,
        );
        self.shared.clear_owned_zoom();
        if !matches!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed) {
            self.zoom_hooks = None;
        }
        outcome
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
            let previous_owned_window = self
                .shared
                .session
                .lock()
                .map_err(|_| FlowFailure::Stale)?
                .meeterm_zoomed_window;
            Some(fresh_selection_observation(
                pane,
                &panes,
                previous_owned_window,
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
            let strict_candidate = self.pending_zoom_ownership;
            if matches!(
                strict_zoom_commit_decision(strict_candidate, &panes, selected),
                Ok(None) if strict_candidate.is_some()
            ) {
                // The final readback proves the original window/pane is
                // correct but has only one pane, so the zoom mutation is no
                // longer needed. Reconcile its exact hooks before the same
                // epoch commit; otherwise Ready could publish while an old
                // allocation remains remotely active.
                let outcome = match self
                    .shared
                    .zoom_cleanup_record_for(&self.runtime_identity)?
                {
                    Some(record) => self.reconcile_zoom_cleanup_record(&record).await?,
                    None => self.restore_zoom().await,
                };
                if matches!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed) {
                    return Err(FlowFailure::TmuxRuntimeMissing);
                }
                self.pending_zoom_ownership = None;
            }
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
                    let strict_zoom_ownership =
                        strict_zoom_commit_decision(strict_zoom_ownership, &panes, selected)?;
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
                if matches!(failure, FlowFailure::TmuxRuntimeMissing)
                    && self.strict_recovery
                    && !self.remote_session_closed
                    && self.has_zoom_cleanup_intent()
                {
                    // The stream is still verified and live at this point.
                    // Give the actor one bounded same-stream cleanup attempt,
                    // while retaining the durable record if its result is
                    // unknown. The public Ready/topology commit above has
                    // not run.
                    let outcome = self.restore_zoom().await;
                    self.shared.record_zoom_cleanup(outcome);
                }
                return Err(failure);
            }
            if let Some(candidate) = strict_zoom_ownership
                && strict_zoom_candidate_matches(candidate, &panes, selected)
                && let Some(hooks) = candidate.hooks
            {
                self.shared.confirm_zoom_cleanup_intent(
                    &self.runtime_identity,
                    candidate.window,
                    candidate.pane,
                    hooks,
                );
            }
            // Shared ownership is now authoritative. Only after the commit
            // succeeds may the actor drop its duplicate cleanup source.
            shared.clear_zoom_cleanup_pending();
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
        // The first non-strict synchronization carries the topology read
        // from before `select_with_observation` applied the initial zoom.
        // Preserve the ownership just published by that selection; the dirty
        // follow-up synchronization is the first authoritative post-mutation
        // readback and will validate the zoomed window normally.
        if initial {
            self.commit_initial_topology(
                &mapping,
                &windows,
                &panes,
                &flat,
                selected,
                expected_epoch,
            )?;
        } else {
            self.commit_topology(&mapping, &windows, &panes, &flat, selected, expected_epoch)?;
        }
        if let Ok(Some(record)) = self.shared.zoom_cleanup_record_for(&self.runtime_identity)
            && record.pane == selected
            && panes.iter().any(|pane| {
                pane.pane_id == selected && pane.window_id == record.window && pane.zoomed
            })
        {
            // Only an authoritative post-mutation topology read confirms
            // that the saved zoom/slot pair is now active. The initial
            // pre-mutation snapshot cannot do so.
            if !initial {
                self.shared.confirm_zoom_cleanup_intent(
                    &self.runtime_identity,
                    record.window,
                    record.pane,
                    record.hooks,
                );
            }
        }
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

    fn commit_initial_topology(
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
        // The initial snapshot was read before the selection/zoom mutation.
        // Do not validate ownership against that pre-mutation state; the
        // subsequent dirty synchronization performs the authoritative check.
        apply_committed_topology(&mut state, mapping, windows, panes, flat, selected, true);
        Ok(())
    }
}

fn parse_cleanup_panes(blocks: &[tmux::CommandBlock]) -> Result<Vec<tmux::PaneInfo>, FlowFailure> {
    blocks
        .iter()
        .flat_map(|block| block.lines.iter())
        .map(|line| tmux::parse_pane_line(line).map_err(|_| FlowFailure::TmuxProtocol))
        .collect()
}

fn apply_committed_topology(
    state: &mut SessionState,
    mapping: &HashMap<u64, u64>,
    windows: &[tmux::WindowInfo],
    panes: &[tmux::PaneInfo],
    flat: &[PaneSnapshot],
    selected: u64,
    preserve_zoom_ownership: bool,
) {
    if preserve_zoom_ownership {
        apply_topology_snapshot(state, mapping, windows, panes, flat, selected);
    } else {
        apply_topology_state(state, mapping, windows, panes, flat, selected);
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
    if state.meeterm_zoomed {
        let Some(owned_window) = state.meeterm_zoomed_window else {
            state.meeterm_zoomed = false;
            state.meeterm_zoomed_window = None;
            state.meeterm_zoomed_pane = None;
            return apply_topology_snapshot(state, mapping, windows, panes, flat, selected);
        };
        let window_exists = windows
            .iter()
            .any(|window| window.window_id == owned_window);
        let zoomed_panes = panes
            .iter()
            .filter(|pane| pane.window_id == owned_window && pane.zoomed)
            .collect::<Vec<_>>();
        if !window_exists || zoomed_panes.is_empty() {
            state.meeterm_zoomed = false;
            state.meeterm_zoomed_window = None;
            state.meeterm_zoomed_pane = None;
        } else if !zoomed_panes
            .iter()
            .any(|pane| Some(pane.pane_id) == state.meeterm_zoomed_pane)
        {
            // A pane may be replaced or the mobile selection may move inside
            // the same zoomed window. Keep ownership on the authoritative
            // zoomed window and retarget the transient pane identity.
            state.meeterm_zoomed_pane = zoomed_panes
                .iter()
                .find(|pane| pane.pane_id == selected)
                .or_else(|| zoomed_panes.first())
                .map(|pane| pane.pane_id);
        }
    } else {
        state.meeterm_zoomed_window = None;
    }
    apply_topology_snapshot(state, mapping, windows, panes, flat, selected);
}

fn apply_topology_snapshot(
    state: &mut SessionState,
    mapping: &HashMap<u64, u64>,
    windows: &[tmux::WindowInfo],
    panes: &[tmux::PaneInfo],
    flat: &[PaneSnapshot],
    selected: u64,
) {
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

    fn cleanup_reply(blocks: Vec<Vec<Vec<u8>>>) -> ScriptedCleanupResponse {
        ScriptedCleanupResponse { blocks }
    }

    fn cleanup_topology_reply(window: u64, pane_id: u64, zoomed: bool) -> ScriptedCleanupResponse {
        cleanup_reply(vec![vec![
            format!(
                "@{window}\t%{pane_id}\t0\t1\t80\t24\tpane-{pane_id}\t{}\t1",
                u8::from(zoomed)
            )
            .into_bytes(),
        ]])
    }

    fn owned_hook_reply(
        allocation: tmux::ZoomRecoveryHookAllocation,
        window: u64,
    ) -> ScriptedCleanupResponse {
        let body = format!(
            "if-shell -F -t \"=$0:@{window}\" \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t =$0:@{window}\" ; set-hook -u -t \"=$0:\" client-detached[{}] ; set-hook -u -t \"=$0:\" client-session-changed[{}]",
            allocation.index, allocation.index
        );
        cleanup_reply(vec![vec![
            format!("client-detached[{}] {body}", allocation.index).into_bytes(),
            format!("client-session-changed[{}] {body}", allocation.index).into_bytes(),
        ]])
    }

    fn cleanup_client(
        shared: Arc<ConnectionShared>,
        runtime: tmux::SessionEpoch,
        responses: Vec<ScriptedCleanupResponse>,
    ) -> ControlClient {
        let (pane_sender, pane_receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        ControlClient {
            shared,
            session: runtime.session_id.clone(),
            runtime_identity: runtime,
            reader: None,
            writer: None,
            cleanup_transport: Some(ScriptedCleanupTransport::new(responses)),
            decoder: tmux::Decoder::new(),
            events: VecDeque::new(),
            routes: HashMap::new(),
            pane_sender,
            pane_receiver,
            viewport: (80, 24),
            dirty: false,
            capturing: None,
            capture_complete: false,
            capture_output: Vec::new(),
            command_number: 0,
            zoom_hooks: None,
            strict_recovery: false,
            mapping: HashMap::new(),
            staged_native: Vec::new(),
            command_epoch: None,
            operation_epoch: 1,
            staged_captures: Vec::new(),
            strict_sync_pending: false,
            pending_zoom_cleanup: None,
            pending_zoom_ownership: None,
            remote_session_closed: false,
        }
    }

    fn cleanup_shared(
        owner: TerminalId,
        generation: u64,
        runtime: &tmux::SessionEpoch,
        hooks: tmux::ZoomRecoveryHookAllocation,
    ) -> Arc<ConnectionShared> {
        let known_hosts_path =
            std::path::PathBuf::from(format!("/tmp/control-cleanup-{generation}-known-hosts"));
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "control-cleanup.example".to_owned(),
            22,
            known_hosts_path.clone(),
        ));
        {
            let mut state = shared.session.lock().expect("cleanup session");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "control-cleanup.example".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path,
                backend: Backend::Tmux,
                runtime: Some(runtime.session_id.clone()),
            });
        }
        assert!(shared.record_zoom_cleanup_intent(runtime.clone(), 1, 17, hooks));
        shared
    }

    fn run_cleanup(
        mut client: ControlClient,
    ) -> (ZoomCleanupOutcome, Arc<ConnectionShared>, Vec<Vec<u8>>) {
        let shared = Arc::clone(&client.shared);
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("cleanup runtime");
        let outcome = runtime.block_on(async { client.restore_zoom().await });
        let sent = client
            .cleanup_transport
            .take()
            .expect("scripted cleanup transport")
            .sent;
        (outcome, shared, sent)
    }

    #[test]
    fn restore_zoom_replays_real_cleanup_transcript_and_fails_closed_on_replaced_hooks() {
        let owner = registry::create_terminal(80, 24).expect("replaced cleanup terminal");
        let runtime = tmux::SessionEpoch {
            session_id: "$0".to_owned(),
            server_pid: 42,
            server_start_time: 4_200_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_030 };
        let shared = cleanup_shared(owner, 77_030, &runtime, hooks);
        let responses = vec![
            cleanup_topology_reply(1, 17, false),
            cleanup_topology_reply(1, 17, false),
            cleanup_reply(vec![vec![
                format!("client-detached[{}] third-party-body", hooks.index).into_bytes(),
                format!("client-session-changed[{}] third-party-body", hooks.index).into_bytes(),
            ]]),
        ];
        let (outcome, shared, sent) = run_cleanup(cleanup_client(
            Arc::clone(&shared),
            runtime.clone(),
            responses,
        ));
        assert_eq!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed);
        assert_eq!(
            *shared
                .zoom_cleanup_outcome
                .lock()
                .expect("replaced cleanup outcome"),
            ZoomCleanupOutcome::UnconfirmedOrFailed
        );
        assert!(
            sent.iter()
                .any(|request| request.starts_with(b"show-hooks"))
        );
        assert!(!sent.iter().any(|request| {
            request
                .windows(b"set-hook -u".len())
                .any(|window| window == b"set-hook -u")
        }));
        assert!(matches!(
            shared.zoom_cleanup_record_for(&runtime),
            Ok(Some(_))
        ));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn restore_zoom_replays_real_cleanup_transcript_and_fails_closed_on_classifier_error() {
        let owner = registry::create_terminal(80, 24).expect("classifier cleanup terminal");
        let runtime = tmux::SessionEpoch {
            session_id: "$0".to_owned(),
            server_pid: 43,
            server_start_time: 4_300_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_031 };
        let shared = cleanup_shared(owner, 77_031, &runtime, hooks);
        let responses = vec![
            cleanup_topology_reply(1, 17, false),
            cleanup_topology_reply(1, 17, false),
            cleanup_reply(vec![vec![
                format!("client-detached[{} broken", hooks.index).into_bytes(),
            ]]),
        ];
        let (outcome, shared, sent) = run_cleanup(cleanup_client(
            Arc::clone(&shared),
            runtime.clone(),
            responses,
        ));
        assert_eq!(outcome, ZoomCleanupOutcome::UnconfirmedOrFailed);
        assert_eq!(
            *shared
                .zoom_cleanup_outcome
                .lock()
                .expect("classifier cleanup outcome"),
            ZoomCleanupOutcome::UnconfirmedOrFailed
        );
        assert!(
            sent.iter()
                .any(|request| request.starts_with(b"show-hooks"))
        );
        assert!(!sent.iter().any(|request| {
            request
                .windows(b"set-hook -u".len())
                .any(|window| window == b"set-hook -u")
        }));
        assert!(matches!(
            shared.zoom_cleanup_record_for(&runtime),
            Ok(Some(_))
        ));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn restore_zoom_replays_owned_and_absent_cleanup_transcripts() {
        for (index, hook_response, expected_removal) in [(1_032, true, true), (1_033, false, false)]
        {
            let owner = registry::create_terminal(80, 24).expect("positive cleanup terminal");
            let runtime = tmux::SessionEpoch {
                session_id: "$0".to_owned(),
                server_pid: 44 + u64::from(!hook_response),
                server_start_time: 4_400_000_000 + u64::from(!hook_response),
            };
            let hooks = tmux::ZoomRecoveryHookAllocation { index };
            let shared = cleanup_shared(owner, 77_040 + u64::from(!hook_response), &runtime, hooks);
            let mut responses = vec![
                cleanup_topology_reply(1, 17, false),
                cleanup_topology_reply(1, 17, false),
            ];
            responses.push(if hook_response {
                owned_hook_reply(hooks, 1)
            } else {
                cleanup_reply(Vec::new())
            });
            if expected_removal {
                responses.push(cleanup_reply(Vec::new()));
                responses.push(cleanup_reply(Vec::new()));
            }
            let (outcome, shared, sent) = run_cleanup(cleanup_client(
                Arc::clone(&shared),
                runtime.clone(),
                responses,
            ));
            assert_eq!(outcome, ZoomCleanupOutcome::NotNeeded);
            assert_eq!(
                *shared
                    .zoom_cleanup_outcome
                    .lock()
                    .expect("positive cleanup outcome"),
                ZoomCleanupOutcome::NotNeeded
            );
            let removals = sent
                .iter()
                .filter(|request| {
                    request
                        .windows(b"set-hook -u".len())
                        .any(|window| window == b"set-hook -u")
                })
                .count();
            assert_eq!(removals, usize::from(expected_removal));
            if expected_removal {
                let removal = sent
                    .iter()
                    .find(|request| {
                        request
                            .windows(b"set-hook -u".len())
                            .any(|window| window == b"set-hook -u")
                    })
                    .expect("owned cleanup removal request");
                let detached = format!("client-detached[{index}] ");
                let session_changed = format!("client-session-changed[{index}] ");
                assert!(
                    removal
                        .windows(detached.len())
                        .any(|window| window == detached.as_bytes())
                );
                assert!(
                    removal
                        .windows(session_changed.len())
                        .any(|window| window == session_changed.as_bytes())
                );
            }
            assert!(matches!(shared.zoom_cleanup_record_for(&runtime), Ok(None)));
            registry::destroy_terminal(owner);
        }
    }

    #[test]
    fn closed_zoom_target_keeps_actor_hook_cleanup_authority() {
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_013 };
        let target = PendingZoomCleanup {
            window: 1,
            pane: 17,
            hooks: Some(hooks),
        };

        // Once the owned window disappears, shared layout ownership can be
        // gone before the actor's finalizer runs. The actor-local allocation
        // must still select the hook-only cleanup path.
        assert_eq!(zoom_cleanup_hook_allocation(None, Some(hooks)), Some(hooks));
        assert_eq!(
            zoom_cleanup_hook_allocation(Some(target), None),
            Some(hooks)
        );
    }

    #[test]
    fn zoom_cleanup_readback_is_proven_at_owned_window_scope() {
        let still_zoomed = [zoomed_pane(18, true, true)];
        assert_eq!(
            classify_zoom_cleanup_readback(true, Some(&still_zoomed), 1, true),
            ZoomCleanupOutcome::UnconfirmedOrFailed
        );

        let mut window_gone_pane = pane(18, true, true);
        window_gone_pane.window_id = 2;
        assert_eq!(
            classify_zoom_cleanup_readback(true, Some(&[window_gone_pane]), 1, true),
            ZoomCleanupOutcome::NotNeeded
        );

        let surviving_unzoomed = [pane(18, true, true)];
        assert_eq!(
            classify_zoom_cleanup_readback(true, Some(&surviving_unzoomed), 1, true),
            ZoomCleanupOutcome::RestoredConfirmed
        );
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
        publish_selection_state(&mut state, 1, 17, true);
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
    }

    #[test]
    fn initial_selection_commit_defers_zoom_ownership_validation() {
        let initial_readback = pane(17, true, true);
        let flat = PaneSnapshot {
            window_id: initial_readback.window_id,
            pane_id: initial_readback.pane_id,
            terminal_id: 99,
            window_name: "window".to_owned(),
            active: initial_readback.active,
            selected: true,
            index: initial_readback.index,
            columns: initial_readback.columns,
            rows: initial_readback.rows,
            pane_name: initial_readback.pane_name.clone(),
            title: initial_readback.title.clone(),
        };
        let mut mapping = HashMap::new();
        mapping.insert(initial_readback.pane_id, flat.terminal_id);
        let windows = [tmux::WindowInfo {
            window_id: initial_readback.window_id,
            name: "window".to_owned(),
        }];
        let mut state = SessionState {
            meeterm_zoomed: true,
            meeterm_zoomed_window: Some(initial_readback.window_id),
            meeterm_zoomed_pane: Some(initial_readback.pane_id),
            ..SessionState::default()
        };

        // This is the topology read taken before the initial selection's
        // remote zoom mutation. It must not erase the ownership published by
        // that mutation before the dirty post-selection readback arrives.
        apply_committed_topology(
            &mut state,
            &mapping,
            &windows,
            std::slice::from_ref(&initial_readback),
            std::slice::from_ref(&flat),
            initial_readback.pane_id,
            true,
        );
        assert!(state.meeterm_zoomed);
        assert_eq!(
            state.meeterm_zoomed_window,
            Some(initial_readback.window_id)
        );
        assert_eq!(state.meeterm_zoomed_pane, Some(initial_readback.pane_id));

        // A later authoritative readback still performs the normal ownership
        // validation and drops the owner if the remote zoom did not stick.
        apply_committed_topology(
            &mut state,
            &mapping,
            &windows,
            std::slice::from_ref(&initial_readback),
            std::slice::from_ref(&flat),
            initial_readback.pane_id,
            false,
        );
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_window, None);
        assert_eq!(state.meeterm_zoomed_pane, None);
    }

    #[test]
    fn strict_initial_selection_only_reuses_proven_same_window_ownership() {
        let target = zoomed_pane(17, true, true);
        let previous = pane(23, false, false);
        let panes = vec![target.clone(), previous.clone()];
        let same_window = fresh_selection_observation(&target, &panes, Some(previous.window_id));

        // A retained owner in the same freshly read window is the one
        // permitted exception: restore then re-zoom may retain ownership.
        assert!(same_window.previous_owned_same_window);
        assert!(!selection_is_zoomed(Some(same_window), 1, 17, false, false));
        let mut state = SessionState::default();
        publish_selection_state(&mut state, 1, 17, false);
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, Some(17));

        // A missing prior pane does not erase window ownership: the fresh
        // target still belongs to the same authoritative window.
        let missing = fresh_selection_observation(&target, std::slice::from_ref(&target), Some(1));
        assert!(missing.previous_owned_same_window);
        assert!(!selection_is_zoomed(Some(missing), 1, 17, false, false));

        let mut other_window = previous;
        other_window.window_id = 2;
        let other_window_panes = vec![target.clone(), other_window];
        let other_window_observation =
            fresh_selection_observation(&target, &other_window_panes, Some(2));
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
        publish_selection_state(&mut state, 1, 23, zoomed);
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, None);
        assert_eq!(select_zoom_cleanup_target(None, None), None);
    }

    #[test]
    fn topology_keeps_zoom_ownership_on_the_window_when_pane_changes() {
        let mut state = SessionState {
            meeterm_zoomed: true,
            meeterm_zoomed_window: Some(1),
            meeterm_zoomed_pane: Some(17),
            ..SessionState::default()
        };
        let replacement = zoomed_pane(23, true, true);
        let flat = PaneSnapshot {
            window_id: replacement.window_id,
            pane_id: replacement.pane_id,
            terminal_id: 99,
            window_name: "window".to_owned(),
            active: replacement.active,
            selected: true,
            index: replacement.index,
            columns: replacement.columns,
            rows: replacement.rows,
            pane_name: replacement.pane_name.clone(),
            title: replacement.title.clone(),
        };
        let mut mapping = HashMap::new();
        mapping.insert(replacement.pane_id, flat.terminal_id);
        apply_topology_state(
            &mut state,
            &mapping,
            &[tmux::WindowInfo {
                window_id: 1,
                name: "window".to_owned(),
            }],
            std::slice::from_ref(&replacement),
            std::slice::from_ref(&flat),
            replacement.pane_id,
        );
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_window, Some(1));
        assert_eq!(state.meeterm_zoomed_pane, Some(23));
    }

    #[test]
    fn topology_drops_zoom_ownership_only_when_the_owned_window_is_gone() {
        let mut state = SessionState {
            meeterm_zoomed: true,
            meeterm_zoomed_window: Some(1),
            meeterm_zoomed_pane: Some(17),
            ..SessionState::default()
        };
        let replacement = pane(23, true, true);
        let flat = PaneSnapshot {
            window_id: replacement.window_id,
            pane_id: replacement.pane_id,
            terminal_id: 99,
            window_name: "other".to_owned(),
            active: replacement.active,
            selected: true,
            index: replacement.index,
            columns: replacement.columns,
            rows: replacement.rows,
            pane_name: replacement.pane_name.clone(),
            title: replacement.title.clone(),
        };
        let mut mapping = HashMap::new();
        mapping.insert(replacement.pane_id, flat.terminal_id);
        apply_topology_state(
            &mut state,
            &mapping,
            &[tmux::WindowInfo {
                window_id: 2,
                name: "other".to_owned(),
            }],
            std::slice::from_ref(&replacement),
            std::slice::from_ref(&flat),
            replacement.pane_id,
        );
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_window, None);
        assert_eq!(state.meeterm_zoomed_pane, None);
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
        publish_selection_state(&mut state, 1, 17, true);
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
            window: 1,
            pane: 17,
            hooks: Some(allocation),
        };
        let target = PendingZoomCleanup {
            window: 1,
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
            window: 1,
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
            window: 1,
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
        publish_selection_state(&mut state, 1, 17, false);
        assert!(state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_pane, previous);
    }

    #[test]
    fn strict_publish_rejects_same_pane_after_move_and_keeps_hook_intent() {
        let mut state = SessionState::default();
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_014 };
        let candidate = PendingZoomCleanup {
            window: 1,
            pane: 17,
            hooks: Some(hooks),
        };
        let mut final_pane = zoomed_pane(17, true, true);
        final_pane.window_id = 2;

        assert!(!strict_zoom_candidate_matches(
            candidate,
            std::slice::from_ref(&final_pane),
            17
        ));
        publish_strict_zoom_ownership(
            &mut state,
            Some(candidate),
            std::slice::from_ref(&final_pane),
            17,
        );
        assert!(!state.meeterm_zoomed);
        assert_eq!(state.meeterm_zoomed_window, None);
        assert_eq!(
            zoom_cleanup_hook_allocation(None, candidate.hooks),
            Some(hooks)
        );
    }

    #[test]
    fn strict_zoom_commit_decision_rejects_window_mismatch_and_accepts_one_pane_unneeded() {
        let candidate = PendingZoomCleanup {
            window: 1,
            pane: 17,
            hooks: Some(tmux::ZoomRecoveryHookAllocation { index: 1_015 }),
        };
        let mut moved = zoomed_pane(17, true, true);
        moved.window_id = 2;
        assert!(matches!(
            strict_zoom_commit_decision(Some(candidate), std::slice::from_ref(&moved), 17),
            Err(FlowFailure::TmuxRuntimeMissing)
        ));

        let unzoomed_multi = [pane(17, true, true), pane(23, false, false)];
        assert!(matches!(
            strict_zoom_commit_decision(Some(candidate), &unzoomed_multi, 17),
            Err(FlowFailure::TmuxRuntimeMissing)
        ));

        let unzoomed_one = [pane(17, true, true)];
        assert!(matches!(
            strict_zoom_commit_decision(Some(candidate), &unzoomed_one, 17),
            Ok(None)
        ));
        assert!(matches!(
            strict_zoom_commit_decision(Some(candidate), &[zoomed_pane(17, true, true)], 17),
            Ok(Some(value)) if value == candidate
        ));
        assert!(matches!(
            strict_zoom_commit_decision(None, &[zoomed_pane(17, true, true)], 17),
            Ok(None)
        ));
    }

    #[test]
    fn strict_zoom_mismatch_is_rejected_before_ready_commit_and_retains_record() {
        let owner = registry::create_terminal(80, 24).expect("strict mismatch terminal");
        let generation = 77_015;
        let shared = ConnectionShared::new(
            owner,
            generation,
            "strict-mismatch.example.test".to_owned(),
            22,
            std::path::PathBuf::from("/tmp/strict-mismatch-known-hosts"),
        );
        let runtime = tmux::SessionEpoch {
            session_id: "$15".to_owned(),
            server_pid: 15,
            server_start_time: 1_500_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_016 };
        {
            let mut state = shared.session.lock().expect("strict mismatch state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "strict-mismatch.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: std::path::PathBuf::from("/tmp/strict-mismatch-known-hosts"),
                backend: Backend::Tmux,
                runtime: Some("$15".to_owned()),
            });
        }
        assert!(shared.record_zoom_cleanup_intent(runtime.clone(), 1, 17, hooks));
        let expected_epoch = shared
            .begin_recovery("strict_mismatch", 1)
            .expect("strict mismatch recovery epoch");
        let before_snapshot = shared
            .session
            .lock()
            .expect("strict mismatch snapshot state")
            .snapshot
            .clone();
        let callback_published = std::sync::atomic::AtomicBool::new(false);
        let mut moved = zoomed_pane(17, true, true);
        moved.window_id = 2;
        let result = shared.commit_ready_at_epoch_result(expected_epoch, |_state| {
            // This is the same decision that the real strict finalizer runs
            // before applying captures/topology. A mismatch must short-circuit
            // before the simulated public mutation is marked.
            let decision = strict_zoom_commit_decision(
                Some(PendingZoomCleanup {
                    window: 1,
                    pane: 17,
                    hooks: Some(hooks),
                }),
                std::slice::from_ref(&moved),
                17,
            )?;
            callback_published.store(true, std::sync::atomic::Ordering::Release);
            publish_strict_zoom_ownership(_state, decision, std::slice::from_ref(&moved), 17);
            Ok(())
        });
        assert!(matches!(result, Err(FlowFailure::TmuxRuntimeMissing)));
        assert!(!callback_published.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            shared
                .session
                .lock()
                .expect("strict mismatch retained state")
                .snapshot,
            before_snapshot
        );
        assert_ne!(
            shared.info.lock().expect("strict mismatch info").state,
            ConnectionState::Ready
        );
        assert!(
            shared
                .zoom_cleanup_record_for(&runtime)
                .ok()
                .flatten()
                .is_some()
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn replaced_zoom_hook_release_never_builds_a_remove_command() {
        let allocation = tmux::ZoomRecoveryHookAllocation { index: 1_027 };
        assert!(matches!(
            ControlClient::release_record_hooks_command(
                tmux::ZoomRecoveryHookState::Replaced,
                "meeterm",
                allocation,
            ),
            Err(FlowFailure::TmuxRuntimeMissing)
        ));
        assert!(matches!(
            ControlClient::release_record_hooks_command(
                tmux::ZoomRecoveryHookState::Absent,
                "meeterm",
                allocation,
            ),
            Ok(None)
        ));
        let remove = match ControlClient::release_record_hooks_command(
            tmux::ZoomRecoveryHookState::Owned,
            "meeterm",
            allocation,
        ) {
            Ok(Some(remove)) => remove,
            Ok(None) | Err(_) => panic!("owned hook must produce a remove command"),
        };
        assert!(remove.contains("set-hook -u"));
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
            window: 1,
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
