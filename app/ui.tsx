import type { ReactNode } from 'react';
import {
  Image,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  View,
  useColorScheme,
} from 'react-native';
import type { StyleProp, ViewStyle } from 'react-native';

export const LIGHT = {
  background: '#fbf7ef',
  surface: '#f0e9de',
  elevated: '#e6dccd',
  border: '#e4dbcd',
  text: '#352b22',
  muted: '#786b5c',
  placeholder: '#7d6e5b',
  accent: '#926020',
  accentFill: '#956222',
  onAccent: '#fff9ee',
  danger: '#ab4938',
  terminal: '#fbf7ef',
};

export const DARK: typeof LIGHT = {
  background: '#24211d',
  surface: '#2e2a25',
  elevated: '#3b342c',
  border: '#453d32',
  text: '#f5eddf',
  muted: '#bfb3a3',
  placeholder: '#a79a89',
  accent: '#dbb378',
  accentFill: '#dbb378',
  onAccent: '#352719',
  danger: '#dfa79a',
  terminal: '#211f1b',
};

export type Palette = typeof LIGHT;
export const MONO = Platform.OS === 'ios' ? 'Menlo' : 'monospace';

export function usePalette(preference: 'system' | 'light' | 'dark' = 'system') {
  const system = useColorScheme();
  return (preference === 'system' ? system : preference) === 'dark' ? DARK : LIGHT;
}

type IconName = 'search' | 'server' | 'terminal' | 'back' | 'chevron' | 'down' | 'close' | 'menu' | 'check' | 'plus';

/** A single restrained line vocabulary, matching the existing mock's chrome. */
export function Icon({ name, color, size = 22 }: { name: IconName; color: string; size?: number }) {
  if (name === 'search') {
    return <View accessible={false} style={{ width: size, height: size }}>
      <View style={{ position: 'absolute', width: size * .62, height: size * .62, top: 1, left: 1, borderRadius: size, borderWidth: 1.5, borderColor: color }} />
      <View style={{ position: 'absolute', width: size * .4, height: 1.5, backgroundColor: color, left: size * .58, top: size * .76, transform: [{ rotate: '45deg' }] }} />
    </View>;
  }
  if (name === 'server') {
    return <View accessible={false} style={{ width: size, height: size, justifyContent: 'center', gap: 3 }}>
      {[0, 1].map(key => <View key={key} style={{ height: size * .31, borderWidth: 1, borderColor: color, borderRadius: 2, borderCurve: 'continuous', paddingLeft: 3, justifyContent: 'center' }}><View style={{ width: 3, height: 1, backgroundColor: color }} /></View>)}
    </View>;
  }
  const glyphs = { terminal: '›_', back: '‹', chevron: '›', down: '⌄', close: '×', menu: '…', check: '✓', plus: '+' };
  return <Text accessible={false} allowFontScaling={false} style={{ color, fontSize: name === 'terminal' ? size * .9 : size * 1.35, fontFamily: name === 'terminal' ? MONO : undefined, lineHeight: size * 1.5, textAlign: 'center', width: size + 4 }}>{glyphs[name]}</Text>;
}

export function IconButton({ icon, label, onPress, colors, disabled, style }: {
  icon: IconName;
  label: string;
  onPress: () => void;
  colors: Palette;
  disabled?: boolean;
  style?: StyleProp<ViewStyle>;
}) {
  return <Pressable accessibilityRole="button" accessibilityLabel={label} accessibilityState={{ disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [ui.iconButton, style, pressed && { backgroundColor: colors.surface }, disabled && { opacity: .45 }]}>
    <Icon name={icon} color={colors.accent} />
  </Pressable>;
}

export function Button({ children, label, onPress, colors, secondary, disabled, style }: {
  children: ReactNode;
  label?: string;
  onPress: () => void;
  colors: Palette;
  secondary?: boolean;
  disabled?: boolean;
  style?: StyleProp<ViewStyle>;
}) {
  return <Pressable accessibilityRole="button" accessibilityLabel={label} accessibilityState={{ disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [ui.button, { backgroundColor: secondary ? colors.surface : colors.accentFill }, style, pressed && { opacity: .7 }, disabled && { opacity: .45 }]}>
    <Text style={[ui.buttonText, { color: secondary ? colors.accent : colors.onAccent }]}>{children}</Text>
  </Pressable>;
}

export function Companion({ small = false, dark = false }: { small?: boolean; dark?: boolean }) {
  // The source is the original white-matte mock asset. Native layouts frame it
  // on warm paper in dark mode; no CSS-only white-removal filter is assumed.
  return <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" style={[small ? ui.smallCompanion : ui.companion, dark && ui.companionPaper]}>
    <View style={{ mixBlendMode: 'multiply' }}>
      <Image source={require('./assets/meerkat-companion.webp')} resizeMode="contain" style={small ? ui.smallCompanionImage : ui.companionImage} />
    </View>
  </View>;
}

export const ui = StyleSheet.create({
  iconButton: { width: 44, height: 44, borderRadius: 12, borderCurve: 'continuous', alignItems: 'center', justifyContent: 'center' },
  button: { minHeight: 52, paddingHorizontal: 20, paddingVertical: 14, borderRadius: 12, borderCurve: 'continuous', alignItems: 'center', justifyContent: 'center' },
  buttonText: { fontSize: 16, fontWeight: '600', lineHeight: 24, textAlign: 'center' },
  companion: { width: 148, height: 200, alignSelf: 'center', overflow: 'hidden', borderRadius: 12, borderCurve: 'continuous' },
  companionImage: { width: 220, height: 220, left: -36, top: -8 },
  smallCompanion: { width: 72, height: 108, overflow: 'hidden', borderRadius: 12, borderCurve: 'continuous' },
  smallCompanionImage: { width: 120, height: 120, left: -24, top: -4 },
  companionPaper: { backgroundColor: LIGHT.background },
});
