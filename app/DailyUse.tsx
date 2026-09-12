import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ActionSheetIOS, ActivityIndicator, Alert, FlatList, Keyboard,
  KeyboardAvoidingView, Modal, Platform, Pressable, ScrollView,
  StatusBar, StyleSheet, Switch, Text, TextInput, View,
} from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';

import type { ServerProfile, TerminalPreferences } from '../modules/meeterm-terminal';
import { Button, DARK, Icon, IconButton, MONO } from './ui';
import type { Palette } from './ui';

export const DEFAULT_PREFERENCES: TerminalPreferences = {
  fontSize: 15, theme: 'system', scrollbackLines: 10000, automaticReconnect: true,
};

export function itemActions(title: string, edit: () => void, remove: () => void, kind: 'profile' | 'workspace' = 'profile') {
  const editLabel = kind === 'profile' ? '名前・情報を編集' : '名前を変更';
  const removeLabel = kind === 'profile' ? '削除' : '終了';
  if (Platform.OS === 'ios') {
    ActionSheetIOS.showActionSheetWithOptions({ title, options: [editLabel, removeLabel, 'キャンセル'], cancelButtonIndex: 2, destructiveButtonIndex: 1 }, index => {
      if (index === 0) edit();
      if (index === 1) remove();
    });
  } else {
    Alert.alert(title, undefined, [
      { text: 'キャンセル', style: 'cancel' },
      { text: editLabel, onPress: edit },
      { text: removeLabel, style: 'destructive', onPress: remove },
    ]);
  }
}

export function ProfileList({ profiles, selectedId, loading, error, busy, colors, onRetry, onAdd, onConnect, onEdit, onDelete }: {
  profiles: ServerProfile[];
  selectedId: string;
  loading: boolean;
  error: boolean;
  busy: boolean;
  colors: Palette;
  onRetry: () => void;
  onAdd: () => void;
  onConnect: (profile: ServerProfile) => void;
  onEdit: (profile: ServerProfile) => void;
  onDelete: (profile: ServerProfile) => void;
}) {
  return <FlatList data={profiles} keyExtractor={profile => profile.id} contentInsetAdjustmentBehavior="automatic" contentContainerStyle={styles.list}
    ListHeaderComponent={<View style={styles.listHeader}>
      <Text style={[styles.helper, { color: colors.muted }]}>サーバーを選んで、いつもの作業へ。接続先はこの端末に保存されます。</Text>
      <Button label="Add server" colors={colors} onPress={onAdd} disabled={busy}>サーバーを追加</Button>
      {error ? <View style={styles.errorBlock}><Text accessibilityRole="alert" style={[styles.helper, { color: colors.danger }]}>保存済みサーバーを読み込めませんでした。</Text><Button label="Reload saved servers" colors={colors} secondary onPress={onRetry} disabled={loading}>再読み込み</Button></View> : null}
    </View>}
    ListEmptyComponent={loading ? <ActivityIndicator color={colors.accent} /> : !error ? <View style={styles.empty}><Icon name="server" color={colors.muted} size={32} /><Text style={[styles.body, { color: colors.text }]}>保存済みサーバーはありません</Text><Text style={[styles.helper, { color: colors.muted }]}>接続先を追加すると、ここから選べます。</Text></View> : null}
    renderItem={({ item }) => <View style={[styles.profileRow, { borderBottomColor: colors.border }]}>
      <Pressable accessibilityRole="button" accessibilityLabel={`Connect saved server ${item.name}`} accessibilityState={{ selected: item.id === selectedId, disabled: busy }} testID={`server-profile-${item.id}`} disabled={busy} onPress={() => onConnect(item)} style={({ pressed }) => [styles.profileTarget, pressed && { backgroundColor: colors.surface }]}>
        <Icon name="server" color={item.id === selectedId ? colors.accent : colors.muted} size={22} />
        <View style={styles.copy}><Text numberOfLines={2} style={[styles.rowTitle, { color: colors.text }]}>{item.name}</Text><Text numberOfLines={2} style={[styles.helper, { color: colors.muted }]}>{item.username}@{item.host.includes(':') ? `[${item.host}]` : item.host}{item.port !== 22 ? `:${item.port}` : ''}</Text><Text style={[styles.caption, { color: colors.muted }]}>{item.credentialSaved ? '認証情報を保存済み' : '接続時に認証情報を入力'}{item.id === selectedId ? ' · 選択中' : ''}</Text></View>
      </Pressable>
      <IconButton icon="menu" label={`Server options ${item.name}`} disabled={busy} colors={colors} onPress={() => itemActions(item.name, () => onEdit(item), () => onDelete(item))} />
    </View>}
  />;
}

function FormModal({ visible, title, submitLabel, submitId, submitText = '保存', busy, colors, onClose, onSubmit, children }: {
  visible: boolean; title: string; submitLabel: string; submitId: string; submitText?: string;
  busy: boolean; colors: Palette; onClose: () => void; onSubmit: () => void; children: ReactNode;
}) {
  // Apply after Android registers the dialog window; its initial appearance
  // can still reflect the preceding palette during a theme transition.
  return <Modal visible={visible} animationType="slide" presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} onRequestClose={onClose} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
    <SafeAreaProvider><SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.flex, { backgroundColor: colors.background }]}>
      <StatusBar barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} />
      <KeyboardAvoidingView style={styles.flex} behavior={Platform.OS === 'ios' ? 'padding' : 'height'}>
        <View style={[styles.formHeader, { borderBottomColor: colors.border }]}>
          <Pressable accessibilityRole="button" accessibilityLabel="Cancel" disabled={busy} onPress={onClose} style={styles.headerAction}><Text style={[styles.actionText, { color: colors.accent, opacity: busy ? .45 : 1 }]}>キャンセル</Text></Pressable>
          <Text accessibilityRole="header" style={[styles.formTitle, { color: colors.text }]}>{title}</Text>
          <Pressable accessibilityRole="button" accessibilityLabel={submitLabel} accessibilityState={{ disabled: busy, busy }} testID={submitId} disabled={busy} onPress={onSubmit} style={[styles.headerAction, styles.headerEnd]}>{busy ? <ActivityIndicator color={colors.accent} /> : <Text style={[styles.actionText, { color: colors.accent, fontWeight: '600' }]}>{submitText}</Text>}</Pressable>
        </View>
        <ScrollView pointerEvents={busy ? 'none' : 'auto'} contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'} contentContainerStyle={styles.formContent}>{children}</ScrollView>
      </KeyboardAvoidingView>
    </SafeAreaView></SafeAreaProvider>
  </Modal>;
}

function confirmDiscard(dirty: boolean, close: () => void) {
  if (!dirty) { Keyboard.dismiss(); close(); return; }
  Alert.alert('変更を破棄しますか？', '入力した変更は保存されません。', [
    { text: '編集を続ける', style: 'cancel' },
    { text: '破棄', style: 'destructive', onPress: () => { Keyboard.dismiss(); close(); } },
  ]);
}

export function NameForm({ visible, title, initialName, colors, onClose, onSave }: {
  visible: boolean; title: string; initialName: string; colors: Palette;
  onClose: () => void; onSave: (name: string) => Promise<boolean>;
}) {
  const [name, setName] = useState(initialName);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  useEffect(() => { if (visible) { setName(initialName); setError(''); setBusy(false); pending.current = false; } }, [initialName, visible]);
  const submit = async () => {
    if (pending.current) return;
    const next = name.trim();
    if (!next || next.length > 80 || /[\x00-\x1f\x7f]/.test(next)) { setError('制御文字を含まない1〜80文字の名前を入力してください。'); return; }
    pending.current = true;
    setBusy(true);
    setError('');
    Keyboard.dismiss();
    try { if (!await onSave(next)) setError('変更できませんでした。接続状態を確認して、もう一度試してください。'); }
    catch { setError('変更できませんでした。接続状態を確認して、もう一度試してください。'); }
    finally { pending.current = false; setBusy(false); }
  };
  return <FormModal visible={visible} title={title} submitLabel="Save name" submitId="name-submit" submitText={title.includes('作成') ? '作成' : '保存'} busy={busy} colors={colors} onClose={() => { if (!pending.current) confirmDiscard(name !== initialName, onClose); }} onSubmit={() => { void submit(); }}>
    <View style={styles.field}><Text style={[styles.body, { color: colors.text }]}>名前</Text><TextInput accessibilityLabel="Workspace or terminal name" testID="workspace-terminal-name" value={name} onChangeText={setName} autoFocus autoComplete="off" autoCorrect={false} returnKeyType="done" onSubmitEditing={submit} placeholder="例: 開発用" placeholderTextColor={colors.placeholder} selectionColor={colors.accent} style={[styles.input, { color: colors.text, backgroundColor: colors.surface, borderColor: colors.border }]} />
      {error ? <Text accessibilityRole="alert" style={[styles.helper, { color: colors.danger }]}>{error}</Text> : null}
    </View>
    <Text style={[styles.helper, { color: colors.muted }]}>PC 側にも同じ名前が表示されます。</Text>
  </FormModal>;
}

const THEME_LABELS = { system: 'システムに合わせる', light: 'ライト', dark: 'ダーク' };

export function SettingsForm({ visible, preferences, colors, onClose, onSave }: {
  visible: boolean; preferences: TerminalPreferences; colors: Palette;
  onClose: () => void; onSave: (preferences: TerminalPreferences) => Promise<boolean>;
}) {
  const [fontSize, setFontSize] = useState(String(preferences.fontSize));
  const [scrollback, setScrollback] = useState(String(preferences.scrollbackLines));
  const [theme, setTheme] = useState(preferences.theme);
  const [automaticReconnect, setAutomaticReconnect] = useState(preferences.automaticReconnect);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  useEffect(() => {
    if (!visible) return;
    setFontSize(String(preferences.fontSize)); setScrollback(String(preferences.scrollbackLines));
    setTheme(preferences.theme); setAutomaticReconnect(preferences.automaticReconnect); setError('');
    setBusy(false); pending.current = false;
  }, [preferences, visible]);
  const dirty = fontSize !== String(preferences.fontSize) || scrollback !== String(preferences.scrollbackLines)
    || theme !== preferences.theme || automaticReconnect !== preferences.automaticReconnect;
  const chooseTheme = () => {
    Keyboard.dismiss();
    const themes = ['system', 'light', 'dark'] as const;
    if (Platform.OS === 'ios') {
      ActionSheetIOS.showActionSheetWithOptions({ title: 'テーマ', options: [...themes.map(item => THEME_LABELS[item]), 'キャンセル'], cancelButtonIndex: 3 }, index => { if (index < 3) setTheme(themes[index]); });
    } else {
      Alert.alert('テーマ', undefined, themes.map(value => ({ text: THEME_LABELS[value], onPress: () => setTheme(value) })), { cancelable: true });
    }
  };
  const submit = async () => {
    if (pending.current) return;
    const size = Number(fontSize), lines = Number(scrollback);
    if (!/^\d+$/.test(fontSize) || size < 10 || size > 24) { setError('文字サイズは10〜24の整数で入力してください。'); return; }
    if (!/^\d+$/.test(scrollback) || lines < 1000 || lines > 50000) { setError('履歴の行数は1,000〜50,000の整数で入力してください。'); return; }
    pending.current = true; setBusy(true); setError(''); Keyboard.dismiss();
    try { if (!await onSave({ fontSize: size, theme, scrollbackLines: lines, automaticReconnect })) setError('設定を保存できませんでした。もう一度試してください。'); }
    catch { setError('設定を保存できませんでした。もう一度試してください。'); }
    finally { pending.current = false; setBusy(false); }
  };
  const numericStyle = [styles.numericInput, { color: colors.text, backgroundColor: colors.background, borderColor: colors.border }];
  return <FormModal visible={visible} title="ターミナル設定" submitLabel="Save settings" submitId="settings-submit" busy={busy} colors={colors} onClose={() => { if (!pending.current) confirmDiscard(dirty, onClose); }} onSubmit={() => { void submit(); }}>
    <View style={styles.section}><Text style={[styles.sectionLabel, { color: colors.muted }]}>表示</Text>
      <View style={[styles.group, { backgroundColor: colors.surface }]}>
        <View style={[styles.settingRow, { borderBottomColor: colors.border }]}><View style={styles.copy}><Text style={[styles.body, { color: colors.text }]}>文字サイズ</Text><Text style={[styles.caption, { color: colors.muted }]}>10〜24 pt</Text></View><TextInput accessibilityLabel="Terminal font size" testID="terminal-font-size" inputMode="numeric" keyboardType="number-pad" autoComplete="off" maxLength={2} value={fontSize} onChangeText={setFontSize} selectionColor={colors.accent} style={numericStyle} /></View>
        <Pressable accessibilityRole="button" accessibilityLabel="Terminal theme" testID="terminal-theme" onPress={chooseTheme} style={({ pressed }) => [styles.settingRow, styles.noBorder, pressed && { backgroundColor: colors.elevated }]}><View style={styles.copy}><Text style={[styles.body, { color: colors.text }]}>テーマ</Text><Text style={[styles.helper, { color: colors.muted }]}>{THEME_LABELS[theme]}</Text></View><Icon name="chevron" color={colors.muted} size={18} /></Pressable>
      </View>
      <Text style={[styles.preview, { color: colors.text, fontSize: Math.min(24, Math.max(10, Number(fontSize) || 15)) }]}>Aa 0123 日本語</Text>
    </View>
    <View style={styles.section}><Text style={[styles.sectionLabel, { color: colors.muted }]}>履歴</Text>
      <View style={[styles.group, { backgroundColor: colors.surface }]}><View style={[styles.settingRow, styles.noBorder]}><View style={styles.copy}><Text style={[styles.body, { color: colors.text }]}>保持する行数</Text><Text style={[styles.caption, { color: colors.muted }]}>1,000〜50,000 行</Text></View><TextInput accessibilityLabel="Scrollback lines" testID="scrollback-lines" inputMode="numeric" keyboardType="number-pad" autoComplete="off" maxLength={5} value={scrollback} onChangeText={setScrollback} selectionColor={colors.accent} style={[numericStyle, { minWidth: 96 }]} /></View></View>
      <Text style={[styles.helper, { color: colors.muted }]}>すべてのターミナルに適用します。行数を減らすと古い履歴は削除されます。アプリ終了後は、サーバーに残る履歴を最大2,000行復元します。</Text>
    </View>
    <View style={styles.section}><Text style={[styles.sectionLabel, { color: colors.muted }]}>接続</Text>
      <View style={[styles.group, { backgroundColor: colors.surface }]}><View style={[styles.settingRow, styles.noBorder]}><View style={styles.copy}><Text style={[styles.body, { color: colors.text }]}>自動で再接続</Text></View><Switch accessibilityLabel="Automatic reconnect" testID="automatic-reconnect" value={automaticReconnect} onValueChange={setAutomaticReconnect} trackColor={{ true: colors.accentFill }} /></View></View>
      <Text style={[styles.helper, { color: colors.muted }]}>通信が途切れたときやアプリへ戻ったときに再接続します。自分で切断した接続は再開しません。</Text>
    </View>
    {error ? <Text accessibilityRole="alert" style={[styles.helper, { color: colors.danger }]}>{error}</Text> : null}
  </FormModal>;
}

const styles = StyleSheet.create({
  flex: { flex: 1 },
  list: { padding: 24, paddingBottom: 40 },
  listHeader: { gap: 20, paddingBottom: 16 },
  errorBlock: { gap: 12 },
  empty: { paddingVertical: 40, gap: 16, alignItems: 'center' },
  profileRow: { flexDirection: 'row', alignItems: 'center', gap: 4, borderBottomWidth: StyleSheet.hairlineWidth },
  profileTarget: { flex: 1, minWidth: 0, minHeight: 96, flexDirection: 'row', alignItems: 'center', gap: 16, paddingVertical: 16 },
  copy: { flex: 1, minWidth: 0, gap: 4 },
  rowTitle: { fontSize: 17, lineHeight: 25, fontWeight: '600' },
  caption: { fontSize: 12, lineHeight: 20 },
  helper: { fontSize: 13, lineHeight: 22 },
  body: { fontSize: 16, lineHeight: 24 },
  formHeader: { minHeight: 60, paddingHorizontal: 12, flexDirection: 'row', alignItems: 'center', gap: 4, borderBottomWidth: StyleSheet.hairlineWidth },
  headerAction: { minWidth: 72, minHeight: 48, justifyContent: 'center' },
  headerEnd: { alignItems: 'flex-end' },
  actionText: { fontSize: 14, lineHeight: 24 },
  formTitle: { flex: 1, textAlign: 'center', fontSize: 17, lineHeight: 25, fontWeight: '600' },
  formContent: { padding: 24, gap: 28, paddingBottom: 40 },
  field: { gap: 12 },
  input: { minHeight: 52, paddingHorizontal: 12, paddingVertical: 12, fontSize: 16, borderWidth: 1, borderRadius: 12, borderCurve: 'continuous' },
  section: { gap: 12 },
  sectionLabel: { fontSize: 13, lineHeight: 20, fontWeight: '600' },
  group: { borderRadius: 12, borderCurve: 'continuous', overflow: 'hidden' },
  settingRow: { minHeight: 72, padding: 16, gap: 12, flexDirection: 'row', alignItems: 'center', borderBottomWidth: StyleSheet.hairlineWidth },
  noBorder: { borderBottomWidth: 0 },
  numericInput: { minWidth: 64, minHeight: 44, paddingHorizontal: 12, paddingVertical: 8, textAlign: 'right', fontSize: 16, fontVariant: ['tabular-nums'], borderWidth: 1, borderRadius: 8, borderCurve: 'continuous' },
  preview: { paddingHorizontal: 16, fontFamily: MONO, lineHeight: 32 },
});
