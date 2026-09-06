import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  Alert,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  StyleSheet,
  Text,
  TextInput,
  View,
  useColorScheme,
} from 'react-native';
import {
  SafeAreaProvider,
  SafeAreaView,
} from 'react-native-safe-area-context';

import MeetermTerminal, { TerminalView } from './modules/meeterm-terminal';
import type {
  SshConnectOptions,
  SshConnectionState,
  TmuxPane,
} from './modules/meeterm-terminal';

const TERMINAL_ID = 'poc-main';

const INITIAL_CONNECTION_STATE: SshConnectionState = {
  state: 'Disconnected',
  host: '',
  port: 0,
  fingerprint: '',
  algorithm: '',
  knownFingerprint: '',
  errorCode: '',
  errorMessage: '',
};

const LIGHT_COLORS = {
  background: '#f2f2f7',
  surface: '#ffffff',
  field: '#f2f2f7',
  border: '#d8d8df',
  label: '#111318',
  secondaryLabel: '#666a73',
  placeholder: '#8c9099',
  accent: '#246fc5',
  danger: '#b42318',
};

const DARK_COLORS = {
  background: '#0b0d10',
  surface: '#171a1f',
  field: '#22262d',
  border: '#303640',
  label: '#f1f4f8',
  secondaryLabel: '#9ba3ae',
  placeholder: '#737c89',
  accent: '#65aaf3',
  danger: '#ff8c83',
};

type FormErrors = Partial<
  Record<'host' | 'port' | 'username' | 'privateKey', string>
>;

type FieldProps = {
  label: string;
  optional?: boolean;
  error?: string;
  action?: {
    label: string;
    accessibilityLabel: string;
    onPress: () => void;
  };
  children: ReactNode;
};

type ConnectionModalProps = {
  visible: boolean;
  onClose: () => void;
  onSubmit: (options: SshConnectOptions) => void;
};

function Field({ label, optional, error, action, children }: FieldProps) {
  const colorScheme = useColorScheme();
  const colors = colorScheme === 'light' ? LIGHT_COLORS : DARK_COLORS;

  return (
    <View style={layoutStyles.fieldBlock}>
      <View style={layoutStyles.fieldLabelRow}>
        <Text style={[layoutStyles.fieldLabel, { color: colors.label }]}>
          {label}
          {optional ? (
            <Text
              style={[
                layoutStyles.optionalLabel,
                { color: colors.secondaryLabel },
              ]}
            >
              {' '}
              · Optional
            </Text>
          ) : null}
        </Text>
        {action ? (
          <Pressable
            accessibilityLabel={action.accessibilityLabel}
            accessibilityRole="button"
            hitSlop={8}
            onPress={action.onPress}
            style={({ pressed }) => [
              layoutStyles.fieldAction,
              pressed && layoutStyles.plainActionPressed,
            ]}
          >
            <Text
              style={[layoutStyles.fieldActionText, { color: colors.accent }]}
            >
              {action.label}
            </Text>
          </Pressable>
        ) : null}
      </View>
      {children}
      {error ? (
        <Text
          accessibilityLiveRegion="polite"
          style={[layoutStyles.fieldError, { color: colors.danger }]}
        >
          {error}
        </Text>
      ) : null}
    </View>
  );
}

function ConnectionModal({ visible, onClose, onSubmit }: ConnectionModalProps) {
  const colorScheme = useColorScheme();
  const colors = colorScheme === 'light' ? LIGHT_COLORS : DARK_COLORS;
  const styles = useMemo(() => createModalStyles(colors), [colors]);

  const [host, setHost] = useState('');
  const [port, setPort] = useState('22');
  const [username, setUsername] = useState('');
  const [privateKey, setPrivateKey] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const [showPassphrase, setShowPassphrase] = useState(false);
  const [errors, setErrors] = useState<FormErrors>({});

  const hostRef = useRef<TextInput>(null);
  const portRef = useRef<TextInput>(null);
  const usernameRef = useRef<TextInput>(null);
  const privateKeyRef = useRef<TextInput>(null);
  const submittingRef = useRef(false);

  const clearSecrets = useCallback(() => {
    setPrivateKey('');
    setPassphrase('');
    setShowPrivateKey(false);
    setShowPassphrase(false);
  }, []);

  useEffect(() => {
    if (visible) {
      submittingRef.current = false;
      return;
    }

    clearSecrets();
    setErrors({});
  }, [clearSecrets, visible]);

  const close = useCallback(() => {
    submittingRef.current = false;
    clearSecrets();
    setErrors({});
    onClose();
  }, [clearSecrets, onClose]);

  const updateHost = useCallback((value: string) => {
    setHost(value);
    setErrors((current) => ({ ...current, host: undefined }));
  }, []);

  const updatePort = useCallback((value: string) => {
    setPort(value.replace(/[^0-9]/g, ''));
    setErrors((current) => ({ ...current, port: undefined }));
  }, []);

  const updateUsername = useCallback((value: string) => {
    setUsername(value);
    setErrors((current) => ({ ...current, username: undefined }));
  }, []);

  const updatePrivateKey = useCallback((value: string) => {
    setPrivateKey(value);
    setErrors((current) => ({ ...current, privateKey: undefined }));
  }, []);

  const submit = useCallback(() => {
    if (submittingRef.current) {
      return;
    }

    const trimmedHost = host.trim();
    const parsedPort = Number(port);
    const trimmedUsername = username.trim();
    const trimmedPrivateKey = privateKey.trim();
    const nextErrors: FormErrors = {};

    if (!trimmedHost) {
      nextErrors.host = 'Enter a hostname or IP address.';
    } else if (/\s/.test(trimmedHost)) {
      nextErrors.host = 'The host cannot contain spaces.';
    }

    if (!/^\d+$/.test(port) || parsedPort < 1 || parsedPort > 65535) {
      nextErrors.port = 'Use a port from 1 to 65535.';
    }

    if (!trimmedUsername) {
      nextErrors.username = 'Enter the SSH username.';
    } else if (/\s/.test(trimmedUsername)) {
      nextErrors.username = 'The username cannot contain spaces.';
    }

    if (
      !trimmedPrivateKey.startsWith('-----BEGIN OPENSSH PRIVATE KEY-----') ||
      !trimmedPrivateKey.endsWith('-----END OPENSSH PRIVATE KEY-----')
    ) {
      nextErrors.privateKey =
        'Paste a complete OpenSSH private key, including its BEGIN and END lines.';
    }

    if (Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      if (nextErrors.host) {
        hostRef.current?.focus();
      } else if (nextErrors.port) {
        portRef.current?.focus();
      } else if (nextErrors.username) {
        usernameRef.current?.focus();
      } else {
        privateKeyRef.current?.focus();
      }
      return;
    }

    submittingRef.current = true;
    const options: SshConnectOptions = {
      host: trimmedHost,
      port: parsedPort,
      username: trimmedUsername,
      privateKey: trimmedPrivateKey,
      passphrase,
    };

    // Authentication material is never persisted. Remove it from React state
    // before handing this one-shot command to the native control plane.
    clearSecrets();
    setErrors({});
    onSubmit(options);
  }, [clearSecrets, host, onSubmit, passphrase, port, privateKey, username]);

  return (
    <Modal
      animationType="slide"
      onRequestClose={close}
      presentationStyle={Platform.OS === 'ios' ? 'pageSheet' : 'fullScreen'}
      visible={visible}
    >
      <SafeAreaProvider>
        <SafeAreaView edges={['top']} style={styles.modalRoot}>
          <StatusBar
            backgroundColor={colors.background}
            barStyle={colorScheme === 'light' ? 'dark-content' : 'light-content'}
            hidden={false}
          />
          <View style={layoutStyles.modalKeyboardView}>
          <View style={[layoutStyles.modalHeader, styles.modalHeader]}>
            <Pressable
              accessibilityRole="button"
              onPress={close}
              style={({ pressed }) => [
                layoutStyles.headerAction,
                pressed && layoutStyles.plainActionPressed,
              ]}
            >
              <Text style={styles.headerActionText}>Cancel</Text>
            </Pressable>
            <Text accessibilityRole="header" style={styles.modalTitle}>
              SSH connection
            </Text>
            <Pressable
              accessibilityHint="Connect using the entered SSH details"
              accessibilityRole="button"
              onPress={submit}
              style={({ pressed }) => [
                layoutStyles.headerAction,
                layoutStyles.headerActionEnd,
                pressed && layoutStyles.plainActionPressed,
              ]}
            >
              <Text style={styles.headerActionStrong}>Connect</Text>
            </Pressable>
          </View>

          <ScrollView
            automaticallyAdjustKeyboardInsets
            contentContainerStyle={layoutStyles.formContent}
            contentInsetAdjustmentBehavior="automatic"
            keyboardDismissMode={Platform.OS === 'ios' ? 'interactive' : 'on-drag'}
            keyboardShouldPersistTaps="handled"
          >
            <View style={layoutStyles.introBlock}>
              <Text style={styles.introTitle}>Connect with a private key</Text>
              <Text style={styles.introBody}>
                Public key authentication only. Connection details stay in this
                form for this app session. The key is cleared from this form on
                connect or cancel; native memory retains it for reconnect until
                the app closes.
              </Text>
            </View>

            <View style={layoutStyles.sectionBlock}>
              <Text style={styles.sectionLabel}>SERVER</Text>
              <View style={styles.sectionCard}>
                <View style={layoutStyles.hostPortRow}>
                  <View style={layoutStyles.hostColumn}>
                    <Field label="Host" error={errors.host}>
                      <TextInput
                        ref={hostRef}
                        accessibilityLabel="Host"
                        autoCapitalize="none"
                        autoComplete="off"
                        autoCorrect={false}
                        onChangeText={updateHost}
                        onSubmitEditing={() => portRef.current?.focus()}
                        placeholder="server.example.com"
                        placeholderTextColor={colors.placeholder}
                        returnKeyType="next"
                        selectionColor={colors.accent}
                        spellCheck={false}
                        style={[styles.input, errors.host && styles.inputErrorBorder]}
                        value={host}
                      />
                    </Field>
                  </View>
                  <View style={layoutStyles.portColumn}>
                    <Field label="Port" error={errors.port}>
                      <TextInput
                        ref={portRef}
                        accessibilityLabel="Port"
                        autoComplete="off"
                        inputMode="numeric"
                        keyboardType="number-pad"
                        maxLength={5}
                        onChangeText={updatePort}
                        onSubmitEditing={() => usernameRef.current?.focus()}
                        placeholder="22"
                        placeholderTextColor={colors.placeholder}
                        returnKeyType="next"
                        selectionColor={colors.accent}
                        style={[
                          styles.input,
                          layoutStyles.numericInput,
                          errors.port && styles.inputErrorBorder,
                        ]}
                        value={port}
                      />
                    </Field>
                  </View>
                </View>

                <Field label="Username" error={errors.username}>
                  <TextInput
                    ref={usernameRef}
                    accessibilityLabel="Username"
                    autoCapitalize="none"
                    autoComplete="off"
                    autoCorrect={false}
                    onChangeText={updateUsername}
                    onSubmitEditing={() => privateKeyRef.current?.focus()}
                    placeholder="developer"
                    placeholderTextColor={colors.placeholder}
                    returnKeyType="next"
                    selectionColor={colors.accent}
                    spellCheck={false}
                    style={[styles.input, errors.username && styles.inputErrorBorder]}
                    value={username}
                  />
                </Field>
              </View>
            </View>

            <View style={layoutStyles.sectionBlock}>
              <Text style={styles.sectionLabel}>AUTHENTICATION</Text>
              <View style={styles.sectionCard}>
                <Field
                  action={{
                    label: showPrivateKey ? 'Hide' : 'Show',
                    accessibilityLabel: showPrivateKey
                      ? 'Hide private key'
                      : 'Show private key',
                    onPress: () => setShowPrivateKey((current) => !current),
                  }}
                  error={errors.privateKey}
                  label="Private OpenSSH key"
                >
                  <View
                    style={[
                      styles.keyInputShell,
                      errors.privateKey && styles.inputErrorBorder,
                    ]}
                  >
                    <TextInput
                      ref={privateKeyRef}
                      accessibilityLabel="Private OpenSSH key"
                      accessibilityValue={{
                        text: privateKey ? 'Private key entered' : 'Empty',
                      }}
                      autoCapitalize="none"
                      autoComplete="off"
                      autoCorrect={false}
                      caretHidden={!showPrivateKey}
                      importantForAutofill="no"
                      multiline
                      onChangeText={updatePrivateKey}
                      placeholder={
                        showPrivateKey
                          ? '-----BEGIN OPENSSH PRIVATE KEY-----'
                          : undefined
                      }
                      placeholderTextColor={colors.placeholder}
                      selectionColor={
                        showPrivateKey ? colors.accent : 'transparent'
                      }
                      spellCheck={false}
                      style={[
                        styles.keyInput,
                        !showPrivateKey && layoutStyles.concealedKeyInput,
                      ]}
                      textAlignVertical="top"
                      value={privateKey}
                    />
                    {!showPrivateKey ? (
                      <View
                        accessibilityElementsHidden
                        importantForAccessibility="no-hide-descendants"
                        pointerEvents="none"
                        style={layoutStyles.keyMask}
                      >
                        <Text style={styles.keyMaskText}>
                          {privateKey
                            ? `Private key entered · ${privateKey.length.toLocaleString()} characters`
                            : 'Paste the complete private key'}
                        </Text>
                      </View>
                    ) : null}
                  </View>
                  <Text style={styles.helperText}>
                    Paste the OpenSSH key. Nothing is saved, and the field is
                    cleared immediately after submission.
                  </Text>
                </Field>

                <Field
                  action={{
                    label: showPassphrase ? 'Hide' : 'Show',
                    accessibilityLabel: showPassphrase
                      ? 'Hide passphrase'
                      : 'Show passphrase',
                    onPress: () => setShowPassphrase((current) => !current),
                  }}
                  label="Key passphrase"
                  optional
                >
                  <TextInput
                    accessibilityLabel="Key passphrase, optional"
                    autoCapitalize="none"
                    autoComplete="off"
                    autoCorrect={false}
                    importantForAutofill="no"
                    onChangeText={setPassphrase}
                    onSubmitEditing={submit}
                    placeholder="Enter only if the key is encrypted"
                    placeholderTextColor={colors.placeholder}
                    returnKeyType="go"
                    secureTextEntry={!showPassphrase}
                    selectionColor={colors.accent}
                    spellCheck={false}
                    style={styles.input}
                    value={passphrase}
                  />
                </Field>
              </View>
            </View>
          </ScrollView>
          </View>
        </SafeAreaView>
      </SafeAreaProvider>
    </Modal>
  );
}

function connectionPresentation(connection: SshConnectionState) {
  switch (connection.state) {
    case 'Connecting':
      return { label: 'Connecting…', tone: 'pending' as const };
    case 'HostKeyPending':
      return { label: 'Verify host key', tone: 'pending' as const };
    case 'Authenticating':
      return { label: 'Authenticating…', tone: 'pending' as const };
    case 'OpeningPty':
      return { label: 'Opening terminal…', tone: 'pending' as const };
    case 'AttachingTmux':
      return { label: 'Opening workspace…', tone: 'pending' as const };
    case 'Synchronizing':
      return { label: 'Restoring terminals…', tone: 'pending' as const };
    case 'Reconnecting':
      return { label: 'Reconnecting…', tone: 'pending' as const };
    case 'Ready':
      return { label: 'Connected', tone: 'ready' as const };
    case 'Closing':
      return { label: 'Disconnecting…', tone: 'pending' as const };
    case 'Failed':
      return { label: 'Connection failed', tone: 'failed' as const };
    default:
      return { label: 'Not connected', tone: 'idle' as const };
  }
}

function hostKeyChangeId(connection: SshConnectionState) {
  if (connection.errorCode !== 'host_key_changed') {
    return '';
  }

  return [
    connection.host,
    connection.port,
    connection.algorithm,
    connection.knownFingerprint,
    connection.fingerprint,
  ].join('|');
}

function printable(value: string) {
  return value || '(unavailable)';
}

export default function App() {
  const [panes, setPanes] = useState<TmuxPane[]>([]);
  const [connection, setConnection] = useState<SshConnectionState>(
    INITIAL_CONNECTION_STATE,
  );
  const [connectionModalVisible, setConnectionModalVisible] = useState(false);
  const [controlMessage, setControlMessage] = useState('');
  const [removedHostKeyId, setRemovedHostKeyId] = useState('');
  const shownHostKeyPrompt = useRef('');
  const commandPending = useRef(false);
  const selectionVersion = useRef(0);

  useEffect(() => {
    let mounted = true;
    let polling = false;

    const refreshConnection = async () => {
      if (polling) {
        return;
      }

      polling = true;
      const version = selectionVersion.current;
      try {
        const next = await MeetermTerminal.getConnectionState(TERMINAL_ID);
        const session = await MeetermTerminal.getSessionState(TERMINAL_ID);
        if (mounted) {
          setConnection(next);
          if (version === selectionVersion.current && !commandPending.current) {
            setPanes(session.panes);
          }
        }
      } catch {
        if (mounted) {
          setControlMessage('Connection status is temporarily unavailable.');
        }
      } finally {
        polling = false;
      }
    };

    void refreshConnection();
    const interval = setInterval(() => {
      void refreshConnection();
    }, 1000);

    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    if (connection.state !== 'Disconnected' && connection.state !== 'Failed') {
      setControlMessage('');
    }

    const currentChangeId = hostKeyChangeId(connection);
    setRemovedHostKeyId((removed) =>
      removed && removed !== currentChangeId ? '' : removed,
    );
  }, [connection]);

  useEffect(() => {
    if (
      connection.state !== 'HostKeyPending' ||
      !connection.fingerprint ||
      !connection.host ||
      !connection.port
    ) {
      if (connection.state !== 'HostKeyPending') {
        shownHostKeyPrompt.current = '';
      }
      return;
    }

    const promptId = [
      connection.host,
      connection.port,
      connection.algorithm,
      connection.fingerprint,
    ].join('|');
    if (shownHostKeyPrompt.current === promptId) {
      return;
    }
    shownHostKeyPrompt.current = promptId;

    const respond = (accept: boolean) => {
      void MeetermTerminal.respondToHostKey(
        TERMINAL_ID,
        connection.fingerprint,
        accept,
      ).catch(() => {
        setControlMessage('The host key response could not be sent.');
      });
    };

    Alert.alert(
      'Trust this SSH host?',
      `${connection.host}:${connection.port}\n\nAlgorithm: ${printable(
        connection.algorithm,
      )}\nSHA256 fingerprint:\n${connection.fingerprint}\n\nVerify this fingerprint through a trusted channel before connecting.`,
      [
        {
          text: 'Cancel',
          style: 'cancel',
          onPress: () => respond(false),
        },
        {
          text: 'Trust and connect',
          onPress: () => respond(true),
        },
      ],
      { cancelable: false },
    );
  }, [connection]);

  const startConnection = useCallback((options: SshConnectOptions) => {
    setConnectionModalVisible(false);
    setControlMessage('');
    setRemovedHostKeyId('');

    try {
      const request = MeetermTerminal.connect(TERMINAL_ID, options);
      void request.catch(() => {
        setControlMessage('The native connection request could not be started.');
      });
    } catch {
      setControlMessage('The native connection request could not be started.');
    }
  }, []);

  const disconnect = useCallback(() => {
    if (commandPending.current) {
      return;
    }

    commandPending.current = true;
    setControlMessage('');
    void MeetermTerminal.disconnect(TERMINAL_ID)
      .catch(() => {
        setControlMessage('The disconnect request could not be sent.');
      })
      .finally(() => {
        commandPending.current = false;
      });
  }, []);

  const reconnect = useCallback(() => {
    if (commandPending.current) return;
    commandPending.current = true;
    setControlMessage('');
    void MeetermTerminal.reconnect(TERMINAL_ID)
      .catch(() => setControlMessage('Reconnect could not start. Connect again with your SSH key.'))
      .finally(() => { commandPending.current = false; });
  }, []);

  const selectPane = useCallback((pane: TmuxPane) => {
    if (commandPending.current) return;
    commandPending.current = true;
    selectionVersion.current += 1;
    const previous = panes;
    // Input always follows the displayed borrowed handle, including while
    // Rust queues tmux selection/zoom. Do not keep typing into the old pane
    // until the next periodic metadata poll.
    setPanes((current) => current.map((candidate) => ({
      ...candidate,
      selected: candidate.paneId === pane.paneId,
    })));
    setControlMessage('');
    void MeetermTerminal.selectPane(TERMINAL_ID, pane.paneId)
      .catch(() => {
        setPanes(previous);
        setControlMessage('The terminal could not be selected.');
      })
      .finally(() => { commandPending.current = false; });
  }, [panes]);

  const reviewChangedHostKey = useCallback(() => {
    const changeId = hostKeyChangeId(connection);
    if (!changeId || removedHostKeyId === changeId) {
      return;
    }

    Alert.alert(
      'Host key changed',
      `Stop and verify the server identity. A changed key can mean the server was rebuilt, or that someone is intercepting the connection.\n\n${connection.host}:${connection.port}\nAlgorithm: ${printable(
        connection.algorithm,
      )}\n\nTrusted SHA256 fingerprint:\n${printable(
        connection.knownFingerprint,
      )}\n\nReceived SHA256 fingerprint:\n${printable(
        connection.fingerprint,
      )}\n\nReset trust only after confirming the new fingerprint with the server administrator through a separate trusted channel.`,
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Reset trusted key',
          style: 'destructive',
          onPress: () => {
            void MeetermTerminal.forgetHostKey(
              connection.host,
              connection.port,
            )
              .then(() => {
                setRemovedHostKeyId(changeId);
                setControlMessage(
                  'Trusted key removed. Connect again to verify the new host key.',
                );
              })
              .catch(() => {
                setControlMessage('The trusted host key could not be removed.');
              });
          },
        },
      ],
      { cancelable: false },
    );
  }, [connection, removedHostKeyId]);

  const presentation = connectionPresentation(connection);
  const target = connection.host
    ? `${connection.host}:${connection.port || 22}`
    : 'SSH terminal';
  const isActive =
    connection.state !== 'Disconnected' &&
    connection.state !== 'Failed' &&
    connection.state !== 'Closing';
  const isClosing = connection.state === 'Closing';
  const changeId = hostKeyChangeId(connection);
  const hostKeyWasRemoved = Boolean(changeId && removedHostKeyId === changeId);
  const selectedPane = panes.find((pane) => pane.selected);
  const workspaces = panes.filter((pane, index) =>
    panes.findIndex((candidate) => candidate.windowId === pane.windowId) === index,
  );
  const visiblePanes = panes.filter((pane) => pane.windowId === selectedPane?.windowId);
  const canReconnect = !isActive && !isClosing && panes.length > 0;

  return (
    <SafeAreaProvider>
      <SafeAreaView edges={['top']} style={appStyles.container}>
        <StatusBar
          backgroundColor={CHROME_COLORS.background}
          barStyle="light-content"
          hidden={false}
        />
        <View style={appStyles.toolbar}>
        <View style={appStyles.statusBlock}>
          <View
            style={[
              appStyles.statusDot,
              presentation.tone === 'pending' && appStyles.statusDotPending,
              presentation.tone === 'ready' && appStyles.statusDotReady,
              presentation.tone === 'failed' && appStyles.statusDotFailed,
            ]}
          />
          <View style={appStyles.statusCopy}>
            <Text
              accessibilityLiveRegion="polite"
              numberOfLines={1}
              style={appStyles.statusLabel}
            >
              {presentation.label}
            </Text>
            <Text numberOfLines={1} selectable style={appStyles.statusTarget}>
              {target}
            </Text>
          </View>
        </View>
        {canReconnect ? (
          <Pressable
            accessibilityRole="button"
            accessibilityHint="Resume the same remote workspace"
            onPress={reconnect}
            style={({ pressed }) => [appStyles.toolbarButton, pressed && appStyles.toolbarButtonPressed]}
          >
            <Text style={appStyles.toolbarButtonText}>Reconnect</Text>
          </Pressable>
        ) : null}
        <Pressable
          accessibilityHint={
            isActive ? 'Disconnect from this SSH host' : 'Enter SSH connection details'
          }
          accessibilityRole="button"
          accessibilityState={{ disabled: isClosing }}
          disabled={isClosing}
          onPress={
            isActive ? disconnect : () => setConnectionModalVisible(true)
          }
          style={({ pressed }) => [
            appStyles.toolbarButton,
            pressed && appStyles.toolbarButtonPressed,
            isClosing && appStyles.toolbarButtonDisabled,
          ]}
        >
          <Text style={appStyles.toolbarButtonText}>
            {isClosing ? 'Closing…' : isActive ? 'Disconnect' : 'Connect'}
          </Text>
        </Pressable>
        </View>

        {connection.state === 'Failed' || controlMessage ? (
          <View
          accessibilityLiveRegion="polite"
          style={[
            appStyles.connectionNotice,
            connection.state === 'Failed' && appStyles.connectionErrorNotice,
          ]}
        >
          {connection.state === 'Failed' ? (
            <>
              <Text style={appStyles.connectionNoticeTitle}>
                {connection.errorCode === 'host_key_changed'
                  ? 'Host identity changed'
                  : 'Could not connect'}
              </Text>
              <Text numberOfLines={3} style={appStyles.connectionNoticeBody}>
                {connection.errorMessage || 'The SSH connection failed.'}
              </Text>
              {changeId && !hostKeyWasRemoved ? (
                <Pressable
                  accessibilityRole="button"
                  hitSlop={6}
                  onPress={reviewChangedHostKey}
                  style={({ pressed }) => [
                    appStyles.noticeAction,
                    pressed && appStyles.plainActionPressed,
                  ]}
                >
                  <Text style={appStyles.noticeActionText}>Review key change</Text>
                </Pressable>
              ) : null}
            </>
          ) : null}
          {controlMessage ? (
            <Text style={appStyles.connectionNoticeBody}>{controlMessage}</Text>
          ) : null}
          </View>
        ) : null}

        {panes.length > 0 ? (
          <View style={appStyles.sessionControls}>
            <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={appStyles.tabContent}>
              {workspaces.map((workspace) => (
                <Pressable
                  key={workspace.windowId}
                  accessibilityRole="tab"
                  accessibilityLabel={`Workspace ${workspace.windowName}`}
                  accessibilityState={{ selected: workspace.windowId === selectedPane?.windowId, disabled: connection.state !== 'Ready' }}
                  disabled={connection.state !== 'Ready'}
                  onPress={() => selectPane(workspace)}
                  style={({ pressed }) => [appStyles.sessionTab, workspace.windowId === selectedPane?.windowId && appStyles.selectedTab, pressed && appStyles.toolbarButtonPressed]}
                >
                  <Text numberOfLines={1} style={appStyles.tabText}>{workspace.windowName || 'Workspace'}</Text>
                </Pressable>
              ))}
            </ScrollView>
            <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={appStyles.tabContent}>
              {visiblePanes.map((pane, index) => (
                <Pressable
                  key={pane.paneId}
                  accessibilityRole="tab"
                  accessibilityLabel={`Terminal ${pane.paneId}`}
                  accessibilityState={{ selected: pane.selected, disabled: connection.state !== 'Ready' }}
                  disabled={connection.state !== 'Ready'}
                  onPress={() => selectPane(pane)}
                  style={({ pressed }) => [appStyles.sessionTab, pane.selected && appStyles.selectedTab, pressed && appStyles.toolbarButtonPressed]}
                >
                  <Text style={appStyles.tabText}>Terminal {index + 1}</Text>
                </Pressable>
              ))}
            </ScrollView>
          </View>
        ) : null}
        <TerminalView terminalId={selectedPane?.terminalId ?? TERMINAL_ID} style={appStyles.terminal} />

        <ConnectionModal
          onClose={() => setConnectionModalVisible(false)}
          onSubmit={startConnection}
          visible={connectionModalVisible}
        />
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

const CHROME_COLORS = {
  background: '#10141a',
  border: '#272d37',
  label: '#e6edf3',
  secondaryLabel: '#8b949e',
  accent: '#65aaf3',
  idle: '#6e7681',
  pending: '#d6a74a',
  ready: '#57ab6b',
  failed: '#e56b64',
  errorSurface: '#281719',
};

const appStyles = StyleSheet.create({
  sessionControls: {
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: CHROME_COLORS.border,
  },
  tabContent: { paddingHorizontal: 8, gap: 4 },
  sessionTab: {
    minHeight: 44,
    justifyContent: 'center',
    paddingHorizontal: 12,
    borderBottomWidth: 2,
    borderBottomColor: 'transparent',
    maxWidth: 240,
  },
  selectedTab: { borderBottomColor: CHROME_COLORS.accent },
  tabText: { color: CHROME_COLORS.label, fontSize: 13 },
  container: {
    backgroundColor: CHROME_COLORS.background,
    flex: 1,
  },
  toolbar: {
    alignItems: 'center',
    backgroundColor: CHROME_COLORS.background,
    borderBottomColor: CHROME_COLORS.border,
    borderBottomWidth: StyleSheet.hairlineWidth,
    flexDirection: 'row',
    gap: 12,
    minHeight: 58,
    paddingHorizontal: 12,
  },
  statusBlock: {
    alignItems: 'center',
    flex: 1,
    flexDirection: 'row',
    gap: 10,
    minWidth: 0,
  },
  statusDot: {
    backgroundColor: CHROME_COLORS.idle,
    borderRadius: 4,
    height: 8,
    width: 8,
  },
  statusDotPending: {
    backgroundColor: CHROME_COLORS.pending,
  },
  statusDotReady: {
    backgroundColor: CHROME_COLORS.ready,
  },
  statusDotFailed: {
    backgroundColor: CHROME_COLORS.failed,
  },
  statusCopy: {
    flex: 1,
    minWidth: 0,
  },
  statusLabel: {
    color: CHROME_COLORS.label,
    fontSize: 13,
    fontWeight: '600',
    lineHeight: 17,
  },
  statusTarget: {
    color: CHROME_COLORS.secondaryLabel,
    fontSize: 11,
    fontVariant: ['tabular-nums'],
    lineHeight: 15,
  },
  toolbarButton: {
    alignItems: 'center',
    borderColor: CHROME_COLORS.border,
    borderCurve: 'continuous',
    borderRadius: 10,
    borderWidth: 1,
    justifyContent: 'center',
    minHeight: 44,
    minWidth: 88,
    paddingHorizontal: 12,
  },
  toolbarButtonPressed: {
    backgroundColor: '#202630',
  },
  toolbarButtonDisabled: {
    opacity: 0.55,
  },
  toolbarButtonText: {
    color: CHROME_COLORS.accent,
    fontSize: 14,
    fontWeight: '600',
  },
  connectionNotice: {
    backgroundColor: CHROME_COLORS.background,
    borderBottomColor: CHROME_COLORS.border,
    borderBottomWidth: StyleSheet.hairlineWidth,
    gap: 3,
    paddingBottom: 10,
    paddingHorizontal: 20,
    paddingTop: 8,
  },
  connectionErrorNotice: {
    backgroundColor: CHROME_COLORS.errorSurface,
  },
  connectionNoticeTitle: {
    color: '#ffd7d3',
    fontSize: 13,
    fontWeight: '700',
    lineHeight: 18,
  },
  connectionNoticeBody: {
    color: '#c9d1d9',
    fontSize: 12,
    lineHeight: 17,
  },
  noticeAction: {
    alignSelf: 'flex-start',
    justifyContent: 'center',
    minHeight: 44,
    paddingRight: 12,
  },
  noticeActionText: {
    color: CHROME_COLORS.accent,
    fontSize: 13,
    fontWeight: '600',
  },
  terminal: {
    backgroundColor: '#090b0f',
    flex: 1,
  },
  plainActionPressed: {
    opacity: 0.45,
  },
});

const layoutStyles = StyleSheet.create({
  modalKeyboardView: {
    flex: 1,
  },
  modalHeader: {
    alignItems: 'center',
    flexDirection: 'row',
    minHeight: 56,
    paddingHorizontal: 8,
  },
  headerAction: {
    justifyContent: 'center',
    minHeight: 44,
    minWidth: 82,
    paddingHorizontal: 8,
  },
  headerActionEnd: {
    alignItems: 'flex-end',
  },
  plainActionPressed: {
    opacity: 0.45,
  },
  formContent: {
    gap: 24,
    paddingBottom: 40,
    paddingHorizontal: 20,
    paddingTop: 18,
  },
  introBlock: {
    gap: 6,
  },
  sectionBlock: {
    gap: 8,
  },
  hostPortRow: {
    alignItems: 'flex-start',
    flexDirection: 'row',
    gap: 12,
  },
  hostColumn: {
    flex: 1,
    minWidth: 0,
  },
  portColumn: {
    width: 92,
  },
  fieldBlock: {
    gap: 7,
  },
  fieldLabelRow: {
    alignItems: 'center',
    flexDirection: 'row',
    justifyContent: 'space-between',
    minHeight: 22,
  },
  fieldLabel: {
    fontSize: 13,
    fontWeight: '600',
    lineHeight: 18,
  },
  optionalLabel: {
    fontSize: 12,
    fontWeight: '400',
  },
  fieldAction: {
    justifyContent: 'center',
    marginVertical: -11,
    minHeight: 44,
    paddingLeft: 16,
  },
  fieldActionText: {
    fontSize: 13,
    fontWeight: '600',
  },
  fieldError: {
    fontSize: 12,
    lineHeight: 17,
  },
  numericInput: {
    fontVariant: ['tabular-nums'],
  },
  keyMask: {
    bottom: 0,
    justifyContent: 'center',
    left: 12,
    position: 'absolute',
    right: 12,
    top: 0,
  },
  concealedKeyInput: {
    color: 'transparent',
  },
});

function createModalStyles(colors: typeof LIGHT_COLORS | typeof DARK_COLORS) {
  return StyleSheet.create({
    modalRoot: {
      backgroundColor: colors.background,
      flex: 1,
    },
    modalHeader: {
      backgroundColor: colors.background,
      borderBottomColor: colors.border,
      borderBottomWidth: StyleSheet.hairlineWidth,
    },
    headerActionText: {
      color: colors.accent,
      fontSize: 16,
      lineHeight: 21,
    },
    headerActionStrong: {
      color: colors.accent,
      fontSize: 16,
      fontWeight: '700',
      lineHeight: 21,
    },
    modalTitle: {
      color: colors.label,
      flex: 1,
      fontSize: 17,
      fontWeight: '700',
      lineHeight: 22,
      textAlign: 'center',
    },
    introTitle: {
      color: colors.label,
      fontSize: 22,
      fontWeight: '700',
      lineHeight: 28,
    },
    introBody: {
      color: colors.secondaryLabel,
      fontSize: 14,
      lineHeight: 20,
    },
    sectionLabel: {
      color: colors.secondaryLabel,
      fontSize: 12,
      fontWeight: '600',
      letterSpacing: 0.7,
      lineHeight: 16,
      paddingHorizontal: 4,
    },
    sectionCard: {
      backgroundColor: colors.surface,
      borderCurve: 'continuous',
      borderRadius: 16,
      gap: 18,
      padding: 16,
    },
    input: {
      backgroundColor: colors.field,
      borderColor: colors.border,
      borderCurve: 'continuous',
      borderRadius: 12,
      borderWidth: 1,
      color: colors.label,
      fontSize: 16,
      minHeight: 48,
      paddingHorizontal: 12,
      paddingVertical: 10,
    },
    inputErrorBorder: {
      borderColor: colors.danger,
      borderWidth: 1.5,
    },
    keyInputShell: {
      backgroundColor: colors.field,
      borderColor: colors.border,
      borderCurve: 'continuous',
      borderRadius: 12,
      borderWidth: 1,
      minHeight: 132,
      overflow: 'hidden',
    },
    keyInput: {
      color: colors.label,
      fontFamily: Platform.select({
        ios: 'Menlo',
        android: 'monospace',
        default: 'monospace',
      }),
      fontSize: 12,
      lineHeight: 17,
      minHeight: 132,
      padding: 12,
    },
    keyMaskText: {
      color: colors.placeholder,
      fontSize: 14,
      lineHeight: 20,
    },
    helperText: {
      color: colors.secondaryLabel,
      fontSize: 12,
      lineHeight: 17,
    },
  });
}
