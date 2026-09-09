import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  Keyboard,
  KeyboardAvoidingView,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';

import type { SshConnectOptions } from '../modules/meeterm-terminal';
import { DARK, MONO, usePalette } from './ui';
import type { Palette } from './ui';

type AuthMethod = 'publicKey' | 'password';
type FormErrors = Partial<Record<'host' | 'port' | 'username' | 'privateKey' | 'password', string>>;

function Field({ label, error, optional, action, children, colors }: {
  label: string;
  error?: string;
  optional?: boolean;
  action?: { label: string; accessibilityLabel: string; onPress: () => void };
  children: ReactNode;
  colors: Palette;
}) {
  return <View style={styles.field}>
    <View style={styles.labelRow}>
      <Text style={[styles.label, { color: colors.text }]}>{label}{optional ? <Text style={{ color: colors.muted, fontWeight: '400' }}> · 任意</Text> : null}</Text>
      {action ? <Pressable accessibilityRole="button" accessibilityLabel={action.accessibilityLabel} onPress={action.onPress} style={({ pressed }) => [styles.fieldAction, pressed && styles.pressed]}><Text style={{ color: colors.accent, fontSize: 14 }}>{action.label}</Text></Pressable> : null}
    </View>
    {children}
    {error ? <Text accessibilityLiveRegion="polite" style={[styles.error, { color: colors.danger }]}>{error}</Text> : null}
  </View>;
}

export function ConnectionForm({ visible, onClose, onSubmit }: {
  visible: boolean;
  onClose: () => void;
  onSubmit: (options: SshConnectOptions) => void;
}) {
  const colors = usePalette();
  const [host, setHost] = useState('');
  const [port, setPort] = useState('22');
  const [username, setUsername] = useState('');
  const [authMethod, setAuthMethod] = useState<AuthMethod>('publicKey');
  const [privateKey, setPrivateKey] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [password, setPassword] = useState('');
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const [showPassphrase, setShowPassphrase] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [errors, setErrors] = useState<FormErrors>({});
  const hostRef = useRef<TextInput>(null);
  const portRef = useRef<TextInput>(null);
  const usernameRef = useRef<TextInput>(null);
  const privateKeyRef = useRef<TextInput>(null);
  const passwordRef = useRef<TextInput>(null);
  const scrollRef = useRef<ScrollView>(null);
  const submitting = useRef(false);

  const scrollPasswordIntoView = useCallback(() => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (passwordRef.current?.isFocused()) {
          scrollRef.current?.scrollToEnd({ animated: true });
        }
      });
    });
  }, []);

  const clearSecrets = useCallback(() => {
    setPrivateKey('');
    setPassphrase('');
    setPassword('');
    setShowPrivateKey(false);
    setShowPassphrase(false);
    setShowPassword(false);
  }, []);

  useEffect(() => {
    if (visible) {
      submitting.current = false;
    } else {
      clearSecrets();
      setErrors({});
    }
  }, [clearSecrets, visible]);

  useEffect(() => () => {
    clearSecrets();
  }, [clearSecrets]);

  useEffect(() => {
    const subscription = Keyboard.addListener('keyboardDidShow', () => {
      if (authMethod === 'password' && passwordRef.current?.isFocused()) {
        scrollPasswordIntoView();
      }
    });
    return () => subscription.remove();
  }, [authMethod, scrollPasswordIntoView]);

  const close = useCallback(() => {
    Keyboard.dismiss();
    submitting.current = false;
    clearSecrets();
    setErrors({});
    onClose();
  }, [clearSecrets, onClose]);

  const changeAuthMethod = useCallback((next: AuthMethod) => {
    if (next === authMethod) return;
    Keyboard.dismiss();
    clearSecrets();
    setErrors({});
    setAuthMethod(next);
  }, [authMethod, clearSecrets]);

  const submit = useCallback(() => {
    if (submitting.current) return;
    const trimmedHost = host.trim();
    const parsedPort = Number(port);
    const trimmedUsername = username.trim();
    const trimmedKey = privateKey.trim();
    const nextErrors: FormErrors = {};
    if (!trimmedHost || /[\s\x00-\x1f\x7f]/.test(trimmedHost)) {
      nextErrors.host = '空白を含まないホスト名か IP アドレスを入力してください。';
    }
    if (!/^\d+$/.test(port) || parsedPort < 1 || parsedPort > 65535) {
      nextErrors.port = '1〜65535 の数字を入力してください。';
    }
    if (!trimmedUsername || /[\s\x00-\x1f\x7f]/.test(trimmedUsername)) {
      nextErrors.username = 'SSH のユーザー名を入力してください。空白は使えません。';
    }
    if (authMethod === 'publicKey') {
      if (!trimmedKey.startsWith('-----BEGIN OPENSSH PRIVATE KEY-----') || !trimmedKey.endsWith('-----END OPENSSH PRIVATE KEY-----')) {
        nextErrors.privateKey = 'BEGIN と END の行を含む OpenSSH 形式の秘密鍵を貼り付けてください。';
      }
    } else if (!password || password.includes('\u0000')) {
      nextErrors.password = 'SSH パスワードを入力してください。';
    }
    if (Object.keys(nextErrors).length) {
      setErrors(nextErrors);
      const target = nextErrors.host ? hostRef
        : nextErrors.port ? portRef
          : nextErrors.username ? usernameRef
            : nextErrors.privateKey ? privateKeyRef : passwordRef;
      target.current?.focus();
      return;
    }
    submitting.current = true;
    const options: SshConnectOptions = authMethod === 'password'
      ? { authMethod: 'password', host: trimmedHost, port: parsedPort, username: trimmedUsername, password }
      : { host: trimmedHost, port: parsedPort, username: trimmedUsername, privateKey: trimmedKey, passphrase };
    // Only this one-shot command carries credentials. No UI state or disk
    // persistence retains them after submission or cancellation.
    clearSecrets();
    setErrors({});
    Keyboard.dismiss();
    onSubmit(options);
  }, [authMethod, clearSecrets, host, onSubmit, passphrase, password, port, privateKey, username]);

  const inputStyle = [styles.input, { color: colors.text, backgroundColor: colors.surface, borderColor: colors.border }];
  const inputDefaults = { autoCapitalize: 'none' as const, autoComplete: 'off' as const, autoCorrect: false, spellCheck: false, placeholderTextColor: colors.placeholder, selectionColor: colors.accent };

  return <Modal visible={visible} animationType="slide" presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} onRequestClose={close} onDismiss={clearSecrets}>
    <SafeAreaProvider>
      <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.root, { backgroundColor: colors.background }]}>
        <StatusBar hidden={false} barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} />
        <KeyboardAvoidingView behavior={Platform.OS === 'ios' ? 'padding' : 'height'} style={styles.root}>
          <View style={[styles.header, { borderBottomColor: colors.border }]}>
            <Pressable accessibilityRole="button" accessibilityLabel="Cancel" onPress={close} style={({ pressed }) => [styles.headerAction, pressed && styles.pressed]}><Text style={[styles.headerActionText, { color: colors.accent }]}>キャンセル</Text></Pressable>
            <Text accessibilityRole="header" style={[styles.headerTitle, { color: colors.text }]}>サーバーに接続</Text>
            <Pressable accessibilityRole="button" accessibilityLabel="Connect" testID="ssh-submit" onPress={submit} style={({ pressed }) => [styles.headerAction, styles.headerActionEnd, pressed && styles.pressed]}><Text style={[styles.headerActionText, { color: colors.accent, fontWeight: '600' }]}>接続</Text></Pressable>
          </View>
          <ScrollView ref={scrollRef} onLayout={scrollPasswordIntoView} contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'} contentContainerStyle={styles.content}>
            <View style={styles.intro}>
              <Text style={[styles.title, { color: colors.text }]}>いつもの作業へ。</Text>
              <Text style={[styles.body, { color: colors.muted }]}>SSH の接続先と認証情報を入力してください。接続後に、サーバーのワークスペースが並びます。</Text>
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>接続先</Text>
              <View style={styles.hostPortRow}>
                <View style={styles.hostColumn}>
                  <Field label="ホスト" colors={colors} error={errors.host}>
                    <TextInput ref={hostRef} accessibilityLabel="Host" testID="ssh-host" {...inputDefaults} value={host} onChangeText={value => { setHost(value); setErrors(current => ({ ...current, host: undefined })); }} onSubmitEditing={() => portRef.current?.focus()} placeholder="server.example.com" returnKeyType="next" style={[inputStyle, errors.host && { borderColor: colors.danger }]} />
                  </Field>
                </View>
                <View style={styles.portColumn}>
                  <Field label="ポート" colors={colors} error={errors.port}>
                    <TextInput ref={portRef} accessibilityLabel="Port" testID="ssh-port" autoComplete="off" inputMode="numeric" keyboardType="number-pad" maxLength={5} value={port} onChangeText={value => { setPort(value.replace(/[^0-9]/g, '')); setErrors(current => ({ ...current, port: undefined })); }} onSubmitEditing={() => usernameRef.current?.focus()} returnKeyType="next" selectionColor={colors.accent} style={[inputStyle, { fontVariant: ['tabular-nums'] }, errors.port && { borderColor: colors.danger }]} />
                  </Field>
                </View>
              </View>
              <Field label="ユーザー名" colors={colors} error={errors.username}>
                <TextInput ref={usernameRef} accessibilityLabel="Username" testID="ssh-username" {...inputDefaults} value={username} onChangeText={value => { setUsername(value); setErrors(current => ({ ...current, username: undefined })); }} onSubmitEditing={() => authMethod === 'publicKey' ? privateKeyRef.current?.focus() : passwordRef.current?.focus()} placeholder="developer" returnKeyType="next" style={[inputStyle, errors.username && { borderColor: colors.danger }]} />
              </Field>
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>認証</Text>
              <Text style={[styles.authMethodLabel, { color: colors.muted }]}>認証方式</Text>
              <View accessibilityRole="radiogroup" accessibilityLabel="Authentication method" style={[styles.authChoices, { backgroundColor: colors.surface, borderColor: colors.border }]}>
                <Pressable accessibilityRole="radio" accessibilityLabel="Private key authentication" accessibilityState={{ selected: authMethod === 'publicKey', checked: authMethod === 'publicKey' }} testID="ssh-auth-public-key" onPress={() => changeAuthMethod('publicKey')} style={({ pressed }) => [styles.authChoice, authMethod === 'publicKey' && { backgroundColor: colors.accentFill }, pressed && styles.pressed]}>
                  <Text style={[styles.authChoiceText, { color: authMethod === 'publicKey' ? colors.onAccent : colors.text }]}>秘密鍵</Text>
                </Pressable>
                <Pressable accessibilityRole="radio" accessibilityLabel="Password authentication" accessibilityState={{ selected: authMethod === 'password', checked: authMethod === 'password' }} testID="ssh-auth-password" onPress={() => changeAuthMethod('password')} style={({ pressed }) => [styles.authChoice, authMethod === 'password' && { backgroundColor: colors.accentFill }, pressed && styles.pressed]}>
                  <Text style={[styles.authChoiceText, { color: authMethod === 'password' ? colors.onAccent : colors.text }]}>パスワード</Text>
                </Pressable>
              </View>
              {authMethod === 'publicKey' ? <>
                <Field label="OpenSSH 秘密鍵" colors={colors} error={errors.privateKey} action={{ label: showPrivateKey ? '隠す' : '表示', accessibilityLabel: showPrivateKey ? 'Hide private key' : 'Show private key', onPress: () => setShowPrivateKey(value => !value) }}>
                  <View style={[styles.keyShell, { backgroundColor: colors.surface, borderColor: errors.privateKey ? colors.danger : colors.border }]}>
                    <TextInput ref={privateKeyRef} accessibilityLabel="Private OpenSSH key" testID="ssh-private-key" accessibilityValue={{ text: privateKey ? 'Private key entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" multiline caretHidden={!showPrivateKey} value={privateKey} onChangeText={value => { setPrivateKey(value); setErrors(current => ({ ...current, privateKey: undefined })); }} placeholder={showPrivateKey ? '-----BEGIN OPENSSH PRIVATE KEY-----' : undefined} selectionColor={showPrivateKey ? colors.accent : 'transparent'} style={[styles.keyInput, { color: showPrivateKey ? colors.text : 'transparent' }]} textAlignVertical="top" />
                    {!showPrivateKey ? <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" pointerEvents="none" style={styles.keyMask}><Text style={[styles.body, { color: colors.muted }]}>{privateKey ? '秘密鍵を入力しました' : '秘密鍵の全文を貼り付け'}</Text></View> : null}
                  </View>
                </Field>
                <Field label="鍵のパスフレーズ" colors={colors} optional action={{ label: showPassphrase ? '隠す' : '表示', accessibilityLabel: showPassphrase ? 'Hide passphrase' : 'Show passphrase', onPress: () => setShowPassphrase(value => !value) }}>
                  <TextInput accessibilityLabel="Key passphrase, optional" testID="ssh-passphrase" {...inputDefaults} importantForAutofill="no" value={passphrase} onChangeText={setPassphrase} onSubmitEditing={submit} placeholder="暗号化された鍵の場合のみ" returnKeyType="go" secureTextEntry={!showPassphrase} style={inputStyle} />
                </Field>
                <Text style={[styles.helper, { color: colors.muted }]}>秘密鍵とパスフレーズは保存しません。接続・キャンセル時に入力欄から消去します。接続後は、アプリを閉じるまで再接続に使えます。</Text>
              </> : <>
                <Field label="SSH パスワード" colors={colors} error={errors.password} action={{ label: showPassword ? '隠す' : '表示', accessibilityLabel: showPassword ? 'Hide password' : 'Show password', onPress: () => setShowPassword(value => !value) }}>
                  <TextInput ref={passwordRef} accessibilityLabel="SSH password" testID="ssh-password" accessibilityValue={{ text: password ? 'Password entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" value={password} onChangeText={value => { setPassword(value); setErrors(current => ({ ...current, password: undefined })); }} onFocus={scrollPasswordIntoView} onSubmitEditing={submit} placeholder="SSH サーバーのパスワード" returnKeyType="go" secureTextEntry={!showPassword} style={[inputStyle, errors.password && { borderColor: colors.danger }]} />
                </Field>
                <Text style={[styles.helper, { color: colors.muted }]}>パスワードは保存しません。接続・キャンセル時に入力欄から消去します。接続後は、アプリを閉じるまで再接続に使えます。</Text>
              </>}
            </View>
          </ScrollView>
        </KeyboardAvoidingView>
      </SafeAreaView>
    </SafeAreaProvider>
  </Modal>;
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  header: { minHeight: 60, flexDirection: 'row', alignItems: 'center', borderBottomWidth: StyleSheet.hairlineWidth, paddingHorizontal: 12, gap: 4 },
  headerAction: { minWidth: 72, minHeight: 48, justifyContent: 'center' },
  headerActionEnd: { alignItems: 'flex-end' },
  headerActionText: { fontSize: 14 },
  headerTitle: { fontSize: 17, fontWeight: '600', flex: 1, textAlign: 'center' },
  content: { padding: 24, gap: 28, paddingBottom: 40 },
  intro: { gap: 12 },
  title: { fontSize: 26, lineHeight: 36, fontWeight: '600', letterSpacing: -.8 },
  body: { fontSize: 15, lineHeight: 26 },
  section: { gap: 16 },
  sectionLabel: { fontSize: 13, fontWeight: '600' },
  authMethodLabel: { fontSize: 14, lineHeight: 20, fontWeight: '500', marginBottom: -8 },
  authChoices: { minHeight: 52, flexDirection: 'row', borderWidth: 1, borderRadius: 12, borderCurve: 'continuous', overflow: 'hidden' },
  authChoice: { flex: 1, minHeight: 50, alignItems: 'center', justifyContent: 'center', paddingHorizontal: 12 },
  authChoiceText: { fontSize: 15, lineHeight: 23, fontWeight: '600' },
  hostPortRow: { flexDirection: 'row', gap: 12, alignItems: 'flex-start' },
  hostColumn: { flex: 1, minWidth: 0 },
  portColumn: { width: 84 },
  field: { gap: 8 },
  labelRow: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', gap: 8, minHeight: 24 },
  label: { fontSize: 14, fontWeight: '500', flexShrink: 1 },
  fieldAction: { minWidth: 44, minHeight: 44, alignItems: 'flex-end', justifyContent: 'center', marginVertical: -10 },
  input: { minHeight: 52, borderWidth: 1, borderRadius: 12, borderCurve: 'continuous', paddingHorizontal: 12, paddingVertical: 12, fontSize: 16 },
  keyShell: { borderWidth: 1, borderRadius: 12, borderCurve: 'continuous', minHeight: 116, overflow: 'hidden' },
  keyInput: { height: 116, padding: 12, fontFamily: MONO, fontSize: 16, lineHeight: 22 },
  keyMask: { position: 'absolute', top: 0, right: 0, bottom: 0, left: 0, padding: 12 },
  error: { fontSize: 13, lineHeight: 20 },
  helper: { fontSize: 13, lineHeight: 22 },
  pressed: { opacity: .6 },
});
