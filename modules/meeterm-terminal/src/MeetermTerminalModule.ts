import { NativeModule, requireNativeModule } from 'expo';

import type {
  AttachmentActionResult,
  AttachmentBeginResult,
  AttachmentCompositionStatus,
  AttachmentInsertResult,
  AttachmentPickResult,
  AttachmentPrepareResult,
  AttachmentSessionState,
  AttachmentSnapshotResult,
  AttachmentSource,
  AttachmentTarget,
  SavedCredential,
  ServerProfile,
  SshConnectOptions,
  SshConnectionState,
  TerminalPreferences,
  TmuxSessionState,
  RuntimeBackend,
  RuntimeDiscovery,
  RuntimeBoundaryResult,
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
  changeRuntime(terminalId: string, operationEpoch: string): Promise<RuntimeBoundaryResult>;
  disconnectForSwitcher(terminalId: string): Promise<RuntimeBoundaryResult>;
  getRuntimeDiscovery(connectionId: string): Promise<RuntimeDiscovery>;
  refreshRuntimes(connectionId: string): Promise<void>;
  selectRuntime(connectionId: string, candidateId: string): Promise<void>;
  createTmuxSession(connectionId: string, name: string): Promise<void>;
  setLastUsedRuntime(profileId: string, backend: RuntimeBackend, runtime: string): Promise<ServerProfile>;
  respondToHostKey(
    terminalId: string,
    fingerprint: string,
    accept: boolean,
  ): Promise<void>;
  forgetHostKey(host: string, port: number): Promise<void>;
  /**
   * Start one attachment session against the captured remote target. Held
   * while native input composition is active; it never commits or clears it.
   */
  beginAttachment(terminalId: string, target: AttachmentTarget): Promise<AttachmentBeginResult>;
  /**
   * Explicit main-thread query for live IME composition on a terminal.
   * Called before the sheet opens so `held` can refuse `Keyboard.dismiss()`
   * without touching the composition.
   */
  attachmentCompositionStatus(terminalId: string): Promise<AttachmentCompositionStatus>;
  /** OS picker; the chosen image is stream-copied into app-owned staging. */
  pickAttachmentImage(source: AttachmentSource): Promise<AttachmentPickResult>;
  /** Validate, orient, strip metadata, and re-encode the staged image. */
  prepareAttachmentImage(token: string): Promise<AttachmentPrepareResult>;
  /**
   * Cancel any live core operation, dispose its record, and remove the
   * session's local staging/prepared files. Explicit Discard action.
   */
  discardAttachment(): Promise<void>;
  /** Low-frequency session snapshot used to rebind state after remounts. */
  getAttachmentState(): Promise<AttachmentSessionState>;
  /**
   * Explicit Upload of the normalized image over the fenced SSH connection
   * (`meeterm_attachment_begin`). `remoteDirectory` is an absolute or `~/`
   * path; empty means the core's `~/.local/share/meeterm/attachments` default.
   */
  uploadAttachment(terminalId: string, remoteDirectory: string): Promise<AttachmentActionResult>;
  /** Poll the live core operation (`meeterm_attachment_snapshot`). */
  attachmentSnapshot(): Promise<AttachmentSnapshotResult>;
  /** Explicit transfer retry on a pending/failed operation. */
  retryAttachmentUpload(terminalId: string): Promise<AttachmentActionResult>;
  /**
   * IME-safe attachment insertion entry. When a native composition exists it
   * returns `held` without touching it; otherwise the request goes to
   * `meeterm_attachment_insert` for the fenced destination only.
   */
  insertAttachment(terminalId: string): Promise<AttachmentInsertResult>;
  /** Cancel a pending/uploading operation (`meeterm_attachment_cancel`). */
  cancelAttachment(): Promise<AttachmentActionResult>;
  /**
   * Explicit server-side delete of the completed remote file, validated by
   * the core (`meeterm_attachment_delete_remote`).
   */
  deleteRemoteAttachment(terminalId: string): Promise<AttachmentActionResult>;
}

export default requireNativeModule<MeetermTerminalModule>('MeetermTerminal');
