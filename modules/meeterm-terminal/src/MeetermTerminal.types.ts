import type {
  NativeSyntheticEvent,
  ViewProps,
} from 'react-native';

/**
 * Authentication uses an OpenSSH private key. Rust retains the parsed key in
 * process memory for reconnect; credentials are never persisted by this module.
 */
export type SshConnectOptions = {
  host: string;
  port: number;
  username: string;
  privateKey: string;
  passphrase: string;
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
