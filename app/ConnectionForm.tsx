import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ActivityIndicator,
  Alert,
  Keyboard,
  KeyboardAvoidingView,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  View,
} from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';

import type { SavedCredential, ServerProfile } from '../modules/meeterm-terminal';
import { DARK, MONO } from './ui';
import type { Palette } from './ui';

type AuthMethod = 'publicKey' | 'password';
type FormErrors = Partial<Record<'name' | 'host' | 'port' | 'username' | 'privateKey' | 'password', string>>;

export type ConnectionSubmission = {
  profile: Omit<ServerProfile, 'credentialSaved'>;
  credential: SavedCredential | null;
  saveProfile: boolean;
  saveCredential: boolean;
  keepCredential: boolean;
  connect: boolean;
};

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

export function ConnectionForm({ visible, onClose, onSubmit, onDismiss, initialProfile, mode = 'connect', colors }: {
  visible: boolean;
  onClose: () => void;
  onDismiss?: () => void;
  onSubmit: (submission: ConnectionSubmission) => Promise<boolean>;
  initialProfile?: ServerProfile;
  mode?: 'connect' | 'save';
  colors: Palette;
}) {
  const [name, setName] = useState('');
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
  const [saveServer, setSaveServer] = useState(true);
  const [saveCredential, setSaveCredential] = useState(false);
  const [replaceCredential, setReplaceCredential] = useState(false);
  const [busy, setBusy] = useState(false);
  const [submissionError, setSubmissionError] = useState('');
  const nameRef = useRef<TextInput>(null);
  const hostRef = useRef<TextInput>(null);
  const portRef = useRef<TextInput>(null);
  const usernameRef = useRef<TextInput>(null);
  const privateKeyRef = useRef<TextInput>(null);
  const passwordRef = useRef<TextInput>(null);
  const scrollRef = useRef<ScrollView>(null);
  const submitting = useRef(false);

  const credentialMatches = Boolean(initialProfile?.credentialSaved
    && host.trim() === initialProfile.host && Number(port) === initialProfile.port
    && username.trim() === initialProfile.username && authMethod === initialProfile.authMethod);
  const usingSavedCredential = credentialMatches && saveCredential && !replaceCredential;

  const scrollPasswordIntoView = useCallback(() => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (passwordRef.current?.isFocused()) {
          // Settings follow the credential field now, so scrolling to the end
          // would move the focused password out of view.
          scrollRef.current?.getNativeScrollRef()?.measureInWindow((_x, top) => {
            if (passwordRef.current?.isFocused()) {
              scrollRef.current?.scrollResponderScrollNativeHandleToKeyboard(passwordRef.current, top + 16, true);
            }
          });
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
      setBusy(false);
      setName(initialProfile?.name ?? '');
      setHost(initialProfile?.host ?? '');
      setPort(String(initialProfile?.port ?? 22));
      setUsername(initialProfile?.username ?? '');
      setAuthMethod(initialProfile?.authMethod ?? 'publicKey');
      setSaveServer(true);
      setSaveCredential(Boolean(initialProfile?.credentialSaved));
      setReplaceCredential(false);
      setSubmissionError('');
      clearSecrets();
    } else {
      clearSecrets();
      setErrors({});
    }
  }, [clearSecrets, initialProfile, visible]);

  useEffect(() => {
    const subscription = Keyboard.addListener('keyboardDidShow', () => {
      if (authMethod === 'password' && passwordRef.current?.isFocused()) {
        scrollPasswordIntoView();
      }
    });
    return () => subscription.remove();
  }, [authMethod, scrollPasswordIntoView]);

  const discard = useCallback(() => {
    Keyboard.dismiss();
    submitting.current = false;
    clearSecrets();
    setErrors({});
    onClose();
  }, [clearSecrets, onClose]);

  const close = useCallback(() => {
    if (submitting.current) return;
    const dirty = name !== (initialProfile?.name ?? '') || host !== (initialProfile?.host ?? '')
      || port !== String(initialProfile?.port ?? 22) || username !== (initialProfile?.username ?? '')
      || authMethod !== (initialProfile?.authMethod ?? 'publicKey') || Boolean(privateKey || passphrase || password)
      || !saveServer || saveCredential !== Boolean(initialProfile?.credentialSaved);
    if (!dirty) { discard(); return; }
    Alert.alert('変更を破棄しますか？', '入力した変更は保存されません。', [
      { text: '編集を続ける', style: 'cancel' },
      { text: '破棄', style: 'destructive', onPress: discard },
    ]);
  }, [authMethod, discard, host, initialProfile, name, passphrase, password, port, privateKey, saveCredential, saveServer, username]);

  const changeAuthMethod = useCallback((next: AuthMethod) => {
    if (next === authMethod) return;
    Keyboard.dismiss();
    clearSecrets();
    setErrors({});
    setReplaceCredential(false);
    setAuthMethod(next);
  }, [authMethod, clearSecrets]);

  const submit = useCallback(async () => {
    if (submitting.current) return;
    const trimmedHost = host.trim();
    const parsedPort = Number(port);
    const trimmedUsername = username.trim();
    const trimmedKey = privateKey.trim();
    const nextErrors: FormErrors = {};
    if (name.trim().length > 80 || /[\x00-\x1f\x7f]/.test(name)) nextErrors.name = '名前は制御文字を含まない80文字以内で入力してください。';
    if (!trimmedHost || /[\s\x00-\x1f\x7f]/.test(trimmedHost)) {
      nextErrors.host = '空白を含まないホスト名か IP アドレスを入力してください。';
    }
    if (!/^\d+$/.test(port) || parsedPort < 1 || parsedPort > 65535) {
      nextErrors.port = '1〜65535 の数字を入力してください。';
    }
    if (!trimmedUsername || /[\s\x00-\x1f\x7f]/.test(trimmedUsername)) {
      nextErrors.username = 'SSH のユーザー名を入力してください。空白は使えません。';
    }
    const needsCredential = !usingSavedCredential && (mode === 'connect' || saveCredential || Boolean(privateKey || password || passphrase));
    if (needsCredential && authMethod === 'publicKey') {
      if (!trimmedKey.startsWith('-----BEGIN OPENSSH PRIVATE KEY-----') || !trimmedKey.endsWith('-----END OPENSSH PRIVATE KEY-----')) {
        nextErrors.privateKey = 'BEGIN と END の行を含む OpenSSH 形式の秘密鍵を貼り付けてください。';
      }
    } else if (needsCredential && (!password || password.includes('\u0000'))) {
      nextErrors.password = 'SSH パスワードを入力してください。';
    }
    if (Object.keys(nextErrors).length) {
      setErrors(nextErrors);
      const target = nextErrors.name ? nameRef : nextErrors.host ? hostRef
        : nextErrors.port ? portRef
          : nextErrors.username ? usernameRef
            : nextErrors.privateKey ? privateKeyRef : passwordRef;
      target.current?.focus();
      return;
    }
    submitting.current = true;
    setBusy(true);
    setSubmissionError('');
    const credential: SavedCredential | null = needsCredential
      ? authMethod === 'password' ? { authMethod: 'password', password }
        : { authMethod: 'publicKey', privateKey: trimmedKey, passphrase }
      : null;
    // Secrets leave this form through one write-only command. Saved credentials
    // are consumed natively and never populated into a JavaScript field.
    clearSecrets();
    setErrors({});
    Keyboard.dismiss();
    try {
      const accepted = await onSubmit({
        profile: { id: initialProfile?.id ?? '', name: name.trim() || trimmedHost.slice(0, 80), host: trimmedHost, port: parsedPort, username: trimmedUsername, authMethod },
        credential, saveProfile: mode === 'save' || saveServer, saveCredential: saveServer && saveCredential,
        keepCredential: saveServer && usingSavedCredential, connect: mode === 'connect',
      });
      if (!accepted) setSubmissionError('保存または接続を開始できませんでした。接続先を確認して、認証情報を入力し直してください。');
    } catch {
      setSubmissionError('保存または接続を開始できませんでした。認証情報を入力し直して、もう一度試してください。');
    } finally { submitting.current = false; setBusy(false); }
  }, [authMethod, clearSecrets, host, initialProfile, mode, name, onSubmit, passphrase, password, port, privateKey, saveCredential, saveServer, username, usingSavedCredential]);

  const inputStyle = [styles.input, { color: colors.text, backgroundColor: colors.surface, borderColor: colors.border }];
  const inputDefaults = { autoCapitalize: 'none' as const, autoComplete: 'off' as const, autoCorrect: false, spellCheck: false, placeholderTextColor: colors.placeholder, selectionColor: colors.accent };

  return <Modal visible={visible} animationType="slide" presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} onRequestClose={close} onDismiss={() => { clearSecrets(); onDismiss?.(); }}>
    <SafeAreaProvider>
      <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.root, { backgroundColor: colors.background }]}>
        <StatusBar hidden={false} barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} />
        <KeyboardAvoidingView behavior={Platform.OS === 'ios' ? 'padding' : 'height'} style={styles.root}>
          <View style={[styles.header, { borderBottomColor: colors.border }]}>
            <Pressable accessibilityRole="button" accessibilityLabel="Cancel" disabled={busy} onPress={close} style={({ pressed }) => [styles.headerAction, pressed && styles.pressed, busy && { opacity: .45 }]}><Text style={[styles.headerActionText, { color: colors.accent }]}>キャンセル</Text></Pressable>
            <Text accessibilityRole="header" style={[styles.headerTitle, { color: colors.text }]}>{mode === 'save' ? initialProfile ? 'サーバーを編集' : 'サーバーを追加' : 'サーバーに接続'}</Text>
            <Pressable accessibilityRole="button" accessibilityLabel={mode === 'save' ? 'Save server' : 'Connect'} accessibilityState={{ disabled: busy, busy }} disabled={busy} testID="ssh-submit" onPress={submit} style={({ pressed }) => [styles.headerAction, styles.headerActionEnd, pressed && styles.pressed]}>{busy ? <ActivityIndicator color={colors.accent} /> : <Text style={[styles.headerActionText, { color: colors.accent, fontWeight: '600' }]}>{mode === 'save' ? '保存' : '接続'}</Text>}</Pressable>
          </View>
          <ScrollView ref={scrollRef} pointerEvents={busy ? 'none' : 'auto'} onLayout={scrollPasswordIntoView} contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'} contentContainerStyle={styles.content}>
            <View style={styles.intro}>
              <Text style={[styles.title, { color: colors.text }]}>{mode === 'save' ? 'いつもの接続先を。' : 'いつもの作業へ。'}</Text>
              <Text style={[styles.body, { color: colors.muted }]}>{mode === 'save' ? '接続先をこの端末に保存します。認証情報の保存は任意です。' : 'SSH の接続先と認証情報を入力してください。接続後に、サーバーのワークスペースが並びます。'}</Text>
            </View>
            {submissionError ? <Text accessibilityRole="alert" style={[styles.error, { color: colors.danger }]}>{submissionError}</Text> : null}
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
              {usingSavedCredential ? <View style={[styles.savedCredential, { backgroundColor: colors.surface }]}>
                <Text style={[styles.label, { color: colors.text }]}>認証情報を安全に保存済み</Text>
                <Text style={[styles.helper, { color: colors.muted }]}>保存済みの認証情報を使います。内容は画面に表示しません。</Text>
                <Pressable accessibilityRole="button" accessibilityLabel="Replace saved credentials" onPress={() => setReplaceCredential(true)} style={styles.replaceAction}><Text style={[styles.label, { color: colors.accent }]}>認証情報を入れ替える</Text></Pressable>
              </View> : authMethod === 'publicKey' ? <>
                <Field label="OpenSSH 秘密鍵" colors={colors} error={errors.privateKey} action={{ label: showPrivateKey ? '隠す' : '表示', accessibilityLabel: showPrivateKey ? 'Hide private key' : 'Show private key', onPress: () => setShowPrivateKey(value => !value) }}>
                  <View style={[styles.keyShell, { backgroundColor: colors.surface, borderColor: errors.privateKey ? colors.danger : colors.border }]}>
                    <TextInput ref={privateKeyRef} accessibilityLabel="Private OpenSSH key" testID="ssh-private-key" accessibilityValue={{ text: privateKey ? 'Private key entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" multiline caretHidden={!showPrivateKey} value={privateKey} onChangeText={value => { setPrivateKey(value); setErrors(current => ({ ...current, privateKey: undefined })); }} placeholder={showPrivateKey ? '-----BEGIN OPENSSH PRIVATE KEY-----' : undefined} selectionColor={showPrivateKey ? colors.accent : 'transparent'} style={[styles.keyInput, { color: showPrivateKey ? colors.text : 'transparent' }]} textAlignVertical="top" />
                    {!showPrivateKey ? <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" pointerEvents="none" style={styles.keyMask}><Text style={[styles.body, { color: colors.muted }]}>{privateKey ? '秘密鍵を入力しました' : '秘密鍵の全文を貼り付け'}</Text></View> : null}
                  </View>
                </Field>
                <Field label="鍵のパスフレーズ" colors={colors} optional action={{ label: showPassphrase ? '隠す' : '表示', accessibilityLabel: showPassphrase ? 'Hide passphrase' : 'Show passphrase', onPress: () => setShowPassphrase(value => !value) }}>
                  <TextInput accessibilityLabel="Key passphrase, optional" testID="ssh-passphrase" {...inputDefaults} importantForAutofill="no" value={passphrase} onChangeText={setPassphrase} onSubmitEditing={submit} placeholder="暗号化された鍵の場合のみ" returnKeyType="go" secureTextEntry={!showPassphrase} style={inputStyle} />
                </Field>
              </> : <>
                <Field label="SSH パスワード" colors={colors} error={errors.password} action={{ label: showPassword ? '隠す' : '表示', accessibilityLabel: showPassword ? 'Hide password' : 'Show password', onPress: () => setShowPassword(value => !value) }}>
                  <TextInput ref={passwordRef} accessibilityLabel="SSH password" testID="ssh-password" accessibilityValue={{ text: password ? 'Password entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" value={password} onChangeText={value => { setPassword(value); setErrors(current => ({ ...current, password: undefined })); }} onFocus={scrollPasswordIntoView} onSubmitEditing={submit} placeholder="SSH サーバーのパスワード" returnKeyType="go" secureTextEntry={!showPassword} style={[inputStyle, errors.password && { borderColor: colors.danger }]} />
                </Field>
              </>}
              {!usingSavedCredential ? <Text style={[styles.helper, { color: colors.muted }]}>{mode === 'save' && !saveCredential ? '認証情報は空欄のまま保存できます。接続するときに入力します。' : '接続・保存・キャンセル時に、認証情報を入力欄から消去します。'}</Text> : null}
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>この端末に保存</Text>
              {mode === 'connect' ? <View style={styles.switchRow}><Text style={[styles.switchLabel, { color: colors.text }]}>接続先を保存</Text><Switch accessibilityLabel="Save server profile" testID="save-server-profile" value={saveServer} onValueChange={value => { setSaveServer(value); if (!value) setSaveCredential(false); }} disabled={busy || Boolean(initialProfile)} trackColor={{ true: colors.accentFill }} /></View> : null}
              {saveServer || mode === 'save' ? <>
                <Field label="表示名" colors={colors} optional error={errors.name}><TextInput ref={nameRef} accessibilityLabel="Server name" testID="server-profile-name" value={name} onChangeText={setName} placeholder={host.trim() || '自宅のサーバー'} placeholderTextColor={colors.placeholder} selectionColor={colors.accent} autoComplete="off" returnKeyType="done" onSubmitEditing={Keyboard.dismiss} style={inputStyle} /></Field>
                <View style={styles.switchRow}><Text style={[styles.switchLabel, { color: colors.text }]}>認証情報も保存</Text><Switch accessibilityLabel="Save credentials securely" testID="save-credentials" value={saveCredential} onValueChange={setSaveCredential} disabled={busy} trackColor={{ true: colors.accentFill }} /></View>
                <Text style={[styles.helper, { color: colors.muted }]}>{saveCredential ? '秘密鍵・パスワードは OS の安全な保存領域で保護します。次回から入力せず接続できます。' : initialProfile?.credentialSaved ? '保存時に、この接続先の保存済み認証情報を削除します。' : '認証情報は保存しません。アプリを終了した後は、接続時に再入力します。'}</Text>
                {initialProfile?.credentialSaved && !credentialMatches ? <Text style={[styles.helper, { color: colors.muted }]}>接続先・ユーザー・認証方式を変えたため、以前の認証情報は引き継ぎません。</Text> : null}
              </> : null}
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
  switchRow: { minHeight: 52, flexDirection: 'row', gap: 16, alignItems: 'center', justifyContent: 'space-between' },
  switchLabel: { flex: 1, fontSize: 16, lineHeight: 24 },
  savedCredential: { padding: 16, gap: 8, borderRadius: 12, borderCurve: 'continuous' },
  replaceAction: { minHeight: 44, justifyContent: 'center' },
  pressed: { opacity: .6 },
});
