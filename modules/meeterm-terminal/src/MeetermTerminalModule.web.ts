import { registerWebModule, NativeModule } from 'expo';

import type {
  SshConnectOptions,
  SshConnectionState,
} from './MeetermTerminal.types';

const WEB_UNAVAILABLE =
  'MeetermTerminal native SSH is not available on the web platform.';

// Keep the same Promise based control-plane shape on web. A disconnected
// snapshot lets product code render its idle state without probing a native
// module that cannot exist in a browser; actions reject with a sanitized,
// deterministic error.
class MeetermTerminalModule extends NativeModule<{}> {
  async connect(_terminalId: string, _options: SshConnectOptions): Promise<void> {
    throw new Error(WEB_UNAVAILABLE);
  }

  async disconnect(_terminalId: string): Promise<void> {
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
