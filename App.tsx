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
import type { ConnectionSubmission } from './app/ConnectionForm';
import { DEFAULT_PREFERENCES, itemActions, NameForm, ProfileList, SettingsForm } from './app/DailyUse';
import { Button, Companion, DARK, Icon, IconButton, MONO, usePalette } from './app/ui';
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
type SmokeScreen = 'herdr-connection' | 'herdr-groups' | 'herdr-terminal' | 'herdr-workspaces' | 'home' | 'servers' | 'connection' | 'password' | 'workspaces' | 'terminal' | 'settings' | 'workspace-name' | 'terminal-name' | 'handoff';
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
};

function smokeReadyConnection(): SshConnectionState {
  return { ...INITIAL_CONNECTION, state: 'Ready', host: SMOKE_PROFILE.host, port: SMOKE_PROFILE.port };
}

function smokePanes(): RemoteTerminal[] { return SMOKE_PANES.map(pane => ({ ...pane })); }

function smokeWorkspace(panes: RemoteTerminal[], workspaceId: string): Workspace {
  return { id: workspaceId, name: workspaceId === '@smoke-main' ? 'Main workspace' : 'Tools workspace', panes: panes.filter(pane => pane.workspaceId === workspaceId) };
}

function smokeFixture(screen: SmokeScreen): SmokeFixtureState {
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
function smokeWorkspaceState(panes: RemoteTerminal[], herdr = false): WorkspaceState {
  const ids = [...new Set(panes.map(pane => pane.workspaceId))];
  return { ...EMPTY_WORKSPACES, terminals: panes,
    workspaces: ids.map(id => ({ id, name: id === '@smoke-main' ? 'Main workspace' : 'Tools workspace' })),
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
    case 'Ready': return { label: '接続中', accessibility: 'Connected', pending: false };
    case 'Connecting': return { label: '接続しています…', accessibility: 'Connecting…', pending: true };
    case 'HostKeyPending': return { label: 'ホスト鍵を確認', accessibility: 'Verify host key', pending: true };
    case 'Authenticating': return { label: '認証しています…', accessibility: 'Authenticating…', pending: true };
    case 'OpeningPty': return { label: 'ターミナルを準備中…', accessibility: 'Opening terminal…', pending: true };
    case 'AttachingTmux': return { label: 'ワークスペースに接続中…', accessibility: 'Opening workspace…', pending: true };
    case 'Synchronizing': return { label: 'ターミナルを復元中…', accessibility: 'Restoring terminals…', pending: true };
    case 'Reconnecting': return { label: '再接続しています…', accessibility: 'Reconnecting…', pending: true };
    case 'Closing': return { label: '切断しています…', accessibility: 'Disconnecting…', pending: true };
    case 'Failed': return { label: '接続できませんでした', accessibility: 'Connection failed', pending: false };
    default: return { label: '未接続', accessibility: 'Not connected', pending: false };
  }
}
function connectionError(connection: SshConnectionState) {
  const herdrErrors: Record<string, string> = {
    herdr_missing: 'SSHの接続先でHerdrが見つかりません。PCで使っているHerdrを、SSHからも実行できるか確認してください。',
    herdr_session_missing: '指定したHerdrセッションが起動していません。PCでそのセッションを開いてから、接続し直してください。',
    herdr_incompatible: 'このHerdrは対応する接続機能を確認できませんでした。検証済みはHerdr 0.9.0（protocol 22）です。',
    herdr_unsupported: 'このHerdrでは、接続に必要な公開APIまたは状態の通知機能を利用できません。接続先のHerdrの機能を確認してください。',
    herdr_forwarding: 'SSH経由でHerdrにアクセスできません。SSHサーバーでUnix socket転送（AllowStreamLocalForwarding）が許可されているか確認してください。',
    herdr_controller_busy: 'このターミナルは別の接続で操作中です。そちらの操作権を解放してから、再接続してください。',
    herdr_protocol: 'Herdrの応答を読み取れませんでした。接続先のバージョンとセッションを確認してください。',
    herdr_operation: 'Herdrが操作を受け付けませんでした。再接続して、現在のワークスペースを確認してください。',
    herdr_workspace_group: '関連するワークスペースも終了する可能性があるため、この親ワークスペースの終了操作はPCのHerdrで対象を確認して行ってください。',
  };
  if (herdrErrors[connection.errorCode]) return herdrErrors[connection.errorCode];
  if (connection.errorCode === 'host_key_changed') return '保存したホスト鍵と一致しません。サーバーの本人確認が必要です。';
  if (connection.errorCode === 'host_key_rejected') return 'ホスト鍵の確認をキャンセルしました。接続するには、もう一度確認してください。';
  if (connection.errorCode.includes('private_key')) return '秘密鍵を読み込めませんでした。鍵の形式とパスフレーズを確認してください。';
  if (connection.errorCode.includes('auth')) return '認証できませんでした。ユーザー名と、選択した認証方式のパスワードまたは秘密鍵を確認してください。';
  return connection.errorMessage || 'サーバーに接続できませんでした。接続先とネットワークを確認してください。';
}

function ConnectionStatus({ connection, colors }: { connection: SshConnectionState; colors: Palette }) {
  const state = connectionPresentation(connection);
  return <View style={styles.status}>
    {state.pending ? <ActivityIndicator size="small" color={colors.accent} /> : <View style={[styles.statusDot, { backgroundColor: connection.state === 'Ready' ? colors.accent : colors.muted }]} />}
    <Text accessibilityLabel={state.accessibility} accessibilityLiveRegion="polite" style={[styles.statusText, { color: colors.muted }]}>{state.label}</Text>
  </View>;
}

const AGENT_LABELS = { working: '作業中', blocked: '確認待ち', done: '応答完了', idle: '待機中', unknown: '状態未確認' };
function agentSummary(panes: RemoteTerminal[], connected: boolean) {
  const agents = panes.flatMap(pane => pane.agent ? [pane.agent] : []);
  if (!agents.length) return '';
  if (!connected) return `状態未確認 ${agents.length}`;
  const statuses = ['blocked', 'working', 'done', 'idle', 'unknown'] as const;
  return statuses.flatMap(status => {
    const count = agents.filter(agent => agent.status === status).length;
    return count ? [`${AGENT_LABELS[status]} ${count}`] : [];
  }).join(' · ');
}

function SearchField({ value, onChange, colors, label = 'Search workspaces', autoFocus = false }: { value: string; onChange: (value: string) => void; colors: Palette; label?: string; autoFocus?: boolean }) {
  return <View style={[styles.searchField, { backgroundColor: colors.surface, borderColor: colors.border }]}>
    <Icon name="search" color={colors.muted} size={18} />
    <TextInput accessibilityLabel={label} autoFocus={autoFocus} autoCorrect={false} autoCapitalize="none" placeholder="ワークスペース名で検索" placeholderTextColor={colors.placeholder} selectionColor={colors.accent} returnKeyType="search" onSubmitEditing={Keyboard.dismiss} style={[styles.searchInput, { color: colors.text }]} value={value} onChangeText={onChange} />
    {value ? <IconButton icon="close" label="Clear workspace search" onPress={() => onChange('')} colors={colors} /> : null}
  </View>;
}

function WorkspaceRow({ workspace, selected, colors, onPress, onOptions, picker = false, disabled = false, optionsDisabled = false, connected = false }: { workspace: Workspace; selected: boolean; colors: Palette; onPress: () => void; onOptions?: () => void; picker?: boolean; disabled?: boolean; optionsDisabled?: boolean; connected?: boolean }) {
  return <View style={[styles.workspaceContainer, { borderBottomColor: colors.border }]}><Pressable testID={`workspace-row-${workspace.id}`} accessibilityRole="button" accessibilityLabel={`Workspace ${workspace.name}`} accessibilityHint={`${workspace.panes.length} terminals`} accessibilityState={{ selected, disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [styles.workspaceRow, pressed && { backgroundColor: colors.surface }, disabled && { opacity: .5 }]}>
    <Icon name="terminal" color={colors.muted} size={23} />
    <View style={styles.rowCopy}>
      <Text numberOfLines={picker ? undefined : 2} style={[styles.rowTitle, { color: colors.text }]}>{workspace.name}</Text>
      <Text style={[styles.rowSubtitle, { color: colors.muted }]}>{workspace.panes.length} ターミナル</Text>
      {agentSummary(workspace.panes, connected) ? <Text style={[styles.rowSubtitle, { color: colors.muted }]}>{agentSummary(workspace.panes, connected)}</Text> : null}
    </View>
    <Icon name={selected ? 'check' : 'chevron'} color={selected ? colors.accent : colors.muted} size={18} />
  </Pressable>{onOptions ? <IconButton icon="menu" label={`Workspace options ${workspace.name}`} onPress={onOptions} disabled={disabled || optionsDisabled} colors={colors} /> : null}</View>;
}

function NativeSheet({ title, visible, onClose, onDismiss, busy, colors, children }: { title: string; visible: boolean; onClose: () => void; onDismiss: () => void; busy: boolean; colors: Palette; children: ReactNode }) {
  return <Modal visible={visible} animationType="slide" presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} allowSwipeDismissal={!busy} onRequestClose={() => { if (!busy) onClose(); }} onDismiss={onDismiss} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
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
  const [session, setSession] = useState<WorkspaceState>(() => fixture ? smokeWorkspaceState(fixture.panes, smokeScreen?.startsWith('herdr-')) : EMPTY_WORKSPACES);
  const panes = session.terminals;
  const [screen, setScreen] = useState<'workspaces' | 'terminal'>(() => fixture?.screen ?? 'workspaces');
  const [workspaceId, setWorkspaceId] = useState(() => fixture?.workspaceId ?? '');
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
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState('');
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
      setControlMessage('設定を読み込めませんでした。「設定」から読み込みをやり直せます。');
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
        .catch(() => setControlMessage('アプリの状態を接続に反映できませんでした。接続を確認してください。'));
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
        setControlMessage('ホスト鍵への回答を送れませんでした。接続をやり直してください。');
      });
    };
    Alert.alert('Trust this SSH host?', `${connection.host}:${connection.port}\n\nAlgorithm: ${connection.algorithm || '(unavailable)'}\nSHA256 fingerprint:\n${connection.fingerprint}\n\n信頼できる別の経路で、この指紋がサーバーのものか確認してください。承認したホスト鍵は端末に保存されます。`, [
      { text: 'Cancel', style: 'cancel', onPress: () => respond(false) },
      { text: 'Trust and connect', onPress: () => respond(true) },
    ], { cancelable: false });
  }, [connection, formVisible, hostPromptDeferred, smokeFixtureActive]);

  const workspaces = useMemo(() => session.workspaces.map(workspace => ({ ...workspace, panes: panes.filter(pane => pane.workspaceId === workspace.id) })), [session.workspaces, panes]);
  const workspace = workspaces.find(item => item.id === workspaceId);
  const groups = session.groups.filter(group => group.workspaceId === workspaceId);
  const group = groups.find(group => group.selected) ?? groups[0];
  const groupPanes = workspace?.panes.filter(pane => pane.groupId === group?.id) ?? [];
  const chosenPaneId = selectedPaneIds[group?.id ?? ''];
  const selectedPane = groupPanes.find(pane => pane.selected)
      ?? groupPanes.find(pane => pane.id === chosenPaneId)
      ?? groupPanes.find(pane => pane.active)
      ?? groupPanes[0];
  const activeWorkspaceId = panes.find(pane => pane.selected)?.workspaceId
    ?? session.groups.find(group => group.workspaceId === workspaceId && group.selected)?.workspaceId;
  const colors = homeColors;
  const resolvedTheme = homeColors === DARK ? 'dark' : 'light';
  const currentProfile = profiles.find(profile => profile.id === profileId);
  const presentation = connectionPresentation(connection);
  const ready = connection.state === 'Ready';
  useEffect(() => {
    if (smokeFixtureActive) return;
    const visible = screen === 'terminal' && sheet === null && !modalPending && !formVisible && !settingsVisible && !nameRequest && appState === 'active';
    foregroundCommands.current = foregroundCommands.current
      .then(() => MeetermTerminal.setTerminalVisible(CONNECTION_ID, visible))
      .catch(() => setControlMessage('ターミナルの表示状態を接続へ反映できませんでした。再接続してください。'));
  }, [screen, sheet, modalPending, formVisible, settingsVisible, nameRequest, appState, smokeFixtureActive]);
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
    }, 'サーバーの保存または接続を開始できませんでした。接続先と認証情報を確認してください。');
    if (success) finishConnectionForm();
    return success;
  }, [finishConnectionForm, prepareConnection, profileId, resetForConnection, runCommand]);

  const disconnect = useCallback(() => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Closing' }));
    void runCommand(() => MeetermTerminal.disconnect(CONNECTION_ID), '切断の要求を送れませんでした。もう一度試してください。').then(success => {
      if (!success) setConnection(previous);
    });
  }, [connection, runCommand]);

  const reconnect = useCallback(() => {
    if (commandPending.current) return;
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Reconnecting', errorCode: '', errorMessage: '' }));
    void runCommand(() => MeetermTerminal.reconnect(CONNECTION_ID), '再接続を開始できませんでした。「接続情報を入力」から認証情報を入力し直してください。').then(success => {
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
    const success = await runCommand(() => MeetermTerminal.selectPane(CONNECTION_ID, pane.id), 'ターミナルを選択できませんでした。一覧を確認して、もう一度選んでください。');
    if (!success) setSelectedPaneIds(current => {
        const next = { ...current };
        if (previous) next[pane.groupId] = previous; else delete next[pane.groupId];
        return next;
      });
    return success;
  }, [connection.state, runCommand, selectedPaneIds]);

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
      void runCommand(() => MeetermTerminal.selectGroup(CONNECTION_ID, chosenGroup.id), 'Groupを選択できませんでした。').then(success => {
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
      }, '保存済みサーバーに接続できませんでした。「編集」から接続先と認証情報を確認してください。');
    };
    if (active && profile.id !== profileId) {
      Alert.alert('接続先を切り替えますか？', '現在の接続を切断して、選んだサーバーに接続します。サーバー上の作業は続きます。', [
        { text: 'キャンセル', style: 'cancel' }, { text: '切り替える', onPress: connect },
      ]);
    } else if (active && profile.id === profileId) {
      setSheet(null);
    } else connect();
  }, [active, openProfileForm, prepareConnection, profileId, resetForConnection, runCommand]);

  const deleteProfile = useCallback((profile: ServerProfile) => {
    Alert.alert('保存済みサーバーを削除しますか？', `${profile.name}\n\nこの端末の接続先と保存済み認証情報を削除します。サーバー上の作業は残ります。`, [
      { text: 'キャンセル', style: 'cancel' },
      { text: '削除', style: 'destructive', onPress: () => {
        void runCommand(async () => {
          await MeetermTerminal.deleteProfile(profile.id);
          setProfiles(current => current.filter(item => item.id !== profile.id));
          if (profile.id === profileId) setProfileId('');
        }, '保存済みサーバーを削除できませんでした。もう一度試してください。');
      } },
    ]);
  }, [profileId, runCommand]);

  const openSettings = useCallback(() => {
    if (commandPending.current) return;
    if (smokeFixtureActive) {
      setSettingsVisible(true);
      return;
    }
    if (!preferencesLoaded) {
      void runCommand(async () => {
        const next = await MeetermTerminal.getPreferences();
        await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, next.automaticReconnect);
        setPreferences(next);
        setPreferencesLoaded(true);
        showModal(() => setSettingsVisible(true));
      }, '設定を読み込めませんでした。もう一度試してください。');
      return;
    }
    showModal(() => setSettingsVisible(true));
  }, [preferencesLoaded, runCommand, showModal, smokeFixtureActive]);

  const savePreferences = useCallback(async (next: TerminalPreferences) => {
    const success = await runCommand(async () => {
      await MeetermTerminal.setPreferences(next);
      setPreferences(next);
      await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, next.automaticReconnect);
    }, '設定を保存または接続に反映できませんでした。設定画面からもう一度保存してください。');
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
            : MeetermTerminal.renamePane(CONNECTION_ID, request.pane.id, name), '名前を反映できませんでした。接続状態を確認して、もう一度試してください。');
    if (success) setNameRequest(null);
    return success;
  }, [nameRequest, ready, runCommand]);

  const closeWorkspace = useCallback((item: Workspace) => {
    Alert.alert('ワークスペースを終了しますか？', `${item.name}\n\n${item.panes.length}個のターミナルと、その中で実行中のプロセスを終了します。保存していない作業は失われます。`, [
      { text: 'キャンセル', style: 'cancel' },
      { text: '終了', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closeWorkspace(CONNECTION_ID, item.id), 'ワークスペースを終了できませんでした。接続状態を確認してください。').then(success => {
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
      setControlMessage('このGroupには追加の元になるターミナルがありません。メニューから新しいGroupを作成してください。');
      return;
    }
    void runCommand(() => MeetermTerminal.createPane(CONNECTION_ID, workspace.id), 'ターミナルを作成できませんでした。接続状態を確認してください。').then(success => {
      if (success) {
        setSelectedPaneIds(current => { const next = { ...current }; if (group) delete next[group.id]; return next; });
        setSheet(null);
      }
    });
  }, [ready, runCommand, workspace, group, session.groupsSupported, groupPanes.length]);

  const closePane = useCallback(() => {
    if (!selectedPane || !workspace) return;
    const pane = selectedPane;
    const consequence = workspace.panes.length === 1 ? '最後のターミナルのため、ワークスペースも終了します。'
      : session.groupsSupported && groupPanes.length === 1 ? 'このGroupの最後のターミナルのため、Groupも終了します。' : '';
    Alert.alert('ターミナルを終了しますか？', `${pane.name || pane.id}\n\n実行中のプロセスを終了します。保存していない作業は失われます。${consequence}`, [
      { text: 'キャンセル', style: 'cancel' },
      { text: '終了', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closePane(CONNECTION_ID, pane.id), 'ターミナルを終了できませんでした。接続状態を確認してください。').then(success => {
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
    const selection = remembered ? choosePane(remembered) : runCommand(() => MeetermTerminal.selectGroup(CONNECTION_ID, item.id), 'Groupを選択できませんでした。一覧を確認してください。');
    void selection.then(success => { if (success) setSheet(null); });
  }, [ready, runCommand, panes, selectedPaneIds, choosePane]);

  const closeGroup = useCallback((item: TerminalGroup) => {
    const terminals = panes.filter(pane => pane.groupId === item.id);
    const last = session.groups.filter(group => group.workspaceId === item.workspaceId).length === 1;
    Alert.alert('Groupを終了しますか？', `${item.name}\n\n${terminals.length}個のターミナルと、その中のプロセスを終了します。保存していない作業は失われます。${last ? '最後のGroupのため、ワークスペースも終了します。' : ''}`, [
      { text: 'キャンセル', style: 'cancel' },
      { text: '終了', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closeGroup(CONNECTION_ID, item.id), 'Groupを終了できませんでした。接続状態を確認してください。').then(success => {
          if (success) { setSheet(null); if (last) backToWorkspaces(); }
        });
      } },
    ]);
  }, [panes, session.groups, runCommand, backToWorkspaces]);

  const refreshTerminal = useCallback(() => {
    void runCommand(() => MeetermTerminal.refreshTerminal(CONNECTION_ID), '画面の再描画を要求できませんでした。接続状態を確認してください。').then(success => { if (success) setSheet(null); });
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
    Alert.alert('ホスト鍵が変更されています', `${connection.host}:${connection.port}\n\nAlgorithm: ${connection.algorithm || '(unavailable)'}\n\n保存された指紋:\n${connection.knownFingerprint || '(unavailable)'}\n\n受信した指紋:\n${connection.fingerprint || '(unavailable)'}\n\nサーバーの再構築か、通信のなりすましの可能性があります。管理者に別の信頼できる経路で確認した場合だけ、保存済みの鍵を削除してください。`, [
      { text: 'キャンセル', style: 'cancel' },
      { text: '保存した鍵を削除', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.forgetHostKey(connection.host, connection.port), '保存したホスト鍵を削除できませんでした。').then(success => {
          if (success) {
            setRemovedHostKeyId(changeId);
            setControlMessage('保存した鍵を削除しました。接続情報を入力し直して、新しいホスト鍵を確認してください。');
          }
        });
      } },
    ], { cancelable: false });
  }, [connection, removedHostKeyId, runCommand]);

  const statusNotice = attempted && !ready ? <View style={[styles.notice, { backgroundColor: colors.surface }]}>
    <Text style={[styles.noticeTitle, { color: colors.text }]}>{connection.state === 'Failed' && connection.errorCode === 'host_key_changed' ? 'ホストの本人確認が必要です' : connection.state === 'Disconnected' ? 'サーバーから切断しています' : presentation.label}</Text>
    <Text style={[styles.noticeBody, { color: colors.muted }]}>{connection.state === 'Failed' ? connectionError(connection) : connection.state === 'Disconnected' ? hasConnected ? 'サーバー上の作業は続いています。再接続して同じ場所へ戻れます。' : '接続を開始するには、接続情報を入力してください。' : closing ? hasConnected ? 'サーバー上の作業を残して、接続を閉じています。' : '接続をキャンセルしています。' : 'サーバー上のワークスペースを確認しています。'}</Text>
    <View style={styles.noticeActions}>
      {canReconnect ? <Button label="Reconnect" colors={colors} disabled={commandBusy} onPress={reconnect}>再接続</Button> : null}
      {!active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Connect" onPress={openForm} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>接続情報を入力</Text></Pressable> : null}
      {active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Cancel connection" disabled={commandBusy} onPress={disconnect} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>キャンセル</Text></Pressable> : null}
      {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>鍵の変更を確認</Text></Pressable> : null}
    </View>
  </View> : null;

  const feedback = controlMessage || pollProblem ? <View accessibilityLiveRegion="polite" style={[styles.feedback, { backgroundColor: colors.surface }]}>
    <Text style={[styles.noticeBody, { color: colors.danger, flex: 1 }]}>{controlMessage || '接続状態を取得できません。しばらくしてから接続をやり直してください。'}</Text>
    {controlMessage ? <IconButton icon="close" label="Dismiss message" colors={colors} onPress={() => setControlMessage('')} /> : null}
  </View> : null;

  const listHeader = <View>
    {searching ? <View style={styles.searchHeader}>
      <View style={styles.flex}>
        <Text accessibilityRole="header" style={[styles.searchTitle, { color: homeColors.text }]}>ワークスペースを探す</Text>
        <Text numberOfLines={1} style={[styles.searchHost, { color: homeColors.muted }]}>{endpoint(connection)}</Text>
      </View>
      <Pressable accessibilityRole="button" accessibilityLabel="Close workspace search" onPress={closeSearch} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>閉じる</Text></Pressable>
    </View> : <>
      <View style={styles.brandRow}><Text style={[styles.brand, { color: homeColors.text }]}>meeterm</Text><View style={styles.topActions}><IconButton icon="server" label="Saved servers" colors={homeColors} disabled={commandBusy} onPress={() => openSheet('servers')} /><Pressable accessibilityRole="button" accessibilityLabel="Terminal settings" testID="open-settings" disabled={commandBusy} onPress={openSettings} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>設定</Text></Pressable></View></View>
      <View style={styles.hero}>
        <View style={styles.heroCopy}>
          <Text accessibilityRole="header" style={[styles.heroTitle, width < 360 && { fontSize: 24 }, { color: homeColors.text }]}>ワークスペース</Text>
          {attempted ? <Text style={[styles.heroDescription, { color: homeColors.muted }]}>作業を選んで、ターミナルへ。</Text> : null}
        </View>
        {attempted ? <Companion small dark={homeColors === DARK} /> : null}
      </View>
      {attempted ? <View style={[styles.serverRow, { borderBottomColor: homeColors.border }]}>
        <Pressable accessibilityRole="button" accessibilityLabel="Server connection" accessibilityHint={endpoint(connection)} onPress={() => openSheet('server')} style={({ pressed }) => [styles.serverTarget, pressed && { backgroundColor: homeColors.surface }]}>
          <Icon name="server" color={homeColors.muted} size={17} />
          <Text numberOfLines={1} style={[styles.serverName, { color: homeColors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
          <Icon name="down" color={homeColors.muted} size={12} />
        </Pressable>
        <ConnectionStatus connection={connection} colors={homeColors} />
      </View> : null}
    </>}
    {statusNotice ? <View style={styles.horizontal}>{statusNotice}</View> : null}
    {feedback ? <View style={styles.horizontal}>{feedback}</View> : null}
    {searching ? <View style={styles.horizontal}>
      <SearchField value={query} colors={homeColors} onChange={value => { setQuery(value); listOffsets.current.search = 0; workspaceList.current?.scrollToOffset({ offset: 0, animated: false }); }} autoFocus />
      <Text style={[styles.resultCount, { color: homeColors.muted }]}>{filteredWorkspaces.length} 件</Text>
    </View> : attempted && (workspaces.length > 0 || ready) ? <View style={styles.sectionHeader}>
      <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>すべて  {workspaces.length}</Text>
      <View style={styles.topActions}><IconButton icon="search" label="Search workspaces" onPress={() => setSearching(true)} colors={homeColors} /><IconButton icon="plus" label="Create workspace" onPress={() => openName({ kind: 'createWorkspace' })} colors={homeColors} disabled={!ready || commandBusy} /></View>
    </View> : null}
  </View>;

  const emptyList = searching ? <View style={styles.emptySearch}>
    <Icon name="search" color={homeColors.muted} size={28} />
    <Text style={[styles.emptyTitle, { color: homeColors.text }]}>見つかりませんでした</Text>
    <Text style={[styles.emptyBody, { color: homeColors.muted }]}>別の名前で検索してみてください。</Text>
    <Pressable accessibilityRole="button" accessibilityLabel="Clear workspace search" onPress={() => setQuery('')} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>検索をクリア</Text></Pressable>
  </View> : !attempted && profiles.length > 0 ? <View style={styles.savedHome}>
    <Text style={[styles.savedHomeTitle, { color: homeColors.text }]}>接続先を選んで、続きを。</Text>
    {profiles.slice(0, 3).map(profile => <Pressable key={profile.id} accessibilityRole="button" accessibilityLabel={`Connect saved server ${profile.name}`} disabled={commandBusy} onPress={() => connectSavedProfile(profile)} style={({ pressed }) => [styles.savedHomeRow, { borderBottomColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Icon name="server" color={homeColors.muted} /><View style={styles.rowCopy}><Text numberOfLines={2} style={[styles.rowTitle, { color: homeColors.text }]}>{profile.name}</Text><Text numberOfLines={1} style={[styles.rowSubtitle, { color: homeColors.muted }]}>{profile.username}@{profile.host}</Text></View><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>)}
    <Button label="Saved servers" colors={homeColors} secondary onPress={() => openSheet('servers')}>サーバーを管理</Button>
    <Pressable accessibilityRole="button" accessibilityLabel="Connect" onPress={() => openProfileForm()} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>別のサーバーに接続</Text></Pressable>
  </View> : !attempted && profilesLoading ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>接続先を読み込んでいます</Text></View> : !attempted ? <View style={styles.firstUse}>
    <Companion dark={homeColors === DARK} />
    <Text style={[styles.firstUseTitle, { color: homeColors.text }]}>いつものサーバーから。</Text>
    <Text style={[styles.firstUseBody, { color: homeColors.muted }]}>SSH の接続先を追加して、{`\n`}いつもの作業を手元に。</Text>
    <Button label="Connect" colors={homeColors} onPress={openForm} style={styles.fullWidth}>＋  サーバーに接続</Button>
  </View> : ready ? <View style={styles.emptySearch}>
    <Text style={[styles.emptyTitle, { color: homeColors.text }]}>ワークスペースがありません</Text>
    <Text style={[styles.emptyBody, { color: homeColors.muted }]}>ワークスペースを作って、{`\n`}ターミナルで作業を始めましょう。</Text>
    <Button label="Create workspace" colors={homeColors} onPress={() => openName({ kind: 'createWorkspace' })} disabled={commandBusy}>ワークスペースを作成</Button>
  </View> : presentation.pending ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>ワークスペースを取得しています</Text></View> : null;

  if (foundation) return <SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: DARK.background }]}>
    <StatusBar barStyle="light-content" backgroundColor={DARK.background} />
    <View style={styles.terminalHeader}><Text style={[styles.foundationTitle, { color: DARK.text }]}>Native foundation preview</Text><IconButton icon="close" label="Close foundation preview" colors={DARK} onPress={() => setFoundation(false)} /></View>
    <TerminalView terminalId={CONNECTION_ID} fontSize={15} theme="dark" scrollbackLines={10000} style={styles.flex} />
  </SafeAreaView>;

  return <SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: colors.background }]}>
    <StatusBar hidden={false} backgroundColor={colors.background} barStyle={colors === DARK ? 'light-content' : 'dark-content'} />
    {screen === 'workspaces' ? <FlatList
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
    /> : <View style={styles.flex}>
      <View style={styles.terminalHeader}>
        <IconButton icon="back" label="Back to workspaces" colors={colors} onPress={backToWorkspaces} />
        <View style={styles.terminalHeading}>
          <Pressable accessibilityRole="button" accessibilityLabel="Switch workspace" accessibilityHint={workspace?.name} onPress={() => openSheet('workspaces')} style={({ pressed }) => [styles.terminalTitleRow, pressed && { opacity: .65 }]}><Text numberOfLines={1} style={[styles.terminalTitle, { color: colors.text }]}>{workspace?.name ?? 'ワークスペース'}</Text><Icon name="down" color={colors.muted} size={12} /></Pressable>
          <View style={styles.terminalStatusRow}><Text numberOfLines={1} style={[styles.terminalHost, { color: colors.muted }]}>{endpoint(connection)}</Text><ConnectionStatus connection={connection} colors={colors} /></View>
        </View>
        <IconButton icon="menu" label="Terminal menu" colors={colors} onPress={() => openSheet('server')} />
      </View>
      {groups.length > 1 ? <View style={styles.groupBar}>
        <Text style={[styles.groupLabel, { color: colors.muted }]}>Group</Text>
        <Pressable accessibilityRole="button" accessibilityLabel="Switch terminal group" accessibilityHint={group?.name} disabled={!ready || commandBusy} onPress={() => openSheet('groups')} style={({ pressed }) => [styles.groupPicker, { backgroundColor: colors.surface }, pressed && { opacity: .65 }]}>
          <Text numberOfLines={1} style={[styles.groupName, { color: colors.text }]}>{group?.name || 'Groupを選択'}</Text><Icon name="down" color={colors.muted} size={12} />
        </Pressable>
      </View> : null}
      {workspace && groupPanes.length > 0 ? <View style={[styles.paneStrip, { borderBottomColor: colors.border }]}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={styles.paneTabs}>
        {groupPanes.map((pane, index) => <Pressable key={pane.id} accessibilityRole="tab" accessibilityLabel={`Terminal ${pane.id}`} accessibilityHint={pane.name || `ターミナル ${index + 1}`} accessibilityState={{ selected: pane.id === selectedPane?.id, disabled: !ready || commandBusy }} disabled={!ready || commandBusy} onPress={() => choosePane(pane)} onLongPress={() => { if (ready) openName({ kind: 'renamePane', pane }); }} style={({ pressed }) => [styles.paneTab, { borderBottomColor: pane.id === selectedPane?.id ? colors.accent : 'transparent' }, pressed && { backgroundColor: colors.surface }]}><Icon name="terminal" color={pane.id === selectedPane?.id ? colors.accent : colors.muted} size={15} /><Text numberOfLines={1} style={[styles.paneTabText, { color: pane.id === selectedPane?.id ? colors.accent : colors.muted }]}>{pane.name || `ターミナル ${index + 1}`}</Text></Pressable>)}
      </ScrollView><IconButton icon="plus" label="Create terminal" colors={colors} disabled={!ready || commandBusy} onPress={createPane} /></View> : null}
      {selectedPane?.agent ? <View style={styles.agentLine}>
        <Text numberOfLines={1} style={[styles.agentName, { color: colors.muted }]}>{selectedPane.agent.name}</Text>
        <Text accessibilityHint="Herdrが報告した状態です。タスクの正しさやテスト成功を保証するものではありません。" style={[styles.agentStatus, { color: ready && selectedPane.agent.status === 'blocked' ? colors.accent : colors.muted }]}>{AGENT_LABELS[ready ? selectedPane.agent.status : 'unknown']}</Text>
      </View> : null}
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {ready && workspace && selectedPane ? (
        // Unmounting a surface cancels composition; the shared native registry
        // still owns the SSH connection and each terminal's retained state.
        sheet === null && !modalPending && !formVisible && !settingsVisible && !nameRequest && appState === 'active' ? <TerminalView key={selectedPane.terminalId} terminalId={selectedPane.terminalId} fontSize={preferences.fontSize} theme={resolvedTheme} scrollbackLines={preferences.scrollbackLines} style={styles.flex} /> : <View style={[styles.flex, { backgroundColor: colors.terminal }]} />
      ) : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={[styles.terminalUnavailable, { paddingBottom: Math.max(insets.bottom, 24) }]}>
        {statusNotice}
        {ready ? <View style={styles.gone}>
          <Icon name="terminal" color={colors.muted} size={32} />
          <Text accessibilityLabel="Terminal unavailable" style={[styles.emptyTitle, { color: colors.text }]}>{workspace ? 'このターミナルは終了しました' : 'このワークスペースは終了しました'}</Text>
          <Text style={[styles.emptyBody, { color: colors.muted }]}>{workspace ? '別のターミナルを選んで作業を続けられます。' : '一覧から別のワークスペースを選んでください。'}</Text>
          <Button label="Back to workspaces" colors={colors} secondary onPress={backToWorkspaces}>ワークスペースへ</Button>
        </View> : null}
      </ScrollView>}
    </View>}

    <ConnectionForm visible={formVisible} initialProfile={formProfile} mode={formMode} colors={colors} onClose={finishConnectionForm} onDismiss={connectionFormDismissed} onSubmit={submitConnection} />
    <SettingsForm visible={settingsVisible} preferences={preferences} colors={colors} onClose={() => setSettingsVisible(false)} onSave={savePreferences} />
    <NameForm visible={nameRequest !== null} title={nameRequest?.kind === 'createWorkspace' ? 'ワークスペースを作成' : nameRequest?.kind === 'renameWorkspace' ? 'ワークスペースの名前' : nameRequest?.kind === 'createGroup' ? 'Groupを作成' : nameRequest?.kind === 'renameGroup' ? 'Groupの名前' : 'ターミナルの名前'} initialName={nameRequest?.kind === 'renameWorkspace' ? nameRequest.workspace.name : nameRequest?.kind === 'renamePane' ? nameRequest.pane.name : nameRequest?.kind === 'renameGroup' ? nameRequest.group.name : ''} colors={colors} onClose={() => setNameRequest(null)} onSave={saveName} />
    <NativeSheet title={sheet === 'groups' ? 'Groupを切り替える' : sheet === 'workspaces' ? '作業を切り替える' : sheet === 'handoff' ? 'PC で続きを' : sheet === 'servers' ? '保存済みサーバー' : 'サーバー'} visible={sheet !== null} onClose={() => setSheet(null)} busy={commandBusy} onDismiss={() => { setHostPromptDeferred(false); setModalPending(false); const show = pendingModal.current; pendingModal.current = null; show?.(); }} colors={colors}>
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {sheet === 'servers' ? <ProfileList profiles={profiles} selectedId={profileId} loading={profilesLoading} error={profilesError} busy={commandBusy} colors={colors} onRetry={() => { void loadProfiles(); }} onAdd={() => openProfileForm(undefined, 'save')} onConnect={connectSavedProfile} onEdit={profile => openProfileForm(profile, 'save')} onDelete={deleteProfile} /> : sheet === 'workspaces' ? <View style={styles.flex}>
        <View style={styles.pickerHeader}><Text selectable style={[styles.emptyBody, { color: colors.muted }]}>{endpoint(connection)}</Text>{workspaces.length >= 6 ? <SearchField label="Search workspace picker" value={pickerQuery} onChange={setPickerQuery} colors={colors} /> : null}<Button label="Create workspace" colors={colors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createWorkspace' })}>ワークスペースを作成</Button></View>
        <FlatList data={pickerWorkspaces} keyExtractor={item => item.id} contentContainerStyle={styles.pickerList} renderItem={({ item }) => <WorkspaceRow connected={ready} workspace={item} selected={item.id === workspaceId} disabled={presentation.pending || commandBusy} optionsDisabled={!ready} colors={colors} picker onPress={() => openWorkspace(item)} onOptions={() => workspaceOptions(item)} />} ListEmptyComponent={<Text style={[styles.emptyBody, { color: colors.muted }]}>該当するワークスペースがありません。</Text>} keyboardShouldPersistTaps="handled" keyboardDismissMode="on-drag" />
      </View> : sheet === 'groups' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>Groupごとに、まとまったターミナルを切り替えます。</Text>
        {groups.map(item => <View key={item.id} style={[styles.groupRow, { borderColor: colors.border }]}>
          <Pressable accessibilityRole="button" accessibilityLabel={`Group ${item.name}`} accessibilityState={{ selected: item.id === group?.id, disabled: !ready || commandBusy }} disabled={!ready || commandBusy} onPress={() => chooseGroup(item)} style={({ pressed }) => [styles.groupChoice, pressed && { opacity: .65 }]}>
            <Text style={[styles.groupName, { color: item.id === group?.id ? colors.accent : colors.text }]}>{item.name || '名前のないGroup'}</Text>
            <Text style={[styles.rowSubtitle, { color: colors.muted }]}>{panes.filter(pane => pane.groupId === item.id).length} ターミナル{item.id === group?.id ? ' · 選択中' : ''}</Text>
          </Pressable>
          <IconButton icon="menu" label={`Group options ${item.name}`} colors={colors} disabled={!ready || commandBusy} onPress={() => itemActions(item.name, () => openName({ kind: 'renameGroup', group: item }), () => closeGroup(item), 'workspace')} />
        </View>)}
        {workspace ? <Button label="Create group" colors={colors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Groupを追加</Button> : null}
      </ScrollView> : sheet === 'handoff' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.handoffTitle, { color: colors.text }]}>同じ作業を、大きな画面で。</Text>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>スマートフォンの接続を切っても、サーバー上の作業は続きます。</Text>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: colors.accent }]}>1</Text><Text style={[styles.emptyBody, { color: colors.text, flex: 1 }]}>このスマートフォンの接続を切断します。</Text></View>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: colors.accent }]}>2</Text><Text style={[styles.emptyBody, { color: colors.text, flex: 1 }]}>PC から同じサーバー・同じユーザーで SSH 接続します。</Text></View>
        <Text selectable style={[styles.command, { backgroundColor: colors.surface, color: colors.text }]}>{session.backend === 'herdr' ? `herdr --session ${session.runtime || 'default'}` : 'tmux attach -t meeterm'}</Text>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>このコマンドで、同じワークスペースとターミナルを開けます。</Text>
        {active ? <Button label="Disconnect" colors={colors} disabled={commandBusy} onPress={disconnect}>切断して PC へ</Button> : <Button label="Close sheet" colors={colors} secondary onPress={() => setSheet(null)}>閉じる</Button>}
      </ScrollView> : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <View style={styles.serverDetails}>
          <Icon name="server" color={colors.accent} size={28} />
          <Text selectable style={[styles.serverDetailTitle, { color: colors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
          {currentProfile ? <Text selectable style={[styles.emptyBody, { color: colors.muted }]}>{currentProfile.username}@{endpoint(connection)}</Text> : null}
          <ConnectionStatus connection={connection} colors={colors} />
        </View>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>サーバー上のワークスペースに接続しています。切断しても、作業はサーバーに残ります。</Text>
        {canReconnect ? <Button label="Reconnect" colors={colors} disabled={commandBusy} onPress={reconnect}>再接続</Button> : null}
        {!active && !closing ? <Button label="Connect" colors={colors} secondary onPress={openForm}>接続情報を入力</Button> : null}
        {active ? <Button label="Disconnect" colors={colors} secondary disabled={commandBusy} onPress={disconnect}>{ready ? '切断' : '接続をキャンセル'}</Button> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="Saved servers" disabled={commandBusy} onPress={() => setSheet('servers')} style={({ pressed }) => [styles.menuRow, { borderColor: colors.border }, pressed && { backgroundColor: colors.surface }]}><Text style={[styles.actionText, { color: colors.text }]}>保存済みサーバー・切り替え</Text><Icon name="chevron" color={colors.muted} size={18} /></Pressable>
        <Pressable accessibilityRole="button" accessibilityLabel="Terminal settings" disabled={commandBusy} onPress={openSettings} style={({ pressed }) => [styles.menuRow, { borderColor: colors.border }, pressed && { backgroundColor: colors.surface }]}><Text style={[styles.actionText, { color: colors.text }]}>ターミナル設定</Text><Icon name="chevron" color={colors.muted} size={18} /></Pressable>
        {screen === 'terminal' && workspace && selectedPane ? <View style={[styles.terminalActions, { borderColor: colors.border }]}>
          <Text numberOfLines={2} style={[styles.sectionLabel, { color: colors.muted }]}>{selectedPane.name || selectedPane.id}</Text>
          <Button label="Refresh terminal" colors={colors} secondary disabled={!ready || commandBusy} onPress={refreshTerminal}>画面を再描画</Button>
          <Text style={[styles.noticeBody, { color: colors.muted }]}>再接続後に表示が崩れたとき、アプリに画面の再描画を要求します。</Text>
          <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename terminal" disabled={!ready || commandBusy} onPress={() => openName({ kind: 'renamePane', pane: selectedPane })} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>名前を変更</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close terminal" disabled={!ready || commandBusy} onPress={closePane} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>ターミナルを終了</Text></Pressable>
          </View>
          <Pressable accessibilityRole="button" accessibilityLabel={`Workspace options ${workspace.name}`} disabled={!ready || commandBusy} onPress={() => workspaceOptions(workspace)} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>ワークスペースの名前・終了</Text></Pressable>
        </View> : null}
        {screen === 'terminal' && workspace && session.groupsSupported ? <View style={[styles.terminalActions, { borderColor: colors.border }]}>
          <Text style={[styles.sectionLabel, { color: colors.muted }]}>Group</Text>
          <Button label="Create group" colors={colors} secondary disabled={!ready || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Groupを追加</Button>
          {group ? <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename group" disabled={!ready || commandBusy} onPress={() => openName({ kind: 'renameGroup', group })} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Groupの名前を変更</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close group" disabled={!ready || commandBusy} onPress={() => closeGroup(group)} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Groupを終了</Text></Pressable>
          </View> : null}
        </View> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="PC handoff help" onPress={() => setSheet('handoff')} style={({ pressed }) => [styles.menuRow, { borderColor: colors.border }, pressed && { backgroundColor: colors.surface }]}><Text style={[styles.actionText, { color: colors.text }]}>PC で続きを</Text><Icon name="chevron" color={colors.muted} size={18} /></Pressable>
        {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>ホスト鍵の変更を確認</Text></Pressable> : null}
      </ScrollView>}
    </NativeSheet>
  </SafeAreaView>;
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
  brand: { fontSize: 24, letterSpacing: -.8, fontWeight: '600' },
  hero: { paddingHorizontal: 24, paddingTop: 4, paddingBottom: 12, minHeight: 104, flexDirection: 'row', alignItems: 'center', gap: 4 },
  heroCopy: { flex: 1, minWidth: 0 },
  heroTitle: { fontSize: 28, lineHeight: 40, fontWeight: '700', letterSpacing: -1.2 },
  heroDescription: { fontSize: 12, lineHeight: 20, marginTop: 8 },
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
  rowTitle: { fontSize: 18, lineHeight: 26, fontWeight: '500' },
  rowSubtitle: { fontSize: 13, lineHeight: 20, fontVariant: ['tabular-nums'] },
  firstUse: { paddingHorizontal: 28, paddingTop: 4, alignItems: 'center', gap: 20 },
  firstUseTitle: { fontSize: 22, lineHeight: 32, fontWeight: '600', letterSpacing: -.6, marginTop: 8, textAlign: 'center' },
  firstUseBody: { fontSize: 15, lineHeight: 28, textAlign: 'center' },
  savedHome: { paddingHorizontal: 24, gap: 20 },
  savedHomeTitle: { fontSize: 18, lineHeight: 28, fontWeight: '600', marginBottom: 4 },
  savedHomeRow: { minHeight: 88, paddingVertical: 16, flexDirection: 'row', alignItems: 'center', gap: 16, borderBottomWidth: StyleSheet.hairlineWidth },
  fullWidth: { alignSelf: 'stretch', marginTop: 8 },
  emptySearch: { padding: 32, gap: 12, alignItems: 'center' },
  emptyTitle: { fontSize: 17, lineHeight: 27, fontWeight: '600', textAlign: 'center' },
  emptyBody: { fontSize: 14, lineHeight: 25 },
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
