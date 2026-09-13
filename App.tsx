import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ActivityIndicator,
  Alert,
  AppState,
  BackHandler,
  FlatList,
  Keyboard,
  Linking,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  StyleSheet,
  Text,
  TextInput,
  View,
  useWindowDimensions,
} from 'react-native';
import { SafeAreaProvider, SafeAreaView, useSafeAreaInsets } from 'react-native-safe-area-context';

import MeetermTerminal, { TerminalView } from './modules/meeterm-terminal';
import type { ServerProfile, SshConnectOptions, SshConnectionState, TerminalPreferences, RemoteTerminal, TerminalGroup, WorkspaceState } from './modules/meeterm-terminal';
import { ConnectionForm } from './app/ConnectionForm';
import { WorkspaceNavigation } from './app/WorkspaceNavigation';
import type { ConnectionSubmission } from './app/ConnectionForm';
import { DEFAULT_PREFERENCES, itemActions, NameForm, ProfileList, SettingsForm } from './app/DailyUse';
import { Button, Companion, DARK, Icon, IconButton, MONO, usePalette, useReducedMotion } from './app/ui';
import type { Palette } from './app/ui';

// The owner outlives views. Only remote borrowed pane handles are displayed in
// the ordinary app; the owner's local foundation fixture is never a fallback.
const CONNECTION_ID = 'poc-main';
const INITIAL_CONNECTION: SshConnectionState = {
  state: 'Disconnected', host: '', port: 0, fingerprint: '', algorithm: '',
  knownFingerprint: '', errorCode: '', errorMessage: '',
};
type Workspace = { id: string; name: string; panes: RemoteTerminal[] };
type SheetKind = 'server' | 'servers' | 'workspaces' | 'groups' | 'handoff' | null;
type NameRequest = { kind: 'createWorkspace' } | { kind: 'renameWorkspace'; workspace: Workspace } | { kind: 'renamePane'; pane: RemoteTerminal } | { kind: 'createGroup'; workspace: Workspace } | { kind: 'renameGroup'; group: TerminalGroup };
type SmokeScreen = 'welcome' | 'empty' | 'search-empty' | 'disconnected' | 'reconnecting' | 'connection-error' | 'long-workspaces' | 'herdr-connection' | 'herdr-groups' | 'herdr-terminal' | 'herdr-workspaces' | 'home' | 'servers' | 'connection' | 'password' | 'workspaces' | 'terminal' | 'settings' | 'workspace-name' | 'terminal-name' | 'handoff';
type SmokeRoute = { kind: 'foundation' } | { kind: 'screen'; screen: SmokeScreen } | null;

const SMOKE_PROFILE: ServerProfile = {
  id: 'smoke-profile', name: 'Smoke server', host: 'fixture.invalid', port: 22,
  username: 'fixture', authMethod: 'publicKey', credentialSaved: false,
};
const SMOKE_PASSWORD_PROFILE: ServerProfile = {
  ...SMOKE_PROFILE, id: 'smoke-password-profile', name: 'Password server',
  authMethod: 'password',
};
const SMOKE_PROFILES: ServerProfile[] = [
  SMOKE_PROFILE,
  SMOKE_PASSWORD_PROFILE,
];
const SMOKE_PANES: RemoteTerminal[] = [
  { workspaceId: '@smoke-main', id: '%smoke-main-1', terminalId: CONNECTION_ID, groupId: '@smoke-main', agent: null, name: 'Shell', active: true, selected: true },
  { workspaceId: '@smoke-main', id: '%smoke-main-2', terminalId: 'smoke-terminal-2', groupId: '@smoke-main', agent: null, name: 'Logs', active: false, selected: false },
  { workspaceId: '@smoke-tools', id: '%smoke-tools-1', terminalId: 'smoke-terminal-3', groupId: '@smoke-tools', agent: null, name: 'Console', active: true, selected: true },
];

type SmokeFixtureState = {
  preferences: TerminalPreferences;
  connection: SshConnectionState;
  panes: RemoteTerminal[];
  screen: 'workspaces' | 'terminal';
  workspaceId: string;
  selectedPaneIds: Record<string, string>;
  formVisible: boolean;
  formProfile?: ServerProfile;
  formMode: 'connect' | 'save';
  profiles: ServerProfile[];
  profilesLoading: boolean;
  preferencesLoaded: boolean;
  settingsVisible: boolean;
  nameRequest: NameRequest | null;
  profileId: string;
  sheet: SheetKind;
  hasConnected: boolean;
  searching?: boolean;
  query?: string;
};

function smokeReadyConnection(): SshConnectionState {
  return { ...INITIAL_CONNECTION, state: 'Ready', host: SMOKE_PROFILE.host, port: SMOKE_PROFILE.port };
}

function smokePanes(): RemoteTerminal[] { return SMOKE_PANES.map(pane => ({ ...pane })); }

function smokeWorkspace(panes: RemoteTerminal[], workspaceId: string): Workspace {
  return { id: workspaceId, name: workspaceId === '@smoke-main' ? 'Main workspace' : 'Tools workspace', panes: panes.filter(pane => pane.workspaceId === workspaceId) };
}

function smokeFixture(screen: SmokeScreen): SmokeFixtureState {
  if (['welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error', 'long-workspaces'].includes(screen)) {
    const base = smokeFixture(screen === 'welcome' ? 'home' : 'workspaces');
    if (screen === 'welcome') base.profiles = [];
    if (screen === 'empty') base.panes = [];
    if (screen === 'search-empty') { base.searching = true; base.query = 'deployment'; }
    if (screen === 'disconnected') base.connection.state = 'Disconnected';
    if (screen === 'reconnecting') base.connection.state = 'Reconnecting';
    if (screen === 'connection-error') {
      base.connection.state = 'Failed';
      base.connection.errorCode = 'authentication_failed';
    }
    return base;
  }
  if (screen.startsWith('herdr-')) {
    const base = smokeFixture(screen === 'herdr-connection' ? 'connection' : screen === 'herdr-workspaces' ? 'workspaces' : 'terminal');
    base.formProfile = { ...SMOKE_PROFILE, backend: 'herdr', runtime: 'dev' };
    base.panes = [
      { ...SMOKE_PANES[0], groupId: 'smoke-code', name: 'Code', agent: { name: 'Claude Code', status: 'working' } },
      { ...SMOKE_PANES[1], groupId: 'smoke-code', name: 'Shell' },
      { ...SMOKE_PANES[1], id: 'smoke-test-1', groupId: 'smoke-tests', name: 'Tests', agent: { name: 'Codex', status: 'blocked' } },
      { ...SMOKE_PANES[1], id: 'smoke-test-2', groupId: 'smoke-tests', name: 'Review', agent: { name: 'Codex', status: 'done' } },
      { ...SMOKE_PANES[2], groupId: 'smoke-logs', agent: { name: 'Claude Code', status: 'unknown' } },
    ];
    base.selectedPaneIds = { 'smoke-code': base.panes[0].id };
    base.sheet = screen === 'herdr-groups' ? 'groups' : null;
    return base;
  }
  const panes = smokePanes();
  const ready = ['workspaces', 'terminal', 'workspace-name', 'terminal-name', 'handoff'].includes(screen);
  const mainWorkspace = smokeWorkspace(panes, '@smoke-main');
  const selectedPaneIds = { '@smoke-main': '%smoke-main-1', '@smoke-tools': '%smoke-tools-1' };
  const base: SmokeFixtureState = {
    // Keep screenshot colors independent of the Simulator's appearance.
    preferences: { ...DEFAULT_PREFERENCES, theme: 'light' },
    connection: ready ? smokeReadyConnection() : { ...INITIAL_CONNECTION },
    panes: ready ? panes : [],
    screen: screen === 'terminal' || screen === 'terminal-name' || screen === 'handoff' ? 'terminal' : 'workspaces',
    workspaceId: screen === 'terminal' || screen === 'terminal-name' || screen === 'handoff' ? '@smoke-main' : '',
    selectedPaneIds,
    formVisible: screen === 'connection' || screen === 'password',
    formProfile: screen === 'password' ? { ...SMOKE_PASSWORD_PROFILE } : screen === 'connection' ? { ...SMOKE_PROFILE } : undefined,
    formMode: 'connect',
    profiles: SMOKE_PROFILES.map(profile => ({ ...profile })),
    profilesLoading: false,
    preferencesLoaded: true,
    settingsVisible: screen === 'settings',
    nameRequest: screen === 'workspace-name'
      ? { kind: 'renameWorkspace', workspace: mainWorkspace }
      : screen === 'terminal-name'
        ? { kind: 'renamePane', pane: mainWorkspace.panes[0] }
        : null,
    profileId: ready ? SMOKE_PROFILE.id : '',
    sheet: screen === 'servers' ? 'servers' : screen === 'handoff' ? 'handoff' : null,
    hasConnected: ready,
  };
  return base;
}

const SMOKE_SCREEN_NAMES: SmokeScreen[] = [
  'welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error', 'long-workspaces',
  'home', 'servers', 'connection', 'password', 'workspaces', 'terminal',
  'settings', 'workspace-name', 'terminal-name', 'handoff',
  'herdr-connection', 'herdr-groups', 'herdr-terminal', 'herdr-workspaces',
];

function smokeRouteForUrl(url: string | null): SmokeRoute | undefined {
  if (!url || process.env.EXPO_PUBLIC_MEETERM_SMOKE !== '1') return undefined;
  if (url === 'meeterm://foundation?foundation=1') return { kind: 'foundation' };
  if (!url.startsWith('meeterm://smoke?')) return undefined;
  const match = /^meeterm:\/\/smoke\?screen=([^&]+)$/.exec(url);
  if (!match || !SMOKE_SCREEN_NAMES.includes(match[1] as SmokeScreen)) return undefined;
  return { kind: 'screen', screen: match[1] as SmokeScreen };
}

function sameConnection(a: SshConnectionState, b: SshConnectionState) {
  return a.state === b.state && a.host === b.host && a.port === b.port
    && a.fingerprint === b.fingerprint && a.algorithm === b.algorithm
    && a.knownFingerprint === b.knownFingerprint && a.errorCode === b.errorCode
    && a.errorMessage === b.errorMessage;
}
const EMPTY_WORKSPACES: WorkspaceState = { backend: 'tmux', runtime: 'meeterm', groupsSupported: false, workspaces: [], groups: [], terminals: [] };
function smokeWorkspaceState(panes: RemoteTerminal[], herdr = false, longNames = false): WorkspaceState {
  const ids = [...new Set(panes.map(pane => pane.workspaceId))];
  return { ...EMPTY_WORKSPACES, terminals: panes,
    workspaces: ids.map(id => ({ id, name: longNames ? id === '@smoke-main' ? 'Production infrastructure — migration and release preparation' : 'Research / terminal typography and international text' : id === '@smoke-main' ? 'Main workspace' : 'Tools workspace' })),
    backend: herdr ? 'herdr' : 'tmux', runtime: herdr ? 'dev' : 'meeterm', groupsSupported: herdr,
    groups: herdr ? [
      { id: 'smoke-code', workspaceId: '@smoke-main', name: 'Development', selected: true },
      { id: 'smoke-tests', workspaceId: '@smoke-main', name: 'Tests & review', selected: false },
      { id: 'smoke-logs', workspaceId: '@smoke-tools', name: 'Logs', selected: true },
    ] : ids.map(id => ({ id, workspaceId: id, name: '', selected: true })),
  };
}
function sameSession(a: WorkspaceState, b: WorkspaceState) { return JSON.stringify(a) === JSON.stringify(b); }
function normalizeSearch(value: string) { return value.normalize('NFKC').trim().toLocaleLowerCase(); }
function endpoint(connection: SshConnectionState) {
  const host = connection.host.includes(':') ? `[${connection.host}]` : connection.host;
  return connection.port && connection.port !== 22 ? `${host}:${connection.port}` : host;
}
function keyChangeId(connection: SshConnectionState) {
  return connection.errorCode === 'host_key_changed'
    ? [connection.host, connection.port, connection.algorithm, connection.knownFingerprint, connection.fingerprint].join('|')
    : '';
}
function connectionPresentation(connection: SshConnectionState) {
  switch (connection.state) {
    case 'Ready': return { label: 'Connected', accessibility: 'Connected', pending: false };
    case 'Connecting': return { label: 'Connecting…', accessibility: 'Connecting…', pending: true };
    case 'HostKeyPending': return { label: 'Verify host key', accessibility: 'Verify host key', pending: true };
    case 'Authenticating': return { label: 'Authenticating…', accessibility: 'Authenticating…', pending: true };
    case 'OpeningPty': return { label: 'Opening terminal…', accessibility: 'Opening terminal…', pending: true };
    case 'AttachingTmux': return { label: 'Opening workspace…', accessibility: 'Opening workspace…', pending: true };
    case 'Synchronizing': return { label: 'Restoring terminals…', accessibility: 'Restoring terminals…', pending: true };
    case 'Reconnecting': return { label: 'Reconnecting…', accessibility: 'Reconnecting…', pending: true };
    case 'Closing': return { label: 'Disconnecting…', accessibility: 'Disconnecting…', pending: true };
    case 'Failed': return { label: 'Connection failed', accessibility: 'Connection failed', pending: false };
    default: return { label: 'Not connected', accessibility: 'Not connected', pending: false };
  }
}
function connectionError(connection: SshConnectionState) {
  const herdrErrors: Record<string, string> = {
    herdr_missing: 'Herdr was not found. Check that the Herdr you use on your computer is also available over SSH.',
    herdr_session_missing: 'This Herdr session is not running. Open it on your computer, then reconnect.',
    herdr_incompatible: 'This Herdr version is not supported. meeterm supports Herdr 0.9.0, protocol 22.',
    herdr_unsupported: 'This Herdr instance does not provide the required connection or state updates. Check its available features.',
    herdr_forwarding: 'Herdr is unreachable over SSH. Check that your SSH server allows Unix socket forwarding (AllowStreamLocalForwarding).',
    herdr_controller_busy: 'Another connection is controlling this terminal. Release it there, then reconnect.',
    herdr_protocol: 'The Herdr response could not be read. Check the remote version and session.',
    herdr_operation: 'Herdr could not complete this action. Reconnect to refresh your workspaces.',
    herdr_workspace_group: 'Closing this parent may also close related workspaces. Review and close it in Herdr on your computer.',
  };
  if (herdrErrors[connection.errorCode]) return herdrErrors[connection.errorCode];
  if (connection.errorCode === 'host_key_changed') return 'The host key differs from the saved key. Verify the identity of this server before connecting.';
  if (connection.errorCode === 'host_key_rejected') return 'Host verification was canceled. Connect again when you are ready to verify the key.';
  if (connection.errorCode.includes('private_key')) return 'The private key could not be read. Check its format and passphrase.';
  if (connection.errorCode.includes('auth')) return 'Authentication failed. Check your username and the password or private key for your chosen sign-in method.';
  return connection.errorMessage || 'Could not connect. Check the server address and your network, then try again.';
}

function ConnectionStatus({ connection, colors }: { connection: SshConnectionState; colors: Palette }) {
  const state = connectionPresentation(connection);
  return <View style={styles.status}>
    {state.pending ? <ActivityIndicator size="small" color={colors.accent} /> : <View style={[styles.statusDot, { backgroundColor: connection.state === 'Ready' ? colors.accent : colors.muted }]} />}
    <Text accessibilityLabel={state.accessibility} accessibilityLiveRegion="polite" style={[styles.statusText, { color: colors.muted }]}>{state.label}</Text>
  </View>;
}

const AGENT_LABELS = { working: 'Working', blocked: 'Needs attention', done: 'Finished', idle: 'Idle', unknown: 'Status unavailable' };
function agentSummary(panes: RemoteTerminal[], connected: boolean) {
  const agents = panes.flatMap(pane => pane.agent ? [pane.agent] : []);
  if (!agents.length) return '';
  if (!connected) return `Status unavailable ${agents.length}`;
  const statuses = ['blocked', 'working', 'done', 'idle', 'unknown'] as const;
  return statuses.flatMap(status => {
    const count = agents.filter(agent => agent.status === status).length;
    return count ? [`${AGENT_LABELS[status]} ${count}`] : [];
  }).join(' · ');
}

function SearchField({ value, onChange, colors, label = 'Search workspaces', autoFocus = false }: { value: string; onChange: (value: string) => void; colors: Palette; label?: string; autoFocus?: boolean }) {
  return <View style={[styles.searchField, { backgroundColor: colors.surface, borderColor: colors.border }]}>
    <Icon name="search" color={colors.muted} size={18} />
    <TextInput accessibilityLabel={label} autoFocus={autoFocus} autoCorrect={false} autoCapitalize="none" placeholder="Search by workspace name" placeholderTextColor={colors.placeholder} selectionColor={colors.accent} returnKeyType="search" onSubmitEditing={Keyboard.dismiss} style={[styles.searchInput, { color: colors.text }]} value={value} onChangeText={onChange} />
    {value ? <IconButton icon="close" label="Clear workspace search" onPress={() => onChange('')} colors={colors} /> : null}
  </View>;
}

function WorkspaceRow({ workspace, selected, colors, onPress, onOptions, picker = false, disabled = false, optionsDisabled = false, connected = false }: { workspace: Workspace; selected: boolean; colors: Palette; onPress: () => void; onOptions?: () => void; picker?: boolean; disabled?: boolean; optionsDisabled?: boolean; connected?: boolean }) {
  return <View style={[styles.workspaceContainer, { borderBottomColor: colors.border }]}><Pressable testID={`workspace-row-${workspace.id}`} accessibilityRole="button" accessibilityLabel={`Workspace ${workspace.name}`} accessibilityHint={`${workspace.panes.length} terminals`} accessibilityState={{ selected, disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [styles.workspaceRow, pressed && { backgroundColor: colors.surface }, disabled && { opacity: .5 }]}>
    <Icon name="terminal" color={colors.muted} size={23} />
    <View style={styles.rowCopy}>
      <Text numberOfLines={picker ? undefined : 2} style={[styles.rowTitle, { color: colors.text }]}>{workspace.name}</Text>
      <Text numberOfLines={1} style={[styles.rowSubtitle, { color: colors.muted }]}>{workspace.panes.length ? workspace.panes.map((pane, index) => pane.name || `Terminal ${index + 1}`).join(' · ') : 'No terminals'}</Text>
      {agentSummary(workspace.panes, connected) ? <Text style={[styles.rowSubtitle, { color: colors.muted }]}>{agentSummary(workspace.panes, connected)}</Text> : null}
    </View>
    <Icon name={selected ? 'check' : 'chevron'} color={selected ? colors.accent : colors.muted} size={18} />
  </Pressable>{onOptions ? <IconButton icon="menu" label={`Workspace options ${workspace.name}`} onPress={onOptions} disabled={disabled || optionsDisabled} colors={colors} /> : null}</View>;
}

function NativeSheet({ title, visible, onClose, onDismiss, busy, colors, children }: { title: string; visible: boolean; onClose: () => void; onDismiss: () => void; busy: boolean; colors: Palette; children: ReactNode }) {
  const reducedMotion = useReducedMotion();
  return <Modal visible={visible} animationType={reducedMotion ? 'fade' : 'slide'} presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} allowSwipeDismissal={!busy} onRequestClose={() => { if (!busy) onClose(); }} onDismiss={onDismiss} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
    <SafeAreaProvider>
      <SafeAreaView edges={['top', 'left', 'right', 'bottom']} style={[styles.flex, { backgroundColor: colors.background }]}>
        <StatusBar barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} />
        <View style={[styles.sheetHeader, { borderBottomColor: colors.border }]}>
          <Text accessibilityRole="header" style={[styles.sheetTitle, { color: colors.text }]}>{title}</Text>
          {busy ? <ActivityIndicator color={colors.accent} /> : null}
          <IconButton icon="close" label="Close sheet" colors={colors} onPress={onClose} disabled={busy} />
        </View>
        {children}
      </SafeAreaView>
    </SafeAreaProvider>
  </Modal>;
}

function AppContent({ smokeRoute }: { smokeRoute: SmokeRoute }) {
  const smokeScreen = smokeRoute?.kind === 'screen' ? smokeRoute.screen : null;
  const smokeFixtureActive = smokeScreen !== null;
  const fixture = smokeScreen ? smokeFixture(smokeScreen) : undefined;
  const [preferences, setPreferences] = useState<TerminalPreferences>(() => fixture?.preferences ?? DEFAULT_PREFERENCES);
  const homeColors = usePalette(preferences.theme);
  const insets = useSafeAreaInsets();
  const { width } = useWindowDimensions();
  const [connection, setConnection] = useState<SshConnectionState>(() => fixture?.connection ?? INITIAL_CONNECTION);
  const [session, setSession] = useState<WorkspaceState>(() => fixture ? smokeWorkspaceState(fixture.panes, smokeScreen?.startsWith('herdr-'), smokeScreen === 'long-workspaces') : EMPTY_WORKSPACES);
  const panes = session.terminals;
  const [screen, setScreen] = useState<'workspaces' | 'terminal'>(() => fixture?.screen ?? 'workspaces');
  const [rememberedWorkspaceId, setWorkspaceId] = useState(() => fixture?.workspaceId ?? '');
  const [selectedPaneIds, setSelectedPaneIds] = useState<Record<string, string>>(() => fixture?.selectedPaneIds ?? {});
  const [formVisible, setFormVisible] = useState(() => fixture?.formVisible ?? false);
  const [hostPromptDeferred, setHostPromptDeferred] = useState(false);
  const [modalPending, setModalPending] = useState(false);
  const [formProfile, setFormProfile] = useState<ServerProfile | undefined>(() => fixture?.formProfile);
  const [formMode, setFormMode] = useState<'connect' | 'save'>(() => fixture?.formMode ?? 'connect');
  const [profiles, setProfiles] = useState<ServerProfile[]>(() => fixture?.profiles ?? []);
  const [profilesLoading, setProfilesLoading] = useState(() => fixture?.profilesLoading ?? true);
  const [profilesError, setProfilesError] = useState(false);
  const [preferencesLoaded, setPreferencesLoaded] = useState(() => fixture?.preferencesLoaded ?? false);
  const [settingsVisible, setSettingsVisible] = useState(() => fixture?.settingsVisible ?? false);
  const [nameRequest, setNameRequest] = useState<NameRequest | null>(() => fixture?.nameRequest ?? null);
  const [profileId, setProfileId] = useState(() => fixture?.profileId ?? '');
  const [sheet, setSheet] = useState<SheetKind>(() => fixture?.sheet ?? null);
  const [searching, setSearching] = useState(fixture?.searching ?? false);
  const [query, setQuery] = useState(fixture?.query ?? '');
  const [pickerQuery, setPickerQuery] = useState('');
  const [controlMessage, setControlMessage] = useState('');
  const [pollProblem, setPollProblem] = useState(false);
  const [removedHostKeyId, setRemovedHostKeyId] = useState('');
  const [hasConnected, setHasConnected] = useState(() => fixture?.hasConnected ?? false);
  const [commandBusy, setCommandBusy] = useState(false);
  const [appState, setAppState] = useState(AppState.currentState);
  const [foundation, setFoundation] = useState(() => smokeRoute?.kind === 'foundation');
  const commandPending = useRef(false);
  const commandVersion = useRef(0);
  const shownHostKey = useRef('');
  const pendingModal = useRef<(() => void) | null>(null);
  const formSavedProfile = useRef<ServerProfile | undefined>(undefined);
  const returnToServersAfterForm = useRef(false);
  const connectedIdentity = useRef('');
  const foregroundCommands = useRef(Promise.resolve());
  const listOffsets = useRef({ normal: 0, search: 0 });
  const workspaceList = useRef<FlatList<Workspace>>(null);
  const foreground = useRef(AppState.currentState === 'active');

  const loadProfiles = useCallback(async () => {
    if (smokeFixtureActive) return;
    setProfilesLoading(true);
    try { setProfiles(await MeetermTerminal.getProfiles()); setProfilesError(false); }
    catch { setProfilesError(true); }
    finally { setProfilesLoading(false); }
  }, [smokeFixtureActive]);

  const loadPreferences = useCallback(async () => {
    if (smokeFixtureActive) return;
    try {
      const next = await MeetermTerminal.getPreferences();
      await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, next.automaticReconnect);
      setPreferences(next);
      setPreferencesLoaded(true);
    } catch {
      setControlMessage('Settings could not be loaded. Open Settings to try again.');
    }
  }, [smokeFixtureActive]);

  useEffect(() => {
    if (smokeFixtureActive) return;
    void loadProfiles();
    void loadPreferences();
  }, [loadPreferences, loadProfiles, smokeFixtureActive]);

  useEffect(() => {
    if (smokeFixtureActive) return;
    const applyForeground = (isForeground: boolean) => {
      // Preserve OS event order. Rust owns reconnect policy and timers.
      foregroundCommands.current = foregroundCommands.current
        .then(() => MeetermTerminal.setForeground(CONNECTION_ID, isForeground))
        .catch(() => setControlMessage('Could not update the connection after the app changed state. Check your connection.'));
    };
    applyForeground(foreground.current);
    const subscription = AppState.addEventListener('change', state => {
      foreground.current = state === 'active';
      setAppState(state);
      applyForeground(foreground.current);
    });
    return () => subscription.remove();
  }, [smokeFixtureActive]);

  useEffect(() => {
    if (smokeFixtureActive) return;
    let mounted = true;
    let polling = false;
    const refresh = async () => {
      if (polling || !foreground.current) return;
      polling = true;
      const version = commandVersion.current;
      try {
        const next = await MeetermTerminal.getConnectionState(CONNECTION_ID);
        const session = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
        if (mounted && version === commandVersion.current && !commandPending.current) {
          setConnection(current => sameConnection(current, next) ? current : next);
          setSession(current => sameSession(current, session) ? current : session);
          if (next.state === 'Ready') setHasConnected(true);
          setPollProblem(false);
        }
      } catch {
        if (mounted) setPollProblem(true);
      } finally { polling = false; }
    };
    void refresh();
    // Poll metadata only. Native owns reconnect, terminal bytes, and frames.
    const interval = setInterval(() => { void refresh(); }, 1000);
    return () => { mounted = false; clearInterval(interval); };
  }, [smokeFixtureActive]);

  useEffect(() => {
    if (smokeFixtureActive) return;
    if (formVisible || hostPromptDeferred) return;
    if (connection.state !== 'HostKeyPending' || !connection.host || !connection.port || !connection.fingerprint) {
      if (connection.state !== 'HostKeyPending') shownHostKey.current = '';
      return;
    }
    const promptId = [connection.host, connection.port, connection.algorithm, connection.fingerprint].join('|');
    if (shownHostKey.current === promptId) return;
    shownHostKey.current = promptId;
    const respond = (accept: boolean) => {
      void MeetermTerminal.respondToHostKey(CONNECTION_ID, connection.fingerprint, accept).catch(() => {
        shownHostKey.current = '';
        setControlMessage('Your host-key decision could not be sent. Connect again.');
      });
    };
    Alert.alert('Trust this SSH host?', `${connection.host}:${connection.port}\n\nAlgorithm: ${connection.algorithm || '(unavailable)'}\nSHA256 fingerprint:\n${connection.fingerprint}\n\nCompare this fingerprint with your server using another trusted channel. The approved key will be saved on this device.`, [
      { text: 'Cancel', style: 'cancel', onPress: () => respond(false) },
      { text: 'Trust and connect', onPress: () => respond(true) },
    ], { cancelable: false });
  }, [connection, formVisible, hostPromptDeferred, smokeFixtureActive]);

  const nativeSelectedPane = panes.find(pane => pane.selected);
  const selectedWorkspaceId = nativeSelectedPane?.workspaceId;
  // Follow the native selection in the same render as its snapshot. A pane
  // moved by another client keeps its native handle and input controller.
  const workspaceId = screen === 'terminal' ? selectedWorkspaceId ?? rememberedWorkspaceId : rememberedWorkspaceId;
  useEffect(() => {
    // Track native workspace changes only: an empty-Group selection can still
    // be queued when navigation opens the terminal screen. Group.selected is
    // per-workspace, not a global active Group.
    if (selectedWorkspaceId !== undefined) setWorkspaceId(selectedWorkspaceId);
  }, [selectedWorkspaceId]);
  const workspaces = useMemo(() => session.workspaces.map(workspace => ({ ...workspace, panes: panes.filter(pane => pane.workspaceId === workspace.id) })), [session.workspaces, panes]);
  const workspace = workspaces.find(item => item.id === workspaceId);
  const groups = session.groups.filter(group => group.workspaceId === workspaceId);
  const group = groups.find(group => group.id === nativeSelectedPane?.groupId)
    ?? groups.find(group => group.selected) ?? groups[0];
  const groupPanes = workspace?.panes.filter(pane => pane.groupId === group?.id) ?? [];
  // Remembered/active panes are candidates for explicit selectPane commands,
  // never alternate surfaces for a controller still bound to another pane.
  const selectedPane = groupPanes.find(pane => pane.selected);
  const activeWorkspaceId = selectedWorkspaceId
    ?? session.groups.find(group => group.workspaceId === workspaceId && group.selected)?.workspaceId;
  // App appearance and terminal contrast are independent. Remote ANSI palettes
  // remain readable on a dark work surface, including in the light app theme.
  const colors = screen === 'terminal' ? DARK : homeColors;
  const resolvedTheme = 'dark';
  const currentProfile = profiles.find(profile => profile.id === profileId);
  const presentation = connectionPresentation(connection);
  const ready = connection.state === 'Ready';
  const terminalVisible = Boolean(ready && workspace && selectedPane)
    && screen === 'terminal' && sheet === null && !modalPending && !formVisible && !settingsVisible
    && !nameRequest && appState === 'active';
  useEffect(() => {
    if (smokeFixtureActive) return;
    foregroundCommands.current = foregroundCommands.current
      .then(() => MeetermTerminal.setTerminalVisible(CONNECTION_ID, terminalVisible))
      .catch(() => setControlMessage('Could not update terminal visibility. Reconnect to continue.'));
  }, [terminalVisible, smokeFixtureActive]);
  const closing = connection.state === 'Closing';
  const active = !['Disconnected', 'Failed', 'Closing'].includes(connection.state);
  const attempted = Boolean(connection.host);
  const canReconnect = hasConnected && !active && !closing && connection.errorCode !== 'host_key_changed';
  const filteredWorkspaces = useMemo(() => searching ? workspaces.filter(item => normalizeSearch(item.name).includes(normalizeSearch(query))) : workspaces, [query, searching, workspaces]);
  const pickerWorkspaces = useMemo(() => workspaces.filter(item => normalizeSearch(item.name).includes(normalizeSearch(pickerQuery))), [pickerQuery, workspaces]);

  const runCommand = useCallback(async (action: () => Promise<void>, errorMessage: string) => {
    if (smokeFixtureActive) return false;
    if (commandPending.current) return false;
    commandPending.current = true;
    commandVersion.current += 1;
    setCommandBusy(true);
    setControlMessage('');
    try {
      await action();
      try {
        const next = await MeetermTerminal.getConnectionState(CONNECTION_ID);
        const session = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
        setConnection(next); setSession(session); setPollProblem(false);
        if (next.state === 'Ready') setHasConnected(true);
      } catch { setPollProblem(true); }
      return true;
    }
    catch { setControlMessage(errorMessage); return false; }
    finally { commandPending.current = false; setCommandBusy(false); }
  }, [smokeFixtureActive]);

  const finishConnectionForm = useCallback(() => {
    if (Platform.OS === 'ios' && returnToServersAfterForm.current) setModalPending(true);
    setFormVisible(false);
    if (Platform.OS !== 'ios' && returnToServersAfterForm.current) {
      returnToServersAfterForm.current = false;
      setSheet('servers');
    }
  }, []);

  const connectionFormDismissed = useCallback(() => {
    setHostPromptDeferred(false);
    setModalPending(false);
    if (returnToServersAfterForm.current) {
      returnToServersAfterForm.current = false;
      setSheet('servers');
    }
  }, []);

  const resetForConnection = useCallback((profile: Pick<ServerProfile, 'host' | 'port' | 'backend' | 'runtime'>) => {
    if (Platform.OS === 'ios' && (formVisible || sheet !== null)) setHostPromptDeferred(true);
    returnToServersAfterForm.current = false;
    setFormVisible(false);
    setSheet(null);
    setScreen('workspaces');
    setFoundation(false);
    setSession({ ...EMPTY_WORKSPACES, backend: profile.backend ?? 'tmux', runtime: profile.runtime || (profile.backend === 'herdr' ? 'default' : 'meeterm') });
    setSelectedPaneIds({});
    setWorkspaceId('');
    setSearching(false);
    setQuery('');
    listOffsets.current = { normal: 0, search: 0 };
    setRemovedHostKeyId('');
    setHasConnected(false);
    setConnection({ ...INITIAL_CONNECTION, state: 'Connecting', host: profile.host, port: profile.port });
  }, [formVisible, sheet]);

  const prepareConnection = useCallback(async () => {
    if (smokeFixtureActive) return;
    const currentPreferences = preferencesLoaded ? preferences : await MeetermTerminal.getPreferences();
    await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, currentPreferences.automaticReconnect);
    await foregroundCommands.current;
    await MeetermTerminal.setForeground(CONNECTION_ID, foreground.current);
    // Switching endpoints explicitly releases the previous connection owner.
    await MeetermTerminal.disconnect(CONNECTION_ID);
  }, [preferences, preferencesLoaded, smokeFixtureActive]);

  const submitConnection = useCallback(async (submission: ConnectionSubmission) => {
    let savedProfile: ServerProfile | undefined;
    const success = await runCommand(async () => {
      if (submission.saveProfile) {
        savedProfile = await MeetermTerminal.saveProfile({ ...submission.profile, id: submission.profile.id || formSavedProfile.current?.id || '' }, submission.saveCredential ? submission.credential : null, submission.keepCredential);
        formSavedProfile.current = savedProfile;
        setProfiles(current => [...current.filter(item => item.id !== savedProfile!.id), savedProfile!]);
        if (savedProfile.id === profileId && connectedIdentity.current !== JSON.stringify([savedProfile.host, savedProfile.port, savedProfile.username, savedProfile.authMethod, savedProfile.backend ?? 'tmux', savedProfile.runtime ?? ''])) setProfileId('');
      }
      if (!submission.connect) return;
      await prepareConnection();
      if (submission.credential) {
        const options: SshConnectOptions = { host: submission.profile.host, port: submission.profile.port, username: submission.profile.username, backend: submission.profile.backend ?? 'tmux', runtime: submission.profile.runtime ?? '', ...submission.credential };
        await MeetermTerminal.connect(CONNECTION_ID, options);
      } else if (savedProfile?.credentialSaved) {
        await MeetermTerminal.connectProfile(CONNECTION_ID, savedProfile.id);
      } else { throw new Error('Credential required'); }
      resetForConnection(submission.profile);
      connectedIdentity.current = JSON.stringify([submission.profile.host, submission.profile.port, submission.profile.username, submission.profile.authMethod, submission.profile.backend ?? 'tmux', submission.profile.runtime ?? '']);
      setProfileId(savedProfile?.id ?? '');
    }, 'Could not save or connect to this server. Check the address and credentials.');
    if (success) finishConnectionForm();
    return success;
  }, [finishConnectionForm, prepareConnection, profileId, resetForConnection, runCommand]);

  const disconnect = useCallback(() => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Closing' }));
    void runCommand(() => MeetermTerminal.disconnect(CONNECTION_ID), 'Could not disconnect. Please try again.').then(success => {
      if (!success) setConnection(previous);
    });
  }, [connection, runCommand]);

  const reconnect = useCallback(() => {
    if (commandPending.current) return;
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Reconnecting', errorCode: '', errorMessage: '' }));
    void runCommand(() => MeetermTerminal.reconnect(CONNECTION_ID), 'Could not reconnect. Choose Connection details to enter your credentials again.').then(success => {
      if (!success) setConnection(previous);
    });
  }, [connection, runCommand]);

  const choosePane = useCallback(async (pane: RemoteTerminal) => {
    if (commandPending.current || !['Ready', 'Disconnected', 'Failed'].includes(connection.state)) return false;
    Keyboard.dismiss();
    const previous = selectedPaneIds[pane.groupId];
    setSelectedPaneIds(current => ({ ...current, [pane.groupId]: pane.id }));
    // Rust also retains a desired pane while disconnected, so reconnect's
    // restored selection follows an offline workspace choice.
    // Presentation fixtures may navigate only to their existing native demo
    // terminal. They never issue a remote selection or create a fake JS buffer.
    const success = smokeFixtureActive ? pane.terminalId === CONNECTION_ID : await runCommand(() => MeetermTerminal.selectPane(CONNECTION_ID, pane.id), 'Could not open this terminal. Check the list and select it again.');
    if (!success) setSelectedPaneIds(current => {
        const next = { ...current };
        if (previous) next[pane.groupId] = previous; else delete next[pane.groupId];
        return next;
      });
    return success;
  }, [connection.state, runCommand, selectedPaneIds, smokeFixtureActive]);

  const openWorkspace = useCallback((item: Workspace) => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    const chosenGroup = session.groups.find(candidate => candidate.workspaceId === item.id && candidate.selected)
      ?? session.groups.find(candidate => candidate.workspaceId === item.id);
    const candidates = item.panes.filter(candidate => candidate.groupId === chosenGroup?.id);
    const pane = candidates.find(candidate => candidate.id === selectedPaneIds[chosenGroup?.id ?? ''])
      ?? candidates.find(candidate => candidate.active)
      ?? candidates.find(candidate => candidate.selected)
      ?? candidates[0];
    if (pane) {
      void choosePane(pane).then(success => {
        if (!success) return;
        setWorkspaceId(item.id);
        setScreen('terminal');
        setSheet(null);
        setPickerQuery('');
      });
    } else if (chosenGroup && session.groupsSupported) {
      void runCommand(() => MeetermTerminal.selectGroup(CONNECTION_ID, chosenGroup.id), 'Could not open this group.').then(success => {
        if (success) { setWorkspaceId(item.id); setScreen('terminal'); setSheet(null); }
      });
    } else { setWorkspaceId(item.id); setScreen('terminal'); setSheet(null); }
  }, [choosePane, selectedPaneIds, session.groups, session.groupsSupported, runCommand]);

  const backToWorkspaces = useCallback(() => {
    Keyboard.dismiss();
    setScreen('workspaces');
    setSheet(null);
  }, []);
  const openSheet = useCallback((kind: SheetKind) => {
    Keyboard.dismiss();
    setPickerQuery('');
    setSheet(kind);
  }, []);
  const showModal = useCallback((show: () => void) => {
    // iOS must finish dismissing its page sheet before another native modal
    // is presented. Android owns a separate dialog window for each modal.
    if (sheet !== null && Platform.OS === 'ios') {
      setModalPending(true);
      pendingModal.current = show;
      setSheet(null);
      return;
    }
    setSheet(null);
    show();
  }, [sheet]);
  const openProfileForm = useCallback((profile?: ServerProfile, mode: 'connect' | 'save' = 'connect') => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    showModal(() => {
      formSavedProfile.current = undefined;
      returnToServersAfterForm.current = mode === 'save' || sheet === 'servers';
      setFormProfile(profile);
      setFormMode(mode);
      setFormVisible(true);
    });
  }, [sheet, showModal]);
  const openForm = useCallback(() => openProfileForm(currentProfile), [currentProfile, openProfileForm]);

  const connectSavedProfile = useCallback((profile: ServerProfile) => {
    if (commandPending.current) return;
    const connect = () => {
      if (!profile.credentialSaved) { openProfileForm(profile); return; }
      void runCommand(async () => {
        await prepareConnection();
        await MeetermTerminal.connectProfile(CONNECTION_ID, profile.id);
        resetForConnection(profile);
        connectedIdentity.current = JSON.stringify([profile.host, profile.port, profile.username, profile.authMethod, profile.backend ?? 'tmux', profile.runtime ?? '']);
        setProfileId(profile.id);
      }, 'Could not connect to this saved server. Choose Edit server to check its address and credentials.');
    };
    if (active && profile.id !== profileId) {
      Alert.alert('Switch servers?', 'This disconnects the current server and connects to the selected one. Your remote work keeps running.', [
        { text: 'Cancel', style: 'cancel' }, { text: 'Switch server', onPress: connect },
      ]);
    } else if (active && profile.id === profileId) {
      setSheet(null);
    } else connect();
  }, [active, openProfileForm, prepareConnection, profileId, resetForConnection, runCommand]);

  const deleteProfile = useCallback((profile: ServerProfile) => {
    Alert.alert('Remove saved server?', `${profile.name}\n\nThis removes the server and its saved credentials from this device. Your remote work stays on the server.`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Remove', style: 'destructive', onPress: () => {
        void runCommand(async () => {
          await MeetermTerminal.deleteProfile(profile.id);
          setProfiles(current => current.filter(item => item.id !== profile.id));
          if (profile.id === profileId) setProfileId('');
        }, 'Could not remove this saved server. Please try again.');
      } },
    ]);
  }, [profileId, runCommand]);

  const openSettings = useCallback(() => {
    if (commandPending.current) return;
    if (smokeFixtureActive) {
      showModal(() => setSettingsVisible(true));
      return;
    }
    if (!preferencesLoaded) {
      void runCommand(async () => {
        const next = await MeetermTerminal.getPreferences();
        await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, next.automaticReconnect);
        setPreferences(next);
        setPreferencesLoaded(true);
        showModal(() => setSettingsVisible(true));
      }, 'Could not load settings. Please try again.');
      return;
    }
    showModal(() => setSettingsVisible(true));
  }, [preferencesLoaded, runCommand, showModal, smokeFixtureActive]);

  const savePreferences = useCallback(async (next: TerminalPreferences) => {
    const success = await runCommand(async () => {
      await MeetermTerminal.setPreferences(next);
      setPreferences(next);
      await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, next.automaticReconnect);
    }, 'Could not save or apply settings. Open Settings and save again.');
    if (success) setSettingsVisible(false);
    return success;
  }, [runCommand]);

  const openName = useCallback((request: NameRequest) => {
    if (!ready || commandPending.current) return;
    showModal(() => setNameRequest(request));
  }, [ready, showModal]);

  const saveName = useCallback(async (name: string) => {
    if (!nameRequest || !ready) return false;
    const request = nameRequest;
    const success = await runCommand(() => request.kind === 'createWorkspace'
      ? MeetermTerminal.createWorkspace(CONNECTION_ID, name)
      : request.kind === 'renameWorkspace'
        ? MeetermTerminal.renameWorkspace(CONNECTION_ID, request.workspace.id, name)
        : request.kind === 'createGroup'
          ? MeetermTerminal.createGroup(CONNECTION_ID, request.workspace.id, name)
          : request.kind === 'renameGroup'
            ? MeetermTerminal.renameGroup(CONNECTION_ID, request.group.id, name)
            : MeetermTerminal.renamePane(CONNECTION_ID, request.pane.id, name), 'Could not update the name. Check your connection and try again.');
    if (success) setNameRequest(null);
    return success;
  }, [nameRequest, ready, runCommand]);

  const closeWorkspace = useCallback((item: Workspace) => {
    Alert.alert('Close workspace?', `${item.name}\n\n${item.panes.length} terminals and their running processes will close. Unsaved work will be lost.`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Close', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closeWorkspace(CONNECTION_ID, item.id), 'Could not close this workspace. Check your connection.').then(success => {
          if (success) { setSheet(null); if (workspaceId === item.id) backToWorkspaces(); }
        });
      } },
    ]);
  }, [backToWorkspaces, runCommand, workspaceId]);

  const workspaceOptions = useCallback((item: Workspace) => {
    itemActions(item.name, () => openName({ kind: 'renameWorkspace', workspace: item }), () => closeWorkspace(item), 'workspace');
  }, [closeWorkspace, openName]);

  const createPane = useCallback(() => {
    if (!workspace || !ready) return;
    if (session.groupsSupported && groupPanes.length === 0) {
      setControlMessage('This group has no terminal to split. Create a new group from the menu.');
      return;
    }
    void runCommand(() => MeetermTerminal.createPane(CONNECTION_ID, workspace.id), 'Could not create a terminal. Check your connection.').then(success => {
      if (success) {
        setSelectedPaneIds(current => { const next = { ...current }; if (group) delete next[group.id]; return next; });
        setSheet(null);
      }
    });
  }, [ready, runCommand, workspace, group, session.groupsSupported, groupPanes.length]);

  const closePane = useCallback(() => {
    if (!selectedPane || !workspace) return;
    const pane = selectedPane;
    const consequence = workspace.panes.length === 1 ? 'This is the last terminal, so its workspace will also close.'
      : session.groupsSupported && groupPanes.length === 1 ? 'This is the last terminal in its group, so the group will also close.' : '';
    Alert.alert('Close terminal?', `${pane.name || pane.id}\n\nThe running process will stop. Unsaved work will be lost.${consequence}`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Close', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closePane(CONNECTION_ID, pane.id), 'Could not close this terminal. Check your connection.').then(success => {
          if (success) {
            setSelectedPaneIds(current => { const next = { ...current }; delete next[pane.groupId]; return next; });
            setSheet(null);
            if (workspace.panes.length === 1) backToWorkspaces();
          }
        });
      } },
    ]);
  }, [backToWorkspaces, runCommand, selectedPane, workspace, session.groupsSupported, groupPanes.length]);

  const chooseGroup = useCallback((item: TerminalGroup) => {
    if (!ready || commandPending.current) return;
    const remembered = panes.find(pane => pane.groupId === item.id && pane.id === selectedPaneIds[item.id]);
    const selection = remembered ? choosePane(remembered) : runCommand(() => MeetermTerminal.selectGroup(CONNECTION_ID, item.id), 'Could not open this group. Check the list and try again.');
    void selection.then(success => { if (success) setSheet(null); });
  }, [ready, runCommand, panes, selectedPaneIds, choosePane]);

  const closeGroup = useCallback((item: TerminalGroup) => {
    const terminals = panes.filter(pane => pane.groupId === item.id);
    const last = session.groups.filter(group => group.workspaceId === item.workspaceId).length === 1;
    Alert.alert('Close group?', `${item.name}\n\n${terminals.length} terminals and their running processes will close. Unsaved work will be lost.${last ? 'This is the last group, so its workspace will also close.' : ''}`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Close', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closeGroup(CONNECTION_ID, item.id), 'Could not close this group. Check your connection.').then(success => {
          if (success) { setSheet(null); if (last) backToWorkspaces(); }
        });
      } },
    ]);
  }, [panes, session.groups, runCommand, backToWorkspaces]);

  const refreshTerminal = useCallback(() => {
    void runCommand(() => MeetermTerminal.refreshTerminal(CONNECTION_ID), 'Could not refresh this terminal. Check your connection.').then(success => { if (success) setSheet(null); });
  }, [runCommand]);
  const closeSearch = useCallback(() => {
    Keyboard.dismiss();
    setSearching(false);
  }, []);

  useEffect(() => {
    const subscription = BackHandler.addEventListener('hardwareBackPress', () => {
      if (foundation) { setFoundation(false); return true; }
      if (screen === 'terminal') { backToWorkspaces(); return true; }
      if (searching) { closeSearch(); return true; }
      return false;
    });
    return () => subscription.remove();
  }, [backToWorkspaces, closeSearch, foundation, screen, searching]);

  const reviewChangedHostKey = useCallback(() => {
    const changeId = keyChangeId(connection);
    if (!changeId || changeId === removedHostKeyId) return;
    Alert.alert('Host key changed', `${connection.host}:${connection.port}\n\nAlgorithm: ${connection.algorithm || '(unavailable)'}\n\nSaved fingerprint:\n${connection.knownFingerprint || '(unavailable)'}\n\nReceived fingerprint:\n${connection.fingerprint || '(unavailable)'}\n\nThe server may have been rebuilt, or someone may be impersonating it. Remove the saved key only after verifying the change with your administrator through another trusted channel.`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Remove saved key', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.forgetHostKey(connection.host, connection.port), 'Could not remove the saved host key.').then(success => {
          if (success) {
            setRemovedHostKeyId(changeId);
            setControlMessage('Saved key removed. Enter your connection details again to verify the new host key.');
          }
        });
      } },
    ], { cancelable: false });
  }, [connection, removedHostKeyId, runCommand]);

  const statusNotice = attempted && !ready ? <View style={[styles.notice, { backgroundColor: colors.surface }]}>
    <Text style={[styles.noticeTitle, { color: colors.text }]}>{connection.state === 'Failed' && connection.errorCode === 'host_key_changed' ? 'Verify this server' : connection.state === 'Disconnected' ? 'Disconnected' : presentation.label}</Text>
    <Text style={[styles.noticeBody, { color: colors.muted }]}>{connection.state === 'Failed' ? connectionError(connection) : connection.state === 'Disconnected' ? hasConnected ? 'Your work is still running on the server. Reconnect to pick up where you left off.' : 'Enter your connection details to get started.' : closing ? hasConnected ? 'Disconnecting. Your work will keep running on the server.' : 'Canceling the connection.' : 'Checking your remote workspaces.'}</Text>
    <View style={styles.noticeActions}>
      {canReconnect ? <Button label="Reconnect" colors={colors} disabled={commandBusy} onPress={reconnect}>Reconnect</Button> : null}
      {!active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Connect" onPress={openForm} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Connection details</Text></Pressable> : null}
      {active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Cancel connection" disabled={commandBusy} onPress={disconnect} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Cancel</Text></Pressable> : null}
      {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Review key change</Text></Pressable> : null}
    </View>
  </View> : null;

  const feedbackColors = sheet ? homeColors : colors;
  const feedback = controlMessage || pollProblem ? <View accessibilityLiveRegion="polite" style={[styles.feedback, { backgroundColor: feedbackColors.surface }]}>
    <Text style={[styles.noticeBody, { color: feedbackColors.danger, flex: 1 }]}>{controlMessage || 'Connection status is unavailable. Wait a moment, then reconnect.'}</Text>
    {controlMessage ? <IconButton icon="close" label="Dismiss message" colors={feedbackColors} onPress={() => setControlMessage('')} /> : null}
  </View> : null;

  const listHeader = <View>
    {searching ? <View style={styles.searchHeader}>
      <View style={styles.flex}>
        <Text accessibilityRole="header" style={[styles.searchTitle, { color: homeColors.text }]}>Find a workspace</Text>
        <Text numberOfLines={1} style={[styles.searchHost, { color: homeColors.muted }]}>{endpoint(connection)}</Text>
      </View>
      <Pressable accessibilityRole="button" accessibilityLabel="Close workspace search" onPress={closeSearch} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Close</Text></Pressable>
    </View> : <>
      <View style={styles.brandRow}><Text style={[styles.brand, { color: homeColors.text }]}>meeterm</Text><IconButton icon="settings" label="Terminal settings" testID="open-settings" colors={homeColors} disabled={commandBusy} onPress={openSettings} /></View>
      {attempted || profiles.length > 0 ? <View style={styles.hero}>
        <View style={styles.heroCopy}>
          <Text accessibilityRole="header" style={[styles.heroTitle, width < 360 && { fontSize: 24 }, { color: homeColors.text }]}>Workspaces</Text>
          <Text style={[styles.heroDescription, { color: homeColors.muted }]}>{attempted ? 'Your work, right where you left it.' : 'Your servers. Your familiar workspace.'}</Text>
        </View>
        <Companion small />
      </View> : null}
      {attempted ? <View style={[styles.serverRow, { borderBottomColor: homeColors.border }]}>
        <Pressable accessibilityRole="button" accessibilityLabel="Saved servers" accessibilityHint="Choose a saved server" onPress={() => openSheet('servers')} style={({ pressed }) => [styles.serverTarget, pressed && { backgroundColor: homeColors.surface }]}>
          <Icon name="server" color={homeColors.muted} size={17} />
          <Text numberOfLines={1} style={[styles.serverName, { color: homeColors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
          <Icon name="down" color={homeColors.muted} size={12} />
        </Pressable>
        <ConnectionStatus connection={connection} colors={homeColors} />
        <IconButton icon="menu" label="Server connection" colors={homeColors} onPress={() => openSheet('server')} />
      </View> : null}
    </>}
    {statusNotice ? <View style={styles.horizontal}>{statusNotice}</View> : null}
    {feedback ? <View style={styles.horizontal}>{feedback}</View> : null}
    {searching ? <View style={styles.horizontal}>
      <SearchField value={query} colors={homeColors} onChange={value => { setQuery(value); listOffsets.current.search = 0; workspaceList.current?.scrollToOffset({ offset: 0, animated: false }); }} autoFocus />
      <Text style={[styles.resultCount, { color: homeColors.muted }]}>{filteredWorkspaces.length} {filteredWorkspaces.length === 1 ? 'result' : 'results'}</Text>
    </View> : attempted && (workspaces.length > 0 || ready) ? <View style={styles.sectionHeader}>
      <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>All  {workspaces.length}</Text>
      <View style={styles.topActions}><IconButton icon="search" label="Search workspaces" onPress={() => setSearching(true)} colors={homeColors} /><IconButton icon="plus" label="Create workspace" onPress={() => openName({ kind: 'createWorkspace' })} colors={homeColors} disabled={!ready || commandBusy} /></View>
    </View> : null}
  </View>;

  const emptyList = searching ? <View style={styles.emptySearch}>
    <Icon name="search" color={homeColors.muted} size={28} />
    <Text style={[styles.emptyTitle, { color: homeColors.text }]}>No matching workspaces</Text>
    <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Try another name or clear your search.</Text>
    <Pressable accessibilityRole="button" accessibilityLabel="Clear workspace search" onPress={() => setQuery('')} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Clear search</Text></Pressable>
  </View> : !attempted && profiles.length > 0 ? <View style={styles.savedHome}>
    <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>YOUR SERVERS</Text>
    {profiles.slice(0, 3).map(profile => <Pressable key={profile.id} accessibilityRole="button" accessibilityLabel={`Connect saved server ${profile.name}`} disabled={commandBusy} onPress={() => connectSavedProfile(profile)} style={({ pressed }) => [styles.savedHomeRow, { borderBottomColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Icon name="server" color={homeColors.muted} /><View style={styles.rowCopy}><Text numberOfLines={2} style={[styles.rowTitle, { color: homeColors.text }]}>{profile.name}</Text><Text numberOfLines={1} style={[styles.rowSubtitle, { color: homeColors.muted }]}>{profile.username}@{profile.host}</Text></View><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>)}
    <Button label="Saved servers" colors={homeColors} secondary onPress={() => openSheet('servers')}>Manage servers</Button>
    <Pressable accessibilityRole="button" accessibilityLabel="Connect" onPress={() => openProfileForm()} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Connect to another server</Text></Pressable>
  </View> : !attempted && profilesLoading ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>Loading your servers…</Text></View> : !attempted ? <View style={styles.firstUse}>
    <Companion dark={homeColors === DARK} />
    <Text style={[styles.firstUseTitle, { color: homeColors.text }]}>Your workspace. Anywhere.</Text>
    <Text style={[styles.firstUseBody, { color: homeColors.muted }]}>Connect to your server over SSH.{`\n`}Keep your work close.</Text>
    <Button label="Connect" colors={homeColors} onPress={openForm} style={styles.fullWidth}>Connect to a server</Button>
  </View> : ready ? <View style={styles.emptySearch}>
    <Text style={[styles.emptyTitle, { color: homeColors.text }]}>A fresh workspace starts here.</Text>
    <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Create a workspace to open your first terminal.{`\n`}It stays on your server when you leave.</Text>
    <Button label="Create workspace" colors={homeColors} onPress={() => openName({ kind: 'createWorkspace' })} disabled={commandBusy}>Create workspace</Button>
  </View> : presentation.pending ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>Loading workspaces…</Text></View> : null;

  if (foundation) return <SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: DARK.background }]}>
    <StatusBar barStyle="light-content" backgroundColor={DARK.background} />
    <View style={styles.terminalHeader}><Text style={[styles.foundationTitle, { color: DARK.text }]}>Native foundation preview</Text><IconButton icon="close" label="Close foundation preview" colors={DARK} onPress={() => setFoundation(false)} /></View>
    <TerminalView terminalId={CONNECTION_ID} fontSize={15} theme="dark" scrollbackLines={10000} style={styles.flex} />
  </SafeAreaView>;

  return <View style={[styles.flex, { backgroundColor: homeColors.background }]}>
    <StatusBar hidden={false} backgroundColor={colors.background} barStyle={colors === DARK ? 'light-content' : 'dark-content'} />
    <WorkspaceNavigation screen={screen} colors={homeColors} onScreenChange={next => { if (next === 'workspaces') Keyboard.dismiss(); setScreen(next); }}
      workspaces={<SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: homeColors.background }]}><FlatList
      key={searching ? 'workspace-search' : 'workspace-list'}
      ref={workspaceList}
      data={filteredWorkspaces}
      keyExtractor={item => item.id}
      renderItem={({ item }) => <View style={styles.horizontal}><WorkspaceRow connected={ready} workspace={item} selected={item.id === activeWorkspaceId} disabled={presentation.pending || commandBusy} optionsDisabled={!ready} colors={homeColors} onPress={() => openWorkspace(item)} onOptions={() => workspaceOptions(item)} /></View>}
      ListHeaderComponent={listHeader}
      ListEmptyComponent={emptyList}
      contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 20) + 24 }}
      contentOffset={{ x: 0, y: listOffsets.current[searching ? 'search' : 'normal'] }}
      onScroll={event => { listOffsets.current[searching ? 'search' : 'normal'] = event.nativeEvent.contentOffset.y; }}
      scrollEventThrottle={100}
      contentInsetAdjustmentBehavior="automatic"
      keyboardShouldPersistTaps="handled"
      keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'}
    /></SafeAreaView>}
      terminal={<SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: DARK.background }]}><View style={styles.flex}>
      <View style={styles.terminalHeader}>
        <IconButton icon="back" label="Back to workspaces" colors={DARK} onPress={backToWorkspaces} />
        <View style={styles.terminalHeading}>
          <Pressable accessibilityRole="button" accessibilityLabel="Switch workspace" accessibilityHint={workspace?.name} onPress={() => openSheet('workspaces')} style={({ pressed }) => [styles.terminalTitleRow, pressed && { opacity: .65 }]}><Text numberOfLines={1} style={[styles.terminalTitle, { color: DARK.text }]}>{workspace?.name ?? 'Workspaces'}</Text><Icon name="down" color={DARK.muted} size={12} /></Pressable>
          <View style={styles.terminalStatusRow}><Text numberOfLines={1} style={[styles.terminalHost, { color: DARK.muted }]}>{endpoint(connection)}</Text><ConnectionStatus connection={connection} colors={DARK} /></View>
        </View>
        <IconButton icon="menu" label="Terminal menu" colors={DARK} onPress={() => openSheet('server')} />
      </View>
      {groups.length > 1 ? <View style={styles.groupBar}>
        <Text style={[styles.groupLabel, { color: DARK.muted }]}>Group</Text>
        <Pressable accessibilityRole="button" accessibilityLabel="Switch terminal group" accessibilityHint={group?.name} disabled={!ready || commandBusy} onPress={() => openSheet('groups')} style={({ pressed }) => [styles.groupPicker, { backgroundColor: DARK.surface }, pressed && { opacity: .65 }]}>
          <Text numberOfLines={1} style={[styles.groupName, { color: DARK.text }]}>{group?.name || 'Choose a group'}</Text><Icon name="down" color={DARK.muted} size={12} />
        </Pressable>
      </View> : null}
      {workspace && groupPanes.length > 0 ? <View style={[styles.paneStrip, { borderBottomColor: DARK.border }]}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={styles.paneTabs}>
        {groupPanes.map((pane, index) => <Pressable key={pane.id} accessibilityRole="tab" accessibilityLabel={`Terminal ${pane.id}`} accessibilityHint={pane.name || `Terminal ${index + 1}`} accessibilityState={{ selected: pane.id === selectedPane?.id, disabled: !ready || commandBusy }} disabled={!ready || commandBusy} onPress={() => choosePane(pane)} onLongPress={() => { if (ready) openName({ kind: 'renamePane', pane }); }} style={({ pressed }) => [styles.paneTab, { borderBottomColor: pane.id === selectedPane?.id ? DARK.accent : 'transparent' }, pressed && { backgroundColor: DARK.surface }]}><Icon name="terminal" color={pane.id === selectedPane?.id ? DARK.accent : DARK.muted} size={15} /><Text numberOfLines={1} style={[styles.paneTabText, { color: pane.id === selectedPane?.id ? DARK.accent : DARK.muted }]}>{pane.name || `Terminal ${index + 1}`}</Text></Pressable>)}
      </ScrollView><IconButton icon="plus" label="Create terminal" colors={DARK} disabled={!ready || commandBusy} onPress={createPane} /></View> : null}
      {selectedPane?.agent ? <View style={styles.agentLine}>
        <Text numberOfLines={1} style={[styles.agentName, { color: DARK.muted }]}>{selectedPane.agent.name}</Text>
        <Text accessibilityHint="Status reported by Herdr. This does not verify task correctness or passing tests." style={[styles.agentStatus, { color: ready && selectedPane.agent.status === 'blocked' ? DARK.accent : DARK.muted }]}>{AGENT_LABELS[ready ? selectedPane.agent.status : 'unknown']}</Text>
      </View> : null}
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {ready && workspace && selectedPane ? (
        // Unmounting a surface cancels composition; the shared native registry
        // still owns the SSH connection and each terminal's retained state.
        sheet === null && !modalPending && !formVisible && !settingsVisible && !nameRequest && appState === 'active' ? <TerminalView key={selectedPane.terminalId} terminalId={selectedPane.terminalId} fontSize={preferences.fontSize} theme={resolvedTheme} scrollbackLines={preferences.scrollbackLines} style={styles.flex} /> : <View style={[styles.flex, { backgroundColor: DARK.terminal }]} />
      ) : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={[styles.terminalUnavailable, { paddingBottom: Math.max(insets.bottom, 24) }]}>
        {statusNotice}
        {ready ? <View style={styles.gone}>
          <Icon name="terminal" color={DARK.muted} size={32} />
          <Text accessibilityLabel="Terminal unavailable" style={[styles.emptyTitle, { color: DARK.text }]}>{workspace ? 'This terminal has closed' : 'This workspace has closed'}</Text>
          <Text style={[styles.emptyBody, { color: DARK.muted }]}>{workspace ? 'Select another terminal to keep working.' : 'Choose another workspace from the list.'}</Text>
          <Button label="Back to workspaces" colors={DARK} secondary onPress={backToWorkspaces}>Back to workspaces</Button>
        </View> : null}
      </ScrollView>}
    </View></SafeAreaView>}
    />

    <ConnectionForm visible={formVisible} initialProfile={formProfile} mode={formMode} colors={homeColors} onClose={finishConnectionForm} onDismiss={connectionFormDismissed} onSubmit={submitConnection} />
    <SettingsForm visible={settingsVisible} preferences={preferences} colors={homeColors} onClose={() => setSettingsVisible(false)} onSave={savePreferences} />
    <NameForm visible={nameRequest !== null} title={nameRequest?.kind === 'createWorkspace' ? 'Create workspace' : nameRequest?.kind === 'renameWorkspace' ? 'Rename workspace' : nameRequest?.kind === 'createGroup' ? 'Create group' : nameRequest?.kind === 'renameGroup' ? 'Rename group' : 'Rename terminal'} initialName={nameRequest?.kind === 'renameWorkspace' ? nameRequest.workspace.name : nameRequest?.kind === 'renamePane' ? nameRequest.pane.name : nameRequest?.kind === 'renameGroup' ? nameRequest.group.name : ''} colors={homeColors} onClose={() => setNameRequest(null)} onSave={saveName} />
    <NativeSheet title={sheet === 'groups' ? 'Switch group' : sheet === 'workspaces' ? 'Switch workspace' : sheet === 'handoff' ? 'Continue on your computer' : sheet === 'servers' ? 'Saved servers' : 'Server'} visible={sheet !== null} onClose={() => setSheet(null)} busy={commandBusy} onDismiss={() => { setHostPromptDeferred(false); setModalPending(false); const show = pendingModal.current; pendingModal.current = null; show?.(); }} colors={homeColors}>
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {sheet === 'servers' ? <ProfileList profiles={profiles} selectedId={profileId} loading={profilesLoading} error={profilesError} busy={commandBusy} colors={homeColors} onRetry={() => { void loadProfiles(); }} onAdd={() => openProfileForm(undefined, 'save')} onConnect={connectSavedProfile} onEdit={profile => openProfileForm(profile, 'save')} onDelete={deleteProfile} /> : sheet === 'workspaces' ? <View style={styles.flex}>
        <View style={styles.pickerHeader}><Text selectable style={[styles.emptyBody, { color: homeColors.muted }]}>{endpoint(connection)}</Text>{workspaces.length >= 6 ? <SearchField label="Search workspace picker" value={pickerQuery} onChange={setPickerQuery} colors={homeColors} /> : null}<Button label="Create workspace" colors={homeColors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createWorkspace' })}>Create workspace</Button></View>
        <FlatList data={pickerWorkspaces} keyExtractor={item => item.id} contentContainerStyle={styles.pickerList} renderItem={({ item }) => <WorkspaceRow connected={ready} workspace={item} selected={item.id === workspaceId} disabled={presentation.pending || commandBusy} optionsDisabled={!ready} colors={homeColors} picker onPress={() => openWorkspace(item)} onOptions={() => workspaceOptions(item)} />} ListEmptyComponent={<Text style={[styles.emptyBody, { color: homeColors.muted }]}>No matching workspaces.</Text>} keyboardShouldPersistTaps="handled" keyboardDismissMode="on-drag" />
      </View> : sheet === 'groups' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Keep related terminals together. Select a group to switch.</Text>
        {groups.map(item => <View key={item.id} style={[styles.groupRow, { borderColor: homeColors.border }]}>
          <Pressable accessibilityRole="button" accessibilityLabel={`Group ${item.name}`} accessibilityState={{ selected: item.id === group?.id, disabled: !ready || commandBusy }} disabled={!ready || commandBusy} onPress={() => chooseGroup(item)} style={({ pressed }) => [styles.groupChoice, pressed && { opacity: .65 }]}>
            <Text style={[styles.groupName, { color: item.id === group?.id ? homeColors.accent : homeColors.text }]}>{item.name || 'Untitled group'}</Text>
            <Text style={[styles.rowSubtitle, { color: homeColors.muted }]}>{panes.filter(pane => pane.groupId === item.id).length} {panes.filter(pane => pane.groupId === item.id).length === 1 ? 'terminal' : 'terminals'}{item.id === group?.id ? ' · Selected' : ''}</Text>
          </Pressable>
          <IconButton icon="menu" label={`Group options ${item.name}`} colors={homeColors} disabled={!ready || commandBusy} onPress={() => itemActions(item.name, () => openName({ kind: 'renameGroup', group: item }), () => closeGroup(item), 'workspace')} />
        </View>)}
        {workspace ? <Button label="Create group" colors={homeColors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Create group</Button> : null}
      </ScrollView> : sheet === 'handoff' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.handoffTitle, { color: homeColors.text }]}>Same work. Bigger screen.</Text>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Your workspace keeps running when you disconnect your phone.</Text>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: homeColors.accent }]}>1</Text><Text style={[styles.emptyBody, { color: homeColors.text, flex: 1 }]}>Disconnect this phone to release the workspace.</Text></View>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: homeColors.accent }]}>2</Text><Text style={[styles.emptyBody, { color: homeColors.text, flex: 1 }]}>SSH into the same server with the same username on your computer.</Text></View>
        <Text selectable style={[styles.command, { backgroundColor: homeColors.surface, color: homeColors.text }]}>{session.backend === 'herdr' ? `herdr --session ${session.runtime || 'default'}` : 'tmux attach -t meeterm'}</Text>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Run this command to reopen the same workspaces and terminals.</Text>
        {active ? <Button label="Disconnect" colors={homeColors} disabled={commandBusy} onPress={disconnect}>Disconnect this phone</Button> : <Button label="Close sheet" colors={homeColors} secondary onPress={() => setSheet(null)}>Close</Button>}
      </ScrollView> : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <View style={styles.serverDetails}>
          <Icon name="server" color={homeColors.accent} size={28} />
          <Text selectable style={[styles.serverDetailTitle, { color: homeColors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
          {currentProfile ? <Text selectable style={[styles.emptyBody, { color: homeColors.muted }]}>{currentProfile.username}@{endpoint(connection)}</Text> : null}
          <ConnectionStatus connection={connection} colors={homeColors} />
        </View>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Your work lives on this server. Disconnecting leaves it running.</Text>
        {canReconnect ? <Button label="Reconnect" colors={homeColors} disabled={commandBusy} onPress={reconnect}>Reconnect</Button> : null}
        {!active && !closing ? <Button label="Connect" colors={homeColors} secondary onPress={openForm}>Connection details</Button> : null}
        {active ? <Button label="Disconnect" colors={homeColors} secondary disabled={commandBusy} onPress={disconnect}>{ready ? 'Disconnect' : 'Cancel connection'}</Button> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="Saved servers" disabled={commandBusy} onPress={() => setSheet('servers')} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Switch server</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        <Pressable accessibilityRole="button" accessibilityLabel="Terminal settings" disabled={commandBusy} onPress={openSettings} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Settings</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        {screen === 'terminal' && workspace && selectedPane ? <View style={[styles.terminalActions, { borderColor: homeColors.border }]}>
          <Text numberOfLines={2} style={[styles.sectionLabel, { color: homeColors.muted }]}>{selectedPane.name || selectedPane.id}</Text>
          <Button label="Refresh terminal" colors={homeColors} secondary disabled={!ready || commandBusy} onPress={refreshTerminal}>Refresh terminal</Button>
          <Text style={[styles.noticeBody, { color: homeColors.muted }]}>Ask the remote app to redraw if the display looks wrong after reconnecting.</Text>
          <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename terminal" disabled={!ready || commandBusy} onPress={() => openName({ kind: 'renamePane', pane: selectedPane })} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Rename</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close terminal" disabled={!ready || commandBusy} onPress={closePane} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Close terminal</Text></Pressable>
          </View>
          <Pressable accessibilityRole="button" accessibilityLabel={`Workspace options ${workspace.name}`} disabled={!ready || commandBusy} onPress={() => workspaceOptions(workspace)} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Workspace options</Text></Pressable>
        </View> : null}
        {screen === 'terminal' && workspace && session.groupsSupported ? <View style={[styles.terminalActions, { borderColor: homeColors.border }]}>
          <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>Group</Text>
          <Button label="Create group" colors={homeColors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Create group</Button>
          {group ? <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename group" disabled={!ready || commandBusy} onPress={() => openName({ kind: 'renameGroup', group })} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Rename group</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close group" disabled={!ready || commandBusy} onPress={() => closeGroup(group)} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Close group</Text></Pressable>
          </View> : null}
        </View> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="PC handoff help" onPress={() => setSheet('handoff')} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Continue on your computer</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Review host key change</Text></Pressable> : null}
      </ScrollView>}
    </NativeSheet>
  </View>;
}

export default function App() {
  const smokeBuild = process.env.EXPO_PUBLIC_MEETERM_SMOKE === '1';
  const [smokeRoute, setSmokeRoute] = useState<SmokeRoute>(null);
  const [smokeRouteRevision, setSmokeRouteRevision] = useState(0);
  const [smokeRouteResolved, setSmokeRouteResolved] = useState(!smokeBuild);

  useEffect(() => {
    // This build flag and explicit launch URL are both required. Normal
    // installed-app launches always start at the real workspace hub.
    if (!smokeBuild) return;
    let launchEventReceived = false;
    const applyUrl = (url: string | null) => {
      const route = smokeRouteForUrl(url);
      // Ignore unrelated deep links. A valid smoke URL always increments the
      // key so reopening the same foundation or screen URL resets its state.
      if (route !== undefined) {
        setSmokeRoute(route);
        setSmokeRouteRevision(value => value + 1);
      }
      setSmokeRouteResolved(true);
    };
    void Linking.getInitialURL().then(url => {
      if (!launchEventReceived) applyUrl(url);
    }).catch(() => {
      if (!launchEventReceived) setSmokeRouteResolved(true);
    });
    const subscription = Linking.addEventListener('url', event => {
      launchEventReceived = true;
      applyUrl(event.url);
    });
    return () => subscription.remove();
  }, [smokeBuild]);

  if (!smokeRouteResolved) {
    // Do not mount AppContent while the smoke build is still resolving its
    // initial URL. This keeps profile/preferences/lifecycle effects out of a
    // seeded screenshot launch entirely.
    return <SafeAreaProvider><View style={styles.flex} /></SafeAreaProvider>;
  }

  const routeKey = smokeRoute?.kind === 'screen'
    ? `smoke-${smokeRoute.screen}`
    : smokeRoute?.kind === 'foundation' ? 'foundation' : 'normal';
  return <SafeAreaProvider><AppContent key={`${routeKey}-${smokeRouteRevision}`} smokeRoute={smokeRoute} /></SafeAreaProvider>;
}

const styles = StyleSheet.create({
  flex: { flex: 1 },
  groupBar: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 20, paddingBottom: 4 },
  groupLabel: { fontSize: 12, fontWeight: '500' },
  groupPicker: { flexShrink: 1, minHeight: 44, flexDirection: 'row', alignItems: 'center', gap: 10, paddingHorizontal: 14, borderRadius: 10, borderCurve: 'continuous' },
  groupName: { fontSize: 15, lineHeight: 22, fontWeight: '500', flexShrink: 1 },
  groupRow: { flexDirection: 'row', alignItems: 'center', borderBottomWidth: StyleSheet.hairlineWidth },
  groupChoice: { flex: 1, minHeight: 64, paddingVertical: 12, gap: 4 },
  agentLine: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', gap: 12, paddingHorizontal: 20, paddingVertical: 5 },
  agentName: { fontSize: 12, lineHeight: 18, flexShrink: 1 },
  agentStatus: { fontSize: 12, lineHeight: 18 },
  horizontal: { paddingHorizontal: 24 },
  brandRow: { minHeight: 56, paddingHorizontal: 24, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', gap: 16 },
  topActions: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  brand: { fontSize: 22, letterSpacing: -.8, fontWeight: '600' },
  hero: { paddingHorizontal: 24, paddingTop: 12, paddingBottom: 16, minHeight: 128, flexDirection: 'row', alignItems: 'center', gap: 4 },
  heroCopy: { flex: 1, minWidth: 0 },
  heroTitle: { fontSize: 30, lineHeight: 38, fontWeight: '700', letterSpacing: -1 },
  heroDescription: { fontSize: 13, lineHeight: 20, marginTop: 8 },
  serverRow: { marginHorizontal: 24, marginBottom: 16, minHeight: 60, paddingVertical: 12, flexDirection: 'row', alignItems: 'center', gap: 8, borderBottomWidth: StyleSheet.hairlineWidth },
  serverName: { flex: 1, minWidth: 0, fontSize: 15 },
  serverTarget: { flex: 1, minWidth: 0, minHeight: 44, flexDirection: 'row', alignItems: 'center', gap: 8 },
  status: { flexDirection: 'row', alignItems: 'center', gap: 6, flexShrink: 1 },
  statusDot: { width: 6, height: 6, borderRadius: 3 },
  statusText: { fontSize: 11, lineHeight: 18, flexShrink: 1 },
  sectionHeader: { paddingLeft: 24, paddingRight: 16, minHeight: 48, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
  sectionLabel: { fontSize: 13, lineHeight: 20, fontVariant: ['tabular-nums'] },
  workspaceContainer: { flexDirection: 'row', alignItems: 'center', gap: 4, borderBottomWidth: StyleSheet.hairlineWidth },
  workspaceRow: { flex: 1, minWidth: 0, minHeight: 88, paddingVertical: 20, flexDirection: 'row', alignItems: 'center', gap: 14 },
  rowCopy: { flex: 1, minWidth: 0, gap: 4 },
  rowTitle: { fontSize: 18, lineHeight: 26, fontWeight: '600', letterSpacing: -.3 },
  rowSubtitle: { fontSize: 13, lineHeight: 20, fontVariant: ['tabular-nums'] },
  firstUse: { paddingHorizontal: 28, paddingTop: 40, alignItems: 'center', gap: 20 },
  firstUseTitle: { fontSize: 30, lineHeight: 38, fontWeight: '700', letterSpacing: -1, marginTop: 8, textAlign: 'center' },
  firstUseBody: { fontSize: 15, lineHeight: 28, textAlign: 'center' },
  savedHome: { paddingHorizontal: 24, gap: 20 },
  savedHomeTitle: { fontSize: 18, lineHeight: 28, fontWeight: '600', marginBottom: 4 },
  savedHomeRow: { minHeight: 88, paddingVertical: 16, flexDirection: 'row', alignItems: 'center', gap: 16, borderBottomWidth: StyleSheet.hairlineWidth },
  fullWidth: { alignSelf: 'stretch', marginTop: 8 },
  emptySearch: { padding: 32, gap: 12, alignItems: 'center' },
  emptyTitle: { fontSize: 17, lineHeight: 27, fontWeight: '600', textAlign: 'center' },
  emptyBody: { fontSize: 15, lineHeight: 24 },
  loading: { padding: 32, gap: 12, alignItems: 'center' },
  searchHeader: { paddingHorizontal: 24, paddingTop: 24, paddingBottom: 20, flexDirection: 'row', alignItems: 'center', gap: 12 },
  searchTitle: { fontSize: 23, lineHeight: 32, fontWeight: '600', letterSpacing: -.6 },
  searchHost: { fontSize: 12, lineHeight: 20, marginTop: 8 },
  searchField: { minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 10, paddingLeft: 12, paddingRight: 4, borderWidth: 1, borderRadius: 12, borderCurve: 'continuous' },
  searchInput: { minWidth: 0, flex: 1, fontSize: 16, paddingVertical: 12 },
  resultCount: { paddingTop: 20, paddingBottom: 8, fontSize: 12, fontVariant: ['tabular-nums'] },
  textAction: { minHeight: 44, justifyContent: 'center', paddingHorizontal: 4 },
  actionText: { fontSize: 14, lineHeight: 23, fontWeight: '500' },
  notice: { padding: 16, borderRadius: 12, borderCurve: 'continuous', gap: 8, marginBottom: 12 },
  noticeTitle: { fontSize: 15, lineHeight: 24, fontWeight: '600' },
  noticeBody: { fontSize: 13, lineHeight: 22 },
  noticeActions: { flexDirection: 'row', flexWrap: 'wrap', gap: 12, alignItems: 'center' },
  feedback: { padding: 12, borderRadius: 12, borderCurve: 'continuous', gap: 4, marginBottom: 12, flexDirection: 'row', alignItems: 'center' },
  terminalHeader: { minHeight: 64, flexDirection: 'row', paddingHorizontal: 8, gap: 4, alignItems: 'center' },
  terminalHeading: { flex: 1, minWidth: 0, alignItems: 'center', justifyContent: 'center', paddingTop: 0, paddingBottom: 8 },
  terminalTitleRow: { minHeight: 44, alignSelf: 'stretch', flexDirection: 'row', alignItems: 'center', justifyContent: 'center', gap: 6 },
  terminalTitle: { fontSize: 17, lineHeight: 25, fontWeight: '600', flexShrink: 1 },
  terminalStatusRow: { flexDirection: 'row', justifyContent: 'center', alignItems: 'center', gap: 8 },
  terminalHost: { fontSize: 11, lineHeight: 18, flexShrink: 1 },
  paneStrip: { borderBottomWidth: StyleSheet.hairlineWidth, flexDirection: 'row', alignItems: 'center', paddingRight: 4 },
  paneTabs: { paddingHorizontal: 12, gap: 4 },
  paneTab: { minHeight: 48, paddingHorizontal: 12, borderBottomWidth: 2, flexDirection: 'row', alignItems: 'center', gap: 6 },
  paneTabText: { fontSize: 14, lineHeight: 23, maxWidth: 200 },
  terminalFeedback: { paddingHorizontal: 12, paddingTop: 8 },
  terminalActions: { gap: 12, paddingTop: 20, borderTopWidth: StyleSheet.hairlineWidth },
  terminalUnavailable: { flexGrow: 1, padding: 24 },
  gone: { flex: 1, justifyContent: 'center', alignItems: 'center', gap: 20, paddingVertical: 32 },
  sheetHeader: { minHeight: 64, paddingLeft: 24, paddingRight: 12, flexDirection: 'row', alignItems: 'center', gap: 12, borderBottomWidth: StyleSheet.hairlineWidth },
  sheetTitle: { flex: 1, fontSize: 18, lineHeight: 28, fontWeight: '600' },
  sheetContent: { padding: 24, gap: 20 },
  serverDetails: { alignItems: 'flex-start', gap: 12, paddingBottom: 4 },
  serverDetailTitle: { fontSize: 23, lineHeight: 32, fontWeight: '600' },
  menuRow: { minHeight: 56, paddingVertical: 12, flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center', borderTopWidth: StyleSheet.hairlineWidth },
  pickerHeader: { padding: 24, gap: 16 },
  pickerList: { paddingHorizontal: 24, paddingBottom: 24 },
  handoffTitle: { fontSize: 24, lineHeight: 36, fontWeight: '600', letterSpacing: -.6 },
  handoffStep: { flexDirection: 'row', gap: 14, alignItems: 'flex-start' },
  stepNumber: { fontSize: 15, lineHeight: 25, fontVariant: ['tabular-nums'] },
  command: { padding: 16, fontFamily: MONO, fontSize: 16, lineHeight: 26, borderRadius: 12, borderCurve: 'continuous' },
  foundationTitle: { flex: 1, fontSize: 16, paddingLeft: 12 },
});
