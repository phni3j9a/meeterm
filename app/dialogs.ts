import { Alert, Platform } from 'react-native';

import MeetermTerminal from '../modules/meeterm-terminal';
import type { ThemePreference } from '../modules/meeterm-terminal';

export type AppDialogButton = {
  text: string;
  style?: 'default' | 'cancel' | 'destructive';
  onPress?: () => void;
};

/** Per-dialog appearance for iOS sheets; `system` leaves the OS setting. */
export function sheetUserInterfaceStyle(appearance: ThemePreference): 'light' | 'dark' | undefined {
  return appearance === 'system' ? undefined : appearance;
}

/**
 * Present an app-owned alert under the currently applied App appearance.
 * Android delegates to the native `presentAppAlert` presenter, which themes
 * only the dialog context; iOS passes `userInterfaceStyle` per call. A
 * button resolves its original JS index exactly once; back/outside
 * dismissal, an out-of-range index, and any presentation failure invoke no
 * handler, so destructive and trust actions stay fail-closed. `cancelable`
 * keeps the previous RN Alert default of false; only callers that already
 * requested an explicit value may change it.
 */
export function appAlert(
  appearance: ThemePreference,
  title: string,
  message: string | undefined,
  buttons: AppDialogButton[],
  options: { cancelable?: boolean } = {},
): void {
  if (Platform.OS === 'android') {
    let dispatched = false;
    const dispatch = (index: number | null) => {
      if (dispatched) return;
      dispatched = true;
      if (index !== null) buttons[index]?.onPress?.();
    };
    try {
      void MeetermTerminal.presentAppAlert({
        appearance,
        title,
        message,
        cancelable: options.cancelable ?? false,
        buttons: buttons.map(({ text, style }) => ({ text, style: style ?? 'default' })),
      }).then(dispatch, () => dispatch(null));
    } catch {
      dispatch(null);
    }
    return;
  }
  Alert.alert(title, message, buttons, {
    cancelable: options.cancelable ?? false,
    userInterfaceStyle: appearance === 'system' ? 'unspecified' : appearance,
  });
}
