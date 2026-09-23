import { NativeModule, requireNativeModule } from 'expo';

import type {
  SavedCredential,
  ServerProfile,
  SshConnectOptions,
  SshConnectionState,
  TerminalPreferences,
  TmuxSessionState,
  RuntimeBackend,
  RuntimeDiscovery,
  RuntimeBrowseState,
  RuntimeBrowseCommitTarget,
  WorkspaceState,
} from './MeetermTerminal.types';

declare class MeetermTerminalModule extends NativeModule<{}> {
  recordStartupPhase(phase: string): void;
  getProfiles(): Promise<ServerProfile[]>;
  saveProfile(profile: Omit<ServerProfile, 'credentialSaved'>, credential: SavedCredential | null, keepCredential: boolean): Promise<ServerProfile>;
  deleteProfile(profileId: string): Promise<void>;
  /** Host-only authentication. Runtime binding happens after discovery. */
  connectHost(terminalId: string, options: SshConnectOptions): Promise<void>;
  /** Host-only saved-profile path; legacy hint fields are ignored for attach. */
  connectProfileHost(terminalId: string, profileId: string): Promise<void>;
  /** Legacy alias for connectProfileHost; it never selects a runtime. */
  connectProfile(terminalId: string, profileId: string): Promise<void>;
  getPreferences(): Promise<TerminalPreferences>;
  setPreferences(preferences: TerminalPreferences): Promise<void>;
  setForeground(terminalId: string, foreground: boolean): Promise<void>;
  setAutomaticReconnect(terminalId: string, enabled: boolean): Promise<void>;
  createWorkspace(terminalId: string, name: string): Promise<void>;
  renameWorkspace(terminalId: string, windowId: string, name: string): Promise<void>;
  closeWorkspace(terminalId: string, windowId: string): Promise<void>;
  createPane(terminalId: string, windowId: string): Promise<void>;
  renamePane(terminalId: string, paneId: string, name: string): Promise<void>;
  closePane(terminalId: string, paneId: string): Promise<void>;
  refreshTerminal(terminalId: string): Promise<void>;
  /** Legacy alias for connectHost; backend/runtime hints are ignored. */
  connect(terminalId: string, options: SshConnectOptions): Promise<void>;
  disconnect(terminalId: string): Promise<void>;
  reconnect(terminalId: string): Promise<void>;
  getSessionState(terminalId: string): Promise<TmuxSessionState>;
  getWorkspaceState(terminalId: string): Promise<WorkspaceState>;
  createGroup(terminalId: string, workspaceId: string, name: string): Promise<void>;
  renameGroup(terminalId: string, groupId: string, name: string): Promise<void>;
  closeGroup(terminalId: string, groupId: string): Promise<void>;
  selectGroup(terminalId: string, groupId: string): Promise<void>;
  setTerminalVisible(terminalId: string, visible: boolean): Promise<void>;
  selectPane(terminalId: string, paneId: string): Promise<void>;
  getConnectionState(terminalId: string): Promise<SshConnectionState>;
  retryRecovery(terminalId: string, operationEpoch: string): Promise<void>;
  confirmRecovery(terminalId: string, confirmationToken: string): Promise<void>;
  changeRuntime(terminalId: string, operationEpoch: string): Promise<void>;
  getRuntimeDiscovery(connectionId: string): Promise<RuntimeDiscovery>;
  refreshRuntimes(connectionId: string): Promise<void>;
  selectRuntime(connectionId: string, candidateId: string): Promise<void>;
  createTmuxSession(connectionId: string, name: string): Promise<void>;
  runtimeBrowseStartCurrent(terminalId: string): Promise<RuntimeBrowseState>;
  runtimeBrowseStartProfile(terminalId: string, profileId: string): Promise<RuntimeBrowseState>;
  runtimeBrowseStartCredential(terminalId: string, options: SshConnectOptions): Promise<RuntimeBrowseState>;
  runtimeBrowseState(token: string): Promise<RuntimeBrowseState>;
  runtimeBrowseRefresh(token: string): Promise<void>;
  runtimeBrowseCancel(token: string): Promise<void>;
  runtimeBrowseRespondToHostKey(token: string, fingerprint: string, accept: boolean): Promise<void>;
  /** Poll for `committed` or native-confirmed `unchanged` after starting a commit. */
  runtimeBrowseCommit(
    token: string,
    browseGeneration: string,
    discoveryRevision: number,
    target: RuntimeBrowseCommitTarget,
  ): Promise<void>;
  setLastUsedRuntime(profileId: string, backend: RuntimeBackend, runtime: string): Promise<ServerProfile>;
  respondToHostKey(
    terminalId: string,
    fingerprint: string,
    accept: boolean,
  ): Promise<void>;
  forgetHostKey(host: string, port: number): Promise<void>;
}

export default requireNativeModule<MeetermTerminalModule>('MeetermTerminal');
