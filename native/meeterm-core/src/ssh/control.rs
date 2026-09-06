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
    }
}

pub(super) async fn run(
    shared: &Arc<ConnectionShared>,
    session: &mut client::Handle<HostKeyHandler>,
    mut commands: mpsc::Receiver<ControlCommand>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::AttachingTmux);
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
        client.writer.exec(true, tmux::initial_command()),
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
            tmux::Event::Command(_) => return Err(FlowFailure::Tmux),
            event => client.dispatch(event)?,
        }
    }
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
                    None => return Err(FlowFailure::Stale),
                }
            }
            event = client.pane_receiver.recv() => {
                match event {
                    Some(PaneEvent::Input(pane, bytes)) => {
                        if client.routes.contains_key(&pane) {
                            client.query(&tmux::send_bytes_command(pane, &bytes)).await?;
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

    async fn select(&mut self, window: u64, pane: u64) -> Result<(), FlowFailure> {
        let previous = self
            .shared
            .session
            .lock()
            .map_err(|_| FlowFailure::Stale)?
            .meeterm_zoomed_pane;
        let allocation = if let Some(allocation) = self.zoom_hooks {
            allocation
        } else {
            let reply = self.query("show-hooks -t =meeterm:").await?;
            let hooks = reply
                .into_iter()
                .flat_map(|b| b.lines)
                .collect::<Vec<_>>()
                .join(&b'\n');
            let allocation = tmux::choose_zoom_recovery_hook(&hooks).ok_or(FlowFailure::Tmux)?;
            self.zoom_hooks = Some(allocation);
            allocation
        };
        // Install recovery before applying zoom. Existing indexed user hooks
        // remain intact; only our allocated pair is updated on tab selection.
        let mut transition = Vec::new();
        if let Some(previous) = previous {
            transition.push(tmux::restore_layout_command(previous));
        }
        transition.push(tmux::install_zoom_recovery_hooks_command(allocation, pane));
        transition.push(tmux::select_pane_command(None, window, pane));
        self.query(&transition.join(" ; ")).await?;
        let mut state = self.shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        state.selected_pane = Some(pane);
        state.meeterm_zoomed = true;
        state.meeterm_zoomed_pane = Some(pane);
        mark_selected(&mut state.snapshot, pane);
        Ok(())
    }

    async fn restore_zoom(&mut self) {
        let pane = self
            .shared
            .session
            .lock()
            .ok()
            .and_then(|s| s.meeterm_zoomed_pane);
        if let Some(pane) = pane {
            // Cancellation rejects all normal commands. This bounded best
            // effort cleanup is the only write permitted after cancellation.
            let cleanup = if let Some(allocation) = self.zoom_hooks {
                tmux::cleanup_zoom_recovery_hooks_command(allocation, pane)
            } else {
                tmux::restore_layout_command(pane)
            };
            let command = format!("{cleanup}\n");
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                self.writer.data_bytes(command.into_bytes()),
            )
            .await;
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
        self.shared.set_state(ConnectionState::Synchronizing);
        self.dirty = false;
        if initial {
            self.resize_client().await?;
        }
        let windows_reply = self
            .query(std::str::from_utf8(tmux::list_windows_command()).unwrap())
            .await?;
        let panes_reply = self
            .query(std::str::from_utf8(tmux::list_panes_command()).unwrap())
            .await?;
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
                selected: pane.pane_id == selected,
                index: pane.index,
                columns: pane.columns,
                rows: pane.rows,
                title: pane.title.clone(),
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
        let command = format!(
            "capture-pane -p -e -C -N -S -2000 -t %{pane} ; display-message -p -t %{pane} '#{{pane_width}},#{{pane_height}},#{{cursor_x}},#{{cursor_y}},#{{alternate_on}},#{{cursor_flag}},#{{keypad_cursor_flag}},#{{keypad_flag}},#{{?bracket_paste_flag,1,0}},#{{insert_flag}},#{{origin_flag}},#{{wrap_flag}}'"
        );
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
