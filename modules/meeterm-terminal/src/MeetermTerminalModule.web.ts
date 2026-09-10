import { registerWebModule, NativeModule } from 'expo';

import type {
  SavedCredential,
  ServerProfile,
  SshConnectOptions,
  SshConnectionState,
  TerminalPreferences,
  TmuxSessionState,
} from './MeetermTerminal.types';

const WEB_UNAVAILABLE =
  'MeetermTerminal native SSH is not available on the web platform.';

// Keep the same Promise based control-plane shape on web. A disconnected
// snapshot lets product code render its idle state without probing a native
// module that cannot exist in a browser; actions reject with a sanitized,
// deterministic error.
class MeetermTerminalModule extends NativeModule<{}> {
  async getProfiles(): Promise<ServerProfile[]> { return []; }
  async saveProfile(_profile: Omit<ServerProfile, 'credentialSaved'>, _credential: SavedCredential | null, _keepCredential: boolean): Promise<ServerProfile> { throw new Error(WEB_UNAVAILABLE); }
  async deleteProfile(_profileId: string): Promise<void> { throw new Error(WEB_UNAVAILABLE); }
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
}

export default registerWebModule(MeetermTerminalModule, 'MeetermTerminalModule');
