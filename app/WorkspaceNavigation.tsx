import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { DefaultTheme, NavigationContainer, StackActions, useNavigationContainerRef } from '@react-navigation/native';
import { createNativeStackNavigator } from '@react-navigation/native-stack';
import { DARK, useReducedMotion } from './ui';
import type { Palette } from './ui';

type Routes = { workspaces: undefined; terminal: undefined; sessionSwitcher: undefined };
type Destination = 'workspaces' | 'terminal';
const Stack = createNativeStackNavigator<Routes>();

/** Two native screens; the App still owns selection and the remote workspace.
 * Switching panes/workspaces in the terminal never pushes another route.
 */
export function WorkspaceNavigation({ screen, onScreenChange, colors, workspaces, terminal, sessionSwitcherOpen, sessionSwitcher, sessionSwitcherBusy, onSessionSwitcherDismiss, onSessionSwitcherClosed }: {
  screen: Destination;
  onScreenChange: (screen: Destination) => void;
  colors: Palette;
  workspaces: ReactNode;
  terminal: ReactNode;
  sessionSwitcherOpen: boolean;
  sessionSwitcher: ReactNode;
  sessionSwitcherBusy: boolean;
  onSessionSwitcherDismiss: () => void;
  onSessionSwitcherClosed: () => void;
}) {
  const navigation = useNavigationContainerRef<Routes>();
  const [ready, setReady] = useState(false);
  const actual = useRef(screen);
  const currentRoute = useRef<Destination | 'sessionSwitcher'>(screen);
  const reduceMotion = useReducedMotion();
  const initialState = useRef({
    index: screen === 'terminal' ? 1 : 0,
    routes: screen === 'terminal' ? [{ name: 'workspaces' }, { name: 'terminal' }] : [{ name: 'workspaces' }],
  });

  useEffect(() => {
    if (!ready || sessionSwitcherOpen || actual.current === screen) return;
    if (screen === 'terminal') navigation.navigate('terminal');
    else navigation.dispatch(StackActions.popToTop());
  }, [navigation, ready, screen, sessionSwitcherOpen]);

  useEffect(() => {
    if (!ready) return;
    const route = navigation.getCurrentRoute()?.name;
    if (sessionSwitcherOpen && route !== 'sessionSwitcher') {
      navigation.navigate('sessionSwitcher');
    } else if (!sessionSwitcherOpen && route === 'sessionSwitcher') {
      navigation.goBack();
    }
  }, [navigation, ready, sessionSwitcherOpen]);

  return <NavigationContainer ref={navigation} initialState={initialState.current}
    theme={{ ...DefaultTheme, dark: colors === DARK, colors: { ...DefaultTheme.colors, background: colors.background, card: colors.background, text: colors.text, border: colors.border, primary: colors.accent } }}
    onReady={() => setReady(true)}
    onStateChange={() => {
      const next = navigation.getCurrentRoute()?.name;
      if (next === 'sessionSwitcher') {
        currentRoute.current = next;
        return;
      }
      if ((next === 'workspaces' || next === 'terminal') && currentRoute.current === 'sessionSwitcher') {
        currentRoute.current = next;
        onSessionSwitcherDismiss();
        // A successful runtime switch can change the App's destination while
        // this route is open. Do not report the still-mounted old base route
        // back to App and overwrite that destination during dismissal.
        if (next !== screen) {
          if (screen === 'terminal') navigation.navigate('terminal');
          else navigation.dispatch(StackActions.popToTop());
          return;
        }
      } else if (next === 'workspaces' || next === 'terminal') {
        currentRoute.current = next;
      }
      if (next === 'workspaces' || next === 'terminal') {
        actual.current = next;
        onScreenChange(next);
      }
    }}>
    <Stack.Navigator screenOptions={{ headerShown: false, animation: reduceMotion ? 'fade' : 'default', gestureEnabled: true }}>
      <Stack.Screen name="workspaces" options={{ contentStyle: { backgroundColor: colors.background }, statusBarStyle: colors === DARK ? 'light' : 'dark' }}>{() => workspaces}</Stack.Screen>
      <Stack.Screen name="terminal" options={{ contentStyle: { backgroundColor: DARK.background }, statusBarStyle: 'light' }}>{() => terminal}</Stack.Screen>
      <Stack.Screen name="sessionSwitcher" options={{
        presentation: 'formSheet',
        sheetAllowedDetents: [0.84],
        sheetInitialDetentIndex: 0,
        sheetGrabberVisible: true,
        sheetCornerRadius: 20,
        sheetExpandsWhenScrolledToEdge: false,
        contentStyle: { backgroundColor: colors.background },
        statusBarStyle: colors === DARK ? 'light' : 'dark',
        gestureEnabled: !sessionSwitcherBusy,
      }} listeners={{
        transitionEnd: event => { if (event.data.closing) onSessionSwitcherClosed(); },
      }}>{() => sessionSwitcher}</Stack.Screen>
    </Stack.Navigator>
  </NavigationContainer>;
}
