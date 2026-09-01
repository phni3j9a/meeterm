import { requireNativeView } from 'expo';
import * as React from 'react';

import { MeetermTerminalViewProps } from './MeetermTerminal.types';

const NativeView: React.ComponentType<MeetermTerminalViewProps> = requireNativeView<MeetermTerminalViewProps>('MeetermTerminal');

export default function MeetermTerminalView(props: MeetermTerminalViewProps) {
  return <NativeView {...props} />;
}
