import type {
  NativeSyntheticEvent,
  ViewProps,
} from 'react-native';

/** Common endpoint fields shared by both supported SSH authentication methods. */
type SshConnectEndpoint = {
  host: string;
  port: number;
  username: string;
  /** Compatibility-only hint fields; every fresh connect path ignores them. */
  backend?: 'tmux' | 'herdr';
  runtime?: string;
};

/**
 * Authentication credentials are submitted transiently. Rust retains only
 * the parsed credential needed for an in-process reconnect. Optional saved
 * credentials use the separate native secure-storage API and are never read
 * back into JavaScript.
 *
 * `authMethod` is optional on the public-key branch for compatibility with
 * callers that predate password authentication.
 */
export type SshConnectOptions =
  | (SshConnectEndpoint & {
      authMethod?: 'publicKey';
      privateKey: string;
      passphrase: string;
    })
  | (SshConnectEndpoint & {
      authMethod: 'password';
      password: string;
    });

/** Non-secret local metadata. Remote tmux remains the workspace authority. */
export type ServerProfile = {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authMethod: 'publicKey' | 'password';
  credentialSaved: boolean;
  /** Legacy persisted keys, now only a non-authoritative last-used hint. */
  backend?: 'tmux' | 'herdr';
  runtime?: string;
};

/** Write-only secure-storage request; no API returns this shape. */
export type SavedCredential =
  | { authMethod: 'publicKey'; privateKey: string; passphrase: string }
  | { authMethod: 'password'; password: string };

export type TerminalPreferences = {
  fontSize: number;
  theme: 'system' | 'light' | 'dark';
  scrollbackLines: number;
  automaticReconnect: boolean;
};

/** Result of a synchronous request that may retire the selected runtime. */
export type RuntimeBoundaryResult =
  | { status: 'not_invoked'; errorCode: string }
  | { status: 'rejected_before_boundary'; errorCode: string }
  | { status: 'accepted' }
  | { status: 'accepted_after_failure'; errorCode: 'boundary_accepted_failure' };

export type SshConnectionPhase =
  | 'Disconnected'
  | 'Connecting'
  | 'HostKeyPending'
  | 'Authenticating'
  | 'OpeningPty'
  | 'AttachingTmux'
  | 'Synchronizing'
  | 'Reconnecting'
  | 'DiscoveringRuntimes'
  | 'AwaitingRuntimeSelection'
  | 'AttachingRuntime'
  | 'CreatingRuntime'
  | 'Ready'
  | 'Closing'
  | 'Failed';

/** Low-frequency, sanitized connection state. Terminal bytes stay native. */
export type SshConnectionState = {
  state: SshConnectionPhase;
  host: string;
  port: number;
  fingerprint: string;
  algorithm: string;
  knownFingerprint: string;
  errorCode: string;
  errorMessage: string;
};

/** Remote identities and labels only. Screen contents never cross this API. */
export type TmuxPane = {
  windowId: string;
  paneId: string;
  terminalId: string;
  windowName: string;
  paneName: string;
  /** Active pane within this pane's window, including non-selected windows. */
  active: boolean;
  selected: boolean;
};

export type TmuxSessionState = {
  panes: TmuxPane[];
};

/** Opaque IDs are resolved within the owning connection/backend/runtime. */
export type AgentStatus = 'blocked' | 'done' | 'working' | 'idle' | 'unknown';
export type RemoteWorkspace = { id: string; name: string; agentStatus: AgentStatus | null };
export type TerminalGroup = { id: string; workspaceId: string; name: string; selected: boolean; agentStatus: AgentStatus | null };
export type AgentInfo = { name: string; status: AgentStatus };
export type RemoteTerminal = {
  id: string;
  workspaceId: string;
  groupId: string;
  /** Borrowed native registry view binding, formatted by Rust as native:<id>. */
  terminalId: string;
  name: string;
  active: boolean;
  selected: boolean;
  agent: AgentInfo | null;
};

export type RecoveryPhase =
  | 'none'
  | 'reconnecting'
  | 'resynchronizing'
  | 'stopped';

export type WorkspaceRecovery = {
  phase: RecoveryPhase;
  /** Native-owned stable snake_case reason code; never display this raw. */
  reason: string;
  attempt: number;
  maxAttempts: number;
};

export type WorkspaceCleanupWarning = {
  /** Native-issued decimal ID; never convert this value to Number. */
  id: string;
  code: 'layout_restore_unconfirmed';
  message: string;
};

export type WorkspaceControl = {
  /** Decimal u64 string scoped to the owning native connection. */
  operationEpoch: string;
  /** True when the cached workspace/terminal surface is still drawable. */
  hasRetainedWork: boolean;
  /** Native gate for workspace/group/pane remote operations. */
  runtimeOperationsReady: boolean;
  /** Native gate for terminal input, IME, paste, and shortcuts. */
  terminalInputReady: boolean;
  recovery: WorkspaceRecovery;
  /** Latest independent old-connection layout result, if any. */
  cleanupWarning: WorkspaceCleanupWarning | null;
};

/**
 * Safe compatibility value for snapshots produced before Issue #26. The
 * false gates are intentional: JavaScript must never turn an old snapshot
 * into an apparently live remote session.
 */
export const DEFAULT_WORKSPACE_CONTROL: WorkspaceControl = {
  operationEpoch: '',
  hasRetainedWork: false,
  runtimeOperationsReady: false,
  terminalInputReady: false,
  recovery: {
    phase: 'none',
    reason: '',
    attempt: 0,
    maxAttempts: 0,
  },
  cleanupWarning: null,
};

const RECOVERY_PHASES: readonly RecoveryPhase[] = [
  'none',
  'reconnecting',
  'resynchronizing',
  'stopped',
];

function boundedNonNegativeInteger(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? Math.min(Math.floor(value), 0xffff)
    : fallback;
}

/** Normalize optional/legacy native data without granting any readiness. */
export function normalizeWorkspaceControl(value: unknown): WorkspaceControl {
  if (!value || typeof value !== 'object') {
    return {
      ...DEFAULT_WORKSPACE_CONTROL,
      recovery: { ...DEFAULT_WORKSPACE_CONTROL.recovery },
    };
  }
  const source = value as Record<string, unknown>;
  const rawRecovery = source.recovery && typeof source.recovery === 'object'
    ? source.recovery as Record<string, unknown>
    : {};
  const phase = RECOVERY_PHASES.includes(rawRecovery.phase as RecoveryPhase)
    ? rawRecovery.phase as RecoveryPhase
    : 'none';
  const stringValue = (candidate: unknown): string => (
    typeof candidate === 'string' ? candidate.slice(0, 256) : ''
  );
  const rawCleanupWarning = source.cleanupWarning && typeof source.cleanupWarning === 'object'
    ? source.cleanupWarning as Record<string, unknown>
    : null;
  const cleanupWarning = rawCleanupWarning
    && typeof rawCleanupWarning.id === 'string'
    && /^[0-9]+$/.test(rawCleanupWarning.id)
    && rawCleanupWarning.id.length <= 20
    && rawCleanupWarning.code === 'layout_restore_unconfirmed'
    && typeof rawCleanupWarning.message === 'string'
    && rawCleanupWarning.message.length > 0
    ? {
      id: rawCleanupWarning.id,
      code: 'layout_restore_unconfirmed' as const,
      message: stringValue(rawCleanupWarning.message),
    }
    : null;
  return {
    operationEpoch: stringValue(source.operationEpoch),
    hasRetainedWork: source.hasRetainedWork === true,
    runtimeOperationsReady: source.runtimeOperationsReady === true,
    terminalInputReady: source.terminalInputReady === true,
    recovery: {
      phase,
      reason: stringValue(rawRecovery.reason),
      attempt: boundedNonNegativeInteger(rawRecovery.attempt, 0),
      maxAttempts: boundedNonNegativeInteger(rawRecovery.maxAttempts, 0),
    },
    cleanupWarning,
  };
}

export type WorkspaceState = {
  backend: 'tmux' | 'herdr';
  runtime: string;
  groupsSupported: boolean;
  workspaces: RemoteWorkspace[];
  groups: TerminalGroup[];
  terminals: RemoteTerminal[];
  control: WorkspaceControl;
};

export type RuntimeBackend = 'tmux' | 'herdr';
export type RuntimeCandidateState = 'running' | 'stopped';

/** Native-owned opaque runtime identity and display-only state. */
export type RuntimeCandidate = {
  id: string;
  backend: RuntimeBackend;
  name: string;
  state: RuntimeCandidateState;
  /** Native says whether this identity can currently be selected. */
  selectable: boolean;
  isDefault: boolean;
  /** A last-used hint is never an instruction to attach or create. */
  lastUsed: boolean;
  /** Sanitized native-local failure for this candidate, if any. */
  errorCode: string;
  errorMessage: string;
};

export type RuntimeBackendDiscovery = {
  backend: RuntimeBackend;
  state: 'loading' | 'ready' | 'error';
  errorCode: string;
  errorMessage: string;
  candidates: RuntimeCandidate[];
  /** True only for the ordinary tmux backend. */
  canCreate: boolean;
};

/** Bounded, low-frequency runtime metadata; terminal data stays native. */
export type RuntimeDiscovery = {
  /** Decimal u64 identifying the native SSH connection lifecycle. */
  connectionGeneration: string;
  revision: number;
  backends: RuntimeBackendDiscovery[];
};

/** Attachment entry points offered to the native picker adapter. */
export type AttachmentSource = 'photos' | 'files';

/** Re-encoded formats accepted by the first attachment milestone. */
export type AttachmentImageFormat = 'png' | 'jpeg';

/**
 * Opaque remote-target identity captured when an attachment starts. Every
 * field is display/metadata only; the native side revalidates its own
 * terminal identity and operation epoch before any remote operation.
 */
export type AttachmentTarget = {
  terminalId: string;
  paneId: string;
  workspaceId: string;
  backend: RuntimeBackend;
  runtime: string;
  host: string;
  port: number;
};

/** Attachment open result. `held` means native input composition is active. */
export type AttachmentBeginResult =
  | { status: 'ready' }
  | { status: 'held'; reason: 'composing' };

export type AttachmentPickResult =
  | { status: 'picked'; token: string; byteCount: number }
  | { status: 'canceled' }
  | { status: 'error'; errorCode: string; message: string };

export type AttachmentPrepareResult =
  | {
      status: 'prepared';
      fileId: string;
      previewUri: string;
      format: AttachmentImageFormat;
      width: number;
      height: number;
      byteCount: number;
      sourceByteCount: number;
    }
  | { status: 'error'; errorCode: string; message: string };

/**
 * Rust-owned attachment operation phase (attachment-ffi contract). `pending`
 * means the op is blocked and `errorCode` carries the pending reason;
 * `inserted` only proves the native input queue accepted the path line —
 * never that a CLI or model consumed it.
 */
export type AttachmentCorePhase =
  | 'pending'
  | 'uploading'
  | 'uploaded'
  | 'inserted'
  | 'failed'
  | 'cancelled'
  | 'deleted';

/** One complete core snapshot, decoded from the fixed-size C record. */
export type AttachmentOperationSnapshot = {
  phase: AttachmentCorePhase;
  /** Decimal u64; never convert to Number. */
  attachmentId: string;
  bytesUploaded: number;
  sizeBytes: number;
  remotePath: string;
  displayName: string;
  errorCode: string;
  errorMessage: string;
  /** flags & 0x1: the input path accepted the line, delivery unconfirmed. */
  insertUnconfirmed: boolean;
};

/** Uniform answer for upload/retry/cancel/delete/dispose requests. */
export type AttachmentActionResult =
  | { status: 'accepted'; attachmentId: string }
  | { status: 'unavailable'; reason: string }
  | { status: 'error'; errorCode: string; message: string };

/** Snapshot poll answer; `idle` means no operation is live. */
export type AttachmentSnapshotResult =
  | { status: 'snapshot'; operation: AttachmentOperationSnapshot }
  | { status: 'idle' }
  | { status: 'unavailable'; reason: string }
  | { status: 'error'; errorCode: string; message: string };

/** Insertion answer; `held` keeps the composition and reports a reason. */
export type AttachmentInsertResult =
  | { status: 'inserted' }
  | { status: 'held'; reason: 'composing' | 'no_attachment' }
  | { status: 'unavailable'; reason: string }
  | { status: 'error'; errorCode: string; message: string };

/** Low-frequency native attachment session snapshot for remount recovery. */
export type AttachmentSessionState = {
  status: 'idle' | 'staged' | 'prepared';
  fileId: string;
  previewUri: string;
  format: AttachmentImageFormat;
  width: number;
  height: number;
  byteCount: number;
  sourceByteCount: number;
  /** Captured destination binding; needed to restore honestly after remount. */
  target: AttachmentTarget | null;
  /** Last-known core operation, present only while an op is live. */
  operation: AttachmentOperationSnapshot | null;
  errorCode: string;
  message: string;
};

export type NativeReadyEvent = {
  terminalId: string;
  native: true;
};

export type TerminalMetricsEvent = {
  terminalId: string;
  columns: number;
  rows: number;
  cellWidthPx: number;
  cellHeightPx: number;
};

/** Low-frequency control-plane props/events only; terminal data stays native. */
export type MeetermTerminalViewProps = ViewProps & {
  terminalId?: string;
  fontSize?: number;
  theme?: 'light' | 'dark';
  scrollbackLines?: number;
  /** Live native input or a drawable but input-inert retained snapshot. */
  interactionMode?: 'live' | 'cachedReadOnly';
  onNativeReady?: (event: NativeSyntheticEvent<NativeReadyEvent>) => void;
  onMetrics?: (event: NativeSyntheticEvent<TerminalMetricsEvent>) => void;
};
