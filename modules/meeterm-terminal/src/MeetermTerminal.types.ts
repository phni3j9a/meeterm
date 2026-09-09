import type {
  NativeSyntheticEvent,
  ViewProps,
} from 'react-native';

/** Common endpoint fields shared by both supported SSH authentication methods. */
type SshConnectEndpoint = {
  host: string;
  port: number;
  username: string;
};

/**
 * Authentication credentials are submitted transiently. Rust retains only
 * the parsed credential needed for an in-process reconnect; this module never
 * persists either form.
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
  /** Active pane within this pane's window, including non-selected windows. */
  active: boolean;
  selected: boolean;
};

export type TmuxSessionState = {
  panes: TmuxPane[];
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
  onNativeReady?: (event: NativeSyntheticEvent<NativeReadyEvent>) => void;
  onMetrics?: (event: NativeSyntheticEvent<TerminalMetricsEvent>) => void;
};
