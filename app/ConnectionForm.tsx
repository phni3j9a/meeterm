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
import { DARK, MONO, useReducedMotion } from './ui';
import type { Palette } from './ui';

type AuthMethod = 'publicKey' | 'password';
type FormErrors = Partial<Record<'name' | 'host' | 'port' | 'username' | 'privateKey' | 'password' | 'runtime', string>>;

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
      <Text style={[styles.label, { color: colors.text }]}>{label}{optional ? <Text style={{ color: colors.muted, fontWeight: '400' }}> · Optional</Text> : null}</Text>
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
  const reducedMotion = useReducedMotion();
  const [name, setName] = useState('');
  const [host, setHost] = useState('');
  const [port, setPort] = useState('22');
  const [username, setUsername] = useState('');
  const [authMethod, setAuthMethod] = useState<AuthMethod>('publicKey');
  const [backend, setBackend] = useState<'tmux' | 'herdr'>('tmux');
  const [runtime, setRuntime] = useState('');
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
  const runtimeRef = useRef<TextInput>(null);
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
      setBackend(initialProfile?.backend ?? 'tmux');
      setRuntime(initialProfile?.runtime ?? '');
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

  const dirty = name !== (initialProfile?.name ?? '') || host !== (initialProfile?.host ?? '')
      || port !== String(initialProfile?.port ?? 22) || username !== (initialProfile?.username ?? '')
      || authMethod !== (initialProfile?.authMethod ?? 'publicKey') || Boolean(privateKey || passphrase || password)
      || !saveServer || saveCredential !== Boolean(initialProfile?.credentialSaved)
      || backend !== (initialProfile?.backend ?? 'tmux') || runtime !== (initialProfile?.runtime ?? '');
  const close = useCallback(() => {
    if (submitting.current) return;
    if (!dirty) { discard(); return; }
    Alert.alert('Discard changes?', 'Your changes have not been saved.', [
      { text: 'Keep editing', style: 'cancel' },
      { text: 'Discard', style: 'destructive', onPress: discard },
    ]);
  }, [dirty, discard]);

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
    const sessionName = backend === 'herdr' ? runtime.trim() : '';
    if (sessionName && (!/^[A-Za-z0-9._-]{1,64}$/.test(sessionName) || sessionName === '.' || sessionName === '..')) {
      nextErrors.runtime = 'Use up to 64 letters, numbers, periods, hyphens, or underscores.';
    }
    if (name.trim().length > 80 || /[\x00-\x1f\x7f]/.test(name)) nextErrors.name = 'Use up to 80 characters, without control characters.';
    if (!trimmedHost || /[\s\x00-\x1f\x7f]/.test(trimmedHost)) {
      nextErrors.host = 'Enter a hostname or IP address without spaces.';
    }
    if (!/^\d+$/.test(port) || parsedPort < 1 || parsedPort > 65535) {
      nextErrors.port = 'Enter a port from 1 to 65535.';
    }
    if (!trimmedUsername || /[\s\x00-\x1f\x7f]/.test(trimmedUsername)) {
      nextErrors.username = 'Enter your SSH username without spaces.';
    }
    const needsCredential = !usingSavedCredential && (mode === 'connect' || saveCredential || Boolean(privateKey || password || passphrase));
    if (needsCredential && authMethod === 'publicKey') {
      if (!trimmedKey.startsWith('-----BEGIN OPENSSH PRIVATE KEY-----') || !trimmedKey.endsWith('-----END OPENSSH PRIVATE KEY-----')) {
        nextErrors.privateKey = 'Paste an OpenSSH private key, including its BEGIN and END lines.';
      }
    } else if (needsCredential && (!password || password.includes('\u0000'))) {
      nextErrors.password = 'Enter your SSH password.';
    }
    if (Object.keys(nextErrors).length) {
      setErrors(nextErrors);
      const target = nextErrors.name ? nameRef : nextErrors.host ? hostRef
        : nextErrors.port ? portRef
          : nextErrors.username ? usernameRef
            : nextErrors.runtime ? runtimeRef : nextErrors.privateKey ? privateKeyRef : passwordRef;
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
        profile: { id: initialProfile?.id ?? '', name: name.trim() || trimmedHost.slice(0, 80), host: trimmedHost, port: parsedPort, username: trimmedUsername, authMethod, backend, runtime: sessionName },
        credential, saveProfile: mode === 'save' || saveServer, saveCredential: saveServer && saveCredential,
        keepCredential: saveServer && usingSavedCredential, connect: mode === 'connect',
      });
      if (!accepted) setSubmissionError('Could not save or connect. Check the address and enter your credentials again.');
    } catch {
      setSubmissionError('Could not save or connect. Enter your credentials again and retry.');
    } finally { submitting.current = false; setBusy(false); }
  }, [authMethod, backend, runtime, clearSecrets, host, initialProfile, mode, name, onSubmit, passphrase, password, port, privateKey, saveCredential, saveServer, username, usingSavedCredential]);

  const inputStyle = [styles.input, { color: colors.text, backgroundColor: colors.elevated, borderColor: colors.border }];
  const inputDefaults = { autoCapitalize: 'none' as const, autoComplete: 'off' as const, autoCorrect: false, spellCheck: false, placeholderTextColor: colors.placeholder, selectionColor: colors.accent };

  return <Modal visible={visible} animationType={reducedMotion ? 'fade' : 'slide'} presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'} allowSwipeDismissal={!busy && !dirty} onRequestClose={close} onDismiss={() => { clearSecrets(); onDismiss?.(); }} onShow={() => { if (Platform.OS === 'android') StatusBar.setBarStyle(colors === DARK ? 'light-content' : 'dark-content'); }}>
    <SafeAreaProvider>
      <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.root, { backgroundColor: colors.background }]}>
        <StatusBar hidden={false} barStyle={colors === DARK ? 'light-content' : 'dark-content'} backgroundColor={colors.background} />
        <KeyboardAvoidingView behavior={Platform.OS === 'ios' ? 'padding' : 'height'} style={styles.root}>
          <View style={[styles.header, { borderBottomColor: colors.border }]}>
            <Pressable accessibilityRole="button" accessibilityLabel="Cancel" disabled={busy} onPress={close} style={({ pressed }) => [styles.headerAction, pressed && styles.pressed, busy && { opacity: .45 }]}><Text style={[styles.headerActionText, { color: colors.accent }]}>Cancel</Text></Pressable>
            <Text accessibilityRole="header" style={[styles.headerTitle, { color: colors.text }]}>{mode === 'save' ? initialProfile ? 'Edit server' : 'Add server' : 'Connect to server'}</Text>
            <Pressable accessibilityRole="button" accessibilityLabel={mode === 'save' ? 'Save server' : 'Connect'} accessibilityState={{ disabled: busy, busy }} disabled={busy} testID="ssh-submit" onPress={submit} style={({ pressed }) => [styles.headerAction, styles.headerActionEnd, pressed && styles.pressed]}>{busy ? <ActivityIndicator color={colors.accent} /> : <Text style={[styles.headerActionText, { color: colors.accent, fontWeight: '600' }]}>{mode === 'save' ? 'Save' : 'Connect'}</Text>}</Pressable>
          </View>
          <ScrollView ref={scrollRef} pointerEvents={busy ? 'none' : 'auto'} onLayout={scrollPasswordIntoView} contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'} contentContainerStyle={styles.content}>
            <View style={styles.intro}>
              <Text style={[styles.body, { color: colors.muted }]}>{mode === 'save' ? 'Save a server on this device. You can add credentials now or when you connect.' : 'Connect over SSH to open the workspaces on your server.'}</Text>
            </View>
            {submissionError ? <Text accessibilityRole="alert" style={[styles.error, { color: colors.danger }]}>{submissionError}</Text> : null}
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>Server</Text>
              <View style={styles.hostPortRow}>
                <View style={styles.hostColumn}>
                  <Field label="Host" colors={colors} error={errors.host}>
                    <TextInput ref={hostRef} accessibilityLabel="Host" testID="ssh-host" {...inputDefaults} value={host} onChangeText={value => { setHost(value); setErrors(current => ({ ...current, host: undefined })); }} onSubmitEditing={() => portRef.current?.focus()} placeholder="server.example.com" returnKeyType="next" style={[inputStyle, errors.host && { borderColor: colors.danger }]} />
                  </Field>
                </View>
                <View style={styles.portColumn}>
                  <Field label="Port" colors={colors} error={errors.port}>
                    <TextInput ref={portRef} accessibilityLabel="Port" testID="ssh-port" autoComplete="off" inputMode="numeric" keyboardType="number-pad" maxLength={5} value={port} onChangeText={value => { setPort(value.replace(/[^0-9]/g, '')); setErrors(current => ({ ...current, port: undefined })); }} onSubmitEditing={() => usernameRef.current?.focus()} returnKeyType="next" selectionColor={colors.accent} style={[inputStyle, { fontVariant: ['tabular-nums'] }, errors.port && { borderColor: colors.danger }]} />
                  </Field>
                </View>
              </View>
              <Field label="Username" colors={colors} error={errors.username}>
                <TextInput ref={usernameRef} accessibilityLabel="Username" testID="ssh-username" {...inputDefaults} value={username} onChangeText={value => { setUsername(value); setErrors(current => ({ ...current, username: undefined })); }} onSubmitEditing={() => authMethod === 'publicKey' ? privateKeyRef.current?.focus() : passwordRef.current?.focus()} placeholder="developer" returnKeyType="next" style={[inputStyle, errors.username && { borderColor: colors.danger }]} />
              </Field>
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>Workspace runtime</Text>
              <View accessibilityRole="radiogroup" accessibilityLabel="Workspace backend" style={[styles.authChoices, { backgroundColor: colors.surface, borderColor: colors.border }]}>
                {(['tmux', 'herdr'] as const).map(value => <Pressable key={value} accessibilityRole="radio" accessibilityLabel={`${value} backend`} testID={`ssh-backend-${value}`} accessibilityState={{ selected: backend === value, checked: backend === value }} disabled={busy} onPress={() => { setBackend(value); setErrors(current => ({ ...current, runtime: undefined })); }} style={({ pressed }) => [styles.authChoice, backend === value && { backgroundColor: colors.elevated }, pressed && styles.pressed]}>
                  <Text style={[styles.authChoiceText, { color: backend === value ? colors.accent : colors.muted }]}>{value === 'herdr' ? 'Herdr' : 'tmux'}</Text>
                </Pressable>)}
              </View>
              {backend === 'herdr' ? <>
                <Field label="Herdr session" colors={colors} optional error={errors.runtime}>
                  <TextInput ref={runtimeRef} accessibilityLabel="Herdr session name" testID="ssh-runtime" {...inputDefaults} value={runtime} maxLength={64} onChangeText={value => { setRuntime(value); setErrors(current => ({ ...current, runtime: undefined })); }} placeholder="default" returnKeyType="next" onSubmitEditing={() => authMethod === 'publicKey' ? privateKeyRef.current?.focus() : passwordRef.current?.focus()} style={[inputStyle, errors.runtime && { borderColor: colors.danger }]} />
                </Field>
                <Text style={[styles.helper, { color: colors.muted }]}>Connect to an existing Herdr session. Leave blank to use default.</Text>
              </> : <Text style={[styles.helper, { color: colors.muted }]}>Uses the meeterm session in tmux. Your computer can open the same workspaces.</Text>}
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>Authentication</Text>
              <View accessibilityRole="radiogroup" accessibilityLabel="Authentication method" style={[styles.authChoices, { backgroundColor: colors.surface, borderColor: colors.border }]}>
                <Pressable accessibilityRole="radio" accessibilityLabel="Private key authentication" accessibilityState={{ selected: authMethod === 'publicKey', checked: authMethod === 'publicKey' }} testID="ssh-auth-public-key" onPress={() => changeAuthMethod('publicKey')} style={({ pressed }) => [styles.authChoice, authMethod === 'publicKey' && { backgroundColor: colors.elevated }, pressed && styles.pressed]}>
                  <Text style={[styles.authChoiceText, { color: authMethod === 'publicKey' ? colors.accent : colors.muted }]}>Private key</Text>
                </Pressable>
                <Pressable accessibilityRole="radio" accessibilityLabel="Password authentication" accessibilityState={{ selected: authMethod === 'password', checked: authMethod === 'password' }} testID="ssh-auth-password" onPress={() => changeAuthMethod('password')} style={({ pressed }) => [styles.authChoice, authMethod === 'password' && { backgroundColor: colors.elevated }, pressed && styles.pressed]}>
                  <Text style={[styles.authChoiceText, { color: authMethod === 'password' ? colors.accent : colors.muted }]}>Password</Text>
                </Pressable>
              </View>
              {usingSavedCredential ? <View style={[styles.savedCredential, { backgroundColor: colors.surface }]}>
                <Text style={[styles.label, { color: colors.text }]}>Credentials saved securely</Text>
                <Text style={[styles.helper, { color: colors.muted }]}>Connect using the credentials saved on this device.</Text>
                <Pressable accessibilityRole="button" accessibilityLabel="Replace saved credentials" onPress={() => setReplaceCredential(true)} style={styles.replaceAction}><Text style={[styles.label, { color: colors.accent }]}>Replace credentials</Text></Pressable>
              </View> : authMethod === 'publicKey' ? <>
                <Field label="OpenSSH private key" colors={colors} error={errors.privateKey} action={{ label: showPrivateKey ? 'Hide' : 'Show', accessibilityLabel: showPrivateKey ? 'Hide private key' : 'Show private key', onPress: () => setShowPrivateKey(value => !value) }}>
                  <View style={[styles.keyShell, { backgroundColor: colors.surface, borderColor: errors.privateKey ? colors.danger : colors.border }]}>
                    <TextInput ref={privateKeyRef} accessibilityLabel="Private OpenSSH key" testID="ssh-private-key" accessibilityValue={{ text: privateKey ? 'Private key entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" multiline caretHidden={!showPrivateKey} value={privateKey} onChangeText={value => { setPrivateKey(value); setErrors(current => ({ ...current, privateKey: undefined })); }} placeholder={showPrivateKey ? '-----BEGIN OPENSSH PRIVATE KEY-----' : undefined} selectionColor={showPrivateKey ? colors.accent : 'transparent'} style={[styles.keyInput, { color: showPrivateKey ? colors.text : 'transparent' }]} textAlignVertical="top" />
                    {!showPrivateKey ? <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" pointerEvents="none" style={styles.keyMask}><Text style={[styles.body, { color: colors.muted }]}>{privateKey ? 'Private key entered' : 'Paste your complete private key'}</Text></View> : null}
                  </View>
                </Field>
                <Field label="Key passphrase" colors={colors} optional action={{ label: showPassphrase ? 'Hide' : 'Show', accessibilityLabel: showPassphrase ? 'Hide passphrase' : 'Show passphrase', onPress: () => setShowPassphrase(value => !value) }}>
                  <TextInput accessibilityLabel="Key passphrase, optional" testID="ssh-passphrase" {...inputDefaults} importantForAutofill="no" value={passphrase} onChangeText={setPassphrase} onSubmitEditing={submit} placeholder="For encrypted keys only" returnKeyType="go" secureTextEntry={!showPassphrase} style={inputStyle} />
                </Field>
              </> : <>
                <Field label="SSH password" colors={colors} error={errors.password} action={{ label: showPassword ? 'Hide' : 'Show', accessibilityLabel: showPassword ? 'Hide password' : 'Show password', onPress: () => setShowPassword(value => !value) }}>
                  <TextInput ref={passwordRef} accessibilityLabel="SSH password" testID="ssh-password" accessibilityValue={{ text: password ? 'Password entered' : 'Empty' }} {...inputDefaults} importantForAutofill="no" value={password} onChangeText={value => { setPassword(value); setErrors(current => ({ ...current, password: undefined })); }} onFocus={scrollPasswordIntoView} onSubmitEditing={submit} placeholder="Your SSH password" returnKeyType="go" secureTextEntry={!showPassword} style={[inputStyle, errors.password && { borderColor: colors.danger }]} />
                </Field>
              </>}
              {!usingSavedCredential ? <Text style={[styles.helper, { color: colors.muted }]}>{mode === 'save' && !saveCredential ? 'You can leave credentials blank and enter them when you connect.' : 'Credentials are cleared from this form when you connect, save, or cancel.'}</Text> : null}
            </View>
            <View style={styles.section}>
              <Text style={[styles.sectionLabel, { color: colors.muted }]}>Save on this device</Text>
              {mode === 'connect' ? <View style={styles.switchRow}><Text style={[styles.switchLabel, { color: colors.text }]}>Save server</Text><Switch accessibilityLabel="Save server profile" testID="save-server-profile" value={saveServer} onValueChange={value => { setSaveServer(value); if (!value) setSaveCredential(false); }} disabled={busy || Boolean(initialProfile)} trackColor={{ true: colors.accentFill }} /></View> : null}
              {saveServer || mode === 'save' ? <>
                <Field label="Display name" colors={colors} optional error={errors.name}><TextInput ref={nameRef} accessibilityLabel="Server name" testID="server-profile-name" value={name} onChangeText={setName} placeholder={host.trim() || 'Home server'} placeholderTextColor={colors.placeholder} selectionColor={colors.accent} autoComplete="off" returnKeyType="done" onSubmitEditing={Keyboard.dismiss} style={inputStyle} /></Field>
                <View style={styles.switchRow}><Text style={[styles.switchLabel, { color: colors.text }]}>Remember credentials</Text><Switch accessibilityLabel="Save credentials securely" testID="save-credentials" value={saveCredential} onValueChange={setSaveCredential} disabled={busy} trackColor={{ true: colors.accentFill }} /></View>
                <Text style={[styles.helper, { color: colors.muted }]}>{saveCredential ? 'Protected by your device secure storage. Connect next time without entering credentials.' : initialProfile?.credentialSaved ? 'Saving will remove the credentials previously stored for this server.' : 'Credentials will not be saved. Enter them again after restarting the app.'}</Text>
                {initialProfile?.credentialSaved && !credentialMatches ? <Text style={[styles.helper, { color: colors.muted }]}>The server, username, or authentication method changed. Enter credentials for this connection.</Text> : null}
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
  authChoices: { minHeight: 52, padding: 4, gap: 4, flexDirection: 'row', borderWidth: 1, borderRadius: 12, borderCurve: 'continuous', overflow: 'hidden' },
  authChoice: { flex: 1, minHeight: 44, alignItems: 'center', justifyContent: 'center', paddingHorizontal: 12, borderRadius: 8, borderCurve: 'continuous' },
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
