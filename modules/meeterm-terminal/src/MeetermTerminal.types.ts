import type {
  NativeSyntheticEvent,
  ViewProps,
} from 'react-native';

/** Common endpoint fields shared by both supported SSH authentication methods. */
type SshConnectEndpoint = {
  host: string;
  port: number;
  username: string;
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

export type SshConnectionPhase =
  | 'Disconnected'
  | 'Connecting'
  | 'HostKeyPending'
  | 'Authenticating'
  | 'OpeningPty'
  | 'AttachingTmux'
  | 'Synchronizing'
  | 'Reconnecting'
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
export type RemoteWorkspace = { id: string; name: string };
export type TerminalGroup = { id: string; workspaceId: string; name: string; selected: boolean };
export type AgentInfo = { name: string; status: 'working' | 'blocked' | 'done' | 'idle' | 'unknown' };
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
export type WorkspaceState = {
  backend: 'tmux' | 'herdr';
  runtime: string;
  groupsSupported: boolean;
  workspaces: RemoteWorkspace[];
  groups: TerminalGroup[];
  terminals: RemoteTerminal[];
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
  onNativeReady?: (event: NativeSyntheticEvent<NativeReadyEvent>) => void;
  onMetrics?: (event: NativeSyntheticEvent<TerminalMetricsEvent>) => void;
};
