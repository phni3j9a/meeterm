import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import {
  AccessibilityInfo,
  Image,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  View,
  useColorScheme,
} from 'react-native';
import type { StyleProp, ViewStyle } from 'react-native';
import { Search, Server, Terminal, ChevronLeft, ChevronRight, ChevronDown, X, Ellipsis, Check, Plus, Settings2, Layers, Monitor, KeyRound, ArrowRight } from 'lucide-react-native';

export const LIGHT = {
  background: '#FAF8F4',
  surface: '#F1EDE6',
  elevated: '#FFFFFF',
  border: '#E3DED5',
  text: '#302B25',
  muted: '#73695C',
  placeholder: '#746A5D',
  accent: '#8B5E30',
  accentFill: '#85592E',
  onAccent: '#FFFFFF',
  danger: '#AD4437',
  terminal: '#211f1b',
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

export function useReducedMotion() {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    let mounted = true;
    void AccessibilityInfo.isReduceMotionEnabled().then(value => { if (mounted) setReduced(value); }).catch(() => {});
    const listener = AccessibilityInfo.addEventListener('reduceMotionChanged', setReduced);
    return () => { mounted = false; listener.remove(); };
  }, []);
  return reduced;
}

export function usePalette(preference: 'system' | 'light' | 'dark' = 'system') {
  const system = useColorScheme();
  return (preference === 'system' ? system : preference) === 'dark' ? DARK : LIGHT;
}

const ICONS = { search: Search, server: Server, terminal: Terminal, back: ChevronLeft, chevron: ChevronRight, down: ChevronDown, close: X, menu: Ellipsis, check: Check, plus: Plus, settings: Settings2, group: Layers, computer: Monitor, key: KeyRound, arrow: ArrowRight };
type IconName = keyof typeof ICONS;

/** A single restrained line vocabulary, matching the existing mock's chrome. */
export function Icon({ name, color, size = 22 }: { name: IconName; color: string; size?: number }) {
  const Symbol = ICONS[name];
  return <View accessible={false} accessibilityElementsHidden importantForAccessibility="no-hide-descendants" style={{ width: size, height: size }}><Symbol size={size} color={color} strokeWidth={1.65} /></View>;
}

export function IconButton({ icon, label, onPress, colors, disabled, style, testID }: {
  icon: IconName;
  label: string;
  onPress: () => void;
  colors: Palette;
  disabled?: boolean;
  style?: StyleProp<ViewStyle>;
  testID?: string;
}) {
  return <Pressable testID={testID} accessibilityRole="button" accessibilityLabel={label} accessibilityState={{ disabled }} disabled={disabled} onPress={onPress} style={({ pressed }) => [ui.iconButton, style, pressed && { backgroundColor: colors.surface }, disabled && { opacity: .45 }]}>
    <Icon name={icon} color={colors.text} size={21} />
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

export function Companion({ small = false }: { small?: boolean; dark?: boolean }) {
  return <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" style={small ? ui.smallCompanion : ui.companion}>
    <Image source={require('./assets/meerkat-companion-v2.png')} resizeMode="contain" style={small ? ui.smallCompanionImage : ui.companionImage} />
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
});
