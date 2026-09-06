import type {
  NativeSyntheticEvent,
  ViewProps,
} from 'react-native';

/**
 * The first SSH slice deliberately accepts only an OpenSSH private key. The
 * key and passphrase are consumed by native code for one connection request;
 * they are never stored by this module.
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
