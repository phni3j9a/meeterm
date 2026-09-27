import { registerWebModule, NativeModule } from 'expo';
import { DEFAULT_WORKSPACE_CONTROL } from './MeetermTerminal.types';

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
  WorkspaceState,
  RuntimeBackend,
  RuntimeDiscovery,
  RuntimeBoundaryResult,
} from './MeetermTerminal.types';

const WEB_UNAVAILABLE =
  'MeetermTerminal native SSH is not available on the web platform.';

// Keep the same Promise based control-plane shape on web. A disconnected
// snapshot lets product code render its idle state without probing a native
// module that cannot exist in a browser; actions reject with a sanitized,
// deterministic error.
class MeetermTerminalModule extends NativeModule<{}> {
  recordStartupPhase(_phase: string): void {}
  async getProfiles(): Promise<ServerProfile[]> { return []; }
  async saveProfile(_profile: Omit<ServerProfile, 'credentialSaved'>, _credential: SavedCredential | null, _keepCredential: boolean): Promise<ServerProfile> { throw new Error(WEB_UNAVAILABLE); }
  async deleteProfile(_profileId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async connectHost(_terminalId: string, _options: SshConnectOptions): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async connectProfileHost(_terminalId: string, _profileId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  // The native implementation treats this legacy API as a host-only alias;
  // keep the same unavailable web surface without introducing a runtime bind.
  async connectProfile(_terminalId: string, _profileId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async getPreferences(): Promise<TerminalPreferences> { return { fontSize: 15, theme: 'system', scrollbackLines: 10000, automaticReconnect: true }; }
  async setPreferences(_preferences: TerminalPreferences): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async setForeground(_terminalId: string, _foreground: boolean): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async setAutomaticReconnect(_terminalId: string, _enabled: boolean): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async createWorkspace(_terminalId: string, _name: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async renameWorkspace(_terminalId: string, _windowId: string, _name: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async closeWorkspace(_terminalId: string, _windowId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async createPane(_terminalId: string, _windowId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async renamePane(_terminalId: string, _paneId: string, _name: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async closePane(_terminalId: string, _paneId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async refreshTerminal(_terminalId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async connect(_terminalId: string, _options: SshConnectOptions): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async disconnect(_terminalId: string): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async reconnect(_terminalId: string): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async getSessionState(_terminalId: string): Promise<TmuxSessionState> {
    return { panes: [] };
  }

  async selectPane(_terminalId: string, _paneId: string): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async getConnectionState(_terminalId: string): Promise<SshConnectionState> {
    return {
      state: 'Disconnected',
      host: '',
      port: 0,
      fingerprint: '',
      algorithm: '',
      knownFingerprint: '',
      errorCode: '',
      errorMessage: '',
    };
  }

  async getWorkspaceState(_terminalId: string): Promise<WorkspaceState> {
    return {
      backend: 'tmux',
      runtime: '',
      groupsSupported: false,
      workspaces: [],
      groups: [],
      terminals: [],
      control: {
        ...DEFAULT_WORKSPACE_CONTROL,
        recovery: { ...DEFAULT_WORKSPACE_CONTROL.recovery },
      },
    };
  }

  async retryRecovery(_terminalId: string, _operationEpoch: string): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async changeRuntime(_terminalId: string, _operationEpoch: string): Promise<RuntimeBoundaryResult> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async disconnectForSwitcher(_terminalId: string): Promise<RuntimeBoundaryResult> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async getRuntimeDiscovery(_connectionId: string): Promise<RuntimeDiscovery> { throw new Error(WEB_UNAVAILABLE); }
  async refreshRuntimes(_connectionId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async selectRuntime(_connectionId: string, _candidateId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async createTmuxSession(_connectionId: string, _name: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async setLastUsedRuntime(_profileId: string, _backend: RuntimeBackend, _runtime: string): Promise<ServerProfile> { throw new Error(WEB_UNAVAILABLE); }

  async respondToHostKey(
    _terminalId: string,
    _fingerprint: string,
    _accept: boolean,
  ): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async forgetHostKey(_host: string, _port: number): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  // Issue #28 attachments are native-only; keep the same Promise surface so
  // product code on web gets deterministic unavailable answers instead of an
  // undefined binding.
  async beginAttachment(_terminalId: string, _target: AttachmentTarget): Promise<AttachmentBeginResult> { throw new Error(WEB_UNAVAILABLE); }
  async attachmentCompositionStatus(_terminalId: string): Promise<AttachmentCompositionStatus> { return { status: 'ok' }; }
  async pickAttachmentImage(_source: AttachmentSource): Promise<AttachmentPickResult> { throw new Error(WEB_UNAVAILABLE); }
  async prepareAttachmentImage(_token: string): Promise<AttachmentPrepareResult> { throw new Error(WEB_UNAVAILABLE); }
  async discardAttachment(): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
  async getAttachmentState(): Promise<AttachmentSessionState> { throw new Error(WEB_UNAVAILABLE); }
  async uploadAttachment(_terminalId: string, _remoteDirectory: string): Promise<AttachmentActionResult> { throw new Error(WEB_UNAVAILABLE); }
  async attachmentSnapshot(): Promise<AttachmentSnapshotResult> { throw new Error(WEB_UNAVAILABLE); }
  async retryAttachmentUpload(_terminalId: string): Promise<AttachmentActionResult> { throw new Error(WEB_UNAVAILABLE); }
  async insertAttachment(_terminalId: string): Promise<AttachmentInsertResult> { throw new Error(WEB_UNAVAILABLE); }
  async cancelAttachment(): Promise<AttachmentActionResult> { throw new Error(WEB_UNAVAILABLE); }
  async deleteRemoteAttachment(_terminalId: string): Promise<AttachmentActionResult> { throw new Error(WEB_UNAVAILABLE); }
}

export default registerWebModule(MeetermTerminalModule, 'MeetermTerminalModule');
