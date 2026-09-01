import { NativeModule, requireNativeModule } from 'expo';

declare class MeetermTerminalModule extends NativeModule<{}> {}

export default requireNativeModule<MeetermTerminalModule>('MeetermTerminal');
