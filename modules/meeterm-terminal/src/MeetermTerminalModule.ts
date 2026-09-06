import { NativeModule, requireNativeModule } from 'expo';

import type {
  SshConnectOptions,
  SshConnectionState,
} from './MeetermTerminal.types';

declare class MeetermTerminalModule extends NativeModule<{}> {
  connect(terminalId: string, options: SshConnectOptions): Promise<void>;
  disconnect(terminalId: string): Promise<void>;
  getConnectionState(terminalId: string): Promise<SshConnectionState>;
  respondToHostKey(
    terminalId: string,
    fingerprint: string,
    accept: boolean,
  ): Promise<void>;
  forgetHostKey(host: string, port: number): Promise<void>;
}

export default requireNativeModule<MeetermTerminalModule>('MeetermTerminal');
