import { NativeModule, requireNativeModule } from 'expo';

import type {
  SavedCredential,
  ServerProfile,
  SshConnectOptions,
  SshConnectionState,
  TerminalPreferences,
  TmuxSessionState,
} from './MeetermTerminal.types';

declare class MeetermTerminalModule extends NativeModule<{}> {
  getProfiles(): Promise<ServerProfile[]>;
  saveProfile(profile: Omit<ServerProfile, 'credentialSaved'>, credential: SavedCredential | null, keepCredential: boolean): Promise<ServerProfile>;
  deleteProfile(profileId: string): Promise<void>;
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
  connect(terminalId: string, options: SshConnectOptions): Promise<void>;
  disconnect(terminalId: string): Promise<void>;
  reconnect(terminalId: string): Promise<void>;
  getSessionState(terminalId: string): Promise<TmuxSessionState>;
  selectPane(terminalId: string, paneId: string): Promise<void>;
  getConnectionState(terminalId: string): Promise<SshConnectionState>;
  respondToHostKey(
    terminalId: string,
    fingerprint: string,
    accept: boolean,
  ): Promise<void>;
  forgetHostKey(host: string, port: number): Promise<void>;
}

export default requireNativeModule<MeetermTerminalModule>('MeetermTerminal');
