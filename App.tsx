import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ActivityIndicator,
  Alert,
  AppState,
  BackHandler,
  AccessibilityInfo,
  FlatList,
  Image,
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
import { DEFAULT_WORKSPACE_CONTROL, normalizeWorkspaceControl } from './modules/meeterm-terminal';
import type { AgentStatus, AttachmentCorePhase, AttachmentOperationSnapshot, AttachmentSource, AttachmentTarget, RuntimeBackend, RuntimeBoundaryResult, RuntimeCandidate, RuntimeDiscovery, RuntimeBackendDiscovery, ServerProfile, SshConnectOptions, SshConnectionState, TerminalPreferences, RemoteTerminal, RemoteWorkspace, TerminalGroup, WorkspaceControl, WorkspaceState } from './modules/meeterm-terminal';
import { ConnectionForm } from './app/ConnectionForm';
import { WorkspaceNavigation } from './app/WorkspaceNavigation';
import type { ConnectionSubmission } from './app/ConnectionForm';
import { DEFAULT_PREFERENCES, itemActions, NameForm, ProfileList, SettingsForm } from './app/DailyUse';
import { Button, Companion, DARK, Icon, IconButton, MONO, usePalette, useReducedMotion } from './app/ui';
import type { Palette } from './app/ui';

type StartupPhase =
  | 'js_module_loaded'
  | 'root_effect'
  | 'initial_url_requested'
  | 'initial_url_null'
  | 'initial_url_allowed_fixture'
  | 'initial_url_other'
  | 'initial_url_rejected'
  | 'app_content_mounted'
  | 'profiles_requested'
  | 'profiles_succeeded'
  | 'profiles_failed';

function readRuntimeBoundaryResult(value: unknown): RuntimeBoundaryResult | null {
  if (!value || typeof value !== 'object') return null;
  const result = value as Record<string, unknown>;
  if (result.status === 'accepted') return { status: 'accepted' };
  if ((result.status === 'not_invoked' || result.status === 'rejected_before_boundary')
    && typeof result.errorCode === 'string' && result.errorCode.length > 0) {
    return { status: result.status, errorCode: result.errorCode };
  }
  if (result.status === 'accepted_after_failure'
    && result.errorCode === 'boundary_accepted_failure') {
    return { status: 'accepted_after_failure', errorCode: 'boundary_accepted_failure' };
  }
  return null;
}

const SMOKE_BUILD = process.env.EXPO_PUBLIC_MEETERM_SMOKE === '1';

// Startup observations are a smoke-only iOS diagnostic. Keep this bridge
// synchronous and tiny so a bridge failure can never hold up app startup or
// change the normal URL/profile behavior.
function recordStartupPhase(phase: StartupPhase): void {
  if (!SMOKE_BUILD || Platform.OS !== 'ios') return;
  try {
    MeetermTerminal.recordStartupPhase(phase);
  } catch {
    // Diagnostics must remain observational and must not become a startup
    // failure when the native sink is unavailable.
  }
}

if (SMOKE_BUILD) recordStartupPhase('js_module_loaded');

// The owner outlives views. Only remote borrowed pane handles are displayed in
// the ordinary app; the owner's local foundation fixture is never a fallback.
const CONNECTION_ID = 'poc-main';
const INITIAL_CONNECTION: SshConnectionState = {
  state: 'Disconnected', host: '', port: 0, fingerprint: '', algorithm: '',
  knownFingerprint: '', errorCode: '', errorMessage: '',
};
type Workspace = RemoteWorkspace & { panes: RemoteTerminal[] };
type SheetKind = 'server' | 'servers' | 'switcher' | 'workspaces' | 'groups' | 'handoff' | 'recovery' | 'attachment' | null;

// Issue #28 attachment draft. `phase` is the local staging lifecycle; once a
// core operation exists, `operation` (the authoritative native snapshot)
// drives the Upload → progress → Uploaded → Insert flow. The captured
// destination is what the native session is bound to — a changed pane never
// retargets it.
type AttachmentPhase = 'choosing' | 'picking' | 'normalizing' | 'ready' | 'error';
type AttachmentBusyAction = 'upload' | 'retryUpload' | 'insert' | 'cancel' | 'deleteRemote' | 'discard' | null;
type AttachmentPreparedInfo = {
  fileId: string;
  previewUri: string;
  format: string;
  width: number;
  height: number;
  byteCount: number;
  sourceByteCount: number;
};
type AttachmentDestination = {
  /** Captured binding passed back to every native call; never retargeted. */
  terminalId: string;
  server: string;
  session: string;
  workspace: string;
  terminal: string;
};
type AttachmentDraftState = {
  phase: AttachmentPhase;
  prepared: AttachmentPreparedInfo | null;
  operation: AttachmentOperationSnapshot | null;
  remoteDirectory: string;
  busyAction: AttachmentBusyAction;
  /** False after a held begin until a session is actually established. */
  sessionReady: boolean;
  destination: AttachmentDestination;
  notice: string;
  errorCode: string;
  errorMessage: string;
};
type NameRequest = { kind: 'createWorkspace' } | { kind: 'renameWorkspace'; workspace: Workspace } | { kind: 'renamePane'; pane: RemoteTerminal } | { kind: 'createGroup'; workspace: Workspace } | { kind: 'renameGroup'; group: TerminalGroup };
type RuntimeHint = { backend: RuntimeBackend; runtime: string };
type SwitcherTarget = { profile: ServerProfile; isCurrent: boolean };
type RecoveryActionIdentity = {
  epoch: string;
  phase: WorkspaceControl['recovery']['phase'];
  attempt: number;
};
type RecoveryPendingActions = {
  retry: RecoveryActionIdentity | null;
  change: RecoveryActionIdentity | null;
};
type PendingRuntimeRefresh = {
  attempt: number;
  connectionGeneration: string;
  baselineRevision: number;
  clearSelectionErrors: boolean;
};
type SmokeScreen = 'welcome' | 'empty' | 'search-empty' | 'disconnected' | 'reconnecting' | 'connection-error' | 'long-workspaces' | 'runtime-picker' | 'runtime-partial-error' | 'runtime-empty' | 'runtime-create' | 'session-switcher' | 'session-switcher-sessions' | 'herdr-connection' | 'herdr-groups' | 'herdr-terminal' | 'herdr-workspaces' | 'recovery-progress' | 'recovery-exhausted' | 'recovery-mismatch' | 'layout-restore-unconfirmed' | 'runtime-layout-restore-unconfirmed' | 'home' | 'servers' | 'connection' | 'password' | 'workspaces' | 'terminal' | 'settings' | 'workspace-name' | 'terminal-name' | 'handoff' | 'attachment-choose' | 'attachment-ready' | 'attachment-uploading' | 'attachment-pending' | 'attachment-uploaded' | 'attachment-inserted' | 'attachment-failed' | 'attachment-cancelled' | 'attachment-deleted' | 'attachment-error' | 'attachment-blocked';
type SmokeRoute = { kind: 'foundation' } | { kind: 'screen'; screen: SmokeScreen } | null;

// This is the native message published after an explicit disconnect cannot
// prove that its old tmux layout was restored. The smoke fixtures display the
// real error contract; they do not inject terminal bytes or alter production
// failure handling.
const SMOKE_LAYOUT_RESTORE_WARNING = 'The connection closed, but the desktop layout could not be confirmed as restored.';
const SMOKE_CLEANUP_WARNING_MESSAGE = "The old connection's desktop layout restore could not be confirmed.";
const LEGACY_CLEANUP_WARNING_ID = 'legacy-layout-restore-unconfirmed';

// IME-safe attachment insertion: a live composition holds the request instead
// of being committed, cleared, or forwarded by the native path.
const ATTACHMENT_COMPOSING_NOTICE = 'Finish IME composition before inserting.';
const ATTACHMENT_DEFAULT_REMOTE_DIRECTORY = '~/.local/share/meeterm/attachments';
const ATTACHMENT_INSERTED_NOTICE = 'Inserted into terminal input. Review it before sending.';
const ATTACHMENT_INSERT_UNCONFIRMED_NOTICE = 'Insert delivery is unconfirmed — check the terminal input yourself; it is not resent automatically.';

function legacyCleanupWarning(connection: SshConnectionState): WorkspaceControl['cleanupWarning'] {
  if (connection.errorCode !== 'layout_restore_unconfirmed') return null;
  return {
    id: LEGACY_CLEANUP_WARNING_ID,
    code: 'layout_restore_unconfirmed',
    message: connection.errorMessage || SMOKE_CLEANUP_WARNING_MESSAGE,
  };
}

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

// Herdr presentation data is an explicit snapshot fixture. Rollups are kept
// separate from descendant pane status so the fixture exercises the same
// source boundary as the native snapshot (and never relies on JS counting).
const SMOKE_HERDR_PANES: RemoteTerminal[] = [
  { workspaceId: '@smoke-main', id: 'smoke-code-working', terminalId: CONNECTION_ID, groupId: 'smoke-code', agent: { name: 'Claude Code', status: 'working' }, name: 'Code', active: true, selected: true },
  { workspaceId: '@smoke-main', id: 'smoke-code-agentless', terminalId: 'smoke-terminal-agentless', groupId: 'smoke-code', agent: null, name: 'Shell', active: false, selected: false },
  { workspaceId: '@smoke-main', id: 'smoke-code-blocked', terminalId: 'smoke-terminal-blocked', groupId: 'smoke-code', agent: { name: 'Codex', status: 'blocked' }, name: 'Tests', active: false, selected: false },
  { workspaceId: '@smoke-main', id: 'smoke-code-done', terminalId: 'smoke-terminal-done', groupId: 'smoke-code', agent: { name: 'Codex', status: 'done' }, name: 'Review', active: false, selected: false },
  { workspaceId: '@smoke-main', id: 'smoke-code-idle', terminalId: 'smoke-terminal-idle', groupId: 'smoke-code', agent: { name: 'Runner', status: 'idle' }, name: 'Monitor', active: false, selected: false },
  { workspaceId: '@smoke-main', id: 'smoke-code-unknown', terminalId: 'smoke-terminal-unknown', groupId: 'smoke-code', agent: { name: 'Claude Code', status: 'unknown' }, name: 'Logs', active: false, selected: false },
  { workspaceId: '@smoke-main', id: 'smoke-tests-done', terminalId: 'smoke-terminal-tests-done', groupId: 'smoke-tests', agent: { name: 'Codex', status: 'done' }, name: 'Audit', active: false, selected: false },
  { workspaceId: '@smoke-tools', id: 'smoke-logs-unknown', terminalId: 'smoke-terminal-logs', groupId: 'smoke-logs', agent: { name: 'Claude Code', status: 'unknown' }, name: 'Console', active: true, selected: false },
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
  attachmentDraft?: AttachmentDraftState | null;
  searching?: boolean;
  query?: string;
  runtimeDiscovery?: RuntimeDiscovery;
  runtimePickerVisible?: boolean;
  runtimeCreateVisible?: boolean;
  switcherTarget?: SwitcherTarget | null;
  switcherStarted?: boolean;
  runtimeMessage?: string;
  controlMessage?: string;
  control?: WorkspaceControl;
};

type WorkspaceControlOverrides = Partial<Omit<WorkspaceControl, 'recovery'>> & {
  recovery?: Partial<WorkspaceControl['recovery']>;
};

function smokeControl(overrides: WorkspaceControlOverrides = {}): WorkspaceControl {
  return {
    ...DEFAULT_WORKSPACE_CONTROL,
    operationEpoch: '7',
    runtimeOperationsReady: true,
    terminalInputReady: true,
    ...overrides,
    recovery: {
      ...DEFAULT_WORKSPACE_CONTROL.recovery,
      ...overrides.recovery,
    },
  };
}

const SMOKE_RUNTIME_DISCOVERY: RuntimeDiscovery = {
  connectionGeneration: '7',
  revision: 7,
  backends: [
    {
      backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true,
      candidates: [
        { id: 'smoke-tmux-meeterm', backend: 'tmux', name: 'meeterm', state: 'running', selectable: true, isDefault: true, lastUsed: true, errorCode: '', errorMessage: '' },
        { id: 'smoke-tmux-release', backend: 'tmux', name: 'release-prep', state: 'running', selectable: true, isDefault: false, lastUsed: false, errorCode: '', errorMessage: '' },
      ],
    },
    {
      backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false,
      candidates: [
        { id: 'smoke-herdr-default', backend: 'herdr', name: 'default', state: 'running', selectable: true, isDefault: true, lastUsed: false, errorCode: '', errorMessage: '' },
        { id: 'smoke-herdr-paused', backend: 'herdr', name: 'paused', state: 'stopped', selectable: false, isDefault: false, lastUsed: false, errorCode: '', errorMessage: '' },
      ],
    },
  ],
};

function smokeRuntimeDiscovery(screen: SmokeScreen): RuntimeDiscovery {
  if (screen === 'runtime-partial-error') {
    return {
      ...SMOKE_RUNTIME_DISCOVERY,
      backends: [SMOKE_RUNTIME_DISCOVERY.backends[0], {
        backend: 'herdr', state: 'error', errorCode: 'herdr_missing',
        errorMessage: 'Herdr is not available over SSH. Open Herdr on your computer or check its installation.',
        canCreate: false, candidates: [],
      }],
    };
  }
  if (screen === 'runtime-empty') {
    return {
      ...SMOKE_RUNTIME_DISCOVERY,
      backends: [
        { ...SMOKE_RUNTIME_DISCOVERY.backends[0], candidates: [] },
        { ...SMOKE_RUNTIME_DISCOVERY.backends[1], candidates: [{ ...SMOKE_RUNTIME_DISCOVERY.backends[1].candidates[1] }] },
      ],
    };
  }
  return JSON.parse(JSON.stringify(SMOKE_RUNTIME_DISCOVERY)) as RuntimeDiscovery;
}

function smokeReadyConnection(): SshConnectionState {
  return { ...INITIAL_CONNECTION, state: 'Ready', host: SMOKE_PROFILE.host, port: SMOKE_PROFILE.port };
}

function smokePanes(): RemoteTerminal[] { return SMOKE_PANES.map(pane => ({ ...pane })); }

function smokeWorkspace(panes: RemoteTerminal[], workspaceId: string): Workspace {
  return { id: workspaceId, name: workspaceId === '@smoke-main' ? 'Main workspace' : 'Tools workspace', agentStatus: null, panes: panes.filter(pane => pane.workspaceId === workspaceId) };
}

function smokeFixture(screen: SmokeScreen): SmokeFixtureState {
  if (screen === 'runtime-picker' || screen === 'runtime-partial-error' || screen === 'runtime-empty' || screen === 'runtime-create') {
    const base = smokeFixture('workspaces');
    base.connection = { ...base.connection, state: 'AwaitingRuntimeSelection' };
    base.panes = [];
    base.screen = 'workspaces';
    base.workspaceId = '';
    base.profileId = SMOKE_PROFILE.id;
    base.hasConnected = false;
    base.runtimeDiscovery = smokeRuntimeDiscovery(screen);
    base.runtimePickerVisible = true;
    base.runtimeCreateVisible = screen === 'runtime-create';
    base.runtimeMessage = screen === 'runtime-empty'
      ? 'No running runtime is available yet. Stopped Herdr sessions need to be opened on your computer.'
      : '';
    return base;
  }
  if (screen === 'session-switcher' || screen === 'session-switcher-sessions') {
    const base = smokeFixture('workspaces');
    base.sheet = 'switcher';
    if (screen === 'session-switcher-sessions') {
      base.connection = { ...base.connection, state: 'AwaitingRuntimeSelection' };
      base.panes = [];
      base.screen = 'workspaces';
      base.workspaceId = '';
      base.hasConnected = false;
      base.runtimeDiscovery = smokeRuntimeDiscovery('runtime-picker');
      base.runtimePickerVisible = true;
      base.switcherTarget = { profile: { ...SMOKE_PROFILE }, isCurrent: false };
      base.switcherStarted = true;
    }
    return base;
  }
  if (screen === 'herdr-connection') {
    const base = smokeFixture('runtime-picker');
    const discovery = JSON.parse(JSON.stringify(base.runtimeDiscovery)) as RuntimeDiscovery;
    discovery.backends = discovery.backends.map(section => ({
      ...section,
      candidates: section.candidates.map(candidate => ({
        ...candidate,
        lastUsed: candidate.backend === 'herdr' && candidate.name === 'default',
      })),
    }));
    base.runtimeDiscovery = discovery;
    return base;
  }
  if (screen === 'layout-restore-unconfirmed' || screen === 'runtime-layout-restore-unconfirmed') {
    const picker = screen === 'runtime-layout-restore-unconfirmed';
    const base = smokeFixture(picker ? 'runtime-picker' : 'workspaces');
    base.connection = {
      ...base.connection,
      state: picker ? 'AwaitingRuntimeSelection' : 'Disconnected',
      errorCode: 'layout_restore_unconfirmed',
      errorMessage: SMOKE_LAYOUT_RESTORE_WARNING,
    };
    base.hasConnected = true;
    base.control = smokeControl({
      runtimeOperationsReady: false,
      terminalInputReady: false,
      cleanupWarning: {
        id: picker ? '102' : '101',
        code: 'layout_restore_unconfirmed',
        message: SMOKE_CLEANUP_WARNING_MESSAGE,
      },
    });
    return base;
  }
  if (screen === 'recovery-progress' || screen === 'recovery-exhausted' || screen === 'recovery-mismatch') {
    const base = smokeFixture('terminal');
    base.connection = {
      ...base.connection,
      state: screen === 'recovery-progress' ? 'Reconnecting' : 'Failed',
      errorCode: '',
      errorMessage: '',
    };
    base.control = smokeControl({
      hasRetainedWork: true,
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: screen === 'recovery-progress'
        ? { phase: 'resynchronizing', reason: 'runtime_validation', attempt: 2, maxAttempts: 6 }
        : screen === 'recovery-exhausted'
          ? { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 }
          : { phase: 'stopped', reason: 'runtime_identity_mismatch', attempt: 1, maxAttempts: 6 },
    });
    return base;
  }
  if (screen === 'long-workspaces') {
    const base = smokeFixture('herdr-workspaces');
    base.panes = base.panes.map(pane => ({
      ...pane,
      name: pane.workspaceId === '@smoke-main'
        ? `Terminal ${pane.name} — international status review and release preparation`
        : `Terminal ${pane.name} — long-running diagnostics`,
    }));
    return base;
  }
  if (['welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error'].includes(screen)) {
    const base = smokeFixture(screen === 'welcome' ? 'home' : 'workspaces');
    if (screen === 'welcome') base.profiles = [];
    if (screen === 'empty') base.panes = [];
    if (screen === 'search-empty') { base.searching = true; base.query = 'deployment'; }
    if (screen === 'disconnected') base.connection.state = 'Disconnected';
    if (screen === 'reconnecting') base.connection.state = 'Reconnecting';
    if (screen === 'connection-error') {
      base.connection.state = 'Failed';
      base.connection.errorCode = 'authentication_failed';
      base.control = smokeControl({
        runtimeOperationsReady: false,
        terminalInputReady: false,
        cleanupWarning: {
          id: '103',
          code: 'layout_restore_unconfirmed',
          message: SMOKE_CLEANUP_WARNING_MESSAGE,
        },
      });
    }
    return base;
  }
  if (screen.startsWith('herdr-')) {
    const base = smokeFixture(screen === 'herdr-workspaces' ? 'workspaces' : 'terminal');
    base.panes = SMOKE_HERDR_PANES.map(pane => ({ ...pane, selected: pane.id === SMOKE_HERDR_PANES[0].id }));
    base.selectedPaneIds = { 'smoke-code': base.panes[0].id };
    base.sheet = screen === 'herdr-groups' ? 'groups' : null;
    return base;
  }
  if (screen.startsWith('attachment-')) {
    const base = smokeFixture('terminal');
    base.sheet = 'attachment';
    base.attachmentDraft = smokeAttachmentDraft(screen);
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

/** Deterministic Issue #28 drafts covering every UI-visible core phase. */
function smokeAttachmentDraft(screen: SmokeScreen): AttachmentDraftState {
  const prepared: AttachmentPreparedInfo = {
    fileId: 'att_smoke0001.png',
    // A 1×1 PNG keeps the preview deterministic and offline.
    previewUri: 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
    format: 'png',
    width: 1080,
    height: 1920,
    byteCount: 248_912,
    sourceByteCount: 4_203_304,
  };
  const destination: AttachmentDestination = {
    terminalId: CONNECTION_ID,
    server: 'dev',
    session: 'tmux · main',
    workspace: 'main',
    terminal: 'Shell',
  };
  const operation = (phase: AttachmentCorePhase, extra: Partial<AttachmentOperationSnapshot> = {}): AttachmentOperationSnapshot => ({
    phase,
    attachmentId: '42',
    bytesUploaded: 0,
    sizeBytes: prepared.byteCount,
    remotePath: '',
    displayName: prepared.fileId,
    errorCode: '',
    errorMessage: '',
    insertUnconfirmed: false,
    remoteRemoved: false,
    ...extra,
  });
  const draft = (phase: AttachmentPhase, operation: AttachmentOperationSnapshot | null, preparedInfo: AttachmentPreparedInfo | null = prepared): AttachmentDraftState => ({
    phase,
    prepared: preparedInfo,
    operation,
    remoteDirectory: '',
    busyAction: null,
    sessionReady: true,
    destination,
    notice: '',
    errorCode: '',
    errorMessage: '',
  });
  switch (screen) {
    case 'attachment-ready':
      return draft('ready', null);
    case 'attachment-uploading':
      return draft('ready', operation('uploading', { bytesUploaded: 124_456 }));
    case 'attachment-pending':
      return draft('ready', operation('pending', {
        errorCode: 'destination_not_ready',
        errorMessage: 'The SSH connection is not ready for the transfer yet.',
      }));
    case 'attachment-uploaded':
      return draft('ready', operation('uploaded', {
        bytesUploaded: prepared.byteCount,
        remotePath: '/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png',
      }));
    case 'attachment-inserted':
      return draft('ready', operation('inserted', {
        bytesUploaded: prepared.byteCount,
        remotePath: '/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png',
        insertUnconfirmed: true,
      }));
    case 'attachment-failed':
      return draft('ready', operation('failed', {
        errorCode: 'remote_io_failed',
        errorMessage: 'The upload stopped while writing to the server.',
      }));
    case 'attachment-cancelled':
      return draft('ready', operation('cancelled'));
    case 'attachment-deleted':
      return draft('ready', operation('uploaded', {
        bytesUploaded: prepared.byteCount,
        remoteRemoved: true,
      }));
    case 'attachment-error':
      return {
        ...draft('error', null, null),
        errorCode: 'attachment_too_many_pixels',
        errorMessage: 'The image exceeds the attachment size limits.',
      };
    case 'attachment-blocked':
      return { ...draft('ready', null), notice: ATTACHMENT_COMPOSING_NOTICE };
    default:
      return draft('choosing', null, null);
  }
}

const SMOKE_SCREEN_NAMES: SmokeScreen[] = [
  'welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error', 'long-workspaces',
  'runtime-picker', 'runtime-partial-error', 'runtime-empty', 'runtime-create',
  'session-switcher', 'session-switcher-sessions',
  'recovery-progress', 'recovery-exhausted', 'recovery-mismatch',
  'layout-restore-unconfirmed', 'runtime-layout-restore-unconfirmed',
  'home', 'servers', 'connection', 'password', 'workspaces', 'terminal',
  'settings', 'workspace-name', 'terminal-name', 'handoff',
  'attachment-choose', 'attachment-ready', 'attachment-uploading', 'attachment-pending',
  'attachment-uploaded', 'attachment-inserted', 'attachment-failed', 'attachment-cancelled',
  'attachment-deleted', 'attachment-error', 'attachment-blocked',
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

// Operation epochs are native u64 values serialized as decimal strings. Keep
// their ordering exact without converting them through Number, which would
// lose precision for a long-lived connection.
function compareOperationEpochs(leftValue: string, rightValue: string): number | null {
  const left = leftValue.replace(/^0+(?=\d)/, '');
  const right = rightValue.replace(/^0+(?=\d)/, '');
  if (!/^\d+$/.test(left) || !/^\d+$/.test(right)) return leftValue === rightValue ? 0 : null;
  if (left.length !== right.length) return left.length > right.length ? 1 : -1;
  if (left === right) return 0;
  return left > right ? 1 : -1;
}

function operationEpochAtLeast(candidate: string, reference: string): boolean {
  const comparison = compareOperationEpochs(candidate, reference);
  return comparison !== null && comparison >= 0;
}

function operationEpochNewer(candidate: string, reference: string): boolean {
  const comparison = compareOperationEpochs(candidate, reference);
  return comparison !== null && comparison > 0;
}

const EMPTY_WORKSPACES: WorkspaceState = {
  backend: 'tmux', runtime: 'meeterm', groupsSupported: false, workspaces: [], groups: [], terminals: [],
  control: { ...DEFAULT_WORKSPACE_CONTROL, recovery: { ...DEFAULT_WORKSPACE_CONTROL.recovery } },
};
function smokeWorkspaceState(panes: RemoteTerminal[], herdr = false, longNames = false, control?: WorkspaceControl): WorkspaceState {
  const ids = [...new Set(panes.map(pane => pane.workspaceId))];
  const defaultControl = panes.length > 0
    ? smokeControl()
    : smokeControl({ runtimeOperationsReady: false, terminalInputReady: false });
  return { ...EMPTY_WORKSPACES, terminals: panes,
    workspaces: ids.map(id => ({ id, name: longNames ? id === '@smoke-main' ? 'Production infrastructure — migration and release preparation' : 'Research / terminal typography and international text' : id === '@smoke-main' ? 'Main workspace' : 'Tools workspace', agentStatus: herdr ? id === '@smoke-main' ? 'blocked' : 'idle' : null })),
    backend: herdr ? 'herdr' : 'tmux', runtime: herdr ? 'dev' : 'meeterm', groupsSupported: herdr,
    groups: herdr ? [
      { id: 'smoke-code', workspaceId: '@smoke-main', name: 'Development', selected: true, agentStatus: 'working' },
      { id: 'smoke-tests', workspaceId: '@smoke-main', name: 'Tests & review', selected: false, agentStatus: 'done' },
      { id: 'smoke-logs', workspaceId: '@smoke-tools', name: 'Logs', selected: true, agentStatus: 'unknown' },
    ] : ids.map(id => ({ id, workspaceId: id, name: '', selected: true, agentStatus: null })),
    control: control ?? defaultControl,
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
    case 'DiscoveringRuntimes': return { label: 'Finding runtimes…', accessibility: 'Finding runtimes…', pending: true };
    case 'AwaitingRuntimeSelection': return { label: 'Choose a runtime', accessibility: 'Choose a runtime', pending: false };
    case 'AttachingRuntime': return { label: 'Opening runtime…', accessibility: 'Opening runtime…', pending: true };
    case 'CreatingRuntime': return { label: 'Creating runtime…', accessibility: 'Creating runtime…', pending: true };
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

type ResolvedAgentStatus = AgentStatus | 'unavailable';
type AgentStatusMeta = {
  label: string;
  spoken: string;
  color: keyof Palette['agentStatus'];
  shape: 'filled' | 'hollow' | 'dot';
};

// Keep visual grammar, visible words, and screen-reader copy in one table.
// `unavailable` is presentation-only: the stored Herdr status remains intact.
const AGENT_STATUS_META: Record<ResolvedAgentStatus, AgentStatusMeta> = {
  blocked: { label: 'Needs attention', spoken: 'Agent status: blocked, needs attention', color: 'blocked', shape: 'filled' },
  done: { label: 'Finished', spoken: 'Agent status: finished, not yet viewed', color: 'done', shape: 'filled' },
  working: { label: 'Working', spoken: 'Agent status: working', color: 'working', shape: 'filled' },
  idle: { label: 'Idle', spoken: 'Agent status: idle', color: 'idle', shape: 'hollow' },
  unknown: { label: 'Unknown', spoken: 'Agent status: unknown', color: 'unknown', shape: 'dot' },
  unavailable: { label: 'Status unavailable', spoken: 'Agent status unavailable', color: 'unknown', shape: 'dot' },
};

function resolveAgentStatus(status: AgentStatus | null | undefined, live: boolean): ResolvedAgentStatus | null {
  if (status == null) return null;
  return live ? status : 'unavailable';
}

function agentStatusPhrase(status: AgentStatus | null | undefined, live: boolean): string {
  const resolved = resolveAgentStatus(status, live);
  return resolved ? AGENT_STATUS_META[resolved].spoken : '';
}

function AgentStatusIndicator({ status, live, colors, showLabel = false, testID }: {
  status: AgentStatus | null | undefined;
  live: boolean;
  colors: Palette;
  showLabel?: boolean;
  testID?: string;
}) {
  const resolved = resolveAgentStatus(status, live);
  if (!resolved) return null;
  const meta = AGENT_STATUS_META[resolved];
  const color = colors.agentStatus[meta.color];
  const markStyle = meta.shape === 'hollow'
    ? { backgroundColor: 'transparent', borderColor: color, borderWidth: 1.5 }
    : { backgroundColor: color };
  return <View testID={testID} accessible={false} importantForAccessibility="no" style={showLabel ? styles.agentStatusCluster : styles.agentStatusIndicator}>
    <View accessible={false} importantForAccessibility="no-hide-descendants" style={styles.agentStatusSlot}>
      <View accessible={false} style={[styles.agentStatusMark, meta.shape === 'dot' ? styles.agentStatusSmallMark : styles.agentStatusCircleMark, markStyle]} />
    </View>
    {showLabel ? <Text accessible={false} style={[styles.agentStatusLabel, { color: colors.muted }]}>{meta.label}</Text> : null}
  </View>;
}

function SearchField({ value, onChange, colors, label = 'Search workspaces', autoFocus = false }: { value: string; onChange: (value: string) => void; colors: Palette; label?: string; autoFocus?: boolean }) {
  return <View style={[styles.searchField, { backgroundColor: colors.surface, borderColor: colors.border }]}>
    <Icon name="search" color={colors.muted} size={18} />
    <TextInput accessibilityLabel={label} autoFocus={autoFocus} autoCorrect={false} autoCapitalize="none" placeholder="Search by workspace name" placeholderTextColor={colors.placeholder} selectionColor={colors.accent} returnKeyType="search" onSubmitEditing={Keyboard.dismiss} style={[styles.searchInput, { color: colors.text }]} value={value} onChangeText={onChange} />
    {value ? <IconButton icon="close" label="Clear workspace search" onPress={() => onChange('')} colors={colors} /> : null}
  </View>;
}

function WorkspaceRow({ workspace, selected, colors, onPress, onOptions, picker = false, disabled = false, optionsDisabled = false, connected = false }: { workspace: Workspace; selected: boolean; colors: Palette; onPress: () => void; onOptions?: () => void; picker?: boolean; disabled?: boolean; optionsDisabled?: boolean; connected?: boolean }) {
  const statusPhrase = agentStatusPhrase(workspace.agentStatus, connected);
  const accessibilityLabel = `Workspace ${workspace.name}${statusPhrase ? `, ${statusPhrase}` : ''}`;
  const faded = disabled ? styles.workspaceRowDisabledContent : undefined;
  return <View style={[styles.workspaceContainer, { borderBottomColor: colors.border }]}><Pressable testID={`workspace-row-${workspace.id}`} accessibilityRole="button" accessibilityLabel={accessibilityLabel} accessibilityHint={`${workspace.panes.length} ${workspace.panes.length === 1 ? 'terminal' : 'terminals'}`} accessibilityState={{ selected, disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [styles.workspaceRow, pressed && { backgroundColor: colors.surface }]}>
    <View style={faded}><Icon name="terminal" color={colors.muted} size={23} /></View>
    <AgentStatusIndicator status={workspace.agentStatus} live={connected} colors={colors} testID={`workspace-agent-status-${workspace.id}`} />
    <View style={[styles.rowCopy, faded]}>
      <Text numberOfLines={picker ? undefined : 2} style={[styles.rowTitle, { color: colors.text }]}>{workspace.name}</Text>
      <Text numberOfLines={1} style={[styles.rowSubtitle, { color: colors.muted }]}>{workspace.panes.length ? workspace.panes.map((pane, index) => pane.name || `Terminal ${index + 1}`).join(' · ') : 'No terminals'}</Text>
    </View>
    <View style={faded}><Icon name={selected ? 'check' : 'chevron'} color={selected ? colors.accent : colors.muted} size={18} /></View>
  </Pressable>{onOptions ? <IconButton icon="menu" label={`Workspace options ${workspace.name}`} onPress={onOptions} disabled={disabled || optionsDisabled} colors={colors} /> : null}</View>;
}

function NativeSheet({ title, visible, onClose, onDismiss, busy, allowDismissWhileBusy = false, colors, closeLabel = 'Close sheet', children }: { title: string; visible: boolean; onClose: () => void; onDismiss: () => void; busy: boolean; allowDismissWhileBusy?: boolean; closeLabel?: string; colors: Palette; children: ReactNode }) {
  const reducedMotion = useReducedMotion();
  return <Modal visible={visible} animationType={reducedMotion ? 'fade' : 'slide'} presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} allowSwipeDismissal={!busy || allowDismissWhileBusy} onRequestClose={() => { if (!busy || allowDismissWhileBusy) onClose(); }} onDismiss={onDismiss} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
    <SafeAreaProvider>
      <SafeAreaView edges={['top', 'left', 'right', 'bottom']} style={[styles.flex, { backgroundColor: colors.background }]}>
        {Platform.OS === 'android' ? <StatusBar barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} /> : null}
        <View style={[styles.sheetHeader, { borderBottomColor: colors.border }]}>
          <Text accessibilityRole="header" style={[styles.sheetTitle, { color: colors.text }]}>{title}</Text>
          {busy ? <ActivityIndicator color={colors.accent} /> : null}
          <IconButton icon="close" label={closeLabel} colors={colors} onPress={onClose} disabled={busy && !allowDismissWhileBusy} />
        </View>
        {children}
      </SafeAreaView>
    </SafeAreaProvider>
  </Modal>;
}

/**
 * Issue #28 attachment sheet body. Preview always renders the normalized,
 * app-owned output file — never the untrusted picker source. Once the
 * explicit Upload starts, the core operation snapshot drives the
 * Upload → progress → Uploaded → Insert sequence; nothing is inserted or
 * sent without the separate explicit Insert action and the user's own
 * review of the terminal input.
 */
function AttachmentSheet({ draft, colors, currentTerminalId, onPickSource, onRemoteDirectoryChange, onUpload, onRetryUpload, onCancel, onInsert, onDeleteRemote, onDiscard, onChooseDifferent }: {
  draft: AttachmentDraftState | null;
  colors: Palette;
  /** The pane currently shown; mismatches disable Insert, never retarget. */
  currentTerminalId: string;
  onPickSource: (source: AttachmentSource) => void;
  onRemoteDirectoryChange: (value: string) => void;
  onUpload: () => void;
  onRetryUpload: () => void;
  onCancel: () => void;
  onInsert: () => void;
  onDeleteRemote: () => void;
  onDiscard: () => void;
  onChooseDifferent: () => void;
}) {
  const busyAction = draft?.busyAction ?? null;
  const localBusy = draft?.phase === 'picking' || draft?.phase === 'normalizing';
  const busy = localBusy || busyAction !== null;
  const operation = draft?.operation ?? null;
  const destinationMatches = !draft || draft.destination.terminalId === currentTerminalId;
  const destinationBlock = draft ? <View testID="attachment-destination" style={[styles.attachmentDestination, { borderColor: colors.border, backgroundColor: colors.surface }]}>
    <Text style={[styles.attachmentDestinationLine, { color: colors.muted }]}>{`Server · ${draft.destination.server}`}</Text>
    <Text style={[styles.attachmentDestinationLine, { color: colors.muted }]}>{`Session · ${draft.destination.session}`}</Text>
    <Text style={[styles.attachmentDestinationLine, { color: colors.muted }]}>{`Workspace · ${draft.destination.workspace}`}</Text>
    <Text style={[styles.attachmentDestinationLine, { color: colors.muted }]}>{`Terminal · ${draft.destination.terminal}`}</Text>
    <Text style={[styles.attachmentDestinationLine, { color: colors.muted }]}>{`Remote directory · ${draft.remoteDirectory.trim() || ATTACHMENT_DEFAULT_REMOTE_DIRECTORY}`}</Text>
  </View> : null;
  const destinationWarning = !destinationMatches ? <Text accessibilityRole="alert" style={[styles.runtimeHint, { color: colors.danger }]}>This attachment is bound to a different terminal. Return to that terminal to insert, or discard and start again here — it is never inserted into the wrong destination automatically.</Text> : null;
  return <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
    {draft?.notice ? <View testID="attachment-notice" accessibilityLiveRegion="polite" style={[styles.noticeBox, { borderColor: colors.border, backgroundColor: colors.surface }]}><Text style={[styles.emptyBody, { color: colors.text }]}>{draft.notice}</Text></View> : null}
    {draft?.phase === 'error' ? <View style={styles.gone}>
      <Icon name="attach" color={colors.danger} size={32} />
      <Text testID="attachment-error" style={[styles.emptyTitle, { color: colors.text }]}>The image could not be attached</Text>
      <Text style={[styles.emptyBody, { color: colors.muted }]}>{draft.errorMessage || 'Choose a different image and try again.'}</Text>
      <Button label="Choose another image" colors={colors} onPress={onChooseDifferent}>Choose another image</Button>
    </View> : null}
    {draft?.phase === 'choosing' ? <View>
      <Text style={[styles.emptyBody, { color: colors.muted }]}>Choose one image. It is checked against size limits, rotated to its stored orientation, and stripped of location and other metadata before preview.</Text>
      {destinationBlock}
      <Button testID="attachment-pick-photos" label="Choose from photos" colors={colors} disabled={busy} onPress={() => onPickSource('photos')}>Choose from photos</Button>
      <Button testID="attachment-pick-files" label="Choose from files" colors={colors} secondary disabled={busy} onPress={() => onPickSource('files')} style={styles.attachmentActionSpacer}>Choose from files</Button>
    </View> : null}
    {draft?.phase === 'picking' || draft?.phase === 'normalizing' ? <View testID="attachment-progress" style={styles.gone}>
      <ActivityIndicator color={colors.accent} size="large" />
      <Text style={[styles.emptyBody, { color: colors.muted }]}>{draft.phase === 'picking' ? 'Waiting for the system picker…' : 'Checking and normalizing the image…'}</Text>
    </View> : null}
    {draft?.phase === 'ready' && draft.prepared ? <View>
      <Image testID="attachment-preview" source={{ uri: draft.prepared.previewUri }} accessibilityLabel="Normalized image preview" resizeMode="contain" style={[styles.attachmentPreview, { borderColor: colors.border, backgroundColor: colors.surface }]} />
      <Text testID="attachment-meta" style={[styles.emptyBody, { color: colors.muted }]}>{`${draft.prepared.width}×${draft.prepared.height} · ${draft.prepared.format.toUpperCase()} · ${Math.max(1, Math.round(draft.prepared.byteCount / 1024))} KB`}</Text>
      {destinationBlock}
      {destinationWarning}
      {!operation ? <View style={styles.runtimeField}>
        <Text style={[styles.runtimeLabel, { color: colors.text }]}>Remote directory</Text>
        <TextInput testID="attachment-remote-dir" accessibilityLabel="Remote directory" value={draft.remoteDirectory} onChangeText={onRemoteDirectoryChange} autoCapitalize="none" autoComplete="off" autoCorrect={false} editable={!busy} placeholder={ATTACHMENT_DEFAULT_REMOTE_DIRECTORY} placeholderTextColor={colors.placeholder} selectionColor={colors.accent} style={[styles.runtimeInput, { color: colors.text, backgroundColor: colors.elevated, borderColor: colors.border }]} />
      </View> : null}

      {!operation ? <View>
        <Button testID="attachment-upload" label="Upload to server" colors={colors} disabled={busy} onPress={onUpload}>Upload</Button>
        <Text style={[styles.noticeBody, { color: colors.muted }]}>Uploading saves the image on the SSH host. Nothing is added to the terminal yet.</Text>
      </View> : null}

      {operation?.phase === 'pending' ? <View testID="attachment-pending">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Upload waiting</Text>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>{operation.errorMessage || `Waiting: ${operation.errorCode || 'the destination is not ready yet.'}`}</Text>
        <Button testID="attachment-retry-upload" label="Retry upload" colors={colors} disabled={busy} onPress={onRetryUpload}>Retry upload</Button>
        <Button testID="attachment-cancel" label="Cancel upload" colors={colors} secondary disabled={busy} onPress={onCancel} style={styles.attachmentActionSpacer}>Cancel</Button>
      </View> : null}

      {operation?.phase === 'uploading' ? <View testID="attachment-uploading">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Uploading…</Text>
        <View style={[styles.attachmentProgressTrack, { backgroundColor: colors.elevated, borderColor: colors.border }]}>
          <View testID="attachment-progress-fill" style={[styles.attachmentProgressFill, { backgroundColor: colors.accent, width: `${Math.min(100, operation.sizeBytes > 0 ? Math.round((operation.bytesUploaded / operation.sizeBytes) * 100) : 0)}%` }]} />
        </View>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>{`${Math.max(0, Math.round(operation.bytesUploaded / 1024))} KB of ${Math.max(1, Math.round(operation.sizeBytes / 1024))} KB`}</Text>
        <Button testID="attachment-cancel" label="Cancel upload" colors={colors} secondary disabled={busy} onPress={onCancel}>Cancel</Button>
      </View> : null}

      {operation?.phase === 'uploaded' && !operation.remoteRemoved ? <View testID="attachment-uploaded">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Uploaded</Text>
        <Text selectable style={[styles.emptyBody, { color: colors.muted }]}>{operation.remotePath}</Text>
        <Text style={[styles.noticeBody, { color: colors.muted }]}>The image is saved on the SSH host. Sending the terminal request passes it to the AI running there — insert only adds the path reference.</Text>
        {operation.errorMessage || operation.errorCode ? <Text testID="attachment-insert-blocked" accessibilityRole="alert" style={[styles.runtimeHint, { color: colors.danger }]}>{`Insert blocked: ${operation.errorMessage || operation.errorCode}`}</Text> : null}
        <Button testID="attachment-insert" label="Insert into terminal input" colors={colors} disabled={busy || !destinationMatches} onPress={onInsert}>Insert into terminal input</Button>
        <Text style={[styles.noticeBody, { color: colors.muted }]}>Inserting adds the image path to the command line. You still review and send it yourself — nothing is submitted automatically.</Text>
        <View style={styles.noticeActions}>
          <Pressable testID="attachment-delete-remote" accessibilityRole="button" accessibilityLabel="Delete from server" disabled={busy} onPress={onDeleteRemote} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Delete from server</Text></Pressable>
          <Pressable testID="attachment-discard" accessibilityRole="button" accessibilityLabel="Discard local image" disabled={busy} onPress={onDiscard} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Discard</Text></Pressable>
          <Pressable accessibilityRole="button" accessibilityLabel="Choose a different image" disabled={busy} onPress={onChooseDifferent} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Choose a different image</Text></Pressable>
        </View>
      </View> : null}

      {operation?.phase === 'inserted' && !operation.remoteRemoved ? <View testID="attachment-inserted">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>{ATTACHMENT_INSERTED_NOTICE}</Text>
        {operation.insertUnconfirmed ? <Text testID="attachment-unconfirmed" accessibilityRole="alert" style={[styles.runtimeHint, { color: colors.danger }]}>{ATTACHMENT_INSERT_UNCONFIRMED_NOTICE}</Text> : null}
        {operation.insertUnconfirmed ? <Button testID="attachment-retry-insert" label="Insert again" colors={colors} secondary disabled={busy || !destinationMatches} onPress={onInsert} style={styles.attachmentActionSpacer}>Retry insert</Button> : null}
        <Text selectable style={[styles.emptyBody, { color: colors.muted }]}>{operation.remotePath}</Text>
        <View style={styles.noticeActions}>
          <Pressable testID="attachment-delete-remote" accessibilityRole="button" accessibilityLabel="Delete from server" disabled={busy} onPress={onDeleteRemote} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Delete from server</Text></Pressable>
          <Pressable testID="attachment-discard" accessibilityRole="button" accessibilityLabel="Discard local image" disabled={busy} onPress={onDiscard} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Discard</Text></Pressable>
        </View>
      </View> : null}

      {operation?.phase === 'failed' && !operation.remoteRemoved ? <View testID="attachment-failed">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Upload failed</Text>
        <Text accessibilityRole="alert" style={[styles.emptyBody, { color: colors.danger }]}>{operation.errorMessage || operation.errorCode || 'The upload could not be completed.'}</Text>
        <Button testID="attachment-retry-upload" label="Retry upload" colors={colors} disabled={busy} onPress={onRetryUpload}>Retry upload</Button>
        <View style={styles.noticeActions}>
          <Pressable testID="attachment-delete-remote" accessibilityRole="button" accessibilityLabel="Delete from server" disabled={busy} onPress={onDeleteRemote} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Delete from server</Text></Pressable>
          <Pressable testID="attachment-discard" accessibilityRole="button" accessibilityLabel="Discard local image" disabled={busy} onPress={onDiscard} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Discard</Text></Pressable>
          <Pressable accessibilityRole="button" accessibilityLabel="Choose a different image" disabled={busy} onPress={onChooseDifferent} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Choose a different image</Text></Pressable>
        </View>
      </View> : null}

      {operation?.phase === 'cancelled' && !operation.remoteRemoved ? <View testID="attachment-cancelled">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Upload cancelled</Text>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>The transfer was stopped; nothing was inserted into the terminal.</Text>
        <Button testID="attachment-upload" label="Upload to server" colors={colors} disabled={busy} onPress={onUpload}>Upload again</Button>
        <View style={styles.noticeActions}>
          <Pressable testID="attachment-delete-remote" accessibilityRole="button" accessibilityLabel="Delete from server" disabled={busy} onPress={onDeleteRemote} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Delete from server</Text></Pressable>
          <Pressable testID="attachment-discard" accessibilityRole="button" accessibilityLabel="Discard local image" disabled={busy} onPress={onDiscard} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Discard</Text></Pressable>
        </View>
      </View> : null}

      {operation?.remoteRemoved ? <View testID="attachment-deleted">
        <Text style={[styles.emptyTitle, { color: colors.text }]}>Deleted from server</Text>
        <Text style={[styles.emptyBody, { color: colors.muted }]}>{operation.phase === 'inserted'
          ? 'The remote file was removed. The inserted path line already reached the terminal — edit it there yourself.'
          : 'The remote file was removed. Upload again to reattach it.'}</Text>
        {operation.phase === 'uploaded' || operation.phase === 'failed'
          ? <Button testID="attachment-retry-upload" label="Upload to server" colors={colors} disabled={busy} onPress={onRetryUpload}>Upload again</Button>
          : null}
        {operation.phase === 'cancelled'
          ? <Button testID="attachment-upload" label="Upload to server" colors={colors} disabled={busy} onPress={onUpload}>Upload again</Button>
          : null}
        <View style={styles.noticeActions}>
          <Pressable testID="attachment-discard" accessibilityRole="button" accessibilityLabel="Discard local image" disabled={busy} onPress={onDiscard} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Discard</Text></Pressable>
        </View>
      </View> : null}
    </View> : null}
    {!draft ? <Text style={[styles.emptyBody, { color: colors.muted }]}>No image selected.</Text> : null}
  </ScrollView>;
}

function emptyRuntimeBackend(backend: RuntimeBackend): RuntimeBackendDiscovery {
  return { backend, state: 'loading', errorCode: '', errorMessage: '', candidates: [], canCreate: backend === 'tmux' };
}

function runtimeDiscoveryFinal(discovery: RuntimeDiscovery): boolean {
  return discovery.backends.every(backend => backend.state !== 'loading');
}

function suggestedTmuxName(discovery: RuntimeDiscovery | null): string {
  const tmux = discovery?.backends.find(item => item.backend === 'tmux');
  return tmux?.candidates.some(item => item.name === 'meeterm') ? '' : 'meeterm';
}

function validateTmuxSessionName(value: string): string {
  const name = value.trim();
  if (!name) return 'Enter a session name.';
  if (name.length > 64 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name) || name === '.' || name === '..') {
    return 'Use 1–64 letters, numbers, periods, hyphens, or underscores; start with a letter or number.';
  }
  return '';
}

function runtimeFailureMessage(errorCode: string, errorMessage: string, fallback: string): string {
  const detail = errorMessage || errorCode;
  return detail ? `${detail} Refresh runtimes and try again.` : fallback;
}

function createdTmuxCandidate(name: string): RuntimeCandidate {
  return {
    id: '',
    backend: 'tmux',
    name,
    state: 'running',
    selectable: true,
    isDefault: false,
    lastUsed: false,
    errorCode: '',
    errorMessage: '',
  };
}

function RuntimePicker({ visible, serverName, discovery, createVisible, busy, discoveryBusy, cancelDisabled, selectingId, selectionErrors, message, creationError, notification, onCancel, onDismiss, onRefresh, onRetryBackend, onSelect, onOpenCreate, onBackToList, onCreate, colors }: {
  visible: boolean;
  serverName: string;
  discovery: RuntimeDiscovery | null;
  createVisible: boolean;
  busy: boolean;
  discoveryBusy: boolean;
  cancelDisabled: boolean;
  selectingId: string;
  selectionErrors: Record<string, string>;
  message: string;
  creationError: string;
  notification: ReactNode | null;
  onCancel: () => void;
  onDismiss: () => void;
  onRefresh: () => void;
  onRetryBackend: (backend: RuntimeBackend) => void;
  onSelect: (candidate: RuntimeCandidate) => void;
  onOpenCreate: () => void;
  onBackToList: () => void;
  onCreate: (name: string) => Promise<boolean>;
  colors: Palette;
}) {
  const reducedMotion = useReducedMotion();
  const [name, setName] = useState('');
  const [nameError, setNameError] = useState('');
  useEffect(() => {
    if (visible && createVisible) {
      setName(suggestedTmuxName(discovery));
      setNameError('');
    }
  }, [createVisible, visible]);

  const submitCreate = async () => {
    const error = validateTmuxSessionName(name);
    if (error) { setNameError(error); return; }
    setNameError('');
    await onCreate(name.trim());
  };
  const backend = (kind: RuntimeBackend) => discovery?.backends.find(item => item.backend === kind) ?? emptyRuntimeBackend(kind);
  const renderCandidate = (candidate: RuntimeCandidate) => {
    const stopped = candidate.state !== 'running';
    const unavailable = stopped || !candidate.selectable;
    const error = selectionErrors[candidate.id] || candidate.errorMessage || candidate.errorCode;
    return <View key={candidate.id} style={[styles.runtimeRowContainer, { borderBottomColor: colors.border }]}>
      <Pressable testID={`runtime-row-${candidate.backend}-${candidate.id}`} accessibilityRole="button" accessibilityLabel={`${candidate.backend === 'tmux' ? 'tmux' : 'Herdr'} runtime ${candidate.name}`} accessibilityHint={stopped ? 'Open this runtime on your computer, then refresh.' : !candidate.selectable ? 'This runtime is unavailable. Refresh runtimes and try again.' : undefined} accessibilityState={{ disabled: unavailable || busy, selected: selectingId === candidate.id }} disabled={unavailable || busy} onPress={() => onSelect(candidate)} style={({ pressed }) => [styles.runtimeRow, pressed && { backgroundColor: colors.surface }, (unavailable || busy) && { opacity: stopped ? .55 : .8 }]}>
        <View style={styles.runtimeRowCopy}>
          <View style={styles.runtimeNameLine}><Text numberOfLines={2} style={[styles.runtimeName, { color: colors.text }]}>{candidate.name}</Text>{candidate.lastUsed ? <Text style={[styles.runtimeBadge, { color: colors.accent, borderColor: colors.accent }]}>Last used</Text> : null}</View>
          <Text style={[styles.runtimeState, { color: stopped || !candidate.selectable ? colors.muted : colors.accent }]}>{stopped ? 'Stopped' : candidate.selectable ? 'Running' : 'Unavailable'}</Text>
          {stopped ? <Text style={[styles.runtimeHint, { color: colors.muted }]}>Open this session in {candidate.backend === 'herdr' ? 'Herdr' : 'tmux'} on your computer, then tap Refresh.</Text> : null}
          {error ? <Text accessibilityRole="alert" style={[styles.runtimeError, { color: colors.danger }]}>{error}</Text> : null}
        </View>
        {selectingId === candidate.id ? <ActivityIndicator color={colors.accent} /> : <Icon name="chevron" color={stopped ? colors.muted : colors.accent} size={18} />}
      </Pressable>
    </View>;
  };
  const renderSection = (kind: RuntimeBackend, title: string) => {
    const item = backend(kind);
    const sectionError = item.errorMessage || item.errorCode;
    return <View key={kind} style={styles.runtimeSection}>
      <View style={styles.runtimeSectionHeading}><Text style={[styles.runtimeSectionTitle, { color: colors.text }]}>{title}</Text>{item.state === 'loading' ? <ActivityIndicator size="small" color={colors.accent} /> : null}</View>
      {item.state === 'error' || sectionError ? <View style={[styles.runtimeSectionError, { backgroundColor: colors.surface }]}><Text accessibilityRole="alert" style={[styles.runtimeHint, { color: colors.danger }]}>{sectionError || 'This backend could not be inspected.'}</Text>{item.state === 'error' ? <Button label={`Retry ${title} discovery`} colors={colors} secondary disabled={busy || discoveryBusy} onPress={() => onRetryBackend(kind)}>Retry</Button> : null}</View> : null}
      {item.state === 'loading' ? <Text style={[styles.runtimeHint, { color: colors.muted }]}>Looking for running sessions…</Text> : item.candidates.length ? item.candidates.map(renderCandidate) : <Text style={[styles.runtimeHint, { color: colors.muted }]}>{kind === 'tmux' ? 'No running tmux sessions found.' : 'No running Herdr sessions found.'}</Text>}
      {kind === 'tmux' && item.state === 'ready' && item.canCreate ? <Button testID="runtime-create-tmux" label="Create tmux session" colors={colors} secondary disabled={busy || discoveryBusy} onPress={onOpenCreate}>Create tmux session</Button> : null}
    </View>;
  };

  return <Modal visible={visible} animationType={reducedMotion ? 'fade' : 'slide'} presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} allowSwipeDismissal={false} onRequestClose={() => createVisible && !busy ? onBackToList() : onCancel()} onDismiss={onDismiss} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
    <SafeAreaProvider><SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.flex, { backgroundColor: colors.background }]}>
      {Platform.OS === 'android' ? <StatusBar barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} /> : null}
      <View style={[styles.runtimeHeader, { borderBottomColor: colors.border }]}>
        {createVisible ? <Pressable accessibilityRole="button" accessibilityLabel="Back to runtime list" disabled={busy} onPress={onBackToList} style={styles.runtimeHeaderAction}><Text style={[styles.actionText, { color: colors.accent, opacity: busy ? .45 : 1 }]}>Back</Text></Pressable> : <Pressable accessibilityRole="button" accessibilityLabel="Cancel runtime selection" disabled={cancelDisabled} onPress={onCancel} style={styles.runtimeHeaderAction}><Text style={[styles.actionText, { color: colors.accent, opacity: cancelDisabled ? .45 : 1 }]}>Cancel</Text></Pressable>}
        <Text numberOfLines={2} accessibilityRole="header" style={[styles.runtimeHeaderTitle, { color: colors.text }]}>{createVisible ? 'Create tmux session' : `Choose a runtime for ${serverName}`}</Text>
        {createVisible ? <IconButton icon="close" label="Cancel runtime selection" colors={colors} disabled={cancelDisabled} onPress={onCancel} /> : <View style={styles.runtimeHeaderPlaceholder} />}
      </View>
      {notification ? <View style={styles.runtimeNotification}>{notification}</View> : null}
      {createVisible ? <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={styles.runtimeFormContent}>
        <Text style={[styles.runtimeIntroTitle, { color: colors.text }]}>Create a tmux session</Text>
        <Text style={[styles.runtimeHint, { color: colors.muted }]}>This creates a detached session on {serverName} and opens it after the native runtime confirms its identity.</Text>
        <View style={styles.runtimeField}><Text style={[styles.runtimeLabel, { color: colors.text }]}>Session name</Text><TextInput accessibilityLabel="tmux session name" testID="runtime-tmux-name" value={name} onChangeText={value => { setName(value); setNameError(''); }} autoCapitalize="none" autoComplete="off" autoCorrect={false} maxLength={64} returnKeyType="go" onSubmitEditing={() => { void submitCreate(); }} placeholder="meeterm" placeholderTextColor={colors.placeholder} selectionColor={colors.accent} style={[styles.runtimeInput, { color: colors.text, backgroundColor: colors.elevated, borderColor: nameError ? colors.danger : colors.border }]} />{nameError ? <Text accessibilityRole="alert" style={[styles.runtimeError, { color: colors.danger }]}>{nameError}</Text> : null}</View>
        {creationError ? <Text accessibilityRole="alert" style={[styles.runtimeError, { color: colors.danger }]}>{creationError}</Text> : null}
        <Button testID="runtime-tmux-create-submit" label="Create tmux session" colors={colors} disabled={busy} onPress={() => { void submitCreate(); }}>{busy ? 'Creating…' : 'Create and open'}</Button>
        <Text style={[styles.runtimeHint, { color: colors.muted }]}>If the name is already in use or the session changes before creation finishes, meeterm will refresh the list and leave the existing sessions untouched.</Text>
      </ScrollView> : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.runtimePickerContent}>
        <View style={styles.runtimeIntro}><Text accessibilityRole="header" style={[styles.runtimeIntroTitle, { color: colors.text }]}>Where should we continue?</Text><Text numberOfLines={2} style={[styles.runtimeServer, { color: colors.muted }]}>{serverName}</Text><Text style={[styles.runtimeHint, { color: colors.muted }]}>Choose a running session. Your last-used runtime is highlighted only as a hint; nothing opens until you tap a row.</Text></View>
        {message ? <Text accessibilityRole="alert" style={[styles.runtimeMessage, { color: colors.danger }]}>{message}</Text> : null}
        {renderSection('tmux', 'tmux')}
        {renderSection('herdr', 'Herdr')}
        <Button testID="runtime-refresh" label="Refresh runtimes" colors={colors} secondary disabled={busy || discoveryBusy} onPress={onRefresh}>Refresh</Button>
        <Text style={[styles.runtimeHint, { color: colors.muted }]}>Herdr can be selected only when its session is already running. Start or create Herdr sessions on the computer.</Text>
      </ScrollView>}
    </SafeAreaView></SafeAreaProvider>
  </Modal>;
}

function runtimeHintForProfile(profile?: Pick<ServerProfile, 'backend' | 'runtime'>): RuntimeHint | null {
  if (!profile) return null;
  const backend = profile.backend ?? 'tmux';
  return { backend, runtime: profile.runtime || (backend === 'herdr' ? 'default' : 'meeterm') };
}

function applyRuntimeHint(discovery: RuntimeDiscovery, hint: RuntimeHint | null): RuntimeDiscovery {
  return {
    ...discovery,
    backends: discovery.backends.map(backend => ({
      ...backend,
      candidates: backend.candidates.map(candidate => ({
        ...candidate,
        lastUsed: candidate.lastUsed || Boolean(hint && hint.backend === candidate.backend && hint.runtime === candidate.name),
      })),
    })),
  };
}

type RecoveryReasonKind =
  | 'foreground'
  | 'validation'
  | 'resync'
  | 'retryExhausted'
  | 'runtimeMismatch'
  | 'runtimeMissing'
  | 'terminalMissing'
  | 'hostKeyChanged'
  | 'authentication'
  | 'controllerBusy'
  | 'incompatible'
  | 'unknown';

type RecoveryRailCopy = {
  title: string;
  detail: string;
  meta: string;
  progress: boolean;
  danger: boolean;
  assertive: boolean;
  retry: boolean;
  review: boolean;
  reviewLabel?: string;
  reviewAccessibilityLabel?: string;
  connectionDetails: boolean;
  chooseTerminal: boolean;
  change: boolean;
};

function safeRecoveryLabel(value: string, fallback: string): string {
  const cleaned = value.replace(/[\u0000-\u001f\u007f]/g, ' ').replace(/\s+/g, ' ').trim();
  return cleaned ? cleaned.slice(0, 128) : fallback;
}

const RECOVERY_REASON_ALIASES: Record<string, RecoveryReasonKind> = {
  foreground: 'foreground',
  foreground_check: 'foreground',
  checking_connection: 'foreground',
  foreground_lost: 'foreground',
  validation: 'validation',
  validating: 'validation',
  workspace_validation: 'validation',
  runtime_validation: 'validation',
  topology_validation: 'validation',
  identity_validation: 'validation',
  capability_validation: 'validation',
  resync: 'resync',
  resynchronizing: 'resync',
  screen_resync: 'resync',
  screen_refresh: 'resync',
  terminal_refresh: 'resync',
  retry_exhausted: 'retryExhausted',
  exhausted: 'retryExhausted',
  offline: 'retryExhausted',
  automatic_reconnect_disabled: 'retryExhausted',
  runtime_mismatch: 'runtimeMismatch',
  runtime_identity_mismatch: 'runtimeMismatch',
  runtime_identity_uncertain: 'runtimeMismatch',
  runtime_replaced: 'runtimeMismatch',
  runtime_restarted: 'runtimeMismatch',
  candidate_changed: 'runtimeMismatch',
  identity_mismatch: 'runtimeMismatch',
  tmux_runtime_missing: 'runtimeMismatch',
  tmux_runtime_collision: 'runtimeMismatch',
  tmux_runtime_unknown: 'runtimeMismatch',
  tmux_topology_unsafe: 'runtimeMismatch',
  runtime_missing: 'runtimeMissing',
  runtime_not_found: 'runtimeMissing',
  session_missing: 'runtimeMissing',
  herdr_session_missing: 'runtimeMissing',
  terminal_missing: 'terminalMissing',
  pane_missing: 'terminalMissing',
  selected_terminal_missing: 'terminalMissing',
  herdr_terminal_missing: 'terminalMissing',
  host_key_changed: 'hostKeyChanged',
  changed_host_key: 'hostKeyChanged',
  authentication_failed: 'authentication',
  auth_failed: 'authentication',
  host_authentication_failed: 'authentication',
  controller_busy: 'controllerBusy',
  herdr_controller_busy: 'controllerBusy',
  controller_conflict: 'controllerBusy',
  incompatible: 'incompatible',
  incompatible_runtime: 'incompatible',
  runtime_incompatible: 'incompatible',
  herdr_incompatible: 'incompatible',
  continuity_uncertain: 'unknown',
  herdr_continuity_uncertain: 'unknown',
};

function recoveryReasonKind(value: string): RecoveryReasonKind {
  const normalized = value
    .replace(/([a-z])([A-Z])/g, '$1_$2')
    .replace(/[^A-Za-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '')
    .toLowerCase();
  return RECOVERY_REASON_ALIASES[normalized] ?? 'unknown';
}

function recoveryRailCopy(control: WorkspaceControl, backend: RuntimeBackend, runtime: string, server: string, recovered = false): RecoveryRailCopy | null {
  const runtimeLabel = safeRecoveryLabel(runtime, 'this runtime');
  const serverLabel = safeRecoveryLabel(server, 'this server');
  const reason = recoveryReasonKind(control.recovery.reason);
  const meta = 'Last received output · Input paused';
  const validationProgress = reason === 'validation'
    || reason === 'runtimeMismatch'
    || reason === 'runtimeMissing'
    || reason === 'terminalMissing'
    || reason === 'incompatible';
  if (recovered) {
    return {
      title: 'Back online',
      detail: 'Terminal is live · Input available',
      meta: 'Terminal is live · Input available',
      progress: false,
      danger: false,
      assertive: false,
      retry: false,
      review: false,
      connectionDetails: false,
      chooseTerminal: false,
      change: false,
    };
  }

  if (control.recovery.phase === 'reconnecting') {
    if (reason === 'foreground') {
      return { title: 'Checking connection…', detail: 'Checking whether this workspace is still available.', meta, progress: true, danger: false, assertive: false, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: false };
    }
    if (validationProgress) {
      return { title: 'Verifying this workspace…', detail: 'Checking the server, runtime, and terminal.', meta, progress: true, danger: false, assertive: false, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: false };
    }
    return { title: 'Reconnecting…', detail: 'Waiting for the server.', meta, progress: true, danger: false, assertive: false, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: false };
  }

  if (control.recovery.phase === 'resynchronizing') {
    if (validationProgress) {
      return { title: 'Verifying this workspace…', detail: 'Checking the server, runtime, and terminal.', meta, progress: true, danger: false, assertive: false, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: false };
    }
    return { title: 'Refreshing terminal…', detail: 'Receiving the current remote screen.', meta, progress: true, danger: false, assertive: false, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: false };
  }

  if (control.recovery.phase !== 'stopped') return null;

  switch (reason) {
    case 'runtimeMismatch':
      return { title: 'This runtime can’t be restored', detail: `The runtime named “${runtimeLabel}” is not the same instance as before.`, meta, progress: false, danger: true, assertive: true, retry: true, review: false, connectionDetails: false, chooseTerminal: false, change: true };
    case 'runtimeMissing':
      return { title: 'This runtime can’t be restored', detail: `The runtime named “${runtimeLabel}” is no longer available.`, meta, progress: false, danger: true, assertive: true, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: true };
    case 'terminalMissing':
      return { title: 'This terminal no longer exists', detail: 'Use Change… for a fresh runtime or server selection before sending anything.', meta: 'Last received output · Input paused · Change for fresh selection', progress: false, danger: true, assertive: true, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: true };
    case 'hostKeyChanged':
      return { title: 'Server identity changed', detail: 'Review the host key before connecting again.', meta, progress: false, danger: false, assertive: true, retry: false, review: true, reviewLabel: 'Review key', reviewAccessibilityLabel: 'Review key change', connectionDetails: false, chooseTerminal: false, change: false };
    case 'authentication':
      return { title: 'Sign-in is required', detail: 'Enter your connection details to continue.', meta, progress: false, danger: false, assertive: true, retry: false, review: false, connectionDetails: true, chooseTerminal: false, change: false };
    case 'controllerBusy':
      return { title: 'Another client is controlling this terminal.', detail: 'Release it there, then try again.', meta, progress: false, danger: false, assertive: true, retry: true, review: false, connectionDetails: false, chooseTerminal: false, change: true };
    case 'incompatible':
      return { title: 'This runtime is no longer compatible with meeterm.', detail: 'Choose another runtime to continue.', meta, progress: false, danger: true, assertive: true, retry: false, review: false, connectionDetails: false, chooseTerminal: false, change: true };
    case 'retryExhausted':
    case 'unknown':
    default:
      return { title: 'Still offline', detail: `Couldn’t reach ${serverLabel}.`, meta, progress: false, danger: false, assertive: true, retry: true, review: false, connectionDetails: false, chooseTerminal: false, change: true };
  }
}

function RecoveryRail({ copy, colors, busy, onRetry, onReview, onConnectionDetails, onChooseTerminal, onChange }: {
  copy: RecoveryRailCopy;
  colors: Palette;
  busy: { retry: boolean; change: boolean };
  onRetry: () => void;
  onReview: () => void;
  onConnectionDetails: () => void;
  onChooseTerminal: () => void;
  onChange: () => void;
}) {
  const accessibilityLabel = `${copy.title}. ${copy.detail}. ${copy.meta.replace(' · ', '. ')}.`;
  const reducedMotion = useReducedMotion();
  return <View testID="recovery-rail" style={[styles.recoveryRail, { backgroundColor: DARK.surface, borderTopColor: DARK.border, borderBottomColor: DARK.border }]}>
    <View accessible={false} importantForAccessibility="no-hide-descendants" style={styles.recoveryRailIcon}>
      {copy.progress && !reducedMotion ? <ActivityIndicator accessibilityElementsHidden color={DARK.agentStatus.working} /> : <Icon name={copy.danger ? 'key' : 'terminal'} color={copy.danger ? DARK.danger : DARK.agentStatus.working} size={20} />}
    </View>
    <View style={styles.recoveryRailBody}>
      <View accessible accessibilityRole="text" accessibilityLabel={accessibilityLabel} accessibilityLiveRegion={copy.assertive ? 'assertive' : 'polite'} style={styles.recoveryRailText}>
        <Text testID="recovery-title" style={[styles.recoveryRailTitle, { color: colors.text }]}>{copy.title}</Text>
        <Text testID="recovery-detail" style={[styles.recoveryRailDetail, { color: colors.muted }]}>{copy.detail}</Text>
        <Text testID="recovery-meta" style={[styles.recoveryRailMeta, { color: colors.muted }]}>{copy.meta}</Text>
      </View>
      {copy.retry || copy.review || copy.connectionDetails || copy.chooseTerminal || copy.change ? <View style={styles.recoveryRailActions}>
        {copy.retry ? <Button testID="recovery-retry" label="Retry recovery" colors={colors} disabled={busy.retry} onPress={onRetry}>Retry</Button> : null}
        {copy.review ? <Pressable testID="recovery-review" accessibilityRole="button" accessibilityLabel={copy.reviewAccessibilityLabel ?? 'Review recovery'} onPress={onReview} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>{copy.reviewLabel ?? 'Review'}</Text></Pressable> : null}
        {copy.connectionDetails ? <Pressable testID="recovery-connection-details" accessibilityRole="button" accessibilityLabel="Connection details" onPress={onConnectionDetails} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Connection details</Text></Pressable> : null}
        {copy.chooseTerminal ? <Pressable testID="recovery-choose-terminal" accessibilityRole="button" accessibilityLabel="Choose another terminal" onPress={onChooseTerminal} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Choose terminal…</Text></Pressable> : null}
        {copy.change ? <Pressable testID="recovery-change" accessibilityRole="button" accessibilityLabel="Change connection or runtime" accessibilityState={{ disabled: busy.change }} disabled={busy.change} onPress={onChange} style={[styles.textAction, { opacity: busy.change ? .45 : 1 }]}><Text style={[styles.actionText, { color: colors.accent }]}>Change…</Text></Pressable> : null}
      </View> : null}
    </View>
  </View>;
}

function announceRecovery(copy: RecoveryRailCopy): void {
  if (Platform.OS !== 'ios') return;
  const announce = AccessibilityInfo?.announceForAccessibilityWithOptions;
  if (typeof announce !== 'function') return;
  try {
    void announce(`${copy.title}. ${copy.detail}. ${copy.meta.replace(' · ', '. ')}.`, { queue: true });
  } catch {
    // Accessibility announcements are observational and must not affect the
    // native recovery state or interrupt the current screen.
  }
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
  const [session, setSession] = useState<WorkspaceState>(() => fixture ? smokeWorkspaceState(fixture.panes, smokeScreen?.startsWith('herdr-') || smokeScreen === 'long-workspaces', smokeScreen === 'long-workspaces', fixture.control) : EMPTY_WORKSPACES);
  // Native control metadata is authoritative. A missing/invalid control
  // object deliberately yields false gates through normalizeWorkspaceControl.
  const control = normalizeWorkspaceControl((session as WorkspaceState & { control?: unknown }).control);
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
  const [runtimeDiscovery, setRuntimeDiscovery] = useState<RuntimeDiscovery | null>(() => fixture?.runtimeDiscovery ?? null);
  const [runtimePickerVisible, setRuntimePickerVisible] = useState(() => fixture?.runtimePickerVisible ?? false);
  const [runtimeCreateVisible, setRuntimeCreateVisible] = useState(() => fixture?.runtimeCreateVisible ?? false);
  const [switcherTarget, setSwitcherTarget] = useState<SwitcherTarget | null>(() => fixture?.switcherTarget ?? null);
  const [switcherStarted, setSwitcherStarted] = useState(() => fixture?.switcherStarted ?? false);
  const [switcherAccepting, setSwitcherAccepting] = useState(false);
  const [switcherMessage, setSwitcherMessage] = useState('');
  const [runtimeSelectingId, setRuntimeSelectingId] = useState('');
  const [runtimeBusy, setRuntimeBusy] = useState(false);
  const [runtimeActionBusy, setRuntimeActionBusy] = useState(false);
  const [runtimeSelectionErrors, setRuntimeSelectionErrors] = useState<Record<string, string>>({});
  const [runtimeMessage, setRuntimeMessage] = useState(() => fixture?.runtimeMessage ?? '');
  const runtimeNameRef = useRef('');
  const [runtimeCreationError, setRuntimeCreationError] = useState('');
  const [runtimeHint, setRuntimeHint] = useState<RuntimeHint | null>(null);
  const [, setRuntimeBound] = useState(() => Boolean(fixture?.hasConnected));
  const [controlMessage, setControlMessage] = useState(() => fixture?.controlMessage ?? '');
  const [cleanupWarning, setCleanupWarning] = useState<WorkspaceControl['cleanupWarning']>(() => (
    fixture?.control?.cleanupWarning ?? (fixture ? legacyCleanupWarning(fixture.connection) : null)
  ));
  const [, setDismissedCleanupWarningId] = useState('');
  const [pollProblem, setPollProblem] = useState(false);
  const [removedHostKeyId, setRemovedHostKeyId] = useState('');
  const [hasConnected, setHasConnected] = useState(() => fixture?.hasConnected ?? false);
  const [attachment, setAttachment] = useState<AttachmentDraftState | null>(() => fixture?.attachmentDraft ?? null);
  const [commandBusy, setCommandBusy] = useState(false);
  const [appState, setAppState] = useState(AppState.currentState);
  const [foundation, setFoundation] = useState(() => smokeRoute?.kind === 'foundation');
  const [recoveryInvalidated, setRecoveryInvalidated] = useState(false);
  const [recoveryPending, setRecoveryPending] = useState({ retry: false, change: false });
  const [recoveredEpoch, setRecoveredEpoch] = useState('');
  const commandPending = useRef(false);
  const commandVersion = useRef(0);
  const shownHostKey = useRef('');
  const pendingModal = useRef<(() => void) | null>(null);
  const formSavedProfile = useRef<ServerProfile | undefined>(undefined);
  const returnToServersAfterForm = useRef(false);
  const foregroundCommands = useRef(Promise.resolve());
  const listOffsets = useRef({ normal: 0, search: 0 });
  const workspaceList = useRef<FlatList<Workspace>>(null);
  const foreground = useRef(AppState.currentState === 'active');
  const runtimeBoundRef = useRef(Boolean(fixture?.hasConnected));
  const runtimeSelectionRequired = useRef(false);
  const ignoreReadyUntilNewConnection = useRef(false);
  const selectedRuntimeRef = useRef<RuntimeCandidate | null>(null);
  const pendingRuntimeSelection = useRef<RuntimeCandidate | null>(null);
  const pendingRuntimeCreation = useRef('');
  const pendingRuntimeSelectionBaseline = useRef({ connectionGeneration: '', revision: -1, errorCode: '' });
  const pendingRuntimeCreationBaseline = useRef({ connectionGeneration: '', revision: -1, errorCode: '' });
  const pendingRuntimeRefresh = useRef<PendingRuntimeRefresh | null>(null);
  const runtimeDiscoveryLoading = useRef(false);
  const runtimeDiscoveryAttempt = useRef(0);
  const runtimeDiscoveryLoadedAttempt = useRef(-1);
  const runtimeDiscoveryRequestedPhase = useRef('');
  const runtimeConnectionGeneration = useRef<string | null>(fixture?.runtimeDiscovery?.connectionGeneration ?? null);
  const runtimeHintRef = useRef<RuntimeHint | null>(null);
  const switcherOperation = useRef(false);
  const switcherBoundary = useRef(Boolean(fixture?.switcherStarted));
  const switcherCancelFence = useRef(false);
  const switcherCancelReleaseIssued = useRef(false);
  const boundaryFailureFence = useRef<{ errorCode: string; errorMessage: string } | null>(null);
  const returnToSwitcherAfterForm = useRef(false);
  const switcherFormConnected = useRef(false);
  const controlRef = useRef(control);
  const dismissedCleanupWarningIdRef = useRef('');
  const recoveryInvalidatedRef = useRef(recoveryInvalidated);
  const workspaceObservationRef = useRef(Boolean(fixture?.hasConnected && fixture.panes.length > 0));
  const retainedPaneRef = useRef<RemoteTerminal | null>(null);
  const recoveryPendingRef = useRef<RecoveryPendingActions>({ retry: null, change: null });
  const attachmentGeneration = useRef(0);
  const recoveryMilestoneRef = useRef<{ epoch: string; phase: WorkspaceControl['recovery']['phase']; attempt: number; retained: boolean; strongReady: boolean } | null>(null);
  const completedRecoveryEpochRef = useRef('');
  const recoveredTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const announcedRecoveryRef = useRef('');

  controlRef.current = control;
  recoveryInvalidatedRef.current = recoveryInvalidated;
  workspaceObservationRef.current = !recoveryInvalidated
    && ((control.hasRetainedWork && control.recovery.phase !== 'none')
      || (runtimeBoundRef.current && panes.length > 0));

  const updateRuntimeBound = useCallback((value: boolean) => {
    runtimeBoundRef.current = value;
    setRuntimeBound(value);
  }, []);

  const failClosedForBoundaryResult = useCallback((errorCode: string, errorMessage: string) => {
    boundaryFailureFence.current = { errorCode, errorMessage };
    recoveryInvalidatedRef.current = true;
    workspaceObservationRef.current = false;
    setRecoveryInvalidated(true);
    setRecoveredEpoch('');
    ignoreReadyUntilNewConnection.current = true;
    runtimeSelectionRequired.current = true;
    updateRuntimeBound(false);
    setHasConnected(false);
    setConnection(current => ({
      ...current,
      state: 'Failed',
      errorCode,
      errorMessage,
    }));
    setControlMessage(errorMessage);
  }, [updateRuntimeBound]);

  const observeCleanupWarning = useCallback((candidate: WorkspaceControl['cleanupWarning']) => {
    if (!candidate) return;
    if (candidate.id === dismissedCleanupWarningIdRef.current) return;
    setCleanupWarning(current => current?.id === candidate.id ? current : candidate);
  }, []);

  const dismissCleanupWarning = useCallback(() => {
    const id = cleanupWarning?.id;
    if (!id) return;
    dismissedCleanupWarningIdRef.current = id;
    setDismissedCleanupWarningId(id);
    setCleanupWarning(null);
  }, [cleanupWarning]);

  const invalidateRuntimeDiscovery = useCallback((showPicker: boolean) => {
    // A late result from the previous host/runtime identity must not repopulate
    // the picker after a host-key change or a lost selected runtime.
    runtimeDiscoveryAttempt.current += 1;
    runtimeDiscoveryLoadedAttempt.current = -1;
    runtimeDiscoveryRequestedPhase.current = '';
    runtimeConnectionGeneration.current = null;
    pendingRuntimeSelectionBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    pendingRuntimeCreationBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    pendingRuntimeRefresh.current = null;
    setRuntimeDiscovery(null);
    setRuntimeCreateVisible(false);
    setRuntimeSelectionErrors({});
    setRuntimeCreationError('');
    setRuntimePickerVisible(showPicker);
  }, []);

  useEffect(() => {
    recordStartupPhase('app_content_mounted');
  }, []);

  const loadProfiles = useCallback(async () => {
    if (smokeFixtureActive) return;
    recordStartupPhase('profiles_requested');
    setProfilesLoading(true);
    try {
      setProfiles(await MeetermTerminal.getProfiles());
      setProfilesError(false);
      recordStartupPhase('profiles_succeeded');
    }
    catch {
      setProfilesError(true);
      recordStartupPhase('profiles_failed');
    }
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
    const applyForeground = (isForeground: boolean) => {
      // A public fixture follows the real view lifecycle, but must not
      // reconnect or otherwise touch a remote runtime.
      if (smokeFixtureActive) return;
      // Preserve OS event order. Rust owns reconnect policy and timers.
      foregroundCommands.current = foregroundCommands.current
        .then(() => MeetermTerminal.setForeground(CONNECTION_ID, isForeground))
        .catch(() => setControlMessage('Could not update the connection after the app changed state. Check your connection.'));
    };
    // A deep link may mount its screen while iOS is still inactive. Keep
    // presentation state live even when native connection effects are off.
    foreground.current = AppState.currentState === 'active';
    setAppState(AppState.currentState);
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
      const observationAttempt = runtimeDiscoveryAttempt.current;
      const pendingSelectionAtStart = pendingRuntimeSelection.current;
      const pendingCreationAtStart = pendingRuntimeCreation.current;
      const pendingRefreshAtStart = pendingRuntimeRefresh.current;
      const observePendingRuntime = Boolean(pendingSelectionAtStart || pendingCreationAtStart || pendingRefreshAtStart);
      try {
        let next = await MeetermTerminal.getConnectionState(CONNECTION_ID);
        const forcedBoundaryFailure = boundaryFailureFence.current;
        if (forcedBoundaryFailure) {
          next = {
            ...next,
            state: 'Failed',
            errorCode: forcedBoundaryFailure.errorCode,
            errorMessage: forcedBoundaryFailure.errorMessage,
          };
        }
        if (!forcedBoundaryFailure && switcherCancelFence.current && next.state === 'Ready'
          && !pendingSelectionAtStart
          && !commandPending.current && version === commandVersion.current) {
          // A native runtime-selection request may finish after the user
          // canceled the switch. Keep the canceled UI fail-closed and release
          // that late owner once. It stays fenced until a candidate from the
          // new discovery generation is explicitly selected and reaches Ready.
          if (!switcherCancelReleaseIssued.current) {
            switcherCancelReleaseIssued.current = true;
            try {
              await MeetermTerminal.disconnect(CONNECTION_ID);
              next = await MeetermTerminal.getConnectionState(CONNECTION_ID);
            } catch {
              next = { ...next, state: 'Disconnected' };
            }
          }
          if (next.state === 'Ready') next = { ...next, state: 'Disconnected' };
        }
        // Host authentication and fresh runtime discovery do not have
        // workspace metadata yet. A retained-work recovery is the exception:
        // keep polling its coherent cached snapshot so the existing native
        // surface can remain mounted while the actor validates its identity.
        const observeRetainedWork = workspaceObservationRef.current;
        const observeRecoveryState = ['Reconnecting', 'Synchronizing', 'Failed', 'Disconnected', 'Closing', 'HostKeyPending', 'AwaitingRuntimeSelection', 'DiscoveringRuntimes'].includes(next.state);
        const nextSession = (next.state === 'Ready' && !runtimeSelectionRequired.current && !ignoreReadyUntilNewConnection.current)
          || observeRetainedWork || observeRecoveryState
          ? await MeetermTerminal.getWorkspaceState(CONNECTION_ID)
          : null;
        const nextSessionControl = nextSession
          ? normalizeWorkspaceControl((nextSession as WorkspaceState & { control?: unknown }).control)
          : null;
        // Initial discovery and queued select/create calls can remain in an
        // intermediate phase while the Rust actor works. Observe only the
        // bounded runtime snapshot here; never issue a discovery refresh from
        // this poll.
        const observeInitialRuntime = ['DiscoveringRuntimes', 'AwaitingRuntimeSelection'].includes(next.state)
          && runtimeDiscoveryLoadedAttempt.current !== observationAttempt;
        let pendingDiscovery: RuntimeDiscovery | null = null;
        if ((observePendingRuntime || observeInitialRuntime) && !['Failed', 'Disconnected', 'Closing', 'HostKeyPending'].includes(next.state)) {
          try {
            pendingDiscovery = await MeetermTerminal.getRuntimeDiscovery(CONNECTION_ID);
          } catch {
            // A transient snapshot read failure is not an operation failure.
            // The next existing connection poll can observe it again.
          }
        }
        const operationStillPending = pendingRuntimeSelection.current === pendingSelectionAtStart
          && pendingRuntimeCreation.current === pendingCreationAtStart
          && pendingRuntimeRefresh.current === pendingRefreshAtStart;
        if (mounted && version === commandVersion.current && observationAttempt === runtimeDiscoveryAttempt.current && !commandPending.current) {
          const generationMatches = pendingDiscovery
            && (runtimeConnectionGeneration.current === null
              || runtimeConnectionGeneration.current === pendingDiscovery.connectionGeneration);
          if (pendingDiscovery && generationMatches && operationStillPending) {
            if (runtimeConnectionGeneration.current === null) {
              runtimeConnectionGeneration.current = pendingDiscovery.connectionGeneration;
            }
            if (pendingRefreshAtStart) {
              if (pendingDiscovery.connectionGeneration === pendingRefreshAtStart.connectionGeneration
                && pendingDiscovery.revision > pendingRefreshAtStart.baselineRevision
                && runtimeDiscoveryFinal(pendingDiscovery)) {
                pendingRuntimeRefresh.current = null;
                setRuntimeDiscovery(applyRuntimeHint(pendingDiscovery, runtimeHintRef.current));
                if (pendingRefreshAtStart.clearSelectionErrors) setRuntimeSelectionErrors({});
                setRuntimeCreationError('');
                setRuntimeMessage('');
                setRuntimeBusy(false);
                setRuntimeActionBusy(false);
              }
            } else {
              setRuntimeDiscovery(applyRuntimeHint(pendingDiscovery, runtimeHintRef.current));
              if (observeInitialRuntime && runtimeDiscoveryFinal(pendingDiscovery)) {
                runtimeDiscoveryLoadedAttempt.current = observationAttempt;
              }
            }
          } else if (pendingRefreshAtStart && operationStillPending && ['Failed', 'Disconnected', 'Closing'].includes(next.state)) {
            pendingRuntimeRefresh.current = null;
            setRuntimeBusy(false);
            setRuntimeActionBusy(false);
            setRuntimeMessage('Could not refresh runtimes. Check the connection and try again.');
          }
          setConnection(current => sameConnection(current, next) ? current : next);
          if (nextSessionControl?.cleanupWarning) observeCleanupWarning(nextSessionControl.cleanupWarning);
          else observeCleanupWarning(legacyCleanupWarning(next));
          const readySession = Boolean(nextSession
            && next.state === 'Ready'
            && !runtimeSelectionRequired.current
            && !ignoreReadyUntilNewConnection.current
            && !recoveryInvalidatedRef.current
            && nextSessionControl?.runtimeOperationsReady);
          const retainedSession = Boolean(nextSession && nextSessionControl?.hasRetainedWork
            && nextSessionControl.recovery.phase !== 'none'
            && !recoveryInvalidatedRef.current);
          if (!readySession && !retainedSession && nextSession && nextSessionControl
            && !recoveryInvalidatedRef.current && next.state !== 'Ready') {
            // A disconnected native snapshot can advance its operation epoch
            // before it publishes an active recovery phase. Refresh lifecycle
            // control while keeping the last workspace and selected terminal
            // metadata bound to the cached screen.
            setSession(current => {
              const nextControlOnly = { ...current, control: nextSessionControl };
              return sameSession(current, nextControlOnly) ? current : nextControlOnly;
            });
          }
          if (readySession || retainedSession) {
            if (readySession) {
              // A Ready snapshot with both native gates open is a valid
              // binding; this is the only path that updates the live session
              // during ordinary polling.
              updateRuntimeBound(true);
              setHasConnected(true);
            }
            if (retainedSession) {
              // A stale picker/modal must not remain above a retained
              // recovery surface merely because the transport reports an
              // intermediate picker-compatible state.
              setRuntimePickerVisible(false);
              setRuntimeCreateVisible(false);
              setHasConnected(true);
            }
            setSession(current => sameSession(current, nextSession!) ? current : nextSession!);
          } else if (next.state === 'AwaitingRuntimeSelection' || next.state === 'DiscoveringRuntimes') {
            if (!runtimeSelectionRequired.current) {
              const wasEstablishedBinding = runtimeBoundRef.current;
              runtimeSelectionRequired.current = true;
              invalidateRuntimeDiscovery(true);
              updateRuntimeBound(false);
              if (wasEstablishedBinding) setRuntimeMessage('The previous runtime needs to be selected again. Choose a runtime to continue.');
            }
            setRuntimePickerVisible(true);
          } else if (next.state === 'HostKeyPending' || next.errorCode === 'host_key_changed') {
            // Never show stale runtime rows while a host identity is awaiting
            // verification.
            invalidateRuntimeDiscovery(false);
            updateRuntimeBound(false);
          }
          if (next.state === 'Ready' && !runtimeSelectionRequired.current && !ignoreReadyUntilNewConnection.current) setHasConnected(true);
          setPollProblem(false);
      }
      } catch {
        if (mounted) setPollProblem(true);
      } finally { polling = false; }
    };
    void refresh();
    // Poll low-frequency metadata only. Native owns reconnect, terminal bytes,
    // frames, and all remote discovery/actor work.
    const interval = setInterval(() => { void refresh(); }, 1000);
    return () => { mounted = false; clearInterval(interval); };
  }, [invalidateRuntimeDiscovery, observeCleanupWarning, smokeFixtureActive, updateRuntimeBound]);

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

  const recoveryPhaseActive = !recoveryInvalidated && control.recovery.phase !== 'none';
  const retainedWorkAvailable = !recoveryInvalidated && control.hasRetainedWork;
  const nativeSelectedPaneCandidate = panes.find(pane => pane.selected);
  const retainedPane = recoveryPhaseActive ? retainedPaneRef.current : null;
  const nativeSelectedPane = retainedPane
    && (!nativeSelectedPaneCandidate || nativeSelectedPaneCandidate.terminalId !== retainedPane.terminalId)
    ? retainedPane
    : nativeSelectedPaneCandidate;
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
  // During retained recovery, the last bound native terminal wins over a
  // replacement selected by a newer/uncertain remote snapshot.
  const selectedPaneCandidate = groupPanes.find(pane => pane.selected);
  const selectedPane = recoveryPhaseActive && retainedPane
    && (!selectedPaneCandidate || selectedPaneCandidate.terminalId !== retainedPane.terminalId)
    ? retainedPane
    : selectedPaneCandidate;
  useEffect(() => {
    if (recoveryPhaseActive && selectedPaneCandidate && !retainedPaneRef.current) {
      retainedPaneRef.current = selectedPaneCandidate;
    }
    if (!recoveryPhaseActive && selectedPaneCandidate && control.recovery.phase === 'none') {
      retainedPaneRef.current = selectedPaneCandidate;
    }
    if (!retainedWorkAvailable && !recoveryPhaseActive) retainedPaneRef.current = null;
  }, [control.recovery.phase, recoveryPhaseActive, retainedWorkAvailable, selectedPaneCandidate]);
  const activeWorkspaceId = selectedWorkspaceId
    ?? session.groups.find(group => group.workspaceId === workspaceId && group.selected)?.workspaceId;
  // App appearance and terminal contrast are independent. Remote ANSI palettes
  // remain readable on a dark work surface, including in the light app theme.
  const colors = screen === 'terminal' ? DARK : homeColors;
  const resolvedTheme = 'dark';
  const currentProfile = profiles.find(profile => profile.id === profileId);
  const effectiveRuntimeHint = runtimeHint ?? runtimeHintForProfile(currentProfile);
  runtimeHintRef.current = effectiveRuntimeHint;

  useEffect(() => {
    if (!switcherStarted || sheet !== 'switcher') return;
    if (connection.state === 'Failed') {
      setSwitcherMessage(connectionError(connection));
    } else if (connection.state === 'HostKeyPending') {
      setSwitcherMessage('Verify the host key to continue to this server’s sessions.');
    } else if (['Connecting', 'Authenticating', 'OpeningPty', 'AttachingTmux', 'Synchronizing', 'DiscoveringRuntimes', 'AwaitingRuntimeSelection', 'AttachingRuntime', 'CreatingRuntime'].includes(connection.state)) {
      setSwitcherMessage('');
    }
  }, [connection, sheet, switcherStarted]);

  const loadRuntimeDiscovery = useCallback(async (refresh = false, clearSelectionErrors = refresh) => {
    if (smokeFixtureActive) return runtimeDiscovery;
    if (refresh) {
      if (runtimeDiscoveryLoading.current || pendingRuntimeRefresh.current) return null;
      const attempt = runtimeDiscoveryAttempt.current;
      const connectionGeneration = runtimeDiscovery?.connectionGeneration ?? runtimeConnectionGeneration.current;
      if (!connectionGeneration) {
        setRuntimeMessage('Runtimes could not be refreshed until discovery finishes. Try again in a moment.');
        return null;
      }
      const pending: PendingRuntimeRefresh = {
        attempt,
        connectionGeneration,
        baselineRevision: runtimeDiscovery?.revision ?? -1,
        clearSelectionErrors,
      };
      pendingRuntimeRefresh.current = pending;
      setRuntimeBusy(true);
      setRuntimeActionBusy(true);
      try {
        await MeetermTerminal.refreshRuntimes(CONNECTION_ID);
        if (attempt !== runtimeDiscoveryAttempt.current || pendingRuntimeRefresh.current !== pending) return null;
        // Native accepted the request, but the actor may not have published
        // the new snapshot yet. The existing connection poll completes it.
        return null;
      } catch {
        if (attempt === runtimeDiscoveryAttempt.current && pendingRuntimeRefresh.current === pending) {
          pendingRuntimeRefresh.current = null;
          setRuntimeBusy(false);
          setRuntimeActionBusy(false);
          setRuntimeMessage('Could not refresh runtimes. Check the connection and try again.');
        }
        return null;
      } finally {
        if (attempt === runtimeDiscoveryAttempt.current && pendingRuntimeRefresh.current === pending) {
          setRuntimeActionBusy(false);
        }
      }
    }
    if (runtimeDiscoveryLoading.current) return null;
    const attempt = runtimeDiscoveryAttempt.current;
    runtimeDiscoveryLoading.current = true;
    setRuntimeBusy(true);
    try {
      if (attempt !== runtimeDiscoveryAttempt.current) return null;
      const next = await MeetermTerminal.getRuntimeDiscovery(CONNECTION_ID);
      if (attempt !== runtimeDiscoveryAttempt.current) return null;
      if (runtimeConnectionGeneration.current !== null
        && runtimeConnectionGeneration.current !== next.connectionGeneration) return null;
      runtimeConnectionGeneration.current = next.connectionGeneration;
      setRuntimeDiscovery(applyRuntimeHint(next, effectiveRuntimeHint));
      if (runtimeDiscoveryFinal(next)) runtimeDiscoveryLoadedAttempt.current = attempt;
      return next;
    } catch {
      setRuntimeMessage('Runtimes could not be loaded. Tap Refresh to try again.');
      return null;
    } finally {
      runtimeDiscoveryLoading.current = false;
      setRuntimeBusy(false);
    }
  }, [effectiveRuntimeHint, runtimeDiscovery, smokeFixtureActive]);

  useEffect(() => {
    if (smokeFixtureActive || recoveryPhaseActive
      || !['DiscoveringRuntimes', 'AwaitingRuntimeSelection'].includes(connection.state)) return;
    setRuntimePickerVisible(true);
    if (runtimeDiscoveryLoadedAttempt.current === runtimeDiscoveryAttempt.current) return;
    const requestToken = `${runtimeDiscoveryAttempt.current}:${connection.state}`;
    if (runtimeDiscoveryRequestedPhase.current === requestToken) return;
    runtimeDiscoveryRequestedPhase.current = requestToken;
    void loadRuntimeDiscovery();
  }, [connection.state, loadRuntimeDiscovery, recoveryPhaseActive, smokeFixtureActive]);

  const presentation = connectionPresentation(connection);
  const ready = connection.state === 'Ready';
  // These are native-published gates, not guesses derived from the transport
  // phase or the presence of cached JS metadata. Keep the two gates separate
  // so a drawable recovery surface can never imply writable input.
  const runtimeReady = Boolean(ready && control.runtimeOperationsReady && !recoveryPhaseActive && !recoveryInvalidated);
  const terminalInputReady = Boolean(ready && control.terminalInputReady && !recoveryPhaseActive && !recoveryInvalidated);
  const strongReady = runtimeReady && terminalInputReady;
  const surfaceAvailable = Boolean(workspace && selectedPane && (strongReady || retainedWorkAvailable));
  const surfaceVisible = Boolean(surfaceAvailable)
    && screen === 'terminal' && sheet === null && !modalPending && !formVisible && !settingsVisible
    && !nameRequest && appState === 'active';
  const recoveryServerLabel = currentProfile?.name ?? endpoint(connection);
  const recoveryCopy = recoveryRailCopy(control, session.backend, session.runtime, recoveryServerLabel);
  const recoveredCopy = recoveredEpoch === control.operationEpoch && strongReady && surfaceAvailable
    ? recoveryRailCopy(control, session.backend, session.runtime, recoveryServerLabel, true)
    : null;

  useEffect(() => {
    const previous = recoveryMilestoneRef.current;
    const completedRecovery = Boolean(previous
      && strongReady
      && !previous.strongReady
      && !recoveryInvalidatedRef.current
      && control.operationEpoch
      // A normal live surface can briefly lose the input gate while a view or
      // sheet is hidden. Only a real recovery phase with its native epoch and
      // attempt may authorize a recovered announcement.
      && previous.phase !== 'none'
      && previous.epoch
      && previous.attempt >= 0
      && operationEpochAtLeast(control.operationEpoch, previous.epoch)
      // Do not replay the same recovery after a healthy visibility cycle or
      // accept an old epoch that arrives after a newer recovery completed.
      && (!completedRecoveryEpochRef.current
        || operationEpochNewer(control.operationEpoch, completedRecoveryEpochRef.current)));
    if (completedRecovery) {
      const epoch = control.operationEpoch;
      completedRecoveryEpochRef.current = epoch;
      setRecoveredEpoch(epoch);
      if (recoveredTimerRef.current) clearTimeout(recoveredTimerRef.current);
      recoveredTimerRef.current = setTimeout(() => {
        setRecoveredEpoch(current => current === epoch ? '' : current);
      }, 2000);
      const readyCopy = recoveryRailCopy(control, session.backend, session.runtime, recoveryServerLabel, true);
      if (readyCopy && announcedRecoveryRef.current !== `${epoch}:ready`) {
        announcedRecoveryRef.current = `${epoch}:ready`;
        announceRecovery(readyCopy);
      }
    } else if (retainedWorkAvailable && recoveryCopy) {
      const milestone = `${control.operationEpoch}:${control.recovery.phase}:${control.recovery.reason}`;
      if (announcedRecoveryRef.current !== milestone) {
        announcedRecoveryRef.current = milestone;
        announceRecovery(recoveryCopy);
      }
    }
    recoveryMilestoneRef.current = {
      epoch: control.operationEpoch,
      phase: control.recovery.phase,
      attempt: control.recovery.attempt,
      retained: retainedWorkAvailable,
      strongReady,
    };
  }, [control.operationEpoch, control.recovery.phase, control.recovery.reason, recoveryCopy, recoveryServerLabel, retainedWorkAvailable, session.backend, session.runtime, strongReady]);

  useEffect(() => () => {
    if (recoveredTimerRef.current) clearTimeout(recoveredTimerRef.current);
  }, []);

  useEffect(() => {
    if (smokeFixtureActive) return;
    foregroundCommands.current = foregroundCommands.current
      // A cached recovery view is visible to the user but is never reported
      // as writable/live. Native treats visibility as a surface lifecycle
      // signal and keeps controller acquisition/input behind its recovery
      // gates, so a cached surface may still report visible here.
      .then(() => MeetermTerminal.setTerminalVisible(CONNECTION_ID, surfaceVisible))
      .catch(() => setControlMessage('Could not update terminal visibility. Reconnect to continue.'));
  }, [smokeFixtureActive, strongReady, surfaceVisible]);
  const closing = connection.state === 'Closing';
  const active = !['Disconnected', 'Failed', 'Closing'].includes(connection.state);
  const attempted = Boolean(connection.host);
  const currentSessionDescription = runtimeReady || retainedWorkAvailable
    ? `${session.backend === 'herdr' ? 'Herdr' : 'tmux'} · ${session.runtime}`
    : active ? 'No Session selected' : 'Not connected';
  const currentSwitcherProfile = currentProfile ?? (attempted ? {
    id: profileId || '__current_connection__',
    name: endpoint(connection),
    host: connection.host,
    port: connection.port,
    username: '',
    authMethod: 'publicKey' as const,
    credentialSaved: false,
    backend: session.backend,
    runtime: session.runtime,
  } : null);
  const switcherServerRows = [
    ...profiles.map(profile => ({
      profile,
      isCurrent: active && profile.id === profileId,
    })),
    ...(currentSwitcherProfile && !profiles.some(profile => profile.id === currentSwitcherProfile.id)
      ? [{ profile: currentSwitcherProfile, isCurrent: active }]
      : []),
  ];
  const switcherSessionServerName = switcherTarget?.profile.name ?? currentProfile?.name ?? endpoint(connection);
  const canReconnect = !recoveryPhaseActive && !retainedWorkAvailable && !boundaryFailureFence.current
    && hasConnected && !active && !closing && connection.errorCode !== 'host_key_changed';
  const canRetryRetainedFromList = retainedWorkAvailable && !boundaryFailureFence.current
    && (control.recovery.phase === 'reconnecting'
      || (control.recovery.phase === 'stopped' && recoveryCopy?.retry === true));
  const canShowReconnect = canReconnect || canRetryRetainedFromList;
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
        const nextSession = (next.state === 'Ready' && !runtimeSelectionRequired.current && !ignoreReadyUntilNewConnection.current)
          || ['Failed', 'Disconnected', 'Closing', 'HostKeyPending', 'AwaitingRuntimeSelection', 'DiscoveringRuntimes'].includes(next.state)
          ? await MeetermTerminal.getWorkspaceState(CONNECTION_ID)
          : null;
        const nextSessionControl = nextSession
          ? normalizeWorkspaceControl((nextSession as WorkspaceState & { control?: unknown }).control)
          : null;
        setConnection(next);
        if (nextSessionControl?.cleanupWarning) observeCleanupWarning(nextSessionControl.cleanupWarning);
        else observeCleanupWarning(legacyCleanupWarning(next));
        const readySession = Boolean(nextSession
          && next.state === 'Ready'
          && !runtimeSelectionRequired.current
          && !ignoreReadyUntilNewConnection.current
          && nextSessionControl?.runtimeOperationsReady);
        const retainedSession = Boolean(nextSession && nextSessionControl?.hasRetainedWork
          && nextSessionControl.recovery.phase !== 'none'
          && !recoveryInvalidatedRef.current);
        if (readySession || retainedSession) {
          if (recoveryInvalidatedRef.current && nextSessionControl?.recovery.phase === 'none'
            && nextSessionControl.runtimeOperationsReady) {
            recoveryInvalidatedRef.current = false;
            setRecoveryInvalidated(false);
          }
          updateRuntimeBound(true);
          setSession(nextSession!);
          setHasConnected(true);
        } else if (next.state === 'AwaitingRuntimeSelection' || next.state === 'DiscoveringRuntimes') {
          if (!runtimeSelectionRequired.current) {
            const wasEstablishedBinding = runtimeBoundRef.current;
            runtimeSelectionRequired.current = true;
            invalidateRuntimeDiscovery(true);
            if (wasEstablishedBinding) setRuntimeMessage('The previous runtime needs to be selected again. Choose a runtime to continue.');
          }
          updateRuntimeBound(false);
          setRuntimePickerVisible(true);
        } else if (next.state === 'HostKeyPending' || next.errorCode === 'host_key_changed') {
          invalidateRuntimeDiscovery(false);
          updateRuntimeBound(false);
        }
        setPollProblem(false);
      } catch { setPollProblem(true); }
      return true;
    }
    catch { setControlMessage(errorMessage); return false; }
    finally { commandPending.current = false; setCommandBusy(false); }
  }, [invalidateRuntimeDiscovery, observeCleanupWarning, smokeFixtureActive, updateRuntimeBound]);

  const startRecoveryAction = useCallback((kind: keyof RecoveryPendingActions, identity: RecoveryActionIdentity) => {
    if (!identity.epoch || recoveryPendingRef.current[kind]) return false;
    recoveryPendingRef.current = { ...recoveryPendingRef.current, [kind]: identity };
    setRecoveryPending(current => ({ ...current, [kind]: true }));
    return true;
  }, []);

  const clearRecoveryAction = useCallback((kind: keyof RecoveryPendingActions, expected?: RecoveryActionIdentity) => {
    const current = recoveryPendingRef.current[kind];
    if (!current || (expected && (current.epoch !== expected.epoch
      || current.phase !== expected.phase || current.attempt !== expected.attempt))) return;
    recoveryPendingRef.current = { ...recoveryPendingRef.current, [kind]: null };
    setRecoveryPending(value => ({ ...value, [kind]: false }));
  }, []);

  const requestRetainedRecovery = useCallback((allowReconnecting: boolean) => {
    const current = controlRef.current;
    if (recoveryInvalidatedRef.current || !current.hasRetainedWork) return false;
    if (current.recovery.phase === 'stopped') {
      const copy = recoveryRailCopy(current, session.backend, session.runtime, recoveryServerLabel);
      if (!copy?.retry) return false;
    } else if (current.recovery.phase !== 'reconnecting' || !allowReconnecting) {
      return false;
    }
    const identity: RecoveryActionIdentity = {
      epoch: current.operationEpoch,
      phase: current.recovery.phase,
      attempt: current.recovery.attempt,
    };
    if (!startRecoveryAction('retry', identity)) return false;
    void MeetermTerminal.retryRecovery(CONNECTION_ID, identity.epoch)
      .catch(() => {
        clearRecoveryAction('retry', identity);
        setControlMessage('Recovery could not be started. Try again or change the destination.');
      });
    return true;
  }, [clearRecoveryAction, recoveryServerLabel, session.backend, session.runtime, startRecoveryAction]);

  const retryRecovery = useCallback(() => {
    const current = controlRef.current;
    const copy = recoveryRailCopy(current, session.backend, session.runtime, recoveryServerLabel);
    if (!copy?.retry || current.recovery.phase !== 'stopped') return;
    requestRetainedRecovery(false);
  }, [recoveryServerLabel, requestRetainedRecovery, session.backend, session.runtime]);

  const openRecoveryChange = useCallback(() => {
    const current = controlRef.current;
    if (recoveryInvalidatedRef.current || !current.hasRetainedWork
      || current.recovery.phase === 'none' || recoveryPendingRef.current.change) return;
    Keyboard.dismiss();
    setSheet('recovery');
  }, []);

  const changeRecoveryDestination = useCallback(async (destination: 'runtime' | 'server') => {
    const current = controlRef.current;
    if (recoveryInvalidatedRef.current || !current.hasRetainedWork
      || current.recovery.phase === 'none') return;
    const identity: RecoveryActionIdentity = {
      epoch: current.operationEpoch,
      phase: current.recovery.phase,
      attempt: current.recovery.attempt,
    };
    if (!startRecoveryAction('change', identity)) return;

    let boundaryAttempted = false;
    let boundaryOutcomeObserved = false;
    let boundaryUiCleared = false;
    let acceptedAfterFailure = false;
    const clearRecoveryBinding = () => {
      commandVersion.current += 1;
      recoveryInvalidatedRef.current = true;
      workspaceObservationRef.current = false;
      ignoreReadyUntilNewConnection.current = true;
      runtimeSelectionRequired.current = true;
      setRecoveryInvalidated(true);
      setRecoveredEpoch('');
      recoveryMilestoneRef.current = null;
      completedRecoveryEpochRef.current = '';
      retainedPaneRef.current = null;
      setSession(EMPTY_WORKSPACES);
      setSelectedPaneIds({});
      setWorkspaceId('');
      setScreen('workspaces');
      setSheet(null);
      setRuntimeCreateVisible(false);
      setRuntimeMessage('');
      updateRuntimeBound(false);
      boundaryUiCleared = true;
    };

    try {
      if (!smokeFixtureActive) {
        boundaryAttempted = true;
        const outcome = readRuntimeBoundaryResult(
          await MeetermTerminal.changeRuntime(CONNECTION_ID, identity.epoch),
        );
        if (!outcome) {
          const message = 'The switch result could not be confirmed. Start a new connection before continuing.';
          setSheet(null);
          failClosedForBoundaryResult('switch_outcome_unknown', message);
          return;
        }
        boundaryOutcomeObserved = true;
        if (outcome.status === 'not_invoked' || outcome.status === 'rejected_before_boundary') {
          setControlMessage('Could not start the destination change. Check the connection and try again.');
          return;
        }
        acceptedAfterFailure = outcome.status === 'accepted_after_failure';
        if (!acceptedAfterFailure) {
          // These snapshots refresh ordinary state and cleanup notices only;
          // the typed return value above is the boundary authority.
          try {
            const released = await MeetermTerminal.getConnectionState(CONNECTION_ID);
            try {
              const releasedSession = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
              const releasedControl = normalizeWorkspaceControl((releasedSession as WorkspaceState & { control?: unknown }).control);
              observeCleanupWarning(releasedControl.cleanupWarning ?? legacyCleanupWarning(released));
            } catch {
              observeCleanupWarning(legacyCleanupWarning(released));
            }
          } catch {
            // The accepted boundary remains authoritative even if a later
            // low-frequency display snapshot cannot be read.
          }
        }
      }

      // Native acceptance is the one-way boundary. Keep the exact retained
      // surface mounted while the request is pending or rejected; only after
      // acceptance may a picker/server destination bind and release the old
      // recovery cache. Bump the observation version first so an old Ready
      // poll already in flight cannot restore the retired session afterward.
      clearRecoveryBinding();

      if (acceptedAfterFailure) {
        const message = 'Could not start the new connection. Choose a server and Session, then try again.';
        failClosedForBoundaryResult('runtime_switch_start_failed', message);
        invalidateRuntimeDiscovery(false);
        setSheet('servers');
        return;
      }

      if (destination === 'runtime') {
        invalidateRuntimeDiscovery(true);
      } else {
        invalidateRuntimeDiscovery(false);
        setSheet('servers');
      }
    } catch {
      if (boundaryAttempted && !boundaryOutcomeObserved) {
        const message = 'The switch result could not be confirmed. Start a new connection before continuing.';
        setSheet(null);
        failClosedForBoundaryResult('switch_outcome_unknown', message);
      } else if (boundaryOutcomeObserved) {
        if (!boundaryUiCleared) clearRecoveryBinding();
        const message = 'Could not start the new connection. Choose a server and Session, then try again.';
        failClosedForBoundaryResult('runtime_switch_start_failed', message);
        invalidateRuntimeDiscovery(false);
        setSheet('servers');
      } else {
        setControlMessage('Could not start the destination change. Check the connection and try again.');
      }
    } finally {
      clearRecoveryAction('change', identity);
    }
  }, [clearRecoveryAction, failClosedForBoundaryResult, invalidateRuntimeDiscovery, observeCleanupWarning, smokeFixtureActive, startRecoveryAction, updateRuntimeBound]);

  useEffect(() => {
    if (recoveryInvalidatedRef.current) {
      clearRecoveryAction('retry');
      return;
    }
    const current = controlRef.current;
    const pending = recoveryPendingRef.current.retry;
    if (pending && (pending.epoch !== current.operationEpoch
      || pending.phase !== current.recovery.phase
      || pending.attempt !== current.recovery.attempt)) {
      clearRecoveryAction('retry', pending);
    }
  }, [clearRecoveryAction, control.operationEpoch, control.recovery.attempt, control.recovery.phase, recoveryInvalidated]);

  const finishRuntimeSelection = useCallback((candidate: RuntimeCandidate) => {
    // This callback is reached only after a Ready snapshot. Keeping the hint
    // write here prevents failed/stale taps from changing profile metadata.
    recoveryInvalidatedRef.current = false;
    setRecoveryInvalidated(false);
    setRecoveredEpoch('');
    pendingRuntimeSelection.current = null;
    pendingRuntimeCreation.current = '';
    pendingRuntimeSelectionBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    pendingRuntimeCreationBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    runtimeSelectionRequired.current = false;
    selectedRuntimeRef.current = candidate;
    updateRuntimeBound(true);
    setRuntimePickerVisible(false);
    setRuntimeCreateVisible(false);
    setRuntimeSelectingId('');
    setRuntimeBusy(false);
    setRuntimeActionBusy(false);
    setRuntimeMessage('');
    setRuntimeCreationError('');
    setScreen('workspaces');
    switcherBoundary.current = false;
    switcherOperation.current = false;
    setSwitcherTarget(null);
    setSwitcherStarted(false);
    setSwitcherAccepting(false);
    setSwitcherMessage('');
    setSheet(null);
    setPickerQuery('');

    // A picker cancellation deliberately ignores the old Ready snapshot.
    // Clear the cancel fence only after an explicit candidate from the new
    // discovery generation reaches Ready.
    switcherCancelFence.current = false;
    switcherCancelReleaseIssued.current = false;
    ignoreReadyUntilNewConnection.current = false;
    const profileIdForHint = profileId;
    if (!smokeFixtureActive && profileIdForHint) {
      void MeetermTerminal.setLastUsedRuntime(profileIdForHint, candidate.backend, candidate.name)
        .then(updated => {
          setProfiles(current => current.map(profile => profile.id === updated.id ? updated : profile));
          setRuntimeHint({ backend: candidate.backend, runtime: candidate.name });
        })
        .catch(() => {
          // Selection remains successful; a hint is convenience metadata only.
          setControlMessage('Runtime opened. The last-used hint could not be saved.');
        });
    }
  }, [profileId, smokeFixtureActive, updateRuntimeBound]);

  const failPendingRuntime = useCallback((kind: 'selection' | 'creation' | 'connection', candidate: RuntimeCandidate | null, errorCode: string, errorMessage: string) => {
    const wasCreating = Boolean(pendingRuntimeCreation.current);
    pendingRuntimeSelection.current = null;
    pendingRuntimeCreation.current = '';
    pendingRuntimeSelectionBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    pendingRuntimeCreationBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    setRuntimeSelectingId('');
    setRuntimeBusy(false);
    setRuntimeActionBusy(false);
    runtimeSelectionRequired.current = true;
    updateRuntimeBound(false);
    setRuntimePickerVisible(true);
    if (kind === 'selection' && candidate) {
      setRuntimeSelectionErrors(current => ({
        ...current,
        [candidate.id]: runtimeFailureMessage(errorCode, errorMessage, 'This runtime could not be opened.'),
      }));
      setRuntimeMessage('The other runtime sections remain available.');
    } else if (kind === 'creation') {
      setRuntimeCreationError(runtimeFailureMessage(errorCode, errorMessage, 'The tmux session could not be created.'));
      setRuntimeMessage('Choose another session name or refresh the runtime list.');
    } else {
      const message = runtimeFailureMessage(errorCode, errorMessage, 'The connection failed while opening this runtime. Check the connection and reconnect.');
      if (wasCreating) setRuntimeCreationError(message);
      setRuntimeMessage(message);
    }
  }, [updateRuntimeBound]);

  useEffect(() => {
    if (smokeFixtureActive) return;
    const candidate = pendingRuntimeSelection.current;
    const creationName = pendingRuntimeCreation.current;
    if (!candidate && !creationName) return;

    // Ready is the only successful completion signal. In particular, an
    // initial AwaitingRuntimeSelection snapshot while the Rust actor is still
    // dequeuing the request is not a failure.
    if (connection.state === 'Ready') {
      const operationAttempt = runtimeDiscoveryAttempt.current;
      finishRuntimeSelection(candidate ?? createdTmuxCandidate(creationName));
      void MeetermTerminal.getWorkspaceState(CONNECTION_ID)
        .then(next => {
          if (operationAttempt !== runtimeDiscoveryAttempt.current) return;
          setSession(next);
          setHasConnected(true);
        })
        .catch(() => {
          if (operationAttempt === runtimeDiscoveryAttempt.current) setPollProblem(true);
        });
      return;
    }
    if (connection.state === 'Failed') {
      failPendingRuntime('connection', candidate, connection.errorCode, connection.errorMessage);
      return;
    }

    const snapshot = runtimeDiscovery;
    if (!snapshot) return;
    if (candidate) {
      const observed = snapshot.backends.flatMap(item => item.candidates).find(item => item.id === candidate.id);
      const baseline = pendingRuntimeSelectionBaseline.current;
      if (snapshot.connectionGeneration === baseline.connectionGeneration
        && observed?.errorCode && (observed.errorCode !== baseline.errorCode || snapshot.revision !== baseline.revision)) {
        failPendingRuntime('selection', candidate, observed.errorCode, observed.errorMessage);
      }
      return;
    }

    const tmux = snapshot.backends.find(item => item.backend === 'tmux');
    const baseline = pendingRuntimeCreationBaseline.current;
    if (snapshot.connectionGeneration === baseline.connectionGeneration
      && tmux?.errorCode && (tmux.errorCode !== baseline.errorCode || snapshot.revision !== baseline.revision)) {
      failPendingRuntime('creation', null, tmux.errorCode, tmux.errorMessage);
    }
  }, [connection, failPendingRuntime, finishRuntimeSelection, runtimeDiscovery, runtimeActionBusy, runtimeSelectingId, smokeFixtureActive]);

  const selectRuntime = useCallback(async (candidate: RuntimeCandidate) => {
    if (candidate.state !== 'running' || !candidate.selectable || commandPending.current || runtimeBusy) return false;
    const operationAttempt = runtimeDiscoveryAttempt.current;
    const baselineDiscovery = runtimeDiscovery;
    const baseline = baselineDiscovery?.backends.flatMap(item => item.candidates).find(item => item.id === candidate.id);
    pendingRuntimeSelectionBaseline.current = {
      connectionGeneration: baselineDiscovery?.connectionGeneration ?? '',
      revision: baselineDiscovery?.revision ?? -1,
      errorCode: baseline?.errorCode ?? candidate.errorCode,
    };
    setRuntimeSelectionErrors(current => {
      const next = { ...current };
      delete next[candidate.id];
      return next;
    });
    setRuntimeSelectingId(candidate.id);
    setRuntimeActionBusy(true);
    setRuntimeBusy(true);
    setRuntimeMessage('');
    pendingRuntimeSelection.current = candidate;
    try {
      if (!smokeFixtureActive) await MeetermTerminal.selectRuntime(CONNECTION_ID, candidate.id);
      if (operationAttempt !== runtimeDiscoveryAttempt.current || !pendingRuntimeSelection.current) return false;
      setRuntimeMessage('Runtime selected. Waiting for its workspace to become ready…');
      return true;
    } catch {
      if (operationAttempt !== runtimeDiscoveryAttempt.current) return false;
      pendingRuntimeSelection.current = null;
      pendingRuntimeSelectionBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
      setRuntimeSelectionErrors(current => ({ ...current, [candidate.id]: 'This runtime could not be opened. It may have stopped or changed; refresh and try again.' }));
      setRuntimeMessage('The other runtime sections remain available.');
      void loadRuntimeDiscovery(true, false);
      return false;
    } finally {
      if (operationAttempt === runtimeDiscoveryAttempt.current && !pendingRuntimeSelection.current) {
        setRuntimeSelectingId('');
        if (!runtimeDiscoveryLoading.current) setRuntimeBusy(false);
      }
      if (operationAttempt === runtimeDiscoveryAttempt.current) setRuntimeActionBusy(false);
    }
  }, [loadRuntimeDiscovery, runtimeBusy, runtimeDiscovery, smokeFixtureActive]);

  const refreshRuntimes = useCallback(() => {
    if (commandPending.current || runtimeActionBusy || runtimeBusy) return;
    void loadRuntimeDiscovery(true);
  }, [loadRuntimeDiscovery, runtimeActionBusy, runtimeBusy]);

  const cancelRuntimeSelection = useCallback(() => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    invalidateRuntimeDiscovery(false);
    setRuntimeMessage('');
    pendingRuntimeSelection.current = null;
    pendingRuntimeCreation.current = '';
    setRuntimeSelectingId('');
    setRuntimeBusy(false);
    setRuntimeActionBusy(false);
    ignoreReadyUntilNewConnection.current = true;
    runtimeSelectionRequired.current = false;
    updateRuntimeBound(false);
    if (smokeFixtureActive) return;
    setConnection(current => ({ ...current, state: 'Closing' }));
    void runCommand(() => MeetermTerminal.disconnect(CONNECTION_ID), 'Could not cancel the provisional connection. Please try again.');
  }, [invalidateRuntimeDiscovery, runCommand, smokeFixtureActive, updateRuntimeBound]);

  const createTmuxSession = useCallback(async (name: string) => {
    if (commandPending.current || runtimeActionBusy) return false;
    const operationAttempt = runtimeDiscoveryAttempt.current;
    pendingRuntimeCreationBaseline.current = {
      connectionGeneration: runtimeDiscovery?.connectionGeneration ?? '',
      revision: runtimeDiscovery?.revision ?? -1,
      // A new native create request clears any previous create-operation
      // section error when it is accepted. Baseline the new attempt against
      // that empty error so the same code can be reported again on failure.
      errorCode: '',
    };
    setRuntimeActionBusy(true);
    setRuntimeBusy(true);
    setRuntimeCreationError('');
    pendingRuntimeCreation.current = name;
    try {
      if (!smokeFixtureActive) await MeetermTerminal.createTmuxSession(CONNECTION_ID, name);
      if (operationAttempt !== runtimeDiscoveryAttempt.current || !pendingRuntimeCreation.current) return false;
      setRuntimeMessage('Session creation requested. Waiting for it to become ready…');
      return true;
    } catch {
      if (operationAttempt !== runtimeDiscoveryAttempt.current) return false;
      pendingRuntimeCreation.current = '';
      pendingRuntimeCreationBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
      setRuntimeCreationError('The session could not be created. Refresh the list to check whether the name is already in use.');
      setRuntimeMessage('Choose another session name or refresh the runtime list.');
      return false;
    } finally {
      if (operationAttempt === runtimeDiscoveryAttempt.current) {
        if (!pendingRuntimeCreation.current && !pendingRuntimeSelection.current && !runtimeDiscoveryLoading.current) setRuntimeBusy(false);
        setRuntimeActionBusy(false);
      }
    }
  }, [runtimeActionBusy, runtimeDiscovery, smokeFixtureActive]);

  const finishConnectionForm = useCallback(() => {
    if (Platform.OS === 'ios' && (returnToServersAfterForm.current || returnToSwitcherAfterForm.current)) setModalPending(true);
    setFormVisible(false);
    if (Platform.OS !== 'ios' && returnToServersAfterForm.current) {
      returnToServersAfterForm.current = false;
      setSheet('servers');
    }
    if (Platform.OS !== 'ios' && returnToSwitcherAfterForm.current) {
      returnToSwitcherAfterForm.current = false;
      if (!switcherFormConnected.current) {
        setSwitcherTarget(null);
        switcherBoundary.current = false;
        setSwitcherStarted(false);
        setSwitcherMessage('The connection was canceled. The previous session remains disconnected. Choose a server to continue.');
      }
      switcherFormConnected.current = false;
      setSheet('switcher');
    }
  }, []);

  const connectionFormDismissed = useCallback(() => {
    setHostPromptDeferred(false);
    setModalPending(false);
    if (returnToServersAfterForm.current) {
      returnToServersAfterForm.current = false;
      setSheet('servers');
    }
    if (returnToSwitcherAfterForm.current) {
      returnToSwitcherAfterForm.current = false;
      if (!switcherFormConnected.current) {
        setSwitcherTarget(null);
        switcherBoundary.current = false;
        setSwitcherStarted(false);
        setSwitcherMessage('The connection was canceled. The previous session remains disconnected. Choose a server to continue.');
      }
      switcherFormConnected.current = false;
      setSheet('switcher');
    }
  }, []);

  const resetForConnection = useCallback((profile: Pick<ServerProfile, 'host' | 'port' | 'backend' | 'runtime'>, keepSwitcher = false) => {
    if (Platform.OS === 'ios' && (formVisible || (sheet !== null && !keepSwitcher))) setHostPromptDeferred(true);
    switcherCancelReleaseIssued.current = false;
    boundaryFailureFence.current = null;
    recoveryInvalidatedRef.current = false;
    setRecoveryInvalidated(false);
    setRecoveredEpoch('');
    recoveryMilestoneRef.current = null;
    completedRecoveryEpochRef.current = '';
    retainedPaneRef.current = null;
    recoveryPendingRef.current = { retry: null, change: null };
    setRecoveryPending({ retry: false, change: false });
    returnToServersAfterForm.current = false;
    setFormVisible(false);
    if (!keepSwitcher) {
      switcherBoundary.current = false;
      switcherOperation.current = false;
      setSwitcherTarget(null);
      setSwitcherStarted(false);
      setSwitcherAccepting(false);
      setSwitcherMessage('');
    }
    setSheet(keepSwitcher ? 'switcher' : null);
    setScreen('workspaces');
    setFoundation(false);
    setSession(EMPTY_WORKSPACES);
    setSelectedPaneIds({});
    setWorkspaceId('');
    setSearching(false);
    setQuery('');
    listOffsets.current = { normal: 0, search: 0 };
    setRemovedHostKeyId('');
    setHasConnected(false);
    setRuntimeSelectingId('');
    setRuntimeBusy(false);
    setRuntimeActionBusy(false);
    setRuntimeMessage('');
    selectedRuntimeRef.current = null;
    pendingRuntimeSelection.current = null;
    pendingRuntimeCreation.current = '';
    ignoreReadyUntilNewConnection.current = false;
    runtimeSelectionRequired.current = true;
    invalidateRuntimeDiscovery(false);
    setRuntimeHint(runtimeHintForProfile(profile));
    updateRuntimeBound(false);
    setConnection({ ...INITIAL_CONNECTION, state: 'Connecting', host: profile.host, port: profile.port });
  }, [formVisible, invalidateRuntimeDiscovery, sheet, updateRuntimeBound]);

  const prepareConnection = useCallback(async () => {
    if (smokeFixtureActive) return;
    const currentPreferences = preferencesLoaded ? preferences : await MeetermTerminal.getPreferences();
    await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, currentPreferences.automaticReconnect);
    await foregroundCommands.current;
    await MeetermTerminal.setForeground(CONNECTION_ID, foreground.current);
    // Switching endpoints explicitly releases the previous connection owner.
    await MeetermTerminal.disconnect(CONNECTION_ID);
    const released = await MeetermTerminal.getConnectionState(CONNECTION_ID);
    try {
      const releasedSession = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
      const releasedControl = normalizeWorkspaceControl((releasedSession as WorkspaceState & { control?: unknown }).control);
      observeCleanupWarning(releasedControl.cleanupWarning ?? legacyCleanupWarning(released));
    } catch {
      observeCleanupWarning(legacyCleanupWarning(released));
    }
  }, [observeCleanupWarning, preferences, preferencesLoaded, smokeFixtureActive]);

  const submitConnection = useCallback(async (submission: ConnectionSubmission) => {
    let savedProfile: ServerProfile | undefined;
    const success = await runCommand(async () => {
      if (submission.saveProfile) {
        savedProfile = await MeetermTerminal.saveProfile({ ...submission.profile, id: submission.profile.id || formSavedProfile.current?.id || '' }, submission.saveCredential ? submission.credential : null, submission.keepCredential);
        formSavedProfile.current = savedProfile;
        setProfiles(current => [...current.filter(item => item.id !== savedProfile!.id), savedProfile!]);
      }
      if (!submission.connect) return;
      await prepareConnection();
      if (submission.credential) {
        const options: SshConnectOptions = { host: submission.profile.host, port: submission.profile.port, username: submission.profile.username, ...submission.credential };
        await MeetermTerminal.connectHost(CONNECTION_ID, options);
      } else if (savedProfile?.credentialSaved) {
        await MeetermTerminal.connectProfileHost(CONNECTION_ID, savedProfile.id);
      } else { throw new Error('Credential required'); }
      const returnToSwitcher = returnToSwitcherAfterForm.current;
      resetForConnection(savedProfile ?? submission.profile, returnToSwitcher);
      setProfileId(savedProfile?.id ?? '');
      if (returnToSwitcher) {
        switcherBoundary.current = true;
        switcherFormConnected.current = true;
        setSwitcherStarted(true);
      }
    }, 'Could not save or connect to this server. Check the address and credentials.');
    if (success) finishConnectionForm();
    return success;
  }, [finishConnectionForm, prepareConnection, resetForConnection, runCommand]);

  const disconnect = useCallback(() => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    recoveryInvalidatedRef.current = true;
    setRecoveryInvalidated(true);
    setRecoveredEpoch('');
    recoveryMilestoneRef.current = null;
    completedRecoveryEpochRef.current = '';
    retainedPaneRef.current = null;
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Closing' }));
    void runCommand(() => MeetermTerminal.disconnect(CONNECTION_ID), 'Could not disconnect. Please try again.').then(success => {
      if (!success) setConnection(previous);
    });
  }, [connection, runCommand]);

  const reconnect = useCallback(() => {
    if (commandPending.current) return;
    const current = controlRef.current;
    if (!recoveryInvalidatedRef.current && current.hasRetainedWork) {
      requestRetainedRecovery(true);
      return;
    }
    // Without retained work, reconnect starts a fresh connection generation
    // and follows the explicit runtime picker flow.
    // A deliberate reconnect starts a new native connection generation, so a
    // canceled switcher's Ready fence must not hide the new generation or
    // admit a stale workspace before its explicit runtime selection.
    switcherCancelFence.current = false;
    switcherCancelReleaseIssued.current = false;
    ignoreReadyUntilNewConnection.current = false;
    if (recoveryPhaseActive) {
      recoveryInvalidatedRef.current = true;
      setRecoveryInvalidated(true);
      setRecoveredEpoch('');
      recoveryMilestoneRef.current = null;
      completedRecoveryEpochRef.current = '';
      retainedPaneRef.current = null;
    }
    setSheet(null);
    const previous = connection;
    setConnection(current => ({ ...current, state: 'Reconnecting', errorCode: '', errorMessage: '' }));
    void runCommand(() => MeetermTerminal.reconnect(CONNECTION_ID), 'Could not reconnect. Choose Connection details to enter your credentials again.').then(success => {
      if (!success) setConnection(previous);
    });
  }, [connection, recoveryPhaseActive, requestRetainedRecovery, runCommand]);

  const retainedWorkspaceId = retainedWorkAvailable && retainedPane ? retainedPane.workspaceId : '';
  const choosePane = useCallback(async (pane: RemoteTerminal) => {
    const retainedSelection = Boolean(recoveryPhaseActive && retainedWorkAvailable
      && retainedPane && pane.terminalId === retainedPane.terminalId);
    if (commandPending.current || (!runtimeReady && !retainedSelection)) return false;
    Keyboard.dismiss();
    const previous = selectedPaneIds[pane.groupId];
    setSelectedPaneIds(current => ({ ...current, [pane.groupId]: pane.id }));
    // Rust also retains a desired pane while disconnected, so reconnect's
    // restored selection follows an offline workspace choice.
    // Presentation fixtures may navigate only to their existing native demo
    // terminal. They never issue a remote selection or create a fake JS buffer.
    const success = smokeFixtureActive
      ? pane.terminalId === CONNECTION_ID
      : retainedSelection
        ? true
        : await runCommand(() => MeetermTerminal.selectPane(CONNECTION_ID, pane.id), 'Could not open this terminal. Check the list and select it again.');
    if (!success) setSelectedPaneIds(current => {
        const next = { ...current };
        if (previous) next[pane.groupId] = previous; else delete next[pane.groupId];
        return next;
      });
    return success;
  }, [recoveryPhaseActive, retainedPane, retainedWorkAvailable, runCommand, runtimeReady, selectedPaneIds, smokeFixtureActive]);

  const openWorkspace = useCallback((item: Workspace) => {
    const retainedWorkspace = Boolean(recoveryPhaseActive && retainedWorkAvailable
      && retainedPane && item.id === retainedWorkspaceId);
    if (commandPending.current || (!runtimeReady && !retainedWorkspace)) return;
    Keyboard.dismiss();
    const chosenGroup = session.groups.find(candidate => candidate.workspaceId === item.id && candidate.selected)
      ?? session.groups.find(candidate => candidate.workspaceId === item.id);
    const candidates = item.panes.filter(candidate => candidate.groupId === chosenGroup?.id);
    const pane = (retainedWorkspace ? retainedPane : null)
      ?? candidates.find(candidate => candidate.id === selectedPaneIds[chosenGroup?.id ?? ''])
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
  }, [choosePane, recoveryPhaseActive, retainedPane, retainedWorkAvailable, retainedWorkspaceId, runtimeReady, selectedPaneIds, session.groups, session.groupsSupported, runCommand]);

  const backToWorkspaces = useCallback(() => {
    Keyboard.dismiss();
    setScreen('workspaces');
    setSheet(null);
  }, []);
  const openSheet = useCallback((kind: SheetKind) => {
    if ((kind === 'workspaces' || kind === 'groups') && !runtimeReady) return;
    Keyboard.dismiss();
    setPickerQuery('');
    setSheet(kind);
  }, [runtimeReady]);
  const presentSheetAfterCurrentDismissal = useCallback((kind: Exclude<SheetKind, null>) => {
    Keyboard.dismiss();
    if (Platform.OS === 'ios' && sheet !== null) {
      // A saved-server manager is its own existing page sheet. Finish closing
      // the current server/session sheet before presenting that manager so a
      // later Add/Edit form can use the same ordered modal handoff.
      setModalPending(true);
      pendingModal.current = () => setSheet(kind);
      setSheet(null);
      return;
    }
    setSheet(kind);
  }, [sheet]);
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

  // Issue #28 image attachment. One draft at a time; a generation counter
  // drops late picker/normalize results after discard or a fresh begin. The
  // captured destination never retargets: the native session revalidates its
  // own binding and the sheet disables Insert while another pane is shown.
  const attachmentTargetFor = useCallback((pane: RemoteTerminal): AttachmentTarget => ({
    terminalId: pane.terminalId,
    paneId: pane.id,
    workspaceId: pane.workspaceId,
    backend: runtimeHint?.backend ?? 'tmux',
    runtime: runtimeHint?.runtime ?? '',
    host: connection.host,
    port: connection.port,
  }), [connection.host, connection.port, runtimeHint]);

  const attachmentDestinationFor = useCallback((pane: RemoteTerminal): AttachmentDestination => ({
    terminalId: pane.terminalId,
    server: currentProfile?.name ?? endpoint(connection),
    session: currentSessionDescription,
    workspace: workspaces.find(item => item.id === pane.workspaceId)?.name ?? pane.workspaceId,
    terminal: pane.name,
  }), [connection, currentProfile, currentSessionDescription, workspaces]);

  const attachmentDestinationFromTarget = useCallback((target: AttachmentTarget): AttachmentDestination => ({
    terminalId: target.terminalId,
    server: `${target.host}:${target.port}`,
    session: `${target.backend === 'herdr' ? 'Herdr' : 'tmux'} · ${target.runtime}`,
    workspace: workspaces.find(item => item.id === target.workspaceId)?.name ?? target.workspaceId,
    terminal: panes.find(item => item.id === target.paneId)?.name ?? target.paneId,
  }), [panes, workspaces]);

  /** Fold the authoritative core snapshot into the draft, dropping stale ids. */
  const refreshAttachmentSnapshot = useCallback(() => {
    if (smokeFixtureActive) return;
    void MeetermTerminal.attachmentSnapshot().then(result => {
      if (result.status !== 'snapshot') return;
      const fresh = result.operation;
      setAttachment(current => current && (!current.operation || current.operation.attachmentId === fresh.attachmentId)
        ? { ...current, operation: fresh }
        : current);
    }).catch(() => {});
  }, [smokeFixtureActive]);

  // Low-frequency progress polling: ~300 ms while the sheet is visible and a
  // pending/uploading operation may still be moving. Nothing else polls.
  const attachmentOperationPhase = attachment?.operation?.phase ?? null;
  useEffect(() => {
    if (smokeFixtureActive || sheet !== 'attachment') return;
    if (attachmentOperationPhase !== 'pending' && attachmentOperationPhase !== 'uploading') return;
    const timer = setInterval(refreshAttachmentSnapshot, 300);
    return () => clearInterval(timer);
  }, [attachmentOperationPhase, refreshAttachmentSnapshot, sheet, smokeFixtureActive]);

  /** Closing the sheet retains the draft and native session for remount. */
  const closeAttachmentSheet = useCallback(() => {
    if (sheet === 'attachment') setSheet(null);
  }, [sheet]);

  const openAttachment = useCallback(() => {
    const pane = selectedPane;
    if (!pane || !runtimeReady || commandBusy) return;
    Keyboard.dismiss();
    if (attachment) {
      // Remount within the same process: the retained draft rebinds and a
      // still-running core operation keeps progressing.
      setSheet('attachment');
      refreshAttachmentSnapshot();
      return;
    }
    const generation = ++attachmentGeneration.current;
    setAttachment({
      phase: 'choosing',
      prepared: null,
      operation: null,
      remoteDirectory: '',
      busyAction: null,
      sessionReady: true,
      destination: attachmentDestinationFor(pane),
      notice: '',
      errorCode: '',
      errorMessage: '',
    });
    setSheet('attachment');
    if (smokeFixtureActive) return;
    void (async () => {
      try {
        // Same-process remount recovery: a retained native session rebinds
        // the draft instead of silently starting over.
        const state = await MeetermTerminal.getAttachmentState();
        if (attachmentGeneration.current !== generation) return;
        if (state.status !== 'idle' || state.operation) {
          setAttachment(current => current ? {
            ...current,
            phase: state.status === 'prepared' ? 'ready' : 'choosing',
            prepared: state.status === 'prepared' ? {
              fileId: state.fileId,
              previewUri: state.previewUri,
              format: state.format,
              width: state.width,
              height: state.height,
              byteCount: state.byteCount,
              sourceByteCount: state.sourceByteCount,
            } : current.prepared,
            operation: state.operation,
            destination: state.target ? attachmentDestinationFromTarget(state.target) : current.destination,
            sessionReady: true,
          } : current);
          return;
        }
        const result = await MeetermTerminal.beginAttachment(pane.terminalId, attachmentTargetFor(pane));
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'held') {
          setAttachment(current => current ? { ...current, sessionReady: false, notice: ATTACHMENT_COMPOSING_NOTICE } : current);
        }
      } catch {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, phase: 'error', errorCode: 'attachment_unavailable', errorMessage: 'Could not start the attachment.' } : current);
      }
    })();
  }, [attachment, attachmentDestinationFor, attachmentDestinationFromTarget, attachmentTargetFor, commandBusy, refreshAttachmentSnapshot, runtimeReady, selectedPane, smokeFixtureActive]);

  /** (Re)establish the native session after a held begin or a Discard. */
  const ensureAttachmentSession = useCallback(async (): Promise<boolean> => {
    if (attachment?.sessionReady) return true;
    const pane = selectedPane;
    if (!pane) return false;
    try {
      const result = await MeetermTerminal.beginAttachment(pane.terminalId, attachmentTargetFor(pane));
      if (result.status === 'held') {
        setAttachment(current => current ? { ...current, notice: ATTACHMENT_COMPOSING_NOTICE } : current);
        return false;
      }
      setAttachment(current => current ? {
        ...current,
        sessionReady: true,
        notice: '',
        // A Discard followed by Choose on another pane binds the new target.
        destination: attachmentDestinationFor(pane),
      } : current);
      return true;
    } catch {
      setAttachment(current => current ? { ...current, phase: 'error', errorCode: 'attachment_unavailable', errorMessage: 'Could not start the attachment.' } : current);
      return false;
    }
  }, [attachment?.sessionReady, attachmentDestinationFor, attachmentTargetFor, selectedPane]);

  const pickAttachment = useCallback((source: AttachmentSource) => {
    if (!attachment || attachment.busyAction || attachment.phase === 'picking' || attachment.phase === 'normalizing') return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, phase: 'picking', notice: '', errorCode: '', errorMessage: '' } : current);
    if (smokeFixtureActive) {
      // Fixtures never reach the OS picker; stay on the picking state only in
      // the real flow and return to choosing in smoke screenshots.
      setAttachment(current => current ? { ...current, phase: 'choosing' } : current);
      return;
    }
    void (async () => {
      if (!await ensureAttachmentSession()) {
        setAttachment(current => current && current.phase === 'picking' ? { ...current, phase: 'choosing' } : current);
        return;
      }
      try {
        const result = await MeetermTerminal.pickAttachmentImage(source);
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'canceled') {
          setAttachment(current => current ? { ...current, phase: 'choosing' } : current);
          return;
        }
        if (result.status === 'error') {
          setAttachment(current => current ? { ...current, phase: 'error', errorCode: result.errorCode, errorMessage: result.message } : current);
          return;
        }
        setAttachment(current => current ? { ...current, phase: 'normalizing' } : current);
        const prepared = await MeetermTerminal.prepareAttachmentImage(result.token);
        if (attachmentGeneration.current !== generation) return;
        if (prepared.status === 'prepared') {
          setAttachment(current => current ? { ...current, phase: 'ready', operation: null, prepared: {
            fileId: prepared.fileId,
            previewUri: prepared.previewUri,
            format: prepared.format,
            width: prepared.width,
            height: prepared.height,
            byteCount: prepared.byteCount,
            sourceByteCount: prepared.sourceByteCount,
          } } : current);
        } else {
          setAttachment(current => current ? { ...current, phase: 'error', errorCode: prepared.errorCode, errorMessage: prepared.message } : current);
        }
      } catch {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, phase: 'error', errorCode: 'attachment_io_failed', errorMessage: 'The image could not be prepared.' } : current);
      }
    })();
  }, [attachment, ensureAttachmentSession, smokeFixtureActive]);

  /** Explicit Upload → `meeterm_attachment_begin`; progress comes from polling. */
  const uploadAttachmentDraft = useCallback(() => {
    const operation = attachment?.operation;
    if (!attachment || !attachment.prepared || attachment.busyAction || !selectedPane) return;
    // Double-tap guard: a live op must be retried or cancelled, never rebegun.
    if (operation && operation.phase !== 'failed' && operation.phase !== 'cancelled') return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'upload', notice: '', errorCode: '', errorMessage: '' } : current);
    void MeetermTerminal.uploadAttachment(attachment.destination.terminalId, attachment.remoteDirectory.trim())
      .then(result => {
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'accepted') {
          setAttachment(current => current ? { ...current, busyAction: null, operation: {
            phase: 'uploading',
            attachmentId: result.attachmentId,
            bytesUploaded: 0,
            sizeBytes: current.prepared?.byteCount ?? 0,
            remotePath: '',
            displayName: '',
            errorCode: '',
            errorMessage: '',
            insertUnconfirmed: false,
            remoteRemoved: false,
          } } : current);
          refreshAttachmentSnapshot();
          return;
        }
        setAttachment(current => current ? { ...current, busyAction: null, notice:
          result.status === 'unavailable' ? 'The attachment backend is not available in this build.' : result.message } : current);
      })
      .catch(() => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null, notice: 'The upload could not be started.' } : current);
      });
  }, [attachment, refreshAttachmentSnapshot, selectedPane]);

  /** Explicit transfer retry on a pending/failed operation. */
  const retryAttachmentUpload = useCallback(() => {
    const operation = attachment?.operation;
    if (!attachment || !operation || attachment.busyAction) return;
    if (operation.phase !== 'pending' && operation.phase !== 'failed') return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'retryUpload', notice: '' } : current);
    void MeetermTerminal.retryAttachmentUpload(attachment.destination.terminalId)
      .then(result => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null } : current);
        if (result.status === 'accepted') {
          refreshAttachmentSnapshot();
          return;
        }
        setAttachment(current => current ? { ...current, notice:
          result.status === 'unavailable' ? 'The attachment backend is not available in this build.' : result.message } : current);
      })
      .catch(() => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null, notice: 'The retry could not be started.' } : current);
      });
  }, [attachment, refreshAttachmentSnapshot]);

  /** Explicit cancel of a pending/uploading operation. */
  const cancelAttachmentDraft = useCallback(() => {
    const operation = attachment?.operation;
    if (!attachment || !operation || attachment.busyAction) return;
    if (operation.phase !== 'pending' && operation.phase !== 'uploading') return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'cancel', notice: '' } : current);
    void MeetermTerminal.cancelAttachment()
      .then(result => {
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'accepted') {
          setAttachment(current => current?.operation ? { ...current, busyAction: null, operation: { ...current.operation, phase: 'cancelled' } } : current);
          refreshAttachmentSnapshot();
          return;
        }
        setAttachment(current => current ? { ...current, busyAction: null, notice:
          result.status === 'unavailable' ? 'The attachment backend is not available in this build.' : result.message } : current);
      })
      .catch(() => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null, notice: 'The upload could not be cancelled.' } : current);
      });
  }, [attachment, refreshAttachmentSnapshot]);

  /** Separate explicit Insert — never sent automatically or retried silently. */
  const insertAttachmentDraft = useCallback(() => {
    const operation = attachment?.operation;
    const pane = selectedPane;
    if (!attachment || !operation || operation.phase !== 'uploaded' || attachment.busyAction || !pane) return;
    // Destination protection: insertion goes only to the captured terminal.
    if (pane.terminalId !== attachment.destination.terminalId) return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'insert', notice: '' } : current);
    void MeetermTerminal.insertAttachment(attachment.destination.terminalId)
      .then(result => {
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'inserted') {
          setAttachment(current => current?.operation ? { ...current, busyAction: null, operation: { ...current.operation, phase: 'inserted' }, notice: ATTACHMENT_INSERTED_NOTICE } : current);
          // The next snapshot carries the authoritative insert flag.
          refreshAttachmentSnapshot();
          return;
        }
        setAttachment(current => current ? { ...current, busyAction: null, notice:
          result.status === 'held'
            ? (result.reason === 'composing' ? ATTACHMENT_COMPOSING_NOTICE : 'No prepared image is attached yet.')
            : result.status === 'unavailable'
              ? 'The attachment backend is not available in this build.'
              : result.message } : current);
      })
      .catch(() => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null, notice: 'The image reference could not be inserted.' } : current);
      });
  }, [attachment, refreshAttachmentSnapshot, selectedPane]);

  /** Explicit server-side delete of the completed remote file. */
  const deleteRemoteAttachment = useCallback(() => {
    const operation = attachment?.operation;
    if (!attachment || !operation || attachment.busyAction) return;
    if (operation.phase !== 'uploaded' && operation.phase !== 'inserted' && operation.phase !== 'failed' && operation.phase !== 'cancelled') return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'deleteRemote', notice: '' } : current);
    void MeetermTerminal.deleteRemoteAttachment(attachment.destination.terminalId)
      .then(result => {
        if (attachmentGeneration.current !== generation) return;
        if (result.status === 'accepted') {
          setAttachment(current => current?.operation ? { ...current, busyAction: null, operation: { ...current.operation, remoteRemoved: true } } : current);
          refreshAttachmentSnapshot();
          return;
        }
        setAttachment(current => current ? { ...current, busyAction: null, notice:
          result.status === 'unavailable' ? 'The attachment backend is not available in this build.' : result.message } : current);
      })
      .catch(() => {
        if (attachmentGeneration.current !== generation) return;
        setAttachment(current => current ? { ...current, busyAction: null, notice: 'The remote file could not be deleted.' } : current);
      });
  }, [attachment, refreshAttachmentSnapshot]);

  /** Explicit Discard: cancels/disposes the core op and removes local files. */
  const discardAttachmentDraft = useCallback(() => {
    if (!attachment || attachment.busyAction) return;
    const generation = attachmentGeneration.current;
    setAttachment(current => current ? { ...current, busyAction: 'discard', notice: '' } : current);
    void (async () => {
      if (!smokeFixtureActive) {
        try { await MeetermTerminal.discardAttachment(); } catch { /* local cleanup is best-effort */ }
      }
      if (attachmentGeneration.current !== generation) return;
      setAttachment(current => current ? {
        ...current,
        phase: 'choosing',
        prepared: null,
        operation: null,
        busyAction: null,
        sessionReady: false,
        notice: '',
        errorCode: '',
        errorMessage: '',
      } : current);
    })();
  }, [attachment, smokeFixtureActive]);

  const reopenAttachmentPicker = useCallback(() => {
    setAttachment(current => current ? { ...current, phase: 'choosing', notice: '', errorCode: '', errorMessage: '' } : current);
  }, []);

  const openSwitcher = useCallback(() => {
    if (commandPending.current) return;
    Keyboard.dismiss();
    switcherBoundary.current = false;
    setSwitcherTarget(null);
    setSwitcherStarted(false);
    setSwitcherAccepting(false);
    setSwitcherMessage('');
    setSheet('switcher');
  }, []);

  const beginSwitcherTarget = useCallback(async (target: SwitcherTarget) => {
    if (commandPending.current || switcherOperation.current) return false;
    switcherCancelReleaseIssued.current = false;
    switcherOperation.current = true;
    commandPending.current = true;
    commandVersion.current += 1;
    setCommandBusy(true);
    setSwitcherTarget(target);
    setSwitcherMessage('');
    setSwitcherStarted(false);
    setSwitcherAccepting(true);
    let boundaryAccepted = false;
    let boundaryAttempted = false;
    let boundaryOutcomeObserved = false;
    const profile = target.profile;
    const selectedProfileId = target.isCurrent ? profileId : profile.id;

    const publishBoundary = (state: 'Connecting' | 'Disconnected') => {
      // Do not let a poll which began against the old connection restore its
      // Ready workspace after the native owner accepted this switch.
      resetForConnection(profile, true);
      setProfileId(selectedProfileId);
      switcherBoundary.current = true;
      setSwitcherStarted(true);
      setSwitcherMessage('');
      if (state === 'Disconnected') {
        setConnection({ ...INITIAL_CONNECTION, state, host: profile.host, port: profile.port });
      }
    };

    const rejectBeforeBoundary = () => {
      setSwitcherTarget(null);
      setSwitcherStarted(false);
      setSwitcherMessage('Could not start the switch. Check the connection and try again.');
    };

    const unknownBoundaryResult = () => {
      const message = 'The switch result could not be confirmed. Start a new connection before continuing.';
      setSwitcherTarget(null);
      setSwitcherStarted(false);
      setSwitcherMessage('The switch result could not be confirmed. Choose a server or Session to reconnect.');
      failClosedForBoundaryResult('switch_outcome_unknown', message);
    };

    const finishBoundaryResult = (rawResult: unknown): 'accepted' | 'rejected' | 'failed' | 'unknown' => {
      const outcome = readRuntimeBoundaryResult(rawResult);
      if (!outcome) return 'unknown';
      boundaryOutcomeObserved = true;
      if (outcome.status === 'not_invoked' || outcome.status === 'rejected_before_boundary') {
        return 'rejected';
      }
      boundaryAccepted = true;
      if (outcome.status === 'accepted_after_failure') return 'failed';
      return 'accepted';
    };

    try {
      const currentOwnerCanChangeInPlace = target.isCurrent
        && !['Disconnected', 'Failed', 'Closing'].includes(connection.state)
        && Boolean(control.operationEpoch);
      if (currentOwnerCanChangeInPlace) {
        // This native command drains the current runtime actor and starts a
        // fresh authenticated discovery generation for the same SSH profile.
        // Its synchronous acceptance is the one-way boundary for the old
        // selected runtime.
        boundaryAttempted = true;
        const result = finishBoundaryResult(
          await MeetermTerminal.changeRuntime(CONNECTION_ID, control.operationEpoch),
        );
        if (result === 'rejected') {
          rejectBeforeBoundary();
          return false;
        }
        if (result === 'unknown') {
          unknownBoundaryResult();
          return false;
        }
        publishBoundary('Connecting');
        if (result === 'failed') {
          const message = 'Could not start the new connection. Choose a server or Session and try again.';
          switcherBoundary.current = true;
          setSwitcherStarted(true);
          setSwitcherMessage(message);
          failClosedForBoundaryResult('runtime_switch_start_failed', message);
          return false;
        }
      } else {
        const currentPreferences = preferencesLoaded ? preferences : await MeetermTerminal.getPreferences();
        await MeetermTerminal.setAutomaticReconnect(CONNECTION_ID, currentPreferences.automaticReconnect);
        await foregroundCommands.current;
        await MeetermTerminal.setForeground(CONNECTION_ID, foreground.current);
        // One selected actor per native connection: explicitly release it
        // before connecting to the next SSH endpoint.
        boundaryAttempted = true;
        const result = finishBoundaryResult(
          await MeetermTerminal.disconnectForSwitcher(CONNECTION_ID),
        );
        if (result === 'rejected') {
          rejectBeforeBoundary();
          return false;
        }
        if (result === 'unknown') {
          unknownBoundaryResult();
          return false;
        }
        publishBoundary('Disconnected');
        if (result === 'failed') {
          const message = 'Could not release the current connection cleanly. Choose a server or Session and try again.';
          switcherBoundary.current = true;
          setSwitcherStarted(true);
          setSwitcherMessage(message);
          failClosedForBoundaryResult('switch_release_failed', message);
          return false;
        }

        let released: SshConnectionState | null = null;
        try { released = await MeetermTerminal.getConnectionState(CONNECTION_ID); } catch { /* The release already crossed the boundary. */ }
        try {
          const releasedSession = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
          const releasedControl = normalizeWorkspaceControl((releasedSession as WorkspaceState & { control?: unknown }).control);
          observeCleanupWarning(releasedControl.cleanupWarning ?? legacyCleanupWarning(released ?? INITIAL_CONNECTION));
        } catch {
          observeCleanupWarning(legacyCleanupWarning(released ?? INITIAL_CONNECTION));
        }

        if (profile.credentialSaved) {
          await MeetermTerminal.connectProfileHost(CONNECTION_ID, profile.id);
          resetForConnection(profile, true);
          setProfileId(selectedProfileId);
          switcherBoundary.current = true;
          setSwitcherStarted(true);
        } else {
          // The old owner is gone before opening the existing SSH form. The
          // form returns to this switcher; it never restores the old runtime.
          setConnection({ ...(released ?? INITIAL_CONNECTION), state: 'Disconnected', host: profile.host, port: profile.port });
        }
      }
      return true;
    } catch {
      if (boundaryAccepted) {
        switcherBoundary.current = true;
        setSwitcherStarted(true);
        const message = 'Could not start the new connection. Choose a server or Session and try again.';
        setSwitcherMessage(message);
        failClosedForBoundaryResult('runtime_switch_start_failed', message);
      } else if (boundaryAttempted && !boundaryOutcomeObserved) {
        unknownBoundaryResult();
      } else {
        rejectBeforeBoundary();
      }
      return false;
    } finally {
      setSwitcherAccepting(false);
      switcherOperation.current = false;
      commandPending.current = false;
      setCommandBusy(false);
    }
  }, [control, failClosedForBoundaryResult, observeCleanupWarning, preferences, preferencesLoaded, profileId, resetForConnection]);

  const startSwitcherTarget = useCallback(async (target: SwitcherTarget) => {
    const needsForm = !target.profile.credentialSaved && !(target.isCurrent
      && !['Disconnected', 'Failed', 'Closing'].includes(connection.state)
      && Boolean(control.operationEpoch));
    const started = await beginSwitcherTarget(target);
    if (!started || !needsForm || !switcherBoundary.current) return started;
    switcherFormConnected.current = false;
    returnToSwitcherAfterForm.current = true;
    showModal(() => {
      formSavedProfile.current = undefined;
      returnToServersAfterForm.current = false;
      setFormProfile(target.profile);
      setFormMode('connect');
      setFormVisible(true);
    });
    return true;
  }, [beginSwitcherTarget, connection.state, control.operationEpoch, showModal]);

  const connectSavedProfile = useCallback((profile: ServerProfile) => {
    if (commandPending.current) return;
    if (active && profile.id !== profileId) {
      openSwitcher();
      void startSwitcherTarget({ profile, isCurrent: false });
      return;
    }
    const connect = () => {
      if (!profile.credentialSaved) { openProfileForm(profile); return; }
      void runCommand(async () => {
        await prepareConnection();
        await MeetermTerminal.connectProfileHost(CONNECTION_ID, profile.id);
        resetForConnection(profile);
        setProfileId(profile.id);
      }, 'Could not connect to this saved server. Choose Edit server to check its address and credentials.');
    };
    if (active && profile.id === profileId) {
      setSheet(null);
    } else connect();
  }, [active, openProfileForm, openSwitcher, prepareConnection, profileId, resetForConnection, runCommand, startSwitcherTarget]);

  const cancelSwitcher = useCallback(async (destination: 'close' | 'switcher' | 'servers' = 'close') => {
    if (switcherOperation.current) return;
    const hadStarted = switcherBoundary.current;
    if (!hadStarted) {
      setSwitcherTarget(null);
      setSwitcherStarted(false);
      setSwitcherMessage('');
      setSwitcherAccepting(false);
      if (destination === 'servers') presentSheetAfterCurrentDismissal('servers');
      else setSheet(destination === 'switcher' ? 'switcher' : null);
      return;
    }

    // The switch boundary retired the previous UI binding. Route any later
    // connection through a saved profile or credential form; the native
    // ManualReconnect profile was intentionally cleared before selection.
    switcherOperation.current = true;
    commandPending.current = true;
    commandVersion.current += 1;
    setCommandBusy(true);
    invalidateRuntimeDiscovery(false);
    pendingRuntimeSelection.current = null;
    pendingRuntimeCreation.current = '';
    pendingRuntimeSelectionBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    pendingRuntimeCreationBaseline.current = { connectionGeneration: '', revision: -1, errorCode: '' };
    setRuntimeSelectingId('');
    setRuntimeBusy(false);
    setRuntimeActionBusy(false);
    setSwitcherTarget(null);
    setSwitcherStarted(false);
    setSwitcherAccepting(false);
    setSwitcherMessage(destination === 'switcher'
      ? 'The previous attempt was canceled. Its remote session remains available; choose a server to continue.'
      : '');
    switcherBoundary.current = false;
    switcherCancelFence.current = true;
    switcherCancelReleaseIssued.current = false;
    recoveryInvalidatedRef.current = true;
    setRecoveryInvalidated(true);
    workspaceObservationRef.current = false;
    ignoreReadyUntilNewConnection.current = true;
    runtimeSelectionRequired.current = false;
    retainedPaneRef.current = null;
    setRecoveredEpoch('');
    setSession(EMPTY_WORKSPACES);
    setSelectedPaneIds({});
    setWorkspaceId('');
    setScreen('workspaces');
    updateRuntimeBound(false);
    setConnection(current => ({ ...current, state: 'Closing' }));
    if (destination === 'servers') presentSheetAfterCurrentDismissal('servers');
    else setSheet(destination === 'switcher' ? 'switcher' : null);
    try {
      await MeetermTerminal.disconnect(CONNECTION_ID);
      const disconnected = await MeetermTerminal.getConnectionState(CONNECTION_ID);
      setConnection(disconnected);
      try {
        const disconnectedSession = await MeetermTerminal.getWorkspaceState(CONNECTION_ID);
        const disconnectedControl = normalizeWorkspaceControl((disconnectedSession as WorkspaceState & { control?: unknown }).control);
        observeCleanupWarning(disconnectedControl.cleanupWarning ?? legacyCleanupWarning(disconnected));
      } catch {
        observeCleanupWarning(legacyCleanupWarning(disconnected));
      }
    } catch {
      setControlMessage('Could not cancel the switch request. Retry or choose a server after connection status updates.');
    } finally {
      switcherOperation.current = false;
      commandPending.current = false;
      setCommandBusy(false);
    }
  }, [invalidateRuntimeDiscovery, observeCleanupWarning, presentSheetAfterCurrentDismissal, updateRuntimeBound]);

  const closeSwitcher = useCallback(() => {
    if (switcherAccepting) return;
    void cancelSwitcher('close');
  }, [cancelSwitcher, switcherAccepting]);

  const switcherDismissed = useCallback(() => {
    setHostPromptDeferred(false);
    setModalPending(false);
    const show = pendingModal.current;
    pendingModal.current = null;
    show?.();
    if (sheet === 'switcher' && !returnToSwitcherAfterForm.current && switcherBoundary.current) {
      void cancelSwitcher('close');
    }
    // An iOS swipe-dismissal hides the attachment sheet the same way onClose
    // does: the native session and draft are retained for same-process
    // remount recovery. Only the explicit Discard action cleans them up.
  }, [cancelSwitcher, sheet]);

  const manageFromSwitcher = useCallback(() => {
    if (switcherBoundary.current) void cancelSwitcher('servers');
    else {
      setSwitcherTarget(null);
      setSwitcherMessage('');
      presentSheetAfterCurrentDismissal('servers');
    }
  }, [cancelSwitcher, presentSheetAfterCurrentDismissal]);

  const chooseAnotherSwitcherServer = useCallback(() => {
    if (switcherBoundary.current) void cancelSwitcher('switcher');
    else {
      setSwitcherTarget(null);
      setSwitcherMessage('');
    }
  }, [cancelSwitcher]);

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
    if (!runtimeReady || commandPending.current) return;
    showModal(() => setNameRequest(request));
  }, [runtimeReady, showModal]);

  const saveName = useCallback(async (name: string) => {
    if (!nameRequest || !runtimeReady) return false;
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
  }, [nameRequest, runCommand, runtimeReady]);

  const closeWorkspace = useCallback((item: Workspace) => {
    if (!runtimeReady || commandPending.current) return;
    Alert.alert('Close workspace?', `${item.name}\n\n${item.panes.length} terminals and their running processes will close. Unsaved work will be lost.`, [
      { text: 'Cancel', style: 'cancel' },
      { text: 'Close', style: 'destructive', onPress: () => {
        void runCommand(() => MeetermTerminal.closeWorkspace(CONNECTION_ID, item.id), 'Could not close this workspace. Check your connection.').then(success => {
          if (success) { setSheet(null); if (workspaceId === item.id) backToWorkspaces(); }
        });
      } },
    ]);
  }, [backToWorkspaces, runCommand, runtimeReady, workspaceId]);

  const workspaceOptions = useCallback((item: Workspace) => {
    if (!runtimeReady || commandPending.current) return;
    itemActions(item.name, () => openName({ kind: 'renameWorkspace', workspace: item }), () => closeWorkspace(item), 'workspace');
  }, [closeWorkspace, openName, runtimeReady]);

  const createPane = useCallback(() => {
    if (!workspace || !runtimeReady) return;
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
  }, [runCommand, runtimeReady, workspace, group, session.groupsSupported, groupPanes.length]);

  const closePane = useCallback(() => {
    if (!selectedPane || !workspace || !runtimeReady || commandPending.current) return;
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
  }, [backToWorkspaces, runCommand, runtimeReady, selectedPane, workspace, session.groupsSupported, groupPanes.length]);

  const chooseGroup = useCallback((item: TerminalGroup) => {
    if (!runtimeReady || commandPending.current) return;
    const remembered = panes.find(pane => pane.groupId === item.id && pane.id === selectedPaneIds[item.id]);
    const selection = remembered ? choosePane(remembered) : runCommand(() => MeetermTerminal.selectGroup(CONNECTION_ID, item.id), 'Could not open this group. Check the list and try again.');
    void selection.then(success => { if (success) setSheet(null); });
  }, [runtimeReady, runCommand, panes, selectedPaneIds, choosePane]);

  const closeGroup = useCallback((item: TerminalGroup) => {
    if (!runtimeReady || commandPending.current) return;
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
  }, [panes, runtimeReady, session.groups, runCommand, backToWorkspaces]);

  const refreshTerminal = useCallback(() => {
    if (!runtimeReady || commandPending.current) return;
    void runCommand(() => MeetermTerminal.refreshTerminal(CONNECTION_ID), 'Could not refresh this terminal. Check your connection.').then(success => { if (success) setSheet(null); });
  }, [runCommand, runtimeReady]);
  const closeSearch = useCallback(() => {
    Keyboard.dismiss();
    setSearching(false);
  }, []);

  useEffect(() => {
    const subscription = BackHandler.addEventListener('hardwareBackPress', () => {
      if (foundation) { setFoundation(false); return true; }
      if (sheet === 'switcher') { closeSwitcher(); return true; }
      if (runtimePickerVisible) { cancelRuntimeSelection(); return true; }
      if (screen === 'terminal') { backToWorkspaces(); return true; }
      if (searching) { closeSearch(); return true; }
      return false;
    });
    return () => subscription.remove();
  }, [backToWorkspaces, cancelRuntimeSelection, closeSearch, closeSwitcher, foundation, runtimePickerVisible, screen, searching, sheet]);

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

  const showRecoveryRail = Boolean(recoveryPhaseActive && retainedWorkAvailable && surfaceAvailable && recoveryCopy);
  const statusNotice = attempted && !ready && (!showRecoveryRail || canRetryRetainedFromList) ? <View style={[styles.notice, { backgroundColor: colors.surface }]}>
    <Text style={[styles.noticeTitle, { color: colors.text }]}>{connection.state === 'Failed' && connection.errorCode === 'host_key_changed' ? 'Verify this server' : connection.state === 'Disconnected' ? 'Disconnected' : presentation.label}</Text>
    <Text style={[styles.noticeBody, { color: colors.muted }]}>{connection.state === 'Failed' ? connectionError(connection) : connection.state === 'Disconnected' ? hasConnected ? 'Your work is still running on the server. Reconnect to pick up where you left off.' : 'Enter your connection details to get started.' : closing ? hasConnected ? 'Disconnecting. Your work will keep running on the server.' : 'Canceling the connection.' : 'Checking your remote workspaces.'}</Text>
    <View style={styles.noticeActions}>
      {canShowReconnect ? <Button testID="workspaces-reconnect" label="Reconnect" colors={colors} disabled={commandBusy || recoveryPending.retry} onPress={reconnect}>Reconnect</Button> : null}
      {!active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Connect" onPress={openForm} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Connection details</Text></Pressable> : null}
      {active && !closing ? <Pressable accessibilityRole="button" accessibilityLabel="Cancel connection" disabled={commandBusy} onPress={disconnect} style={styles.textAction}><Text style={[styles.actionText, { color: colors.accent }]}>Cancel</Text></Pressable> : null}
      {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: colors.danger }]}>Review key change</Text></Pressable> : null}
    </View>
  </View> : null;

  const feedbackColors = sheet ? homeColors : colors;
  const cleanupWarningNotice = cleanupWarning ? <View testID="cleanup-warning" accessibilityLiveRegion="polite" style={[styles.feedback, { backgroundColor: feedbackColors.surface }]}>
    <Text style={[styles.noticeBody, { color: feedbackColors.danger, flex: 1 }]}>{cleanupWarning.message}</Text>
    <IconButton icon="close" label="Dismiss desktop layout warning" testID="cleanup-warning-dismiss" colors={feedbackColors} onPress={dismissCleanupWarning} />
  </View> : null;
  const feedback = controlMessage || pollProblem ? <View accessibilityLiveRegion="polite" style={[styles.feedback, { backgroundColor: feedbackColors.surface }]}>
    <Text style={[styles.noticeBody, { color: feedbackColors.danger, flex: 1 }]}>{controlMessage || 'Connection status is unavailable. Wait a moment, then reconnect.'}</Text>
    {controlMessage ? <IconButton icon="close" label="Dismiss message" colors={feedbackColors} onPress={() => setControlMessage('')} /> : null}
  </View> : null;

  const recoveryRail = showRecoveryRail
    ? <RecoveryRail copy={recoveryCopy!} colors={DARK} busy={{ retry: recoveryPending.retry, change: recoveryPending.change }} onRetry={retryRecovery} onReview={reviewChangedHostKey} onConnectionDetails={openForm} onChooseTerminal={() => setControlMessage('Choose another terminal after recovery finishes.')} onChange={openRecoveryChange} />
    : recoveredCopy ? <RecoveryRail copy={recoveredCopy} colors={DARK} busy={{ retry: false, change: false }} onRetry={() => {}} onReview={() => {}} onConnectionDetails={() => {}} onChooseTerminal={() => {}} onChange={() => {}} />
    : null;

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
        <Pressable testID="switch-server-session" accessibilityRole="button" accessibilityLabel="Switch server or session" accessibilityHint={`${currentSessionDescription}. Opens the server and session switcher.`} onPress={openSwitcher} style={({ pressed }) => [styles.serverTarget, pressed && { backgroundColor: homeColors.surface }]}>
          <Icon name="server" color={homeColors.muted} size={17} />
          <View style={styles.serverTargetLabels}>
            <Text numberOfLines={1} style={[styles.serverName, { color: homeColors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
            <Text numberOfLines={1} style={[styles.serverSessionName, { color: homeColors.muted }]}>{currentSessionDescription}</Text>
          </View>
          <Icon name="down" color={homeColors.muted} size={12} />
        </Pressable>
        <ConnectionStatus connection={connection} colors={homeColors} />
        <IconButton icon="menu" label="Server connection" colors={homeColors} onPress={() => openSheet('server')} />
      </View> : null}
    </>}
    {statusNotice ? <View style={styles.horizontal}>{statusNotice}</View> : null}
    {recoveryRail ? <View style={styles.horizontal}>{recoveryRail}</View> : null}
    {cleanupWarningNotice ? <View style={styles.horizontal}>{cleanupWarningNotice}</View> : null}
    {feedback ? <View style={styles.horizontal}>{feedback}</View> : null}
    {searching ? <View style={styles.horizontal}>
      <SearchField value={query} colors={homeColors} onChange={value => { setQuery(value); listOffsets.current.search = 0; workspaceList.current?.scrollToOffset({ offset: 0, animated: false }); }} autoFocus />
      <Text style={[styles.resultCount, { color: homeColors.muted }]}>{filteredWorkspaces.length} {filteredWorkspaces.length === 1 ? 'result' : 'results'}</Text>
    </View> : attempted && (workspaces.length > 0 || runtimeReady) ? <View style={styles.sectionHeader}>
      <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>All  {workspaces.length}</Text>
      <View style={styles.topActions}><IconButton icon="search" label="Search workspaces" onPress={() => setSearching(true)} colors={homeColors} /><IconButton icon="plus" label="Create workspace" onPress={() => openName({ kind: 'createWorkspace' })} colors={homeColors} disabled={!runtimeReady || commandBusy} /></View>
    </View> : null}
  </View>;

  const emptyList = searching ? <View style={styles.emptySearch}>
    <Icon name="search" color={homeColors.muted} size={28} />
    <Text style={[styles.emptyTitle, { color: homeColors.text }]}>No matching workspaces</Text>
    <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Try another name or clear your search.</Text>
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
  </View> : runtimeReady ? <View style={styles.emptySearch}>
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
      renderItem={({ item }) => {
        const retainedWorkspace = Boolean(recoveryPhaseActive && retainedWorkAvailable
          && retainedPane && item.id === retainedWorkspaceId);
        const disabled = commandBusy || (!runtimeReady && !retainedWorkspace)
          || (presentation.pending && !retainedWorkspace);
        return <View style={styles.horizontal}><WorkspaceRow connected={strongReady} workspace={item} selected={item.id === activeWorkspaceId} disabled={disabled} optionsDisabled={!runtimeReady} colors={homeColors} onPress={() => openWorkspace(item)} onOptions={() => workspaceOptions(item)} /></View>;
      }}
      ListHeaderComponent={listHeader}
      ListEmptyComponent={emptyList}
      contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 20) + 24 }}
      contentOffset={{ x: 0, y: listOffsets.current[searching ? 'search' : 'normal'] }}
      onScroll={event => { listOffsets.current[searching ? 'search' : 'normal'] = event.nativeEvent.contentOffset.y; }}
      scrollEventThrottle={100}
      contentInsetAdjustmentBehavior="automatic"
      automaticallyAdjustKeyboardInsets
      keyboardShouldPersistTaps="handled"
      keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'}
    /></SafeAreaView>}
      terminal={<SafeAreaView edges={['top', 'left', 'right']} style={[styles.flex, { backgroundColor: DARK.background }]}><View style={styles.flex}>
      <View style={styles.terminalHeader}>
        <IconButton icon="back" label="Back to workspaces" colors={DARK} onPress={backToWorkspaces} />
        <View style={styles.terminalHeading}>
          <Pressable accessibilityRole="button" accessibilityLabel="Switch workspace" accessibilityHint={workspace?.name} accessibilityState={{ disabled: !runtimeReady }} disabled={!runtimeReady} onPress={() => openSheet('workspaces')} style={({ pressed }) => [styles.terminalTitleRow, !runtimeReady && { opacity: .55 }, pressed && { opacity: .65 }]}><Text numberOfLines={1} style={[styles.terminalTitle, { color: DARK.text }]}>{workspace?.name ?? 'Workspaces'}</Text><Icon name="down" color={DARK.muted} size={12} /></Pressable>
          <View style={styles.terminalStatusRow}>
            <Pressable testID="switch-server-session" accessibilityRole="button" accessibilityLabel="Switch server or session" accessibilityHint={`${currentSessionDescription}. Opens the server and session switcher.`} onPress={openSwitcher} style={styles.terminalServerSession}>
              <Text numberOfLines={1} style={[styles.terminalHost, { color: DARK.muted }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
              <Text numberOfLines={1} style={[styles.terminalHost, { color: DARK.muted }]}>{currentSessionDescription}</Text>
            </Pressable>
            <ConnectionStatus connection={connection} colors={DARK} />
          </View>
        </View>
        <IconButton testID="attach-image" icon="attach" label="Attach image" colors={DARK} disabled={!runtimeReady || commandBusy || !surfaceAvailable} onPress={openAttachment} />
        <IconButton icon="menu" label="Terminal menu" colors={DARK} onPress={() => openSheet('server')} />
      </View>
      {groups.length > 1 ? <View style={styles.groupBar}>
        <Text style={[styles.groupLabel, { color: DARK.muted }]}>Group</Text>
        <Pressable accessibilityRole="button" accessibilityLabel={(() => { const phrase = agentStatusPhrase(group?.agentStatus, strongReady); return phrase ? `Switch terminal group, Group ${group?.name || 'Untitled group'}, ${phrase}` : 'Switch terminal group'; })()} accessibilityHint={group?.name} disabled={!runtimeReady || commandBusy} onPress={() => openSheet('groups')} style={({ pressed }) => [styles.groupPicker, { backgroundColor: DARK.surface }, !runtimeReady && { opacity: .55 }, pressed && { opacity: .65 }]}>
          <AgentStatusIndicator status={group?.agentStatus} live={strongReady} colors={DARK} testID={group ? `group-agent-status-${group.id}` : undefined} />
          <Text numberOfLines={1} style={[styles.groupName, { color: DARK.text }]}>{group?.name || 'Choose a group'}</Text><Icon name="down" color={DARK.muted} size={12} />
        </Pressable>
      </View> : null}
      {workspace && groupPanes.length > 0 ? <View style={[styles.paneStrip, { borderBottomColor: DARK.border }]}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={styles.paneTabs}>
        {groupPanes.map((pane, index) => { const name = pane.name || `Terminal ${index + 1}`; const spokenName = pane.name ? `Terminal ${name}` : name; const phrase = agentStatusPhrase(pane.agent?.status, strongReady); return <Pressable key={pane.id} testID={`terminal-tab-${pane.id}`} accessibilityRole="tab" accessibilityLabel={`${spokenName}${phrase ? `, ${phrase}` : ''}`} accessibilityHint={strongReady ? name : `${name}. Cached output, read only. Input is paused until recovery finishes.`} accessibilityState={{ selected: pane.id === selectedPane?.id, disabled: !runtimeReady || commandBusy }} disabled={!runtimeReady || commandBusy} onPress={() => choosePane(pane)} onLongPress={() => { if (runtimeReady) openName({ kind: 'renamePane', pane }); }} style={({ pressed }) => [styles.paneTab, { borderBottomColor: pane.id === selectedPane?.id ? DARK.accent : 'transparent' }, pressed && { backgroundColor: DARK.surface }]}><Icon name="terminal" color={pane.id === selectedPane?.id ? DARK.accent : DARK.muted} size={15} /><AgentStatusIndicator status={pane.agent?.status} live={strongReady} colors={DARK} testID={`terminal-agent-status-${pane.id}`} /><Text numberOfLines={1} style={[styles.paneTabText, { color: pane.id === selectedPane?.id ? DARK.accent : DARK.muted }]}>{name}</Text></Pressable>; })}
      </ScrollView><IconButton icon="plus" label="Create terminal" colors={DARK} disabled={!runtimeReady || commandBusy} onPress={createPane} /></View> : null}
      {selectedPane?.agent ? (() => { const phrase = agentStatusPhrase(selectedPane.agent.status, strongReady); return <View testID="selected-agent-line" accessible accessibilityRole="text" accessibilityLabel={`${selectedPane.agent.name}, ${phrase}`} accessibilityHint="Status reported by Herdr. This does not verify task correctness or passing tests." accessibilityLiveRegion="polite" style={styles.agentLine}>
        <Text accessible={false} numberOfLines={1} style={[styles.agentName, { color: DARK.muted }]}>{selectedPane.agent.name}</Text>
        <AgentStatusIndicator status={selectedPane.agent.status} live={strongReady} colors={DARK} showLabel testID="selected-agent-status" />
      </View>; })() : null}
      {recoveryRail}
      {cleanupWarningNotice ? <View style={styles.terminalFeedback}>{cleanupWarningNotice}</View> : null}
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {surfaceAvailable ? (
        // Unmounting a surface cancels composition; the shared native registry
        // still owns the SSH connection and each terminal's retained state.
        (recoveryPhaseActive && retainedWorkAvailable) || (sheet === null && !modalPending && !formVisible && !settingsVisible && !nameRequest && appState === 'active') ? <TerminalView key={selectedPane!.terminalId} terminalId={selectedPane!.terminalId} interactionMode={strongReady ? 'live' : 'cachedReadOnly'} accessibilityLabel={strongReady ? 'Terminal' : 'Terminal, cached output, read only'} accessibilityHint={strongReady ? undefined : 'Input is paused until recovery finishes.'} fontSize={preferences.fontSize} theme={resolvedTheme} scrollbackLines={preferences.scrollbackLines} style={styles.flex} /> : <View style={[styles.flex, { backgroundColor: DARK.terminal }]} />
      ) : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={[styles.terminalUnavailable, { paddingBottom: Math.max(insets.bottom, 24) }]}>
        {statusNotice}
        {runtimeReady ? <View style={styles.gone}>
          <Icon name="terminal" color={DARK.muted} size={32} />
          <Text accessibilityLabel="Terminal unavailable" style={[styles.emptyTitle, { color: DARK.text }]}>{workspace ? 'This terminal has closed' : 'This workspace has closed'}</Text>
          <Text style={[styles.emptyBody, { color: DARK.muted }]}>{workspace ? 'Select another terminal to keep working.' : 'Choose another workspace from the list.'}</Text>
          <Button label="Back to workspaces" colors={DARK} secondary onPress={backToWorkspaces}>Back to workspaces</Button>
        </View> : null}
      </ScrollView>}
    </View></SafeAreaView>}
    />

    <RuntimePicker
      // Fresh/manual connects still use the standalone picker. A switch
      // already in progress renders the same discovery snapshot inside its
      // Server / Session sheet so an old runtime picker never covers it.
      visible={runtimePickerVisible && sheet !== 'servers' && sheet !== 'switcher'}
      serverName={currentProfile?.name ?? endpoint(connection)}
      discovery={runtimeDiscovery}
      createVisible={runtimeCreateVisible}
      busy={commandBusy || runtimeActionBusy || runtimeBusy || Boolean(runtimeSelectingId)}
      discoveryBusy={runtimeBusy && !runtimeActionBusy && !runtimeSelectingId}
      cancelDisabled={commandBusy}
      selectingId={runtimeSelectingId}
      selectionErrors={runtimeSelectionErrors}
      message={runtimeMessage}
      creationError={runtimeCreationError}
      notification={runtimePickerVisible ? <>{cleanupWarningNotice}{feedback}</> : null}
      onCancel={cancelRuntimeSelection}
      onDismiss={() => {}}
      onRefresh={refreshRuntimes}
      onRetryBackend={() => { if (!commandPending.current && !runtimeBusy) void loadRuntimeDiscovery(true, false); }}
      onSelect={candidate => { void selectRuntime(candidate); }}
      onOpenCreate={() => { if (!runtimeBusy) { setRuntimeCreationError(''); setRuntimeCreateVisible(true); } }}
      onBackToList={() => { if (!runtimeBusy) { setRuntimeCreationError(''); setRuntimeCreateVisible(false); } }}
      onCreate={createTmuxSession}
      colors={homeColors}
    />
    <ConnectionForm visible={formVisible} initialProfile={formProfile} mode={formMode} colors={homeColors} onClose={finishConnectionForm} onDismiss={connectionFormDismissed} onSubmit={submitConnection} />
    <SettingsForm visible={settingsVisible} preferences={preferences} colors={homeColors} onClose={() => setSettingsVisible(false)} onSave={savePreferences} />
    <NameForm visible={nameRequest !== null} title={nameRequest?.kind === 'createWorkspace' ? 'Create workspace' : nameRequest?.kind === 'renameWorkspace' ? 'Rename workspace' : nameRequest?.kind === 'createGroup' ? 'Create group' : nameRequest?.kind === 'renameGroup' ? 'Rename group' : 'Rename terminal'} initialName={nameRequest?.kind === 'renameWorkspace' ? nameRequest.workspace.name : nameRequest?.kind === 'renamePane' ? nameRequest.pane.name : nameRequest?.kind === 'renameGroup' ? nameRequest.group.name : ''} colors={homeColors} onClose={() => setNameRequest(null)} onSave={saveName} />
    <NativeSheet title={sheet === 'groups' ? 'Switch group' : sheet === 'workspaces' ? 'Switch workspace' : sheet === 'handoff' ? 'Continue on your computer' : sheet === 'attachment' ? 'Attach image' : sheet === 'servers' ? 'Saved servers' : sheet === 'switcher' ? switcherTarget ? `Sessions on ${switcherSessionServerName}` : 'Switch server or session' : sheet === 'recovery' ? 'Change connection or runtime' : 'Server'} visible={sheet !== null} onClose={sheet === 'switcher' ? closeSwitcher : sheet === 'attachment' ? closeAttachmentSheet : () => setSheet(null)} closeLabel={sheet === 'recovery' ? 'Cancel' : sheet === 'switcher' ? 'Cancel server or session switch' : sheet === 'attachment' ? 'Close attachment sheet' : 'Close sheet'} busy={sheet === 'switcher' ? switcherAccepting : commandBusy || recoveryPending.change} allowDismissWhileBusy={sheet === 'switcher' && !switcherAccepting} onDismiss={switcherDismissed} colors={homeColors}>
      {cleanupWarningNotice ? <View style={styles.terminalFeedback}>{cleanupWarningNotice}</View> : null}
      {feedback ? <View style={styles.terminalFeedback}>{feedback}</View> : null}
      {sheet === 'switcher' ? switcherTarget ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.runtimePickerContent}>
        <View style={styles.runtimeIntro}>
          <Text accessibilityRole="header" style={[styles.runtimeIntroTitle, { color: homeColors.text }]}>Choose a Session</Text>
          <Text numberOfLines={2} style={[styles.runtimeServer, { color: homeColors.muted }]}>{switcherTarget.profile.name} · {endpoint({ ...connection, host: switcherTarget.profile.host, port: switcherTarget.profile.port })}</Text>
          <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>Selecting a server disconnects this phone from its current session, then authenticates and finds sessions on the selected server.</Text>
        </View>
        <Button testID="switcher-choose-another-server" label="Choose another server" colors={homeColors} secondary disabled={switcherAccepting} onPress={chooseAnotherSwitcherServer}>Choose another server</Button>
        {switcherStarted && ['Failed', 'Disconnected'].includes(connection.state) ? <Button testID="switcher-retry" label="Retry server connection" colors={homeColors} disabled={switcherAccepting || commandBusy} onPress={() => { void startSwitcherTarget(switcherTarget); }}>Retry connection</Button> : null}
        {switcherAccepting || (switcherStarted && runtimeBusy && !runtimeDiscovery) ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>{switcherAccepting ? 'Releasing the current session…' : 'Finding sessions…'}</Text></View> : null}
        {switcherMessage || runtimeMessage ? <Text accessibilityRole="alert" style={[styles.runtimeMessage, { color: homeColors.danger }]}>{switcherMessage || runtimeMessage}</Text> : null}
        {runtimeCreateVisible ? <View style={styles.runtimeSection}>
          <Text style={[styles.runtimeIntroTitle, { color: homeColors.text }]}>Create a tmux session</Text>
          <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>This creates a detached session on {switcherSessionServerName} and opens it after native confirms its identity.</Text>
          <TextInput accessibilityLabel="tmux session name" testID="runtime-tmux-name" defaultValue={suggestedTmuxName(runtimeDiscovery)} autoCapitalize="none" autoComplete="off" autoCorrect={false} maxLength={64} placeholder="meeterm" placeholderTextColor={homeColors.placeholder} selectionColor={homeColors.accent} style={[styles.runtimeInput, { color: homeColors.text, backgroundColor: homeColors.elevated, borderColor: runtimeCreationError ? homeColors.danger : homeColors.border }]} onChangeText={value => { setRuntimeCreationError(''); (runtimeNameRef.current = value); }} />
          {runtimeCreationError ? <Text accessibilityRole="alert" style={[styles.runtimeError, { color: homeColors.danger }]}>{runtimeCreationError}</Text> : null}
          <Button testID="runtime-tmux-create-submit" label="Create tmux session" colors={homeColors} disabled={runtimeBusy || runtimeActionBusy} onPress={() => { const name = (runtimeNameRef.current || suggestedTmuxName(runtimeDiscovery)).trim(); const error = validateTmuxSessionName(name); if (error) setRuntimeCreationError(error); else void createTmuxSession(name); }}>Create and open</Button>
          <Button label="Back to sessions" colors={homeColors} secondary disabled={runtimeBusy || runtimeActionBusy} onPress={() => setRuntimeCreateVisible(false)}>Back to sessions</Button>
        </View> : null}
        {!switcherAccepting && !(switcherStarted && runtimeBusy && !runtimeDiscovery) && !runtimeMessage && !switcherMessage && !runtimeDiscovery && switcherStarted && !['Failed', 'HostKeyPending'].includes(connection.state) ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>Connecting before session discovery…</Text></View> : null}
        {!switcherAccepting && runtimeDiscovery ? <>
          {runtimeDiscovery.backends.map(item => {
            const sectionError = item.errorMessage || item.errorCode;
            return <View key={item.backend} style={styles.runtimeSection}>
              <View style={styles.runtimeSectionHeading}><Text style={[styles.runtimeSectionTitle, { color: homeColors.text }]}>{item.backend === 'tmux' ? 'tmux' : 'Herdr'}</Text>{item.state === 'loading' ? <ActivityIndicator size="small" color={homeColors.accent} /> : null}</View>
              {item.state === 'error' || sectionError ? <View style={[styles.runtimeSectionError, { backgroundColor: homeColors.surface }]}><Text accessibilityRole="alert" style={[styles.runtimeHint, { color: homeColors.danger }]}>{sectionError || 'This backend could not be inspected.'}</Text>{item.state === 'error' ? <Button label={`Retry ${item.backend === 'tmux' ? 'tmux' : 'Herdr'} discovery`} colors={homeColors} secondary disabled={runtimeBusy || runtimeActionBusy} onPress={() => { if (!commandPending.current && !runtimeBusy) void loadRuntimeDiscovery(true, false); }}>Retry</Button> : null}</View> : null}
              {item.state === 'loading' ? <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>Looking for running sessions…</Text> : item.candidates.length ? item.candidates.map(candidate => {
                const stopped = candidate.state !== 'running';
                const unavailable = stopped || !candidate.selectable;
                const error = runtimeSelectionErrors[candidate.id] || candidate.errorMessage || candidate.errorCode;
                return <View key={candidate.id} style={[styles.runtimeRowContainer, { borderBottomColor: homeColors.border }]}>
                  <Pressable testID={`runtime-row-${candidate.backend}-${candidate.id}`} accessibilityRole="button" accessibilityLabel={`${candidate.backend === 'tmux' ? 'tmux' : 'Herdr'} ${switcherTarget ? 'session' : 'runtime'} ${candidate.name}`} accessibilityHint={stopped ? 'Open this session on your computer, then refresh.' : !candidate.selectable ? 'This session is unavailable. Refresh and try again.' : undefined} accessibilityState={{ disabled: unavailable || runtimeBusy, selected: runtimeSelectingId === candidate.id }} disabled={unavailable || runtimeBusy} onPress={() => { void selectRuntime(candidate); }} style={({ pressed }) => [styles.runtimeRow, pressed && { backgroundColor: homeColors.surface }, (unavailable || runtimeBusy) && { opacity: stopped ? .55 : .8 }]}>
                    <View style={styles.runtimeRowCopy}>
                      <View style={styles.runtimeNameLine}><Text numberOfLines={2} style={[styles.runtimeName, { color: homeColors.text }]}>{candidate.name}</Text>{candidate.lastUsed ? <Text style={[styles.runtimeBadge, { color: homeColors.accent, borderColor: homeColors.accent }]}>Last used</Text> : null}</View>
                      <Text style={[styles.runtimeState, { color: stopped || !candidate.selectable ? homeColors.muted : homeColors.accent }]}>{stopped ? 'Stopped' : candidate.selectable ? 'Running' : 'Unavailable'}</Text>
                      {stopped ? <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>Open this session in {candidate.backend === 'herdr' ? 'Herdr' : 'tmux'} on your computer, then tap Refresh.</Text> : null}
                      {error ? <Text accessibilityRole="alert" style={[styles.runtimeError, { color: homeColors.danger }]}>{error}</Text> : null}
                    </View>
                    {runtimeSelectingId === candidate.id ? <ActivityIndicator color={homeColors.accent} /> : <Icon name="chevron" color={stopped ? homeColors.muted : homeColors.accent} size={18} />}
                  </Pressable>
                </View>;
              }) : <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>{item.backend === 'tmux' ? 'No running tmux sessions found.' : 'No running Herdr sessions found.'}</Text>}
              {item.backend === 'tmux' && item.state === 'ready' && item.canCreate ? <Button testID="runtime-create-tmux" label="Create tmux session" colors={homeColors} secondary disabled={runtimeBusy || runtimeActionBusy} onPress={() => { runtimeNameRef.current = suggestedTmuxName(runtimeDiscovery); setRuntimeCreationError(''); setRuntimeCreateVisible(true); }}>Create tmux session</Button> : null}
            </View>;
          })}
          <Button testID="runtime-refresh" label={switcherTarget ? 'Refresh sessions' : 'Refresh runtimes'} colors={homeColors} secondary disabled={runtimeBusy || runtimeActionBusy || Boolean(runtimeSelectingId)} onPress={refreshRuntimes}>Refresh</Button>
          <Button testID="switcher-manage-servers" label="Manage servers" colors={homeColors} secondary disabled={switcherAccepting} onPress={manageFromSwitcher}>Manage servers</Button>
          <Text style={[styles.runtimeHint, { color: homeColors.muted }]}>Herdr sessions must already be running. Starting or creating Herdr sessions happens on the computer.</Text>
        </> : null}
      </ScrollView> : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Choose a server to reconnect and list its Sessions. Opening this sheet does not connect or search.</Text>
        {switcherMessage ? <Text accessibilityRole="alert" style={[styles.runtimeMessage, { color: homeColors.danger }]}>{switcherMessage}</Text> : null}
        {profilesLoading && switcherServerRows.length === 0 ? <View style={styles.loading}><ActivityIndicator color={homeColors.accent} /><Text style={[styles.emptyBody, { color: homeColors.muted }]}>Loading saved servers…</Text></View> : null}
        {switcherServerRows.map(({ profile, isCurrent }) => {
          const selectedSession = isCurrent && (runtimeReady || retainedWorkAvailable);
          return <Pressable key={profile.id} testID={`switcher-server-${profile.id}`} accessibilityRole="button" accessibilityLabel={`Browse sessions on ${profile.name}`} accessibilityHint="Disconnects this phone from its current session, then reconnects and lists sessions on this server." accessibilityState={{ selected: selectedSession, disabled: switcherAccepting }} disabled={switcherAccepting} onPress={() => { void startSwitcherTarget({ profile, isCurrent }); }} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }, switcherAccepting && { opacity: .6 }]}>
            <View style={styles.rowCopy}><Text numberOfLines={1} style={[styles.rowTitle, { color: homeColors.text }]}>{profile.name}</Text><Text numberOfLines={1} style={[styles.rowSubtitle, { color: homeColors.muted }]}>{isCurrent ? `${selectedSession ? `Current · ${currentSessionDescription}` : currentSessionDescription} · ${endpoint(connection)}` : `${profile.username}@${endpoint({ ...connection, host: profile.host, port: profile.port })}`}</Text></View>
            {selectedSession ? <Text style={[styles.runtimeBadge, { color: homeColors.accent, borderColor: homeColors.accent }]}>Current</Text> : <Icon name="chevron" color={homeColors.muted} size={18} />}
          </Pressable>;
        })}
        {!switcherServerRows.length && !profilesLoading ? <Text style={[styles.emptyBody, { color: homeColors.muted }]}>No saved servers yet. Connect to a server or add one to continue.</Text> : null}
        {active ? <Button testID="switcher-disconnect" label="Disconnect" colors={homeColors} secondary disabled={commandBusy || switcherAccepting} onPress={disconnect}>Disconnect this phone</Button> : null}
        <Button testID="switcher-manage-servers" label="Manage servers" colors={homeColors} secondary disabled={commandBusy || switcherAccepting} onPress={manageFromSwitcher}>Manage servers</Button>
        {!attempted ? <Button label="Connect to another server" colors={homeColors} onPress={() => openProfileForm()}>Connect to another server</Button> : null}
      </ScrollView> : sheet === 'recovery' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text testID="recovery-change-intro" style={[styles.emptyBody, { color: homeColors.muted }]}>Choosing another destination stops recovery for this workspace. Remote work will not be closed.</Text>
        <Pressable testID="recovery-change-runtime" accessibilityRole="button" accessibilityLabel="Choose another runtime" accessibilityState={{ disabled: recoveryPending.change }} disabled={recoveryPending.change} onPress={() => { void changeRecoveryDestination('runtime'); }} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}>
          <View style={styles.rowCopy}><Text style={[styles.rowTitle, { color: homeColors.text }]}>Choose another runtime</Text><Text style={[styles.rowSubtitle, { color: homeColors.muted }]}>Stay on {recoveryServerLabel}</Text></View><Icon name="chevron" color={homeColors.muted} size={18} />
        </Pressable>
        <Pressable testID="recovery-change-server" accessibilityRole="button" accessibilityLabel="Choose another server" accessibilityState={{ disabled: recoveryPending.change }} disabled={recoveryPending.change} onPress={() => { void changeRecoveryDestination('server'); }} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}>
          <Text style={[styles.rowTitle, { color: homeColors.text }]}>Choose another server</Text><Icon name="chevron" color={homeColors.muted} size={18} />
        </Pressable>
      </ScrollView> : sheet === 'servers' ? <ProfileList profiles={profiles} selectedId={profileId} loading={profilesLoading} error={profilesError} busy={commandBusy} colors={homeColors} onRetry={() => { void loadProfiles(); }} onAdd={() => openProfileForm(undefined, 'save')} onConnect={connectSavedProfile} onEdit={profile => openProfileForm(profile, 'save')} onDelete={deleteProfile} /> : sheet === 'workspaces' ? <View style={styles.flex}>
        <View style={styles.pickerHeader}><Text selectable style={[styles.emptyBody, { color: homeColors.muted }]}>{endpoint(connection)}</Text>{workspaces.length >= 6 ? <SearchField label="Search workspace picker" value={pickerQuery} onChange={setPickerQuery} colors={homeColors} /> : null}<Button label="Create workspace" colors={homeColors} secondary disabled={!runtimeReady || commandBusy} onPress={() => openName({ kind: 'createWorkspace' })}>Create workspace</Button></View>
        <FlatList data={pickerWorkspaces} keyExtractor={item => item.id} contentContainerStyle={styles.pickerList} renderItem={({ item }) => <WorkspaceRow connected={strongReady} workspace={item} selected={item.id === workspaceId} disabled={!runtimeReady || presentation.pending || commandBusy} optionsDisabled={!runtimeReady} colors={homeColors} picker onPress={() => openWorkspace(item)} onOptions={() => workspaceOptions(item)} />} ListEmptyComponent={<Text style={[styles.emptyBody, { color: homeColors.muted }]}>No matching workspaces.</Text>} keyboardShouldPersistTaps="handled" keyboardDismissMode="on-drag" automaticallyAdjustKeyboardInsets />
      </View> : sheet === 'groups' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Keep related terminals together. Select a group to switch.</Text>
        {groups.map(item => { const phrase = agentStatusPhrase(item.agentStatus, strongReady); return <View key={item.id} style={[styles.groupRow, { borderColor: homeColors.border }]}>
          <Pressable accessibilityRole="button" accessibilityLabel={`Group ${item.name || 'Untitled group'}${phrase ? `, ${phrase}` : ''}`} accessibilityState={{ selected: item.id === group?.id, disabled: !runtimeReady || commandBusy }} disabled={!runtimeReady || commandBusy} onPress={() => chooseGroup(item)} style={({ pressed }) => [styles.groupChoice, pressed && { opacity: .65 }]}>
            <View style={styles.groupNameRow}><AgentStatusIndicator status={item.agentStatus} live={strongReady} colors={homeColors} testID={`group-agent-status-${item.id}`} /><Text style={[styles.groupName, { color: item.id === group?.id ? homeColors.accent : homeColors.text }]}>{item.name || 'Untitled group'}</Text></View>
            <Text style={[styles.rowSubtitle, { color: homeColors.muted }]}>{panes.filter(pane => pane.groupId === item.id).length} {panes.filter(pane => pane.groupId === item.id).length === 1 ? 'terminal' : 'terminals'}{item.id === group?.id ? ' · Selected' : ''}</Text>
          </Pressable>
          <IconButton icon="menu" label={`Group options ${item.name}`} colors={homeColors} disabled={!runtimeReady || commandBusy} onPress={() => itemActions(item.name, () => openName({ kind: 'renameGroup', group: item }), () => closeGroup(item), 'workspace')} />
        </View>; })}
        {workspace ? <Button label="Create group" colors={homeColors} secondary disabled={!runtimeReady || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Create group</Button> : null}
      </ScrollView> : sheet === 'handoff' ? <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <Text style={[styles.handoffTitle, { color: homeColors.text }]}>Same work. Bigger screen.</Text>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Your workspace keeps running when you disconnect your phone.</Text>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: homeColors.accent }]}>1</Text><Text style={[styles.emptyBody, { color: homeColors.text, flex: 1 }]}>Disconnect this phone to release the workspace.</Text></View>
        <View style={styles.handoffStep}><Text style={[styles.stepNumber, { color: homeColors.accent }]}>2</Text><Text style={[styles.emptyBody, { color: homeColors.text, flex: 1 }]}>SSH into the same server with the same username on your computer.</Text></View>
        <Text selectable style={[styles.command, { backgroundColor: homeColors.surface, color: homeColors.text }]}>{session.backend === 'herdr' ? `herdr --session ${session.runtime || 'default'}` : `tmux attach -t ${session.runtime || 'meeterm'}`}</Text>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Run this command to reopen the same workspaces and terminals.</Text>
        {active ? <Button label="Disconnect" colors={homeColors} disabled={commandBusy} onPress={disconnect}>Disconnect this phone</Button> : <Button label="Close sheet" colors={homeColors} secondary onPress={() => setSheet(null)}>Close</Button>}
      </ScrollView> : sheet === 'attachment' ? <AttachmentSheet draft={attachment} colors={homeColors} currentTerminalId={selectedPane?.terminalId ?? ''} onPickSource={pickAttachment} onRemoteDirectoryChange={value => setAttachment(current => current ? { ...current, remoteDirectory: value } : current)} onUpload={uploadAttachmentDraft} onRetryUpload={retryAttachmentUpload} onCancel={cancelAttachmentDraft} onInsert={insertAttachmentDraft} onDeleteRemote={deleteRemoteAttachment} onDiscard={discardAttachmentDraft} onChooseDifferent={reopenAttachmentPicker} /> : <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.sheetContent}>
        <View style={styles.serverDetails}>
          <Icon name="server" color={homeColors.accent} size={28} />
          <Text selectable style={[styles.serverDetailTitle, { color: homeColors.text }]}>{currentProfile?.name ?? endpoint(connection)}</Text>
          {currentProfile ? <Text selectable style={[styles.emptyBody, { color: homeColors.muted }]}>{currentProfile.username}@{endpoint(connection)}</Text> : null}
          <ConnectionStatus connection={connection} colors={homeColors} />
        </View>
        <Text style={[styles.emptyBody, { color: homeColors.muted }]}>Your work lives on this server. Disconnecting leaves it running.</Text>
        {canShowReconnect ? <Button testID="server-reconnect" label="Reconnect" colors={homeColors} disabled={commandBusy || recoveryPending.retry} onPress={reconnect}>Reconnect</Button> : null}
        {!active && !closing ? <Button label="Connect" colors={homeColors} secondary onPress={openForm}>Connection details</Button> : null}
        {active ? <Button label="Disconnect" colors={homeColors} secondary disabled={commandBusy} onPress={disconnect}>{ready ? 'Disconnect' : 'Cancel connection'}</Button> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="Switch server or session" disabled={commandBusy} onPress={openSwitcher} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Switch server or session</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        <Pressable accessibilityRole="button" accessibilityLabel="Manage servers" disabled={commandBusy} onPress={() => presentSheetAfterCurrentDismissal('servers')} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Manage servers</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        <Pressable accessibilityRole="button" accessibilityLabel="Terminal settings" disabled={commandBusy} onPress={openSettings} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Settings</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        {screen === 'terminal' && workspace && selectedPane ? <View style={[styles.terminalActions, { borderColor: homeColors.border }]}>
          <Text numberOfLines={2} style={[styles.sectionLabel, { color: homeColors.muted }]}>{selectedPane.name || selectedPane.id}</Text>
          <Button label="Refresh terminal" colors={homeColors} secondary disabled={!runtimeReady || commandBusy} onPress={refreshTerminal}>Refresh terminal</Button>
          <Text style={[styles.noticeBody, { color: homeColors.muted }]}>Ask the remote app to redraw if the display looks wrong after reconnecting.</Text>
          <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename terminal" disabled={!runtimeReady || commandBusy} onPress={() => openName({ kind: 'renamePane', pane: selectedPane })} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Rename</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close terminal" disabled={!runtimeReady || commandBusy} onPress={closePane} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Close terminal</Text></Pressable>
          </View>
          <Pressable accessibilityRole="button" accessibilityLabel="Attach image" accessibilityHint="Choose one image, preview it, then insert a reference into the terminal input." disabled={!runtimeReady || commandBusy} onPress={openAttachment} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Attach image</Text></Pressable>
          <Pressable accessibilityRole="button" accessibilityLabel={`Workspace options ${workspace.name}`} disabled={!runtimeReady || commandBusy} onPress={() => workspaceOptions(workspace)} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Workspace options</Text></Pressable>
        </View> : null}
        {screen === 'terminal' && workspace && session.groupsSupported ? <View style={[styles.terminalActions, { borderColor: homeColors.border }]}>
          <Text style={[styles.sectionLabel, { color: homeColors.muted }]}>Group</Text>
          <Button label="Create group" colors={homeColors} secondary disabled={!runtimeReady || commandBusy} onPress={() => openName({ kind: 'createGroup', workspace })}>Create group</Button>
          {group ? <View style={styles.noticeActions}>
            <Pressable accessibilityRole="button" accessibilityLabel="Rename group" disabled={!runtimeReady || commandBusy} onPress={() => openName({ kind: 'renameGroup', group })} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.accent }]}>Rename group</Text></Pressable>
            <Pressable accessibilityRole="button" accessibilityLabel="Close group" disabled={!runtimeReady || commandBusy} onPress={() => closeGroup(group)} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Close group</Text></Pressable>
          </View> : null}
        </View> : null}
        <Pressable accessibilityRole="button" accessibilityLabel="PC handoff help" onPress={() => setSheet('handoff')} style={({ pressed }) => [styles.menuRow, { borderColor: homeColors.border }, pressed && { backgroundColor: homeColors.surface }]}><Text style={[styles.actionText, { color: homeColors.text }]}>Continue on your computer</Text><Icon name="chevron" color={homeColors.muted} size={18} /></Pressable>
        {keyChangeId(connection) && keyChangeId(connection) !== removedHostKeyId ? <Pressable accessibilityRole="button" accessibilityLabel="Review key change" onPress={reviewChangedHostKey} style={styles.textAction}><Text style={[styles.actionText, { color: homeColors.danger }]}>Review host key change</Text></Pressable> : null}
      </ScrollView>}
    </NativeSheet>
  </View>;
}

export default function App() {
  const smokeBuild = SMOKE_BUILD;
  const [smokeRoute, setSmokeRoute] = useState<SmokeRoute>(null);
  const [smokeRouteRevision, setSmokeRouteRevision] = useState(0);
  const [smokeRouteResolved, setSmokeRouteResolved] = useState(!smokeBuild);

  useEffect(() => {
    // This build flag and explicit launch URL are both required. Normal
    // installed-app launches always start at the real workspace hub.
    if (!smokeBuild) return;
    recordStartupPhase('root_effect');
    recordStartupPhase('initial_url_requested');
    let launchEventReceived = false;
    let initialUrlResolved = false;
    const applyUrl = (url: string | null) => {
      const route = smokeRouteForUrl(url);
      if (!initialUrlResolved) {
        initialUrlResolved = true;
        recordStartupPhase(
          url === null
            ? 'initial_url_null'
            : route === undefined
              ? 'initial_url_other'
              : 'initial_url_allowed_fixture',
        );
      }
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
      if (!launchEventReceived && !initialUrlResolved) {
        initialUrlResolved = true;
        recordStartupPhase('initial_url_rejected');
        setSmokeRouteResolved(true);
      }
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
  groupNameRow: { flexDirection: 'row', alignItems: 'center', gap: 8, minWidth: 0 },
  groupName: { fontSize: 15, lineHeight: 22, fontWeight: '500', flexShrink: 1 },
  groupRow: { flexDirection: 'row', alignItems: 'center', borderBottomWidth: StyleSheet.hairlineWidth },
  groupChoice: { flex: 1, minHeight: 64, paddingVertical: 12, gap: 4 },
  agentLine: { flexDirection: 'row', flexWrap: 'wrap', alignItems: 'flex-start', columnGap: 12, rowGap: 4, paddingHorizontal: 20, paddingVertical: 5 },
  agentName: { flex: 1, minWidth: 0, fontSize: 12, lineHeight: 18, flexShrink: 1 },
  agentStatusIndicator: { width: 12, height: 12, flexShrink: 0, alignItems: 'center', justifyContent: 'center' },
  agentStatusCluster: { flexDirection: 'row', alignItems: 'center', columnGap: 6, flexShrink: 0 },
  agentStatusSlot: { width: 12, height: 12, alignItems: 'center', justifyContent: 'center' },
  agentStatusMark: { borderRadius: 999 },
  agentStatusCircleMark: { width: 8, height: 8 },
  agentStatusSmallMark: { width: 4, height: 4 },
  agentStatusLabel: { fontSize: 12, lineHeight: 18, flexShrink: 0 },
  horizontal: { paddingHorizontal: 24 },
  brandRow: { minHeight: 56, paddingHorizontal: 24, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', gap: 16 },
  topActions: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  brand: { fontSize: 22, letterSpacing: -.8, fontWeight: '600' },
  hero: { paddingHorizontal: 24, paddingTop: 12, paddingBottom: 16, minHeight: 128, flexDirection: 'row', alignItems: 'center', gap: 4 },
  heroCopy: { flex: 1, minWidth: 0 },
  heroTitle: { fontSize: 30, lineHeight: 38, fontWeight: '700', letterSpacing: -1 },
  heroDescription: { fontSize: 13, lineHeight: 20, marginTop: 8 },
  serverRow: { marginHorizontal: 24, marginBottom: 16, minHeight: 60, paddingVertical: 12, flexDirection: 'row', alignItems: 'center', gap: 8, borderBottomWidth: StyleSheet.hairlineWidth },
  serverName: { minWidth: 0, fontSize: 15, fontWeight: '600' },
  serverTarget: { flex: 1, minWidth: 0, minHeight: 44, flexDirection: 'row', alignItems: 'center', gap: 8 },
  serverTargetLabels: { flex: 1, minWidth: 0, gap: 1 },
  serverSessionName: { minWidth: 0, fontSize: 11, lineHeight: 16 },
  status: { flexDirection: 'row', alignItems: 'center', gap: 6, flexShrink: 1 },
  statusDot: { width: 6, height: 6, borderRadius: 3 },
  statusText: { fontSize: 11, lineHeight: 18, flexShrink: 1 },
  sectionHeader: { paddingLeft: 24, paddingRight: 16, minHeight: 48, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
  sectionLabel: { fontSize: 13, lineHeight: 20, fontVariant: ['tabular-nums'] },
  workspaceContainer: { flexDirection: 'row', alignItems: 'center', gap: 4, borderBottomWidth: StyleSheet.hairlineWidth },
  workspaceRow: { flex: 1, minWidth: 0, minHeight: 88, paddingVertical: 20, flexDirection: 'row', alignItems: 'center', gap: 14 },
  workspaceRowDisabledContent: { opacity: .5 },
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
  textAction: { minWidth: 44, minHeight: 44, justifyContent: 'center', paddingHorizontal: 4 },
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
  terminalStatusRow: { flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center', gap: 8, alignSelf: 'stretch' },
  terminalServerSession: { flex: 1, minWidth: 0 },
  terminalHost: { fontSize: 11, lineHeight: 18, flexShrink: 1 },
  paneStrip: { borderBottomWidth: StyleSheet.hairlineWidth, flexDirection: 'row', alignItems: 'center', paddingRight: 4 },
  paneTabs: { paddingHorizontal: 12, gap: 4 },
  paneTab: { minHeight: 48, paddingHorizontal: 12, borderBottomWidth: 2, flexDirection: 'row', alignItems: 'center', gap: 6 },
  paneTabText: { fontSize: 14, lineHeight: 23, maxWidth: 200, flexShrink: 1 },
  recoveryRail: { flexDirection: 'row', alignItems: 'flex-start', gap: 12, paddingHorizontal: 16, paddingVertical: 12, borderTopWidth: StyleSheet.hairlineWidth, borderBottomWidth: StyleSheet.hairlineWidth },
  recoveryRailIcon: { width: 20, height: 20, alignItems: 'center', justifyContent: 'center', marginTop: 2 },
  recoveryRailBody: { flex: 1, minWidth: 0, gap: 8 },
  recoveryRailText: { gap: 1 },
  recoveryRailTitle: { fontSize: 14, lineHeight: 20, fontWeight: '600' },
  recoveryRailDetail: { fontSize: 13, lineHeight: 20 },
  recoveryRailMeta: { fontSize: 12, lineHeight: 18 },
  recoveryRailActions: { flexDirection: 'row', flexWrap: 'wrap', gap: 8, alignItems: 'center' },
  terminalFeedback: { paddingHorizontal: 12, paddingTop: 8 },
  terminalActions: { gap: 12, paddingTop: 20, borderTopWidth: StyleSheet.hairlineWidth },
  terminalUnavailable: { flexGrow: 1, padding: 24 },
  gone: { flex: 1, justifyContent: 'center', alignItems: 'center', gap: 20, paddingVertical: 32 },
  sheetHeader: { minHeight: 64, paddingLeft: 24, paddingRight: 12, flexDirection: 'row', alignItems: 'center', gap: 12, borderBottomWidth: StyleSheet.hairlineWidth },
  sheetTitle: { flex: 1, fontSize: 18, lineHeight: 28, fontWeight: '600' },
  sheetContent: { padding: 24, gap: 20 },
  noticeBox: { padding: 14, borderRadius: 12, borderCurve: 'continuous', borderWidth: StyleSheet.hairlineWidth },
  attachmentPreview: { width: '100%', height: 260, borderRadius: 12, borderCurve: 'continuous', borderWidth: StyleSheet.hairlineWidth },
  attachmentActionSpacer: { marginTop: 12 },
  attachmentDestination: { padding: 14, borderRadius: 12, borderCurve: 'continuous', borderWidth: StyleSheet.hairlineWidth, gap: 4 },
  attachmentDestinationLine: { fontSize: 13, lineHeight: 20 },
  attachmentProgressTrack: { height: 8, borderRadius: 4, borderCurve: 'continuous', borderWidth: StyleSheet.hairlineWidth, overflow: 'hidden' },
  attachmentProgressFill: { height: '100%', borderRadius: 4 },
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
  runtimeHeader: { minHeight: 64, paddingLeft: 12, paddingRight: 12, flexDirection: 'row', alignItems: 'center', gap: 8, borderBottomWidth: StyleSheet.hairlineWidth },
  runtimeHeaderAction: { minWidth: 72, minHeight: 48, justifyContent: 'center', paddingHorizontal: 4 },
  runtimeHeaderPlaceholder: { width: 72, minHeight: 48 },
  runtimeHeaderTitle: { flex: 1, textAlign: 'center', fontSize: 18, lineHeight: 28, fontWeight: '600' },
  runtimePickerContent: { padding: 24, paddingBottom: 40, gap: 28 },
  runtimeNotification: { paddingHorizontal: 24, paddingTop: 12 },
  runtimeFormContent: { padding: 24, paddingBottom: 40, gap: 20 },
  runtimeField: { gap: 8 },
  runtimeLabel: { fontSize: 14, lineHeight: 22, fontWeight: '500' },
  runtimeInput: { minHeight: 52, borderWidth: 1, borderRadius: 12, borderCurve: 'continuous', paddingHorizontal: 12, paddingVertical: 12, fontSize: 16 },
  runtimeIntro: { gap: 10 },
  runtimeIntroTitle: { fontSize: 24, lineHeight: 34, fontWeight: '600', letterSpacing: -.5 },
  runtimeServer: { fontSize: 13, lineHeight: 22 },
  runtimeSection: { gap: 12 },
  runtimeSectionHeading: { minHeight: 32, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
  runtimeSectionTitle: { fontSize: 15, lineHeight: 24, fontWeight: '700' },
  runtimeSectionError: { padding: 16, gap: 12, borderRadius: 12, borderCurve: 'continuous' },
  runtimeRowContainer: { borderBottomWidth: StyleSheet.hairlineWidth },
  runtimeRow: { minHeight: 76, paddingVertical: 12, flexDirection: 'row', alignItems: 'center', gap: 12 },
  runtimeRowCopy: { flex: 1, minWidth: 0, gap: 3 },
  runtimeNameLine: { flexDirection: 'row', alignItems: 'center', gap: 8, flexWrap: 'wrap' },
  runtimeName: { flexShrink: 1, fontSize: 17, lineHeight: 25, fontWeight: '600' },
  runtimeState: { fontSize: 12, lineHeight: 19, fontWeight: '600' },
  runtimeHint: { fontSize: 13, lineHeight: 21 },
  runtimeError: { fontSize: 13, lineHeight: 21 },
  runtimeBadge: { paddingHorizontal: 7, paddingVertical: 2, borderWidth: 1, borderRadius: 6, fontSize: 11, lineHeight: 17, fontWeight: '600' },
  runtimeMessage: { padding: 12, fontSize: 13, lineHeight: 21, borderRadius: 10 },
});
