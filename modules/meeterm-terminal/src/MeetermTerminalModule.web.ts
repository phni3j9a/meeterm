import { registerWebModule, NativeModule } from 'expo';

// MeetermTerminalModule is not available on the web platform.
class MeetermTerminalModule extends NativeModule<{}> {}

export default registerWebModule(MeetermTerminalModule, 'MeetermTerminalModule');
