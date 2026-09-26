#!/usr/bin/env node

/*
 * Focused App.tsx regression tests for native Herdr selection reconciliation.
 *
 * This intentionally mounts the real App component.  The native module and
 * React Native host views are small test doubles, while workspace metadata is
 * delivered through the same getWorkspaceState polling path used by the app.
 * It is useful to run this file against an unmodified checkout as a red test:
 *
 *   node scripts/ci/app-selection.test.cjs
 *
 * APP_SELECTION_SOURCE can point at another App.tsx (for example a pristine
 * baseline in /tmp) when comparing an implementation against the old code.
 */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const React = require('react');
const { act } = React;
const { createRoot } = require('test-renderer');
const TypeScript = require('typescript');

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const APP_SOURCE = process.env.APP_SELECTION_SOURCE || path.join(REPO_ROOT, 'App.tsx');

const PREFERENCES = {
  fontSize: 14,
  theme: 'light',
  scrollbackLines: 10000,
  automaticReconnect: true,
};

const LIGHT = {
  background: '#fbf7ef',
  surface: '#f1eadf',
  terminal: '#17130f',
  text: '#352b22',
  muted: '#7b6a5a',
  border: '#d8cbbb',
  accent: '#147d72',
  danger: '#b23b3b',
  placeholder: '#9b8a79',
  agentStatus: {
    blocked: '#b4232f',
    done: '#087e8b',
    working: '#826a00',
    idle: '#2f7d32',
    unknown: '#73695c',
  },
};
const DARK = {
  background: '#241f1b',
  surface: '#332b25',
  terminal: '#0c0a08',
  text: '#f7eee3',
  muted: '#c3b5a6',
  border: '#51443a',
  accent: '#69d5c3',
  danger: '#ff8d8d',
  placeholder: '#98897b',
  agentStatus: {
    blocked: '#f08a91',
    done: '#77d5d1',
    working: '#e5c94f',
    idle: '#8dd18a',
    unknown: '#8f887f',
  },
};

function workspaceControl(overrides = {}) {
  return {
    operationEpoch: '1',
    hasRetainedWork: true,
    runtimeOperationsReady: true,
    terminalInputReady: true,
    cleanupWarning: null,
    ...overrides,
    recovery: {
      phase: 'none',
      reason: '',
      attempt: 0,
      maxAttempts: 6,
      ...(overrides.recovery || {}),
    },
  };
}

const DEFAULT_WORKSPACE_CONTROL = {
  operationEpoch: '',
  hasRetainedWork: false,
  runtimeOperationsReady: false,
  terminalInputReady: false,
  cleanupWarning: null,
  recovery: {
    phase: 'none',
    reason: '',
    attempt: 0,
    maxAttempts: 0,
  },
};

function normalizeWorkspaceControl(value) {
  if (!value || typeof value !== 'object') return clone(DEFAULT_WORKSPACE_CONTROL);
  const recovery = value.recovery && typeof value.recovery === 'object' ? value.recovery : {};
  const phases = new Set(['none', 'reconnecting', 'resynchronizing', 'stopped']);
  return {
    operationEpoch: typeof value.operationEpoch === 'string' ? value.operationEpoch : '',
    hasRetainedWork: value.hasRetainedWork === true,
    runtimeOperationsReady: value.runtimeOperationsReady === true,
    terminalInputReady: value.terminalInputReady === true,
    cleanupWarning: value.cleanupWarning
      && typeof value.cleanupWarning === 'object'
      && /^[0-9]+$/.test(value.cleanupWarning.id || '')
      && value.cleanupWarning.code === 'layout_restore_unconfirmed'
      && typeof value.cleanupWarning.message === 'string'
      ? {
        id: value.cleanupWarning.id,
        code: 'layout_restore_unconfirmed',
        message: value.cleanupWarning.message.slice(0, 256),
      }
      : null,
    recovery: {
      phase: phases.has(recovery.phase) ? recovery.phase : 'none',
      reason: typeof recovery.reason === 'string' ? recovery.reason : '',
      attempt: typeof recovery.attempt === 'number' && recovery.attempt >= 0 ? Math.floor(recovery.attempt) : 0,
      maxAttempts: typeof recovery.maxAttempts === 'number' && recovery.maxAttempts >= 0 ? Math.floor(recovery.maxAttempts) : 0,
    },
  };
}

function runtimeCandidate(id, backend, name, state = 'running', overrides = {}) {
  return {
    id,
    backend,
    name,
    state,
    selectable: state === 'running',
    isDefault: false,
    lastUsed: false,
    errorCode: '',
    errorMessage: '',
    ...overrides,
  };
}

function runtimeBackend(backend, candidates = [], overrides = {}) {
  return {
    backend,
    state: 'ready',
    errorCode: '',
    errorMessage: '',
    canCreate: backend === 'tmux',
    candidates,
    ...overrides,
  };
}

function SafeAreaProvider({ children }) {
  return React.createElement(React.Fragment, null, children);
}

function SafeAreaView(props) {
  return React.createElement('SafeAreaView', props, props.children);
}

function FlatList({
  data = [],
  renderItem,
  ListHeaderComponent,
  ListEmptyComponent,
  ...props
}) {
  const header = ListHeaderComponent
    ? (React.isValidElement(ListHeaderComponent)
      ? ListHeaderComponent
      : React.createElement(ListHeaderComponent))
    : null;
  const rows = data.length === 0
    ? (ListEmptyComponent
      ? (React.isValidElement(ListEmptyComponent)
        ? ListEmptyComponent
        : React.createElement(ListEmptyComponent))
      : null)
    : data.map((item, index) => {
      const rendered = renderItem({ item, index });
      return React.createElement(React.Fragment, { key: item.id || index }, rendered);
    });
  return React.createElement('FlatList', props, header, rows);
}

function makeNativeEnvironment() {
  const environment = {
    snapshot: {
      backend: 'tmux',
      runtime: 'meeterm',
      groupsSupported: false,
      workspaces: [],
      groups: [],
      terminals: [],
      control: workspaceControl({ hasRetainedWork: false, runtimeOperationsReady: false, terminalInputReady: false }),
    },
    runtimeDiscovery: {
      connectionGeneration: '1',
      revision: 1,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [] },
      ],
    },
    connection: {
      state: 'Ready',
      host: 'fixture.example',
      port: 22,
      fingerprint: '',
      algorithm: '',
      knownFingerprint: '',
      errorCode: '',
      errorMessage: '',
    },
    calls: [],
    foregroundCalls: [],
    nativeCalls: [],
    startupPhases: [],
    startupPhaseShouldFail: false,
    initialAppState: 'active',
    initialURL: null,
    platform: 'ios',
    initialURLBehavior: 'resolve',
    profilesShouldFail: false,
    profiles: [],
    selectRuntimeShouldFail: false,
    selectRuntimeMode: 'ready',
    pendingSelection: null,
    createTmuxSessionMode: 'ready',
    pendingCreation: null,
    createTmuxSessionShouldFail: false,
    refreshRuntimeMode: 'ready',
    pendingRefresh: null,
    refreshRuntimeShouldFail: false,
    refreshRuntimeCalls: 0,
    createdRuntime: null,
    lastUsedUpdates: [],
    appStateListeners: new Set(),
    hardwareBackHandler: null,
    visibility: [],
    renderedTerminalIds: [],
    intervalCallbacks: [],
    fakeTimers: false,
    timeoutCallbacks: [],
    nextTimeoutId: 1,
    alert: null,
    accessibilityAnnouncements: [],
    recoveryRetryMode: 'ready',
    recoveryRetryAttempt: 0,
    recoveryRetryShouldFail: false,
    pendingRecoveryRetry: null,
    changeRuntimeShouldFail: false,
    changeRuntimeFailureAfterRelease: false,
    changeRuntimeThrowUnknown: false,
    changeRuntimeMode: 'ready',
    pendingChangeRuntime: null,
    disconnectForSwitcherResult: 'accepted',
    hostKeyPendingProfileId: '',
    disconnectRelease: null,
    changeRuntimeRelease: null,
    presentedModalDismissal: null,
  };
  environment.dismissPresentedModal = () => {
    const dismiss = environment.presentedModalDismissal;
    environment.presentedModalDismissal = null;
    assert.equal(typeof dismiss, 'function', 'an iOS page sheet should be awaiting dismissal');
    dismiss();
  };

  const native = {
    recordStartupPhase(phase) {
      if (environment.startupPhaseShouldFail) throw new Error('diagnostic bridge unavailable');
      environment.startupPhases.push(phase);
    },
    async getProfiles() {
      environment.nativeCalls.push('getProfiles');
      if (environment.profilesShouldFail) throw new Error('profiles unavailable');
      return clone(environment.profiles);
    },
    async getPreferences() {
      environment.nativeCalls.push('getPreferences');
      return { ...PREFERENCES };
    },
    async setPreferences() {},
    async setAutomaticReconnect() {},
    async connectHost(_connectionId, options) {
      environment.nativeCalls.push({ method: 'connectHost', options });
      environment.connection = {
        ...environment.connection,
        state: 'DiscoveringRuntimes',
        host: options.host,
        port: options.port,
      };
    },
    async connectProfileHost(_connectionId, profileId) {
      environment.nativeCalls.push({ method: 'connectProfileHost', profileId });
      const profile = environment.profiles.find(item => item.id === profileId);
      environment.connection = {
        ...environment.connection,
        state: environment.hostKeyPendingProfileId === profileId ? 'HostKeyPending' : 'DiscoveringRuntimes',
        host: profile?.host ?? environment.connection.host,
        port: profile?.port ?? environment.connection.port,
        fingerprint: environment.hostKeyPendingProfileId === profileId ? 'SHA256:unknownFixtureHost' : '',
        algorithm: environment.hostKeyPendingProfileId === profileId ? 'ssh-ed25519' : '',
      };
    },
    async respondToHostKey(_connectionId, fingerprint, accept) {
      environment.nativeCalls.push({ method: 'respondToHostKey', fingerprint, accept });
      environment.connection = {
        ...environment.connection,
        state: accept ? 'DiscoveringRuntimes' : 'Failed',
        fingerprint: '',
      };
    },
    async getRuntimeDiscovery() {
      environment.nativeCalls.push('getRuntimeDiscovery');
      return clone(environment.runtimeDiscovery);
    },
    async refreshRuntimes() {
      environment.nativeCalls.push('refreshRuntimes');
      environment.refreshRuntimeCalls += 1;
      if (environment.refreshRuntimeShouldFail) throw new Error('runtime refresh rejected');
      if (environment.refreshRuntimeMode === 'delayed') {
        const finalDiscovery = clone(environment.runtimeDiscovery);
        finalDiscovery.revision += 1;
        environment.pendingRefresh = { finalDiscovery };
        environment.runtimeDiscovery = {
          connectionGeneration: finalDiscovery.connectionGeneration,
          revision: finalDiscovery.revision,
          backends: [
            runtimeBackend('tmux', [], { state: 'loading' }),
            runtimeBackend('herdr', [], { state: 'loading' }),
          ],
        };
        return;
      }
      completeRefresh(environment);
    },
    async selectRuntime(_connectionId, candidateId) {
      environment.calls.push({ method: 'selectRuntime', candidateId });
      if (environment.selectRuntimeShouldFail) throw new Error('stale runtime');
      const candidate = environment.runtimeDiscovery.backends.flatMap(item => item.candidates).find(item => item.id === candidateId);
      if (!candidate) throw new Error('runtime disappeared');
      if (environment.selectRuntimeMode === 'delayed-ready' || environment.selectRuntimeMode === 'candidate-failure') {
        environment.pendingSelection = { candidateId };
        return;
      }
      completeSelection(environment, candidate);
    },
    async createTmuxSession(_connectionId, name) {
      environment.calls.push({ method: 'createTmuxSession', name });
      if (environment.createTmuxSessionShouldFail) throw new Error('tmux create rejected');
      const tmux = environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux');
      if (tmux.errorCode || tmux.errorMessage) {
        // Native clears an old create-operation error when it accepts a new
        // request. The next failure may publish the same code again.
        tmux.errorCode = '';
        tmux.errorMessage = '';
        environment.runtimeDiscovery.revision += 1;
      }
      if (environment.createTmuxSessionMode === 'delayed-ready' || environment.createTmuxSessionMode === 'delayed-failure') {
        environment.pendingCreation = { name };
        return;
      }
      completeCreation(environment, name);
    },
    async setLastUsedRuntime(profileId, backend, runtime) {
      environment.lastUsedUpdates.push({ profileId, backend, runtime });
      const profile = environment.profiles.find(item => item.id === profileId);
      return { ...(profile || { id: profileId }), backend, runtime };
    },
    async disconnect() {
      environment.nativeCalls.push('disconnect');
      environment.connection = {
        ...environment.connection,
        state: 'Disconnected',
        ...(environment.disconnectRelease || {}),
      };
    },
    async reconnect() {
      environment.nativeCalls.push('reconnect');
      environment.connection = { ...environment.connection, state: 'DiscoveringRuntimes' };
    },
    async disconnectForSwitcher() {
      environment.nativeCalls.push('disconnectForSwitcher');
      if (environment.disconnectForSwitcherResult === 'rejected_before_boundary') {
        return { status: 'rejected_before_boundary', errorCode: 'internal_error' };
      }
      if (environment.disconnectForSwitcherResult === 'not_invoked') {
        return { status: 'not_invoked', errorCode: 'invalid_argument' };
      }
      environment.connection = { ...environment.connection, state: 'Disconnected' };
      if (environment.disconnectForSwitcherResult === 'accepted_after_failure') {
        environment.snapshot.control = workspaceControl({
          ...environment.snapshot.control,
          runtimeOperationsReady: false,
          terminalInputReady: false,
        });
        return { status: 'accepted_after_failure', errorCode: 'boundary_accepted_failure' };
      }
      return { status: 'accepted' };
    },
    async refreshTerminal() {
      environment.nativeCalls.push('refreshTerminal');
      throw new Error('terminal refresh rejected');
    },
    async setForeground(_connectionId, foreground) { environment.foregroundCalls.push(foreground); },
    async getConnectionState() { return { ...environment.connection }; },
    async getWorkspaceState() { return clone(environment.snapshot); },
    async retryRecovery(_connectionId, operationEpoch) {
      environment.calls.push({ method: 'retryRecovery', operationEpoch });
      if (environment.recoveryRetryShouldFail) throw new Error('recovery retry rejected');
      if (environment.recoveryRetryMode === 'noop') return;
      if (environment.recoveryRetryMode === 'pending') {
        await new Promise(resolve => { environment.pendingRecoveryRetry = { operationEpoch, resolve }; });
        return;
      }
      environment.snapshot.control = workspaceControl({
        ...environment.snapshot.control,
        operationEpoch: String(Number(operationEpoch) + 1),
        runtimeOperationsReady: false,
        terminalInputReady: false,
        recovery: { phase: 'reconnecting', reason: 'manual_retry', attempt: environment.recoveryRetryAttempt, maxAttempts: 6 },
      });
      environment.connection.state = 'Reconnecting';
    },
    async changeRuntime(_connectionId, operationEpoch) {
      environment.calls.push({ method: 'changeRuntime', operationEpoch });
      if (environment.changeRuntimeThrowUnknown) throw new Error('unclassified bridge failure');
      if (environment.changeRuntimeShouldFail) {
        return { status: 'rejected_before_boundary', errorCode: 'recovery_stale' };
      }
      if (environment.changeRuntimeFailureAfterRelease) {
        environment.connection = { ...environment.connection, state: 'Disconnected' };
        environment.snapshot.control = workspaceControl({
          ...environment.snapshot.control,
          runtimeOperationsReady: false,
          terminalInputReady: false,
        });
        return { status: 'accepted_after_failure', errorCode: 'boundary_accepted_failure' };
      }
      if (environment.changeRuntimeMode === 'pending') {
        const pendingOutcome = await new Promise(resolve => {
          environment.pendingChangeRuntime = { operationEpoch, resolve };
        });
        if (pendingOutcome === 'reject') {
          return { status: 'rejected_before_boundary', errorCode: 'recovery_stale' };
        }
      }
      environment.connection = {
        ...environment.connection,
        state: 'AwaitingRuntimeSelection',
        ...(environment.changeRuntimeRelease || {}),
      };
      environment.snapshot.control = workspaceControl({
        hasRetainedWork: false,
        operationEpoch: String(Number(operationEpoch) + 1),
        runtimeOperationsReady: false,
        terminalInputReady: false,
      });
      return { status: 'accepted' };
    },
    async setTerminalVisible(_connectionId, visible) {
      environment.visibility.push(visible);
    },
    async selectPane(_connectionId, paneId) {
      environment.calls.push({ method: 'selectPane', paneId });
      selectPane(environment.snapshot, paneId);
    },
    async selectGroup(_connectionId, groupId) {
      environment.calls.push({ method: 'selectGroup', groupId });
      selectGroup(environment.snapshot, groupId);
    },
    async closePane(_connectionId, paneId) {
      environment.calls.push({ method: 'closePane', paneId });
      closePane(environment.snapshot, paneId);
    },
    // Issue #28 attachment surface: benign defaults so screens that merely
    // consult the attachment gate never hit an undefined binding. Focused
    // attachment tests replace these via `configureAttachment`.
    async attachmentCompositionStatus() { return { status: 'ok' }; },
    async getAttachmentState() { return { status: 'idle' }; },
    async beginAttachment() { return { status: 'unavailable' }; },
    async pickAttachmentImage() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
    async prepareAttachmentImage() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
    async discardAttachment() {},
    async uploadAttachment() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
    async attachmentSnapshot() { return { status: 'idle' }; },
    async retryAttachmentUpload() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
    async cancelAttachment() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
    async insertAttachment() { return { status: 'unavailable' }; },
    async deleteRemoteAttachment() { return { status: 'error', errorCode: 'attachment_unavailable', message: 'Attachment unavailable.' }; },
  };
  environment.resolvePendingSelection = outcome => {
    assert.ok(environment.pendingSelection, 'a runtime selection should be pending');
    const { candidateId } = environment.pendingSelection;
    const candidate = environment.runtimeDiscovery.backends.flatMap(item => item.candidates).find(item => item.id === candidateId);
    assert.ok(candidate, 'the pending runtime should still be discoverable');
    if (outcome === 'failure') {
      candidate.selectable = false;
      candidate.errorCode = 'runtime_unavailable';
      candidate.errorMessage = 'The runtime stopped before it could be opened.';
      environment.runtimeDiscovery.revision += 1;
    } else {
      completeSelection(environment, candidate);
    }
    environment.pendingSelection = null;
  };
  environment.resolvePendingCreation = outcome => {
    assert.ok(environment.pendingCreation, 'a tmux creation should be pending');
    const { name } = environment.pendingCreation;
    if (outcome === 'failure') {
      const tmux = environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux');
      tmux.errorCode = 'tmux_create_failed';
      tmux.errorMessage = 'tmux could not create this session.';
      environment.runtimeDiscovery.revision += 1;
    } else {
      completeCreation(environment, name);
    }
    environment.pendingCreation = null;
  };
  environment.resolvePendingRefresh = () => {
    assert.ok(environment.pendingRefresh, 'a runtime refresh should be pending');
    environment.runtimeDiscovery = environment.pendingRefresh.finalDiscovery;
    environment.pendingRefresh = null;
  };
  environment.resolvePendingRecoveryRetry = () => {
    assert.ok(environment.pendingRecoveryRetry, 'a recovery retry should be pending');
    const pending = environment.pendingRecoveryRetry;
    environment.pendingRecoveryRetry = null;
    pending.resolve();
  };
  environment.resolvePendingChangeRuntime = outcome => {
    assert.ok(environment.pendingChangeRuntime, 'a runtime change should be pending');
    const pending = environment.pendingChangeRuntime;
    environment.pendingChangeRuntime = null;
    pending.resolve(outcome);
  };
  environment.runFakeTimers = (maxDelay = Infinity) => {
    const due = environment.timeoutCallbacks.filter(timer => !timer.canceled && timer.delay <= maxDelay);
    environment.timeoutCallbacks = environment.timeoutCallbacks.filter(timer => !due.includes(timer));
    due.forEach(timer => timer.callback());
  };
  return { environment, native };
}

function completeSelection(environment, candidate) {
  environment.snapshot.backend = candidate.backend;
  environment.snapshot.runtime = candidate.name;
  environment.snapshot.control = workspaceControl({
    ...environment.snapshot.control,
    hasRetainedWork: false,
    runtimeOperationsReady: true,
    terminalInputReady: true,
    recovery: { phase: 'none', reason: '', attempt: 0, maxAttempts: 6 },
  });
  environment.connection.state = 'Ready';
}

function completeCreation(environment, name) {
  const candidate = runtimeCandidate(`created-${name}`, 'tmux', name);
  environment.createdRuntime = candidate;
  const tmux = environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux');
  tmux.candidates.push(candidate);
  environment.runtimeDiscovery.revision += 1;
  completeSelection(environment, candidate);
}

function completeRefresh(environment) {
  environment.runtimeDiscovery.revision += 1;
}

function makeReactNativeMocks(environment) {
  const noOpSubscription = { remove() {} };
  const AppState = {
    currentState: environment.initialAppState,
    addEventListener(event, listener) {
      assert.equal(event, 'change');
      environment.appStateListeners.add(listener);
      return { remove() { environment.appStateListeners.delete(listener); } };
    },
  };
  environment.emitAppState = state => {
    AppState.currentState = state;
    for (const listener of environment.appStateListeners) listener(state);
  };
  const BackHandler = {
    addEventListener(_event, listener) {
      environment.hardwareBackHandler = listener;
      return { remove() { if (environment.hardwareBackHandler === listener) environment.hardwareBackHandler = null; } };
    },
  };
  const Keyboard = { dismiss() {} };
  const Linking = {
    async getInitialURL() {
      if (environment.initialURLBehavior === 'pending') return new Promise(() => {});
      if (environment.initialURLBehavior === 'reject') throw new Error('initial URL unavailable');
      return environment.initialURL;
    },
    addEventListener() { return noOpSubscription; },
  };
  const Alert = {
    alert(title, message, buttons) {
      environment.alert = { title, message, buttons: buttons || [] };
    },
  };
  const AccessibilityInfo = {
    announceForAccessibilityWithOptions(message) {
      environment.accessibilityAnnouncements.push(message);
      return Promise.resolve();
    },
  };
  const Platform = { OS: environment.platform || 'ios' };
  function Modal({ visible, children, onDismiss }) {
    if (visible && typeof onDismiss === 'function') environment.presentedModalDismissal = onDismiss;
    return visible ? React.createElement(React.Fragment, null, children) : null;
  }
  function StatusBar(props) { return React.createElement('StatusBar', props); }
  StatusBar.setBarStyle = () => {};
  const StyleSheet = {
    hairlineWidth: 1,
    create(styles) { return styles; },
  };

  return {
    ActivityIndicator: 'ActivityIndicator',
    Alert,
    AccessibilityInfo,
    AppState,
    BackHandler,
    FlatList,
    Image: 'Image',
    Keyboard,
    Linking,
    Modal,
    Platform,
    Pressable: 'Pressable',
    ScrollView: 'ScrollView',
    StatusBar,
    StyleSheet,
    Text: 'Text',
    TextInput: 'TextInput',
    View: 'View',
    useWindowDimensions() { return { width: 390, height: 844, scale: 1, fontScale: 1 }; },
  };
}

function makeSafeAreaMocks() {
  return {
    SafeAreaProvider,
    SafeAreaView,
    useSafeAreaInsets() { return { top: 0, left: 0, right: 0, bottom: 0 }; },
  };
}

function makeUiMocks() {
  function Button({ label, children, onPress, disabled, ...props }) {
    return React.createElement('Button', {
      ...props,
      accessibilityRole: 'button',
      accessibilityLabel: label,
      disabled: Boolean(disabled),
      onPress,
    }, children);
  }
  function IconButton({ label, icon, onPress, disabled, ...props }) {
    return React.createElement('IconButton', {
      ...props,
      icon,
      accessibilityRole: 'button',
      accessibilityLabel: label,
      disabled: Boolean(disabled),
      onPress,
    });
  }
  function Icon(props) { return React.createElement('Icon', props); }
  function Companion(props) { return React.createElement('Companion', props, props.children); }
  function usePalette(theme) { return theme === 'dark' ? DARK : LIGHT; }
  return {
    __esModule: true,
    Button,
    Companion,
    DARK,
    Icon,
    IconButton,
    MONO: 'MONO',
    usePalette,
    useReducedMotion: () => true,
  };
}

function makeFormMocks() {
  const hidden = () => null;
  function ConnectionForm({ visible, initialProfile, onSubmit }) {
    if (!visible) return null;
    const profile = initialProfile || {
      id: '',
      name: 'Manual server',
      host: 'manual.example',
      port: 22,
      username: 'developer',
      authMethod: 'password',
    };
    return React.createElement(
      'ConnectionForm',
      { testID: 'connection-form-visible' },
      React.createElement('Pressable', {
        testID: 'connection-form-test-submit',
        accessibilityRole: 'button',
        accessibilityLabel: 'Submit test connection form',
        onPress: () => onSubmit({
          profile: { ...profile, username: profile.username || 'developer' },
          credential: { authMethod: 'password', password: 'fixture-only' },
          saveProfile: false,
          saveCredential: false,
          keepCredential: false,
          connect: true,
        }),
      }),
    );
  }
  function ProfileList({ profiles = [], busy = false, onAdd, onConnect }) {
    return React.createElement(
      'ProfileList',
      null,
      React.createElement('Pressable', {
        testID: 'profile-list-add',
        accessibilityRole: 'button',
        accessibilityLabel: 'Add server',
        disabled: Boolean(busy),
        onPress: onAdd,
      }),
      profiles.map(profile => React.createElement(
        'Pressable',
        {
          key: profile.id,
          accessibilityRole: 'button',
          accessibilityLabel: `Connect saved server ${profile.name}`,
          disabled: Boolean(busy),
          onPress: () => onConnect(profile),
        },
      )),
    );
  }
  return {
    __esModule: true,
    ConnectionForm,
    DEFAULT_PREFERENCES: { ...PREFERENCES },
    itemActions() {},
    NameForm: hidden,
    ProfileList,
    SettingsForm: hidden,
  };
}

function makeTerminalModule(native, environment) {
  function TerminalView({ terminalId, ...props }) {
    environment.renderedTerminalIds.push(terminalId);
    return React.createElement('TerminalView', { ...props, terminalId });
  }
  const module = {
    __esModule: true,
    default: native,
    TerminalView,
    DEFAULT_WORKSPACE_CONTROL,
    normalizeWorkspaceControl,
  };
  return module;
}

function loadApp(environment, native, presentationOnly = false, smokeEnabled = false) {
  const source = fs.readFileSync(APP_SOURCE, 'utf8');
  const transpiled = TypeScript.transpileModule(source, {
    compilerOptions: {
      target: TypeScript.ScriptTarget.ES2022,
      module: TypeScript.ModuleKind.CommonJS,
      jsx: TypeScript.JsxEmit.ReactJSX,
      esModuleInterop: true,
      sourceMap: false,
    },
    fileName: APP_SOURCE,
  }).outputText;
  const appModule = { exports: {} };
  const rn = makeReactNativeMocks(environment);
  const safeArea = makeSafeAreaMocks();
  const ui = makeUiMocks();
  const forms = makeFormMocks();
  const terminal = makeTerminalModule(native, environment);
  const scheduleTimeout = environment.fakeTimers
    ? (callback, delay) => {
      const id = environment.nextTimeoutId++;
      environment.timeoutCallbacks.push({ id, callback, delay, canceled: false });
      return id;
    }
    : setTimeout;
  const cancelTimeout = environment.fakeTimers
    ? id => {
      const timer = environment.timeoutCallbacks.find(item => item.id === id);
      if (timer) timer.canceled = true;
    }
    : clearTimeout;
  const moduleMap = new Map([
    ['react', React],
    ['react/jsx-runtime', require('react/jsx-runtime')],
    ['react-native', rn],
    ['react-native-safe-area-context', safeArea],
    ['./modules/meeterm-terminal', terminal],
    ['./app/ConnectionForm', forms],
    ['./app/DailyUse', forms],
    ['./app/ui', ui],
    // Navigation's native view/gesture execution belongs to mobile evidence.
    // These tests retain their real App selection and registry assertions.
    ['./app/WorkspaceNavigation', {
      WorkspaceNavigation: ({ screen, workspaces, terminal }) => screen === 'terminal' ? terminal : workspaces,
    }],
  ]);
  function localRequire(request) {
    if (moduleMap.has(request)) return moduleMap.get(request);
    return require(request);
  }
  const processForApp = { ...process, env: { ...process.env } };
  processForApp.env.EXPO_PUBLIC_MEETERM_SMOKE = smokeEnabled ? '1' : '0';
  const context = {
    require: localRequire,
    module: appModule,
    exports: appModule.exports,
    __filename: APP_SOURCE,
    __dirname: path.dirname(APP_SOURCE),
    process: processForApp,
    console,
    setTimeout: scheduleTimeout,
    clearTimeout: cancelTimeout,
    setImmediate,
    clearImmediate,
    setInterval(callback) {
      environment.intervalCallbacks.push(callback);
      return environment.intervalCallbacks.length;
    },
    clearInterval() {},
    globalThis,
  };
  vm.runInNewContext(transpiled, context, { filename: APP_SOURCE });
  if (presentationOnly) return vm.runInNewContext('({ smokeFixture, smokeWorkspaceState, smokeRouteForUrl, resolveAgentStatus, agentStatusPhrase, AGENT_STATUS_META, AgentStatusIndicator })', context);
  return appModule.exports.default;
}

test('public presentation fixtures stay release-gated and do not mutate shared connection state', () => {
  const { environment, native } = makeNativeEnvironment();
  const production = loadApp(environment, native, true);
  assert.deepEqual(environment.startupPhases, []);
  assert.equal(production.smokeRouteForUrl('meeterm://smoke?screen=welcome'), undefined);
  const smoke = loadApp(environment, native, true, true);
  const retiredRoute = ['herdr', 'recovery', 'confirm'].join('-');
  assert.equal(smoke.smokeRouteForUrl(`meeterm://smoke?screen=${retiredRoute}`), undefined);
  for (const screen of ['welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error', 'long-workspaces',
    'runtime-picker', 'runtime-partial-error', 'runtime-empty', 'runtime-create',
    'session-switcher', 'session-switcher-sessions',
    'recovery-progress', 'recovery-exhausted', 'recovery-mismatch',
    'layout-restore-unconfirmed', 'runtime-layout-restore-unconfirmed',
    'attachment-choose', 'attachment-ready', 'attachment-uploading', 'attachment-pending',
    'attachment-uploaded', 'attachment-inserted', 'attachment-failed', 'attachment-cancelled',
    'attachment-deleted', 'attachment-error', 'attachment-blocked']) {
    assert.equal(smoke.smokeRouteForUrl(`meeterm://smoke?screen=${screen}`).screen, screen);
  }
  assert.equal(smoke.smokeRouteForUrl('meeterm://smoke?screen=welcome&host=untrusted'), undefined);
  assert.equal(smoke.smokeFixture('welcome').profiles.length, 0);
  assert.equal(smoke.smokeFixture('empty').panes.length, 0);
  assert.equal(smoke.smokeFixture('search-empty').query, 'deployment');
  assert.equal(smoke.smokeFixture('disconnected').connection.state, 'Disconnected');
  assert.equal(smoke.smokeFixture('connection-error').connection.errorCode, 'authentication_failed');
  assert.equal(smoke.smokeFixture('workspaces').connection.state, 'Ready');
  const picker = smoke.smokeFixture('runtime-picker');
  assert.equal(picker.connection.state, 'AwaitingRuntimeSelection');
  assert.equal(picker.runtimeDiscovery.backends.length, 2);
  assert.equal(picker.runtimeDiscovery.backends[1].candidates[1].state, 'stopped');
  assert.equal(smoke.smokeFixture('runtime-partial-error').runtimeDiscovery.backends[1].state, 'error');
  assert.equal(smoke.smokeFixture('runtime-empty').runtimeDiscovery.backends[0].candidates.length, 0);
  assert.equal(smoke.smokeFixture('runtime-create').runtimeCreateVisible, true);
  assert.equal(smoke.smokeFixture('session-switcher').sheet, 'switcher');
  assert.equal(smoke.smokeFixture('session-switcher-sessions').switcherStarted, true);
  assert.equal(smoke.smokeFixture('session-switcher-sessions').runtimePickerVisible, true);
  const layoutWarning = smoke.smokeFixture('layout-restore-unconfirmed');
  assert.equal(layoutWarning.connection.errorCode, 'layout_restore_unconfirmed');
  assert.equal(layoutWarning.control.cleanupWarning.code, 'layout_restore_unconfirmed');
  assert.equal(layoutWarning.control.cleanupWarning.id, '101');
  assert.equal(layoutWarning.connection.state, 'Disconnected');
  const runtimeLayoutWarning = smoke.smokeFixture('runtime-layout-restore-unconfirmed');
  assert.equal(runtimeLayoutWarning.connection.errorCode, 'layout_restore_unconfirmed');
  assert.equal(runtimeLayoutWarning.control.cleanupWarning.id, '102');
  assert.equal(runtimeLayoutWarning.runtimePickerVisible, true);
  const authAndCleanupWarning = smoke.smokeFixture('connection-error');
  assert.equal(authAndCleanupWarning.connection.errorCode, 'authentication_failed');
  assert.equal(authAndCleanupWarning.control.cleanupWarning.id, '103');
  assert.equal(smoke.smokeFixture('recovery-progress').control.recovery.phase, 'resynchronizing');
  assert.equal(smoke.smokeFixture('recovery-exhausted').control.recovery.phase, 'stopped');
  assert.equal(smoke.smokeFixture('recovery-mismatch').control.recovery.reason, 'runtime_identity_mismatch');
  assert.equal(smoke.smokeFixture('recovery-progress').control.hasRetainedWork, true);
  const herdrPicker = smoke.smokeFixture('herdr-connection');
  assert.equal(herdrPicker.connection.state, 'AwaitingRuntimeSelection');
  assert.equal(herdrPicker.formVisible, false);
  assert.equal(herdrPicker.runtimeDiscovery.backends[0].candidates[0].lastUsed, false);
  assert.equal(herdrPicker.runtimeDiscovery.backends[1].candidates[0].lastUsed, true);
  assert.equal(environment.calls.length, 0);
});

test('Herdr terminal fixture keeps exactly five statuses and an agentless pane', () => {
  const { environment, native } = makeNativeEnvironment();
  const smoke = loadApp(environment, native, true, true);
  const fixture = smoke.smokeFixture('herdr-terminal');
  const statuses = fixture.panes
    .filter(pane => pane.agent)
    .map(pane => pane.agent.status);
  assert.deepEqual([...new Set(statuses)].sort(), ['blocked', 'done', 'idle', 'unknown', 'working']);
  assert.ok(fixture.panes.some(pane => pane.agent === null), 'fixture must include an agentless pane');
});

test('smoke startup diagnostics classify URL and profile boundaries without fixture effects', async t => {
  const cases = [
    {
      name: 'null URL',
      initialURL: null,
      expected: ['js_module_loaded', 'root_effect', 'initial_url_requested', 'initial_url_null',
        'app_content_mounted', 'profiles_requested', 'profiles_succeeded'],
    },
    {
      name: 'other URL',
      initialURL: 'meeterm://unrelated',
      expected: ['js_module_loaded', 'root_effect', 'initial_url_requested', 'initial_url_other',
        'app_content_mounted', 'profiles_requested', 'profiles_succeeded'],
    },
    {
      name: 'rejected URL request',
      initialURLBehavior: 'reject',
      expected: ['js_module_loaded', 'root_effect', 'initial_url_requested', 'initial_url_rejected',
        'app_content_mounted', 'profiles_requested', 'profiles_succeeded'],
    },
    {
      name: 'profile request failure',
      profilesShouldFail: true,
      expected: ['js_module_loaded', 'root_effect', 'initial_url_requested', 'initial_url_null',
        'app_content_mounted', 'profiles_requested', 'profiles_failed'],
    },
  ];

  for (const item of cases) {
    await t.test(item.name, async () => {
      const { environment, native } = makeNativeEnvironment();
      Object.assign(environment, item);
      const App = loadApp(environment, native, false, true);
      const root = createRoot();
      try {
        await act(async () => { root.render(React.createElement(App)); });
        assert.deepEqual(environment.startupPhases, item.expected);
      } finally {
        await act(async () => { root.unmount(); });
      }
    });
  }

  await t.test('allowed fixture URL never touches native remote control', async () => {
    const { environment, native } = makeNativeEnvironment();
    environment.initialURL = 'meeterm://smoke?screen=terminal';
    const App = loadApp(environment, native, false, true);
    const root = createRoot();
    try {
      await act(async () => { root.render(React.createElement(App)); });
      assert.deepEqual(environment.startupPhases, [
        'js_module_loaded', 'root_effect', 'initial_url_requested',
        'initial_url_allowed_fixture', 'app_content_mounted',
      ]);
      assert.deepEqual(environment.nativeCalls, []);
      assert.deepEqual(environment.foregroundCalls, []);
      assert.deepEqual(environment.visibility, []);
      assert.deepEqual(environment.calls, []);
    } finally {
      await act(async () => { root.unmount(); });
    }
  });

  await t.test('pending URL request does not mount AppContent or start native work', async () => {
    const { environment, native } = makeNativeEnvironment();
    environment.initialURLBehavior = 'pending';
    const App = loadApp(environment, native, false, true);
    const root = createRoot();
    try {
      await act(async () => { root.render(React.createElement(App)); });
      assert.deepEqual(environment.startupPhases, [
        'js_module_loaded', 'root_effect', 'initial_url_requested',
      ]);
      assert.equal(terminalViews(root).length, 0);
      assert.deepEqual(environment.nativeCalls, []);
      assert.deepEqual(environment.foregroundCalls, []);
      assert.deepEqual(environment.visibility, []);
      assert.deepEqual(environment.calls, []);
    } finally {
      await act(async () => { root.unmount(); });
    }
  });

  await t.test('diagnostic bridge failure remains observational', async () => {
    const { environment, native } = makeNativeEnvironment();
    environment.startupPhaseShouldFail = true;
    const App = loadApp(environment, native, false, true);
    const root = createRoot();
    try {
      await act(async () => { root.render(React.createElement(App)); });
      assert.equal(terminalViews(root).length, 0);
      assert.deepEqual(environment.nativeCalls, ['getProfiles', 'getPreferences']);
    } finally {
      await act(async () => { root.unmount(); });
    }
  });
});

test('agent status metadata keeps null, live, and unavailable states distinct', () => {
  const { environment, native } = makeNativeEnvironment();
  const { resolveAgentStatus, agentStatusPhrase, AGENT_STATUS_META } = loadApp(environment, native, true);
  const expected = {
    blocked: 'Agent status: blocked, needs attention',
    done: 'Agent status: finished, not yet viewed',
    working: 'Agent status: working',
    idle: 'Agent status: idle',
    unknown: 'Agent status: unknown',
  };
  for (const [status, phrase] of Object.entries(expected)) {
    assert.equal(resolveAgentStatus(status, true), status);
    assert.equal(agentStatusPhrase(status, true), phrase);
    assert.equal(AGENT_STATUS_META[status].spoken, phrase);
  }
  assert.equal(resolveAgentStatus(null, true), null);
  assert.equal(resolveAgentStatus('blocked', true), 'blocked');
  assert.equal(resolveAgentStatus('blocked', false), 'unavailable');
  assert.equal(agentStatusPhrase('working', false), 'Agent status unavailable');
  assert.equal(AGENT_STATUS_META.idle.shape, 'hollow');
  assert.equal(AGENT_STATUS_META.unknown.shape, 'dot');
});

test('status indicators expose the requested mark grammar and visible selected-line labels', async () => {
  const { environment, native } = makeNativeEnvironment();
  const { AgentStatusIndicator } = loadApp(environment, native, true);
  const root = createRoot();
  try {
    await act(async () => {
      root.render(React.createElement('View', null,
        ...['blocked', 'done', 'working', 'idle', 'unknown'].map(status => React.createElement(
          AgentStatusIndicator,
          { key: status, status, live: true, colors: LIGHT, testID: status },
        )),
        React.createElement(AgentStatusIndicator, { status: 'unknown', live: false, colors: DARK, showLabel: true, testID: 'unavailable' }),
        React.createElement(AgentStatusIndicator, { status: null, live: true, colors: LIGHT, testID: 'none' }),
      ));
    });
    for (const status of ['blocked', 'done', 'working', 'idle', 'unknown']) {
      assert.ok(findTestId(root, status), `${status} status indicator is missing`);
    }
    assert.ok(all(root, node => node.props?.style?.some?.(style => style?.width === 8)).length > 0, 'blocked should use the filled circle size');
    assert.ok(all(root, node => node.props?.style?.some?.(style => style?.borderWidth === 1.5)).length > 0, 'idle should use a hollow mark');
    assert.equal(textContent(findTestId(root, 'unavailable')), 'Status unavailable');
    assert.equal(all(root, node => node.props?.testID === 'none').length, 0);
  } finally {
    await act(async () => { root.unmount(); });
  }
});

test('Herdr status metadata renders on its owning surfaces without JS rollups or pane IDs', async t => {
  const snapshot = makeSnapshot({
    workspaces: [workspace('W1', 'Workspace One', 'blocked'), workspace('W2', 'Workspace Two', 'idle')],
    groups: [
      group('G1', 'W1', 'Group One', true, 'working'),
      group('G2', 'W1', 'Group Two', false, 'done'),
      group('G3', 'W2', 'Group Three', true, 'unknown'),
    ],
    terminals: [
      pane('P1', 'W1', 'G1', 'native:P1', true, true, 'Build', { name: 'Claude Code', status: 'working' }),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'Shell'),
      pane('P3', 'W1', 'G2', 'native:P3', false, false, 'Review', { name: 'Codex', status: 'done' }),
    ],
  });
  const fixture = await mountForTest(t, snapshot);
  const workspaceOrder = () => all(fixture.root, node => typeof node.props?.testID === 'string' && node.props.testID.startsWith('workspace-row-')).map(node => node.props.testID);

  assert.match(findTestId(fixture.root, 'workspace-row-W1').props.accessibilityLabel, /Agent status: blocked/);
  assert.match(findTestId(fixture.root, 'workspace-row-W2').props.accessibilityLabel, /Agent status: idle/);
  assert.deepEqual(workspaceOrder(), ['workspace-row-W1', 'workspace-row-W2']);
  assert.equal(all(fixture.root, node => textContent(node).includes('Needs attention 1')).length, 0, 'workspace rows must not contain a JS-generated agent summary');

  await openWorkspace(fixture.root, 'W1');
  const groupPicker = first(fixture.root, node => node.props?.accessibilityLabel?.startsWith('Switch terminal group'), 'missing the group picker');
  assert.match(groupPicker.props.accessibilityLabel, /Agent status: working/);
  assert.equal(findLabel(fixture.root, 'Terminal Build, Agent status: working').props.accessibilityLabel, 'Terminal Build, Agent status: working');
  assert.equal(findLabel(fixture.root, 'Terminal Shell').props.accessibilityLabel, 'Terminal Shell');
  assert.equal(all(fixture.root, node => node.props?.testID === 'terminal-agent-status-P2').length, 0, 'agentless panes must not receive a status mark');
  assert.equal(findTestId(fixture.root, 'selected-agent-line').props.accessibilityLabel, 'Claude Code, Agent status: working');
  assert.equal(all(fixture.root, node => textContent(node) === 'Working').length > 0, true);

  await press(fixture.root, groupPicker);
  assert.match(findLabel(fixture.root, 'Group Group One, Agent status: working').props.accessibilityLabel, /Agent status: working/);
  assert.match(findLabel(fixture.root, 'Group Group Two, Agent status: finished, not yet viewed').props.accessibilityLabel, /Agent status: finished/);

  fixture.environment.connection.state = 'Reconnecting';
  await poll(fixture.environment);
  assert.match(findLabel(fixture.root, 'Group Group One, Agent status unavailable').props.accessibilityLabel, /Agent status unavailable/);
  assert.equal(fixture.environment.snapshot.workspaces[0].agentStatus, 'blocked', 'presentation must not mutate the stored upstream status');
  await press(fixture.root, findLabel(fixture.root, 'Close sheet'));
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));
  assert.match(findTestId(fixture.root, 'workspace-row-W1').props.accessibilityLabel, /Agent status unavailable/);

  const updated = clone(snapshot);
  updated.workspaces = [workspace('W1', 'Workspace One', 'done'), workspace('W2', 'Workspace Two', 'blocked')];
  fixture.environment.connection.state = 'Ready';
  await updateSnapshot(fixture.environment, updated);
  assert.deepEqual(workspaceOrder(), ['workspace-row-W1', 'workspace-row-W2'], 'status changes must not reorder workspaces');
});

test('disabled workspace rows keep the unavailable mark at full contrast', async t => {
  const snapshot = makeSnapshot({
    workspaces: [workspace('W1', 'Workspace One', 'blocked')],
    groups: [group('G1', 'W1', 'Group One', true, 'working')],
    terminals: [pane('P1', 'W1', 'G1', 'native:P1', true, true, 'Build', { name: 'Claude Code', status: 'working' })],
  });
  const fixture = await mountForTest(t, snapshot);
  await settleAsync();

  fixture.environment.connection.state = 'Reconnecting';
  await poll(fixture.environment);

  const row = findTestId(fixture.root, 'workspace-row-W1');
  const status = findTestId(fixture.root, 'workspace-agent-status-W1');
  const styleObjects = value => {
    if (Array.isArray(value)) return value.flatMap(styleObjects);
    return value && typeof value === 'object' ? [value] : [];
  };
  const hasOpacity = value => styleObjects(value).some(style => style.opacity !== undefined);
  const rowStyle = typeof row.props.style === 'function' ? row.props.style({ pressed: false }) : row.props.style;

  // The live mark is a direct child of the pressable. Its ancestor cannot
  // composite a disabled opacity, while the icon/copy affordances still do.
  assert.equal(status.parent, row);
  assert.equal(hasOpacity(rowStyle), false, 'the workspace pressable must not fade the status mark');
  assert.equal(hasOpacity(status.props.style), false, 'the status indicator itself must stay opaque');
  const fadedChildren = row.children.filter(child => typeof child !== 'string')
    .filter(child => child && child !== status && hasOpacity(child.props?.style));
  assert.ok(fadedChildren.length >= 2, 'disabled workspace content should retain its subdued affordance');

  assert.match(row.props.accessibilityLabel, /Agent status unavailable/);
  assert.equal(status.parent, row);
  assert.equal(hasOpacity(status.props.style), false, 'unavailable status mark must remain at full opacity');
});

test('settings appearance has the same visible and accessible meaning', async () => {
  const filename = path.join(REPO_ROOT, 'app/DailyUse.tsx');
  const compiled = TypeScript.transpileModule(fs.readFileSync(filename, 'utf8'), {
    compilerOptions: { module: TypeScript.ModuleKind.CommonJS, jsx: TypeScript.JsxEmit.ReactJSX },
  }).outputText;
  const { environment } = makeNativeEnvironment();
  const rn = { ...makeReactNativeMocks(environment), KeyboardAvoidingView: 'KeyboardAvoidingView', Switch: 'Switch' };
  const modules = new Map([
    ['react', React], ['react/jsx-runtime', require('react/jsx-runtime')],
    ['react-native', rn], ['react-native-safe-area-context', makeSafeAreaMocks()],
    ['./ui', makeUiMocks()],
  ]);
  const dailyUse = { exports: {} };
  vm.runInNewContext(compiled, {
    exports: dailyUse.exports,
    require: name => { assert.ok(modules.has(name), name); return modules.get(name); },
  }, { filename });
  const root = createRoot();
  try {
    await act(async () => {
      root.render(React.createElement(dailyUse.exports.SettingsForm, {
        visible: true, preferences: PREFERENCES, colors: LIGHT,
        onClose() {}, async onSave() { return true; },
      }));
    });
    assert.equal(findTestId(root, 'terminal-theme').props.accessibilityLabel, 'Appearance');
    assert.equal(all(root, node => node.props?.accessibilityLabel === 'Terminal theme').length, 0);
  } finally {
    await act(async () => { root.unmount(); });
  }
});

test('an inactive fixture deep link follows UI foreground changes without reconnecting', async () => {
  const { environment, native } = makeNativeEnvironment();
  environment.initialAppState = 'inactive';
  environment.initialURL = 'meeterm://smoke?screen=terminal';
  const App = loadApp(environment, native, false, true);
  const root = createRoot();
  try {
    await act(async () => { root.render(React.createElement(App)); });
    assert.equal(terminalViews(root).length, 0);
    await act(async () => { environment.emitAppState('active'); });
    assert.deepEqual(terminalViews(root).map(view => view.props.terminalId), ['poc-main']);
    await act(async () => { environment.emitAppState('background'); });
    assert.equal(terminalViews(root).length, 0);
    await act(async () => { environment.emitAppState('active'); });
    assert.deepEqual(terminalViews(root).map(view => view.props.terminalId), ['poc-main']);
    assert.deepEqual(environment.foregroundCalls, []);
    assert.deepEqual(environment.visibility, []);
    assert.deepEqual(environment.calls, []);
    assert.equal(environment.intervalCallbacks.length, 0);
  } finally {
    await act(async () => { root.unmount(); });
  }
  assert.equal(environment.appStateListeners.size, 0);
});

test('normal app foreground events retain ordered native lifecycle delivery', async t => {
  const { root, environment } = await mountForTest(t, makeSnapshot());
  await openWorkspace(root, 'W1');
  await act(async () => { environment.emitAppState('background'); });
  assert.equal(terminalViews(root).length, 0);
  await act(async () => { environment.emitAppState('inactive'); environment.emitAppState('active'); });
  assert.deepEqual(terminalViews(root).map(view => view.props.terminalId), ['native:P1']);
  assert.deepEqual(environment.foregroundCalls, [true, false, false, true]);
});

test('reduced-motion hook reads the initial preference, follows changes and cleans up', async () => {
  // Run the real hook with a mocked platform settings boundary. This verifies
  // its subscription lifecycle, not an OS setting or animation performance.
  const source = TypeScript.createSourceFile('ui.tsx', fs.readFileSync(path.join(REPO_ROOT, 'app/ui.tsx'), 'utf8'), TypeScript.ScriptTarget.Latest, true, TypeScript.ScriptKind.TSX);
  const declaration = source.statements.find(statement => TypeScript.isFunctionDeclaration(statement) && statement.name.text === 'useReducedMotion');
  assert.ok(declaration);
  const compiled = TypeScript.transpileModule(declaration.getText(source), { compilerOptions: { module: TypeScript.ModuleKind.CommonJS } }).outputText;
  let listener;
  let removed = false;
  const moduleExports = {};
  vm.runInNewContext(compiled, {
    exports: moduleExports,
    useEffect: React.useEffect,
    useState: React.useState,
    AccessibilityInfo: {
      async isReduceMotionEnabled() { return true; },
      addEventListener(event, callback) {
        assert.equal(event, 'reduceMotionChanged');
        listener = callback;
        return { remove() { removed = true; } };
      },
    },
  });
  let observed;
  function Probe() { observed = moduleExports.useReducedMotion(); return null; }
  const root = createRoot();
  try {
    await act(async () => { root.render(React.createElement(Probe)); });
    assert.equal(observed, true);
    await act(async () => { listener(false); });
    assert.equal(observed, false);
    await act(async () => { listener(true); });
    assert.equal(observed, true);
  } finally {
    await act(async () => { root.unmount(); });
  }
  assert.equal(removed, true);
});

test('light supporting text and action colors retain readable contrast', () => {
  const source = TypeScript.createSourceFile('ui.tsx', fs.readFileSync(path.join(REPO_ROOT, 'app/ui.tsx'), 'utf8'), TypeScript.ScriptTarget.Latest, true, TypeScript.ScriptKind.TSX);
  let colors;
  for (const statement of source.statements) {
    if (!TypeScript.isVariableStatement(statement)) continue;
    const declaration = statement.declarationList.declarations.find(item => item.name.getText(source) === 'LIGHT');
    if (declaration) colors = Object.fromEntries(declaration.initializer.properties.map(item => [item.name.getText(source), item.initializer.text]));
  }
  assert.ok(colors);
  const luminance = color => {
    const [r, g, b] = color.slice(1).match(/../g).map(value => parseInt(value, 16) / 255)
      .map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
    return r * 0.2126 + g * 0.7152 + b * 0.0722;
  };
  for (const [text, background] of [['text', 'background'], ['muted', 'surface'], ['placeholder', 'surface'], ['accent', 'surface'], ['onAccent', 'accentFill']]) {
    const values = [luminance(colors[text]), luminance(colors[background])].sort((a, b) => b - a);
    assert.ok((values[0] + 0.05) / (values[1] + 0.05) >= 4.5, `${text} on ${background}`);
  }
});

test('agent status palette keeps every non-text mark at three-to-one contrast', () => {
  const source = TypeScript.createSourceFile('ui.tsx', fs.readFileSync(path.join(REPO_ROOT, 'app/ui.tsx'), 'utf8'), TypeScript.ScriptTarget.Latest, true, TypeScript.ScriptKind.TSX);
  const palettes = {};
  for (const statement of source.statements) {
    if (!TypeScript.isVariableStatement(statement)) continue;
    const declaration = statement.declarationList.declarations[0];
    if (!declaration || !['LIGHT', 'DARK'].includes(declaration.name.getText(source)) || !TypeScript.isObjectLiteralExpression(declaration.initializer)) continue;
    const palette = {};
    for (const property of declaration.initializer.properties) {
      if (!TypeScript.isPropertyAssignment(property)) continue;
      const key = property.name.getText(source);
      if (TypeScript.isStringLiteral(property.initializer)) {
        palette[key] = property.initializer.text;
      } else if (TypeScript.isObjectLiteralExpression(property.initializer)) {
        palette[key] = Object.fromEntries(property.initializer.properties
          .filter(TypeScript.isPropertyAssignment)
          .map(item => [item.name.getText(source), item.initializer.text]));
      }
    }
    palettes[declaration.name.getText(source)] = palette;
  }
  const luminance = color => {
    const [r, g, b] = color.slice(1).match(/../g).map(value => parseInt(value, 16) / 255)
      .map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
    return r * 0.2126 + g * 0.7152 + b * 0.0722;
  };
  for (const [name, palette] of Object.entries(palettes)) {
    assert.ok(palette.agentStatus, `${name} agentStatus tokens are missing`);
    const backgrounds = name === 'DARK' ? [palette.background, palette.surface, palette.terminal] : [palette.background, palette.surface];
    for (const [status, color] of Object.entries(palette.agentStatus)) {
      for (const background of backgrounds) {
        const values = [luminance(color), luminance(background)].sort((a, b) => b - a);
        assert.ok((values[0] + 0.05) / (values[1] + 0.05) >= 3, `${name}.${status} on ${background}`);
      }
    }
  }
});

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function pane(id, workspaceId, groupId, terminalId, selected = false, active = false, name = id, agent = null) {
  return {
    id,
    workspaceId,
    groupId,
    terminalId,
    name,
    active,
    selected,
    agent,
  };
}

function group(id, workspaceId, name, selected, agentStatus = null) {
  return { id, workspaceId, name, selected, agentStatus };
}

function workspace(id, name, agentStatus = null) {
  return { id, name, agentStatus };
}

function makeSnapshot({
  selectedPane = 'P1',
  workspaces = [workspace('W1', 'Workspace One'), workspace('W2', 'Workspace Two')],
  groups = [
    group('G1', 'W1', 'Group One', true),
    group('G2', 'W2', 'Group Two', true),
  ],
  terminals = [
    pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1'),
    pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
    pane('P3', 'W2', 'G2', 'native:P3', false, true, 'P3'),
  ],
  control = workspaceControl(),
} = {}) {
  return {
    backend: 'herdr',
    runtime: 'default',
    groupsSupported: true,
    workspaces,
    groups,
    terminals: terminals.map(item => ({ ...item, selected: item.id === selectedPane })),
    control,
  };
}

function selectPane(snapshot, paneId) {
  const selected = snapshot.terminals.find(item => item.id === paneId);
  if (!selected) return;
  snapshot.terminals = snapshot.terminals.map(item => ({ ...item, selected: item.id === paneId }));
  snapshot.groups = snapshot.groups.map(item => item.workspaceId === selected.workspaceId
    ? { ...item, selected: item.id === selected.groupId }
    : item);
}

function selectGroup(snapshot, groupId) {
  const selected = snapshot.groups.find(item => item.id === groupId);
  if (!selected) return;
  snapshot.groups = snapshot.groups.map(item => item.workspaceId === selected.workspaceId
    ? { ...item, selected: item.id === groupId }
    : item);
  snapshot.terminals = snapshot.terminals.map(item => ({ ...item, selected: false }));
}

function closePane(snapshot, paneId) {
  const removed = snapshot.terminals.find(item => item.id === paneId);
  snapshot.terminals = snapshot.terminals.filter(item => item.id !== paneId);
  if (!removed) return;
  const replacement = snapshot.terminals.find(item => item.groupId === removed.groupId)
    || snapshot.terminals.find(item => item.workspaceId === removed.workspaceId);
  if (replacement) selectPane(snapshot, replacement.id);
}

function textContent(node) {
  if (!node) return '';
  return node.children.map(child => typeof child === 'string' ? child : textContent(child)).join('');
}

function all(root, predicate) {
  return root.container.queryAll(predicate);
}

function first(root, predicate, description) {
  const match = all(root, predicate)[0];
  assert.ok(match, description || 'expected a matching rendered host node');
  return match;
}

function findLabel(root, label) {
  return first(root, node => node.props && node.props.accessibilityLabel === label, `missing accessibility label ${label}`);
}

function findText(root, value) {
  return first(root, node => node.type === 'Text' && textContent(node) === value, `missing text ${value}`);
}

function findTestId(root, id) {
  return first(root, node => node.props && node.props.testID === id, `missing testID ${id}`);
}

function terminalViews(root) {
  return all(root, node => node.type === 'TerminalView');
}

function workspaceTitle(root) {
  const switcher = findLabel(root, 'Switch workspace');
  return textContent(switcher);
}

function groupTitle(root) {
  const switcher = all(root, node => node.props && node.props.accessibilityLabel === 'Switch terminal group')[0];
  return switcher ? textContent(switcher) : '';
}

async function mountApp(snapshot, configure) {
  const { environment, native } = makeNativeEnvironment();
  environment.snapshot = clone(snapshot);
  configure?.(environment, native);
  const App = loadApp(environment, native);
  const root = createRoot();
  await act(async () => {
    root.render(React.createElement(App));
  });
  return { root, environment, native };
}

async function mountForTest(t, snapshot, configure) {
  const fixture = await mountApp(snapshot, configure);
  t.after(async () => {
    await act(async () => {
      fixture.root.unmount();
    });
  });
  return fixture;
}

async function mountConfiguredForTest(t, configure) {
  const { environment, native } = makeNativeEnvironment();
  configure(environment, native);
  const App = loadApp(environment, native);
  const root = createRoot();
  await act(async () => {
    root.render(React.createElement(App));
  });
  await settleAsync();
  const fixture = { root, environment, native };
  t.after(async () => {
    await act(async () => {
      fixture.root.unmount();
    });
  });
  return fixture;
}

function pickerProfile(host = 'queued.example') {
  return {
    id: '00000000-0000-4000-8000-000000000021',
    name: 'Queued picker',
    host,
    port: 22,
    username: 'developer',
    authMethod: 'password',
    credentialSaved: true,
    backend: 'tmux',
    runtime: 'meeterm',
  };
}

function pickerDiscovery(revision = 1, tmuxCandidates = [runtimeCandidate('queued-tmux', 'tmux', 'meeterm', 'running', { isDefault: true })]) {
  return {
    connectionGeneration: '1',
    revision,
    backends: [
      runtimeBackend('tmux', tmuxCandidates),
      runtimeBackend('herdr', [runtimeCandidate('queued-herdr', 'herdr', 'default', 'running', { isDefault: true })]),
    ],
  };
}

async function mountSavedPicker(t, configure) {
  const profile = pickerProfile();
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.connection = { ...environment.connection, state: 'Disconnected', host: '', port: 0 };
    environment.profiles = [profile];
    environment.runtimeDiscovery = pickerDiscovery();
    environment.snapshot = makeSnapshot();
    configure(environment, profile);
  });
  await press(fixture.root, findLabel(fixture.root, 'Connect saved server Queued picker'));
  await settleAsync();
  return { fixture, profile };
}

async function connectSavedProfileToRuntime(t, profile, candidate, extraProfiles = []) {
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.connection = { ...environment.connection, state: 'Disconnected', host: '', port: 0 };
    environment.profiles = [profile, ...extraProfiles];
    environment.runtimeDiscovery = pickerDiscovery(1, [candidate]);
    environment.snapshot = makeSnapshot();
  });
  await press(fixture.root, findLabel(fixture.root, `Connect saved server ${profile.name}`));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, `runtime-row-${candidate.backend}-${candidate.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'), 'the saved runtime should be Ready before testing a switch');
  return fixture;
}

async function poll(env) {
  // The metadata poll is always interval 0; attachment transfer/delete polls
  // may legitimately register alongside it while an operation is live.
  assert.ok(env.intervalCallbacks.length >= 1, 'App should register the metadata polling interval');
  await act(async () => {
    env.intervalCallbacks[0]();
  });
}

async function press(root, node) {
  assert.equal(typeof node.props.onPress, 'function', `node ${node.type} must be pressable`);
  await act(async () => {
    const result = node.props.onPress();
    if (result && typeof result.then === 'function') await result;
  });
}

async function settleAsync(rounds = 4) {
  for (let index = 0; index < rounds; index += 1) {
    await act(async () => {
      await new Promise(resolve => setImmediate(resolve));
    });
  }
}

async function openWorkspace(root, id) {
  await press(root, findTestId(root, `workspace-row-${id}`));
  assert.equal(workspaceTitle(root), id === 'W1' ? 'Workspace One' : 'Workspace Two');
}

async function updateSnapshot(env, snapshot) {
  env.snapshot = clone(snapshot);
  await poll(env);
}

async function mountRecovering(t, control, connectionState = 'Reconnecting', configure) {
  const fixture = await mountForTest(t, makeSnapshot(), configure);
  await openWorkspace(fixture.root, 'W1');
  fixture.environment.connection.state = connectionState;
  fixture.environment.snapshot = makeSnapshot({ control });
  await poll(fixture.environment);
  await settleAsync();
  return fixture;
}

async function mountWorkspaceRecovery(t, { phase, reason, operationEpoch, connectionState, errorCode = '' }) {
  const fixture = await mountForTest(t, makeSnapshot());
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));
  fixture.environment.connection = {
    ...fixture.environment.connection,
    state: connectionState,
    errorCode,
    errorMessage: '',
  };
  await updateSnapshot(fixture.environment, makeSnapshot({
    control: workspaceControl({
      operationEpoch,
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase, reason, attempt: phase === 'stopped' ? 6 : 0, maxAttempts: 6 },
    }),
  }));
  await settleAsync();
  return fixture;
}

async function closeCurrentPane(root, environment) {
  await press(root, findLabel(root, 'Terminal menu'));
  const close = findLabel(root, 'Close terminal');
  await press(root, close);
  assert.ok(environment.alert, 'closing a pane should present a confirmation alert');
  const action = environment.alert.buttons[environment.alert.buttons.length - 1];
  assert.equal(typeof action.onPress, 'function', 'close confirmation should have a destructive action');
  await act(async () => {
    action.onPress();
  });
}

test('switcher opens without native work and an explicit saved-server choice replaces the current owner', async t => {
  const target = {
    id: '00000000-0000-4000-8000-000000000031', name: 'Active target',
    host: 'target.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'meeterm',
  };
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.profiles = [target];
    environment.runtimeDiscovery = pickerDiscovery(3, [runtimeCandidate('active-tmux', 'tmux', 'meeterm', 'running', { isDefault: true })]);
    environment.snapshot = makeSnapshot();
  });

  const callsBeforeOpen = fixture.environment.nativeCalls.length;
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();
  assert.equal(fixture.environment.nativeCalls.length, callsBeforeOpen, 'opening the switcher must not connect or discover');
  const currentServerRow = findTestId(fixture.root, 'switcher-server-__current_connection__');
  assert.match(textContent(currentServerRow), /Current · Herdr · default/,
    'Current must reflect the selected native snapshot, alongside the endpoint');
  assert.doesNotMatch(textContent(findTestId(fixture.root, `switcher-server-${target.id}`)), /Current/,
    'the other saved server is a destination, not the current Session');

  await press(fixture.root, findTestId(fixture.root, `switcher-server-${target.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.alert, null);
  const releasedAt = fixture.environment.nativeCalls.indexOf('disconnectForSwitcher');
  const connectedAt = fixture.environment.nativeCalls.findIndex(call => call?.method === 'connectProfileHost' && call.profileId === target.id);
  assert.ok(releasedAt >= 0 && connectedAt > releasedAt, 'the old owner must be released before the new host starts');
  assert.ok(findText(fixture.root, 'Choose a Session'));
  const destinationSession = findTestId(fixture.root, 'runtime-row-tmux-active-tmux');
  assert.ok(destinationSession);
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Choose a runtime for Active target').length, 0, 'the standalone runtime picker must stay hidden');

  await press(fixture.root, destinationSession);
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'), 'the cross-server switch lands in Workspace only after Ready');
  assert.deepEqual(fixture.environment.lastUsedUpdates, [
    { profileId: target.id, backend: 'tmux', runtime: 'meeterm' },
  ]);
  assert.equal(fixture.environment.profiles[0].id, target.id, 'runtime updates preserve the saved profile identity');
  assert.equal(fixture.environment.profiles[0].credentialSaved, true, 'switching preserves secure credential ownership');
});

test('iOS switcher closes before the existing saved-server and add-server sheets are presented', async t => {
  const current = pickerProfile('switcher-handoff.example');
  current.name = 'Switcher handoff';
  const fixture = await connectSavedProfileToRuntime(t, current,
    runtimeCandidate('switcher-handoff-runtime', 'tmux', 'meeterm', 'running'));
  const savedServersTitle = () => all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Saved servers').length;

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  assert.ok(findTestId(fixture.root, 'switcher-manage-servers'));
  await press(fixture.root, findTestId(fixture.root, 'switcher-manage-servers'));

  assert.equal(savedServersTitle(), 0, 'the current switcher page sheet must dismiss before the saved-server manager is presented');
  assert.equal(all(fixture.root, node => node.props?.testID === 'connection-form-visible').length, 0,
    'the add form must not appear above the still-presented switcher');
  await act(async () => { fixture.environment.dismissPresentedModal(); });
  assert.equal(savedServersTitle(), 1, 'the existing Saved servers screen should appear after the switcher dismisses');
  assert.ok(findLabel(fixture.root, 'Add server'));

  await press(fixture.root, findLabel(fixture.root, 'Add server'));
  assert.equal(savedServersTitle(), 0, 'the manager page sheet must dismiss before the add form is presented');
  assert.equal(all(fixture.root, node => node.props?.testID === 'connection-form-visible').length, 0,
    'the add form must wait for the manager dismissal callback');
  await act(async () => { fixture.environment.dismissPresentedModal(); });

  assert.equal(savedServersTitle(), 0);
  assert.ok(findTestId(fixture.root, 'connection-form-visible'), 'the existing ConnectionForm should appear after the manager dismisses');
});

test('iOS switcher verifies an unknown cross-endpoint host before showing its Session list', async t => {
  const current = pickerProfile('trusted-old.example');
  current.name = 'Trusted source';
  const target = {
    ...pickerProfile('untrusted-target.example'),
    id: '00000000-0000-4000-8000-000000000046',
    name: 'Untrusted target',
    port: 2224,
  };
  const fixture = await connectSavedProfileToRuntime(
    t,
    current,
    runtimeCandidate('trusted-source', 'tmux', 'source-session', 'running'),
    [target],
  );
  fixture.environment.hostKeyPendingProfileId = target.id;
  fixture.environment.runtimeDiscovery = pickerDiscovery(2, [
    runtimeCandidate('untrusted-destination', 'tmux', 'destination-session', 'running'),
  ]);

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${target.id}`));
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'HostKeyPending');
  assert.equal(fixture.environment.alert?.title, 'Trust this SSH host?');
  assert.equal(all(fixture.root, node => node.props?.testID === 'runtime-row-tmux-untrusted-destination').length, 0,
    'candidate rows must wait until the user approves the host identity');
  const trust = fixture.environment.alert.buttons.find(button => button.text === 'Trust and connect');
  assert.equal(typeof trust?.onPress, 'function');
  await act(async () => { trust.onPress(); });
  await poll(fixture.environment);
  await settleAsync();

  assert.ok(findText(fixture.root, 'Choose a Session'),
    'the switcher should continue into the Session list after host approval');
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-untrusted-destination'));
  assert.equal(fixture.environment.connection.port, target.port);
});

test('closing the untouched switcher keeps the selected owner and workspace intact', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000044', name: 'Close check',
    host: 'close-check.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'kept-session',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('close-check', 'tmux', 'kept-session', 'running'));
  const nativeCallsBefore = fixture.environment.nativeCalls.length;
  const oldWorkspace = findTestId(fixture.root, 'workspace-row-W1');

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  assert.match(textContent(findTestId(fixture.root, `switcher-server-${profile.id}`)), /Current · tmux · kept-session/);
  await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Ready');
  assert.equal(findTestId(fixture.root, 'workspace-row-W1').props.accessibilityLabel, oldWorkspace.props.accessibilityLabel);
  assert.equal(fixture.environment.nativeCalls.slice(nativeCallsBefore).some(call => call === 'disconnect'
    || call?.method === 'changeRuntime' || call?.method === 'connectProfileHost'), false);
});

test('same-server switch releases through changeRuntime and does not mark the last-used hint current', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000041', name: 'Same server',
    host: 'same.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const initial = runtimeCandidate('same-initial', 'tmux', 'initial', 'running', { isDefault: true });
  const fixture = await connectSavedProfileToRuntime(t, profile, initial);
  assert.deepEqual(fixture.environment.lastUsedUpdates, [{ profileId: profile.id, backend: 'tmux', runtime: 'initial' }]);

  const callsBeforeOpen = fixture.environment.nativeCalls.length;
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();
  assert.equal(fixture.environment.nativeCalls.length, callsBeforeOpen, 'opening the switcher must leave the Ready owner alone');
  assert.match(textContent(findTestId(fixture.root, `switcher-server-${profile.id}`)), /Current · tmux · initial/);

  fixture.environment.runtimeDiscovery = pickerDiscovery(2, [initial]);
  fixture.environment.changeRuntimeMode = 'pending';
  const serverRow = findTestId(fixture.root, `switcher-server-${profile.id}`);
  await act(async () => { serverRow.props.onPress(); serverRow.props.onPress(); });
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'changeRuntime'), [
    { method: 'changeRuntime', operationEpoch: '1' },
  ], 'two taps should enqueue one sequential native change');
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'), 'the old screen remains until native accepts the change');
  assert.equal(all(fixture.root, node => node.props?.testID === 'runtime-row-tmux-same-initial').length, 0);

  fixture.environment.resolvePendingChangeRuntime('accept');
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0);
  await poll(fixture.environment);
  await settleAsync();
  const hintRow = findTestId(fixture.root, 'runtime-row-tmux-same-initial');
  assert.match(textContent(hintRow), /Last used/);
  assert.doesNotMatch(textContent(hintRow), /Current/);
  assert.equal(fixture.environment.lastUsedUpdates.length, 1, 're-discovery alone must not update last-used metadata');

  await press(fixture.root, hintRow);
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'), 'an explicit Session selection reaches Ready');
  assert.deepEqual(fixture.environment.lastUsedUpdates, [
    { profileId: profile.id, backend: 'tmux', runtime: 'initial' },
    { profileId: profile.id, backend: 'tmux', runtime: 'initial' },
  ]);
});

test('same-server changeRuntime failure after release clears the stale workspace', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000047', name: 'Release failure',
    host: 'release-failure.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('release-failure-runtime', 'tmux', 'initial', 'running'));
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();
  fixture.environment.changeRuntimeFailureAfterRelease = true;

  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Disconnected', 'the fake native start failure has no replacement state');
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0,
    'a rejected setup after native release must not preserve the old workspace');
  const screenText = all(fixture.root, node => node.type === 'Text').map(textContent).join(' ');
  assert.match(screenText, /Could not start the new connection/);
  assert.doesNotMatch(screenText, /Connected/);
});

test('same-server stale-epoch rejection keeps the Ready workspace behind a closed input gate', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000048', name: 'Pre-release failure',
    host: 'pre-release-failure.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('pre-release-runtime', 'tmux', 'initial', 'running'));
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();
  // Opening the switcher hides the terminal view. A concurrent native epoch
  // change makes the captured changeRuntime epoch stale while its old actor is
  // still Ready; this must not be confused with release of that owner.
  fixture.environment.snapshot.control = workspaceControl({
    ...fixture.environment.snapshot.control,
    operationEpoch: '2',
    terminalInputReady: false,
  });
  fixture.environment.changeRuntimeShouldFail = true;

  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();

  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'changeRuntime'), [
    { method: 'changeRuntime', operationEpoch: '1' },
  ]);
  assert.equal(fixture.environment.connection.state, 'Ready');
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  const screenText = all(fixture.root, node => node.type === 'Text').map(textContent).join(' ');
  assert.match(screenText, /Could not start the switch/);
});

test('stale switch rejection preserves retained recovery for Reconnecting and Failed owners', async t => {
  for (const connectionState of ['Reconnecting', 'Failed']) {
    await t.test(connectionState, async subtest => {
      const profile = {
        id: `00000000-0000-4000-8000-00000000005${connectionState === 'Failed' ? '1' : '0'}`,
        name: `${connectionState} retained owner`,
        host: `${connectionState.toLowerCase()}-retained.example`, port: 22, username: 'developer',
        authMethod: 'password', credentialSaved: true, backend: 'tmux', runtime: 'initial',
      };
      const fixture = await connectSavedProfileToRuntime(subtest, profile,
        runtimeCandidate(`${connectionState.toLowerCase()}-retained`, 'tmux', 'initial', 'running'));
      await openWorkspace(fixture.root, 'W1');
      fixture.environment.connection.state = connectionState;
      fixture.environment.snapshot.control = workspaceControl({
        hasRetainedWork: true,
        runtimeOperationsReady: false,
        terminalInputReady: false,
        recovery: {
          phase: connectionState === 'Failed' ? 'stopped' : 'reconnecting',
          reason: connectionState === 'Failed' ? 'retry_exhausted' : 'transport',
          attempt: 2,
          maxAttempts: 6,
        },
      });
      await poll(fixture.environment);
      await settleAsync();
      const recoveryTitle = findTestId(fixture.root, 'recovery-title');
      assert.ok(recoveryTitle, `${connectionState} state should show its retained recovery route`);
      const retainedTerminalIds = fixture.environment.renderedTerminalIds.slice();

      await press(fixture.root, findTestId(fixture.root, 'switch-server-session'));
      await settleAsync();
      if (connectionState === 'Failed') {
        fixture.environment.disconnectForSwitcherResult = 'rejected_before_boundary';
      } else {
        fixture.environment.changeRuntimeShouldFail = true;
      }
      await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
      await settleAsync();
      assert.equal(fixture.environment.connection.state, connectionState,
        'a before-boundary rejection leaves concurrent transport recovery untouched');
      assert.match(all(fixture.root, node => node.type === 'Text').map(textContent).join(' '), /Could not start the switch/);

      await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
      await settleAsync();
      assert.equal(findTestId(fixture.root, 'recovery-title').children[0], recoveryTitle.children[0]);
      assert.ok(fixture.environment.renderedTerminalIds.some(id => retainedTerminalIds.includes(id)),
        'the cached Term view remains bound to its original native terminal');
      assert.equal(fixture.environment.snapshot.control.hasRetainedWork, true);
      assert.equal(fixture.environment.snapshot.control.terminalInputReady, false);
    });
  }
});

test('unknown switch bridge failure is never inferred as pre-boundary or restored to Ready', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000052', name: 'Unknown switch result',
    host: 'unknown-switch.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('unknown-switch-runtime', 'tmux', 'initial', 'running'));
  await press(fixture.root, findTestId(fixture.root, 'switch-server-session'));
  await settleAsync();
  fixture.environment.changeRuntimeThrowUnknown = true;

  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Ready', 'the fake native owner remains unchanged');
  assert.match(all(fixture.root, node => node.type === 'Text').map(textContent).join(' '), /switch result could not be confirmed/i);
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length, 0,
    'the App must not report an unverified owner as Ready');

  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Ready', 'polling still observes the fake native state');
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length, 0,
    'an unknown bridge result fences late Ready snapshots until an explicit new connection');
  assert.equal(fixture.environment.snapshot.control.runtimeOperationsReady, true,
    'the UI fence does not invent a native lifecycle mutation');
});

test('cancel after a cross-server switch releases the provisional host and ignores a late Ready selection', async t => {
  const current = pickerProfile('old.example');
  current.name = 'Old server';
  const target = { ...pickerProfile('new.example'), id: '00000000-0000-4000-8000-000000000042', name: 'New server' };
  const oldCandidate = runtimeCandidate('old-tmux', 'tmux', 'old-session', 'running');
  const fixture = await connectSavedProfileToRuntime(t, current, oldCandidate, [target]);
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();

  fixture.environment.runtimeDiscovery = pickerDiscovery(2, [runtimeCandidate('new-tmux', 'tmux', 'destination', 'running')]);
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${target.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-new-tmux'));
  fixture.environment.selectRuntimeMode = 'delayed-ready';
  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-new-tmux'));
  await settleAsync();
  assert.ok(fixture.environment.pendingSelection);

  await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0, 'the old Ready workspace is not restored');
  assert.equal(fixture.environment.calls.some(call => ['closePane', 'closeWorkspace', 'createTmuxSession'].includes(call.method)), false, 'cancel releases the connection without closing remote work');
  assert.ok(fixture.environment.nativeCalls.filter(call => call === 'disconnect' || call === 'disconnectForSwitcher').length >= 3);

  fixture.environment.resolvePendingSelection('ready');
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected', 'a late native Ready is released after cancellation');
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length, 0,
    'the canceled generation must never be shown as Connected');
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0, 'the late old generation cannot bind a workspace');
  assert.equal(all(fixture.root, node => node.props?.testID === 'switcher-server-__current_connection__').length, 0,
    'a canceled generation cannot be shown as the current Session');
  assert.ok(fixture.environment.nativeCalls.filter(call => call === 'disconnect' || call === 'disconnectForSwitcher').length >= 4,
    'the app disconnects again after observing the late native Ready');
  assert.equal(fixture.environment.lastUsedUpdates.length, 1, 'cancel does not save a hint for an uncommitted target');
});

test('cancel after same-server switch routes saved-profile reconnection through a fresh explicit picker', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000049', name: 'Reconnect after cancel',
    host: 'reconnect-after-cancel.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('reconnect-cancel-source', 'tmux', 'initial', 'running'));
  const destination = runtimeCandidate('reconnect-cancel-destination', 'tmux', 'fresh-session', 'running', {
    isDefault: true,
    lastUsed: true,
  });

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-reconnect-cancel-source'),
    'the same-server change must cross into the explicit Session picker first');

  await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(all(fixture.root, node => node.props?.accessibilityLabel === 'Reconnect').length, 0,
    'the in-memory native profile was cleared, so the unavailable reconnect action is hidden');

  // A late Ready from the canceled generation remains fenced even after the
  // user opens the server/session chooser.
  fixture.environment.connection = { ...fixture.environment.connection, state: 'Ready' };
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected');

  fixture.environment.runtimeDiscovery = pickerDiscovery(4, [destination]);
  await press(fixture.root, findTestId(fixture.root, 'switch-server-session'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'DiscoveringRuntimes');
  assert.equal(fixture.environment.nativeCalls.filter(call => call?.method === 'connectProfileHost' && call.profileId === profile.id).length, 2,
    'the saved profile path authenticates a fresh connection instead of using the cleared in-memory profile');
  assert.equal(fixture.environment.nativeCalls.filter(call => call === 'reconnect').length, 0);
  const destinationRow = findTestId(fixture.root, 'runtime-row-tmux-reconnect-cancel-destination');
  assert.ok(destinationRow, 'the fresh generation presents its candidate for an explicit choice');
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length, 0,
    'saved-profile authentication must not treat the canceled generation as Ready');

  await press(fixture.root, destinationRow);
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Ready');
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  assert.ok(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length > 0,
    'the explicitly selected new generation is shown as Connected');
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'selectRuntime').at(-1), {
    method: 'selectRuntime',
    candidateId: 'reconnect-cancel-destination',
  });
});

test('cancel after same-server switch opens the credential form when the profile has no saved credential', async t => {
  const source = runtimeCandidate('manual-source', 'tmux', 'manual-initial', 'running');
  const destination = runtimeCandidate('manual-destination', 'tmux', 'manual-fresh', 'running');
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.platform = 'android';
    environment.connection = { ...environment.connection, state: 'Disconnected', host: '', port: 0 };
    environment.profiles = [];
    environment.runtimeDiscovery = pickerDiscovery(1, [source]);
    environment.snapshot = makeSnapshot();
  });

  await press(fixture.root, findLabel(fixture.root, 'Connect'));
  await press(fixture.root, findTestId(fixture.root, 'connection-form-test-submit'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-manual-source'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Ready');

  await press(fixture.root, findTestId(fixture.root, 'switch-server-session'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'switcher-server-__current_connection__'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-manual-source'));
  await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(all(fixture.root, node => node.props?.accessibilityLabel === 'Reconnect').length, 0);

  fixture.environment.runtimeDiscovery = pickerDiscovery(3, [destination]);
  await press(fixture.root, findTestId(fixture.root, 'switch-server-session'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'switcher-server-__current_connection__'));
  await settleAsync();
  assert.ok(all(fixture.root, node => node.props?.testID === 'connection-form-visible').length > 0,
    `an unsaved profile returns to the existing credential form after owner release: ${JSON.stringify({
      connection: fixture.environment.connection,
      nativeCalls: fixture.environment.nativeCalls,
      text: all(fixture.root, node => node.type === 'Text').map(textContent),
    })}`);
  assert.equal(fixture.environment.nativeCalls.some(call => call?.method === 'connectProfileHost'), false);

  await press(fixture.root, findTestId(fixture.root, 'connection-form-test-submit'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-manual-destination'));
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Connected').length, 0,
    'the credential form starts fresh host discovery but not a selected runtime');

  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-manual-destination'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Ready');
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  assert.equal(fixture.environment.nativeCalls.filter(call => call === 'reconnect').length, 0);
});

test('Android Back cancels an in-progress switcher selection and closes its sheet', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000045', name: 'Back check',
    host: 'back-check.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'initial',
  };
  const fixture = await connectSavedProfileToRuntime(t, profile,
    runtimeCandidate('back-initial', 'tmux', 'initial', 'running'));

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-back-initial'));
  assert.equal(typeof fixture.environment.hardwareBackHandler, 'function');

  await act(async () => { fixture.environment.hardwareBackHandler(); });
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(all(fixture.root, node => node.props?.testID?.startsWith('switcher-server-')).length, 0,
    'Back should close the switcher after its one-way boundary');
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0,
    'Back must not restore the former Ready workspace');
});

test('switcher keeps auth failure in context and retries without restoring the former runtime', async t => {
  const current = pickerProfile('old-auth.example');
  current.name = 'Old auth server';
  const target = { ...pickerProfile('auth-target.example'), id: '00000000-0000-4000-8000-000000000043', name: 'Auth target' };
  const fixture = await connectSavedProfileToRuntime(t, current, runtimeCandidate('auth-old', 'tmux', 'old', 'running'), [target]);
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  fixture.environment.runtimeDiscovery = pickerDiscovery(2, [runtimeCandidate('auth-target-runtime', 'tmux', 'target', 'running')]);
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${target.id}`));
  await settleAsync();

  fixture.environment.connection = {
    ...fixture.environment.connection,
    state: 'Failed',
    errorCode: 'auth_failed',
    errorMessage: 'SSH authentication failed.',
  };
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findText(fixture.root, 'Authentication failed. Check your username and the password or private key for your chosen sign-in method.'));
  assert.ok(findTestId(fixture.root, 'switcher-retry'));
  assert.equal(all(fixture.root, node => node.props?.testID === 'workspace-row-W1').length, 0);

  await press(fixture.root, findTestId(fixture.root, 'switcher-retry'));
  await settleAsync();
  assert.ok(fixture.environment.nativeCalls.filter(call => call?.method === 'connectProfileHost' && call.profileId === target.id).length >= 2);
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Choose a runtime for Auth target').length, 0);
});

test('failed saved-profile connection skips switch confirmation but still opens target picker', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000033', name: 'Failed picker',
    host: 'failed.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'tmux', runtime: 'meeterm',
  };
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.connection = { ...environment.connection, state: 'Failed', host: profile.host, port: profile.port };
    environment.profiles = [profile];
    environment.runtimeDiscovery = pickerDiscovery(4, [runtimeCandidate('failed-tmux', 'tmux', 'meeterm', 'running', { isDefault: true })]);
    environment.snapshot = makeSnapshot();
  });

  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, `switcher-server-${profile.id}`));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.alert, null);
  assert.ok(fixture.environment.nativeCalls.some(call => call.method === 'connectProfileHost'));
  assert.ok(findText(fixture.root, 'Choose a Session'));
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-failed-tmux'));
});

test('saved-server connect authenticates first, then requires explicit runtime selection', async t => {
  const profile = {
    id: '00000000-0000-4000-8000-000000000021', name: 'Picker server',
    host: 'picker.example', port: 22, username: 'developer', authMethod: 'password',
    credentialSaved: true, backend: 'herdr', runtime: 'default',
  };
  const fixture = await mountConfiguredForTest(t, (environment) => {
    environment.connection = { ...environment.connection, state: 'Disconnected', host: '', port: 0 };
    environment.profiles = [profile];
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 2,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [
          runtimeCandidate('tmux-one', 'tmux', 'meeterm', 'running', { isDefault: true }),
        ] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [
          runtimeCandidate('herdr-one', 'herdr', 'default', 'running', { isDefault: true }),
          runtimeCandidate('herdr-stopped', 'herdr', 'paused', 'stopped'),
        ] },
      ],
    };
    environment.snapshot = makeSnapshot();
  });

  await press(fixture.root, findLabel(fixture.root, 'Connect saved server Picker server'));
  await settleAsync();
  assert.ok(fixture.environment.nativeCalls.some(call => call.method === 'connectProfileHost'));
  assert.equal(fixture.environment.nativeCalls.some(call => call.method === 'connectProfile'), false);
  assert.ok(findText(fixture.root, 'Choose a runtime for Picker server'));
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-tmux-one'));
  assert.ok(findTestId(fixture.root, 'runtime-row-herdr-herdr-one'));
  assert.equal(findTestId(fixture.root, 'runtime-row-herdr-herdr-stopped').props.accessibilityState.disabled, true);

  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-tmux-one'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(fixture.environment.calls.some(call => call.method === 'selectRuntime' && call.candidateId === 'tmux-one'));
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-tmux-one').length, 0);
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  assert.deepEqual(fixture.environment.lastUsedUpdates, [{
    profileId: profile.id, backend: 'tmux', runtime: 'meeterm',
  }]);
});

test('runtime picker cancellation disconnects provisional SSH and leaves backend sections independent', async t => {
  const partial = await mountConfiguredForTest(t, (environment) => {
    environment.connection = { ...environment.connection, state: 'DiscoveringRuntimes', host: 'partial.example', port: 22 };
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 3,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [
          runtimeCandidate('partial-tmux', 'tmux', 'meeterm', 'running', { isDefault: true }),
        ] },
        { backend: 'herdr', state: 'error', errorCode: 'herdr_missing', errorMessage: 'Herdr unavailable', canCreate: false, candidates: [] },
      ],
    };
  });
  assert.ok(findTestId(partial.root, 'runtime-row-tmux-partial-tmux'));
  assert.ok(all(partial.root, node => textContent(node).includes('Herdr unavailable')).length > 0);
  await press(partial.root, findLabel(partial.root, 'Cancel runtime selection'));
  await settleAsync();
  assert.ok(partial.environment.nativeCalls.includes('disconnect'));
  assert.equal(partial.environment.connection.state, 'Disconnected');
  assert.equal(all(partial.root, node => node.props && node.props.testID === 'runtime-row-tmux-partial-tmux').length, 0);

  const stopped = await mountConfiguredForTest(t, (environment) => {
    environment.connection = { ...environment.connection, state: 'AwaitingRuntimeSelection', host: 'stopped.example', port: 22 };
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 4,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [
          runtimeCandidate('stopped-herdr', 'herdr', 'default', 'stopped', { isDefault: true }),
        ] },
      ],
    };
  });
  const stoppedRow = findTestId(stopped.root, 'runtime-row-herdr-stopped-herdr');
  assert.equal(stoppedRow.props.accessibilityState.disabled, true);
  assert.match(textContent(stoppedRow), /Stopped/);

  const stale = await mountConfiguredForTest(t, (environment) => {
    environment.connection = { ...environment.connection, state: 'AwaitingRuntimeSelection', host: 'stale.example', port: 22 };
    environment.selectRuntimeShouldFail = true;
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 5,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [
          runtimeCandidate('stale-tmux', 'tmux', 'meeterm', 'running', { isDefault: true }),
        ] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [
          runtimeCandidate('other-herdr', 'herdr', 'default', 'running', { isDefault: true }),
        ] },
      ],
    };
  });
  await press(stale.root, findTestId(stale.root, 'runtime-row-tmux-stale-tmux'));
  await settleAsync();
  await poll(stale.environment);
  await settleAsync();
  assert.ok(stale.environment.calls.some(call => call.method === 'selectRuntime' && call.candidateId === 'stale-tmux'));
  const staleRow = findTestId(stale.root, 'runtime-row-tmux-stale-tmux');
  assert.equal(staleRow.props.accessibilityState.disabled, false);
  assert.match(textContent(staleRow), /could not be opened/);
  assert.ok(findTestId(stale.root, 'runtime-row-herdr-other-herdr'));
});

test('tmux creation is an explicit editable step and opens only after native confirmation', async t => {
  const fixture = await mountConfiguredForTest(t, (environment) => {
    environment.connection = { ...environment.connection, state: 'AwaitingRuntimeSelection', host: 'create.example', port: 22 };
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 6,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [
          runtimeCandidate('create-stopped-herdr', 'herdr', 'default', 'stopped', { isDefault: true }),
        ] },
      ],
    };
    environment.snapshot = makeSnapshot();
  });
  await press(fixture.root, findTestId(fixture.root, 'runtime-create-tmux'));
  const nameInput = findTestId(fixture.root, 'runtime-tmux-name');
  assert.equal(nameInput.props.value, 'meeterm');
  assert.equal(fixture.environment.calls.some(call => call.method === 'createTmuxSession'), false);
  await act(async () => { nameInput.props.onChangeText('scratch'); });
  await press(fixture.root, findTestId(fixture.root, 'runtime-tmux-create-submit'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'createTmuxSession'), [
    { method: 'createTmuxSession', name: 'scratch' },
  ]);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-tmux-create-submit').length, 0);
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
});

test('queued runtime selection waits for delayed Ready without writing a hint early', async t => {
  const { fixture, profile } = await mountSavedPicker(t, environment => {
    environment.selectRuntimeMode = 'delayed-ready';
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-queued-tmux'));
  await settleAsync();
  assert.ok(fixture.environment.pendingSelection);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux'));

  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  fixture.environment.resolvePendingSelection('ready');
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  assert.deepEqual(fixture.environment.lastUsedUpdates, [{
    profileId: profile.id, backend: 'tmux', runtime: 'meeterm',
  }]);
});

test('queued selection failure uses candidate-local error and preserves other rows', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.selectRuntimeMode = 'candidate-failure';
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-queued-tmux'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);

  fixture.environment.resolvePendingSelection('failure');
  await poll(fixture.environment);
  await settleAsync();
  const failedRow = findTestId(fixture.root, 'runtime-row-tmux-queued-tmux');
  assert.equal(failedRow.props.accessibilityState.disabled, true);
  assert.match(textContent(failedRow), /stopped before it could be opened/);
  assert.ok(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr'));
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);
});

test('cancel invalidates a queued selection and ignores a late Ready snapshot', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.selectRuntimeMode = 'delayed-ready';
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-row-tmux-queued-tmux'));
  await settleAsync();
  assert.ok(fixture.environment.pendingSelection);
  await press(fixture.root, findLabel(fixture.root, 'Cancel runtime selection'));
  await settleAsync();
  assert.ok(fixture.environment.nativeCalls.includes('disconnect'));
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-queued-tmux').length, 0);

  // The test double models an actor result arriving after disconnect. The
  // app must not turn that stale Ready into a newly bound workspace.
  fixture.environment.resolvePendingSelection('ready');
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'workspace-row-W1').length, 0);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
});

test('queued tmux creation waits for delayed success and never refreshes implicitly', async t => {
  const { fixture, profile } = await mountSavedPicker(t, environment => {
    environment.createTmuxSessionMode = 'delayed-ready';
    environment.runtimeDiscovery = pickerDiscovery(10, []);
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-create-tmux'));
  const nameInput = findTestId(fixture.root, 'runtime-tmux-name');
  await act(async () => { nameInput.props.onChangeText('scratch'); });
  await press(fixture.root, findTestId(fixture.root, 'runtime-tmux-create-submit'));
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'createTmuxSession'), [
    { method: 'createTmuxSession', name: 'scratch' },
  ]);
  assert.ok(fixture.environment.pendingCreation);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  fixture.environment.resolvePendingCreation('ready');
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'workspace-row-W1'));
  assert.deepEqual(fixture.environment.lastUsedUpdates, [{
    profileId: profile.id, backend: 'tmux', runtime: 'scratch',
  }]);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);
});

test('queued tmux creation reports delayed native failure without hiding Herdr', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.createTmuxSessionMode = 'delayed-failure';
    environment.runtimeDiscovery = pickerDiscovery(11, []);
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-create-tmux'));
  await press(fixture.root, findTestId(fixture.root, 'runtime-tmux-create-submit'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(fixture.environment.pendingCreation);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  fixture.environment.resolvePendingCreation('failure');
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(all(fixture.root, node => textContent(node).includes('tmux could not create this session')).length > 0);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  await press(fixture.root, findLabel(fixture.root, 'Back to runtime list'));
  assert.ok(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr'));
});

test('repeated tmux creation failure clears the old section error and preserves runtimes', async t => {
  const { fixture, profile } = await mountSavedPicker(t, environment => {
    environment.createTmuxSessionMode = 'delayed-failure';
    environment.runtimeDiscovery = pickerDiscovery(12);
  });

  assert.equal(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux').props.accessibilityState.disabled, false);
  assert.equal(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr').props.accessibilityState.disabled, false);
  await press(fixture.root, findTestId(fixture.root, 'runtime-create-tmux'));
  const firstName = findTestId(fixture.root, 'runtime-tmux-name');
  await act(async () => { firstName.props.onChangeText('first-attempt'); });
  await press(fixture.root, findTestId(fixture.root, 'runtime-tmux-create-submit'));
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'createTmuxSession'), [
    { method: 'createTmuxSession', name: 'first-attempt' },
  ]);
  assert.ok(fixture.environment.pendingCreation);
  fixture.environment.resolvePendingCreation('failure');
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux').errorCode, 'tmux_create_failed');
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(findTestId(fixture.root, 'runtime-tmux-create-submit').props.disabled, false);

  await press(fixture.root, findLabel(fixture.root, 'Back to runtime list'));
  const tmuxRow = findTestId(fixture.root, 'runtime-row-tmux-queued-tmux');
  assert.equal(tmuxRow.props.accessibilityState.disabled, false);
  assert.equal(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr').props.accessibilityState.disabled, false);

  await press(fixture.root, findTestId(fixture.root, 'runtime-create-tmux'));
  const secondName = findTestId(fixture.root, 'runtime-tmux-name');
  await act(async () => { secondName.props.onChangeText('second-attempt'); });
  await press(fixture.root, findTestId(fixture.root, 'runtime-tmux-create-submit'));
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'createTmuxSession'), [
    { method: 'createTmuxSession', name: 'first-attempt' },
    { method: 'createTmuxSession', name: 'second-attempt' },
  ]);
  assert.ok(fixture.environment.pendingCreation);
  assert.equal(fixture.environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux').errorCode, '');
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);

  fixture.environment.resolvePendingCreation('failure');
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.runtimeDiscovery.backends.find(item => item.backend === 'tmux').errorCode, 'tmux_create_failed');
  assert.ok(all(fixture.root, node => textContent(node).includes('tmux could not create this session')).length > 0);
  assert.equal(findTestId(fixture.root, 'runtime-tmux-create-submit').props.disabled, false);
  assert.equal(fixture.environment.lastUsedUpdates.length, 0);
  assert.equal(fixture.environment.refreshRuntimeCalls, 0);
  await press(fixture.root, findLabel(fixture.root, 'Back to runtime list'));
  assert.equal(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux').props.accessibilityState.disabled, false);
  assert.equal(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr').props.accessibilityState.disabled, false);
  assert.equal(fixture.environment.profiles.find(item => item.id === profile.id).runtime, 'meeterm');
});

test('initial runtime discovery replaces a loading snapshot with the final rows from the same revision', async t => {
  const finalDiscovery = pickerDiscovery(19);
  const fixture = await mountConfiguredForTest(t, environment => {
    environment.connection = { ...environment.connection, state: 'DiscoveringRuntimes', host: 'slow.example', port: 22 };
    environment.runtimeDiscovery = {
      connectionGeneration: finalDiscovery.connectionGeneration,
      revision: finalDiscovery.revision,
      backends: [
        runtimeBackend('tmux', [], { state: 'loading' }),
        runtimeBackend('herdr', [], { state: 'loading' }),
      ],
    };
  });

  assert.equal(all(fixture.root, node => node.props?.testID === 'runtime-row-tmux-queued-tmux').length, 0);
  fixture.environment.runtimeDiscovery = finalDiscovery;
  fixture.environment.connection.state = 'AwaitingRuntimeSelection';
  await poll(fixture.environment);
  await settleAsync();

  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux'));
  assert.ok(findTestId(fixture.root, 'runtime-row-herdr-queued-herdr'));
  assert.ok(fixture.environment.nativeCalls.filter(call => call === 'getRuntimeDiscovery').length >= 2);
});

test('queued runtime refresh waits for a newer revision without repeating the command', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.refreshRuntimeMode = 'delayed';
    environment.runtimeDiscovery = pickerDiscovery(20);
  });
  const oldRevision = fixture.environment.runtimeDiscovery.revision;

  await press(fixture.root, findTestId(fixture.root, 'runtime-refresh'));
  await settleAsync();
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.ok(fixture.environment.pendingRefresh);
  assert.equal(findTestId(fixture.root, 'runtime-refresh').props.disabled, true);

  await poll(fixture.environment);
  await settleAsync();
  assert.ok(fixture.environment.runtimeDiscovery.revision > oldRevision);
  assert.ok(fixture.environment.runtimeDiscovery.backends.every(item => item.state === 'loading'));
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(findTestId(fixture.root, 'runtime-refresh').props.disabled, true);

  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(findTestId(fixture.root, 'runtime-refresh').props.disabled, true);

  fixture.environment.resolvePendingRefresh();
  const finalRevision = fixture.environment.runtimeDiscovery.revision;
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.runtimeDiscovery.revision, finalRevision);
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(fixture.environment.pendingRefresh, null);
  assert.equal(findTestId(fixture.root, 'runtime-refresh').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux').props.accessibilityState.disabled, false);
});

test('cancel invalidates a queued runtime refresh and ignores a late revision', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.refreshRuntimeMode = 'delayed';
    environment.runtimeDiscovery = pickerDiscovery(21);
  });
  const oldRevision = fixture.environment.runtimeDiscovery.revision;

  await press(fixture.root, findTestId(fixture.root, 'runtime-refresh'));
  await settleAsync();
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.ok(fixture.environment.pendingRefresh);
  await press(fixture.root, findLabel(fixture.root, 'Cancel runtime selection'));
  await settleAsync();
  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-queued-tmux').length, 0);

  fixture.environment.resolvePendingRefresh();
  assert.ok(fixture.environment.runtimeDiscovery.revision > oldRevision);
  await poll(fixture.environment);
  await settleAsync();
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-queued-tmux').length, 0);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-refresh').length, 0);
});

test('refresh enqueue failure releases busy and keeps the existing actionable error', async t => {
  const { fixture } = await mountSavedPicker(t, environment => {
    environment.refreshRuntimeShouldFail = true;
  });

  await press(fixture.root, findTestId(fixture.root, 'runtime-refresh'));
  await settleAsync();
  assert.equal(fixture.environment.refreshRuntimeCalls, 1);
  assert.equal(fixture.environment.pendingRefresh, null);
  assert.equal(findTestId(fixture.root, 'runtime-refresh').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'runtime-row-tmux-queued-tmux').props.accessibilityState.disabled, false);
  assert.ok(all(fixture.root, node => textContent(node).includes('Could not refresh runtimes. Check the connection and try again.')).length > 0);
});

test('verified reconnect skips the picker while an explicit fresh-selection state opens it', async t => {
  const fixture = await mountConfiguredForTest(t, (environment) => {
    environment.snapshot = makeSnapshot();
    environment.runtimeDiscovery = {
      connectionGeneration: '1',
      revision: 8,
      backends: [
        { backend: 'tmux', state: 'ready', errorCode: '', errorMessage: '', canCreate: true, candidates: [
          runtimeCandidate('reconnect-tmux', 'tmux', 'meeterm', 'running', { isDefault: true, lastUsed: true }),
        ] },
        { backend: 'herdr', state: 'ready', errorCode: '', errorMessage: '', canCreate: false, candidates: [] },
      ],
    };
  });
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-reconnect-tmux').length, 0);

  fixture.environment.connection.state = 'Reconnecting';
  await poll(fixture.environment);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-reconnect-tmux').length, 0);

  fixture.environment.connection.state = 'Ready';
  await poll(fixture.environment);
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'runtime-row-tmux-reconnect-tmux').length, 0);

  fixture.environment.connection.state = 'AwaitingRuntimeSelection';
  await poll(fixture.environment);
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'runtime-row-tmux-reconnect-tmux'));
  assert.ok(all(fixture.root, node => textContent(node).includes('previous runtime needs to be selected again')).length > 0);
});

test('Workspaces Reconnect retries retained work with its current epoch and keeps the cached terminal', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));

  const control = workspaceControl({
    operationEpoch: '181',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  });
  fixture.environment.connection.state = 'Failed';
  await updateSnapshot(fixture.environment, makeSnapshot({ control }));
  await settleAsync();
  fixture.environment.recoveryRetryMode = 'pending';
  const reconnect = findTestId(fixture.root, 'workspaces-reconnect');
  assert.ok(reconnect);
  await press(fixture.root, reconnect);
  await settleAsync();
  assert.equal(findTestId(fixture.root, 'workspaces-reconnect').props.disabled, true);
  await press(fixture.root, findTestId(fixture.root, 'workspaces-reconnect'));

  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
    { method: 'retryRecovery', operationEpoch: '181' },
  ]);
  assert.equal(fixture.environment.nativeCalls.includes('reconnect'), false);
  assert.equal(fixture.environment.calls.some(call => call.method === 'selectRuntime'), false);
  assert.equal(all(fixture.root, node => typeof node.props?.testID === 'string' && node.props.testID.startsWith('runtime-row-')).length, 0);
  assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Choose a runtime for fixture.example').length, 0);

  fixture.environment.resolvePendingRecoveryRetry();
  fixture.environment.connection.state = 'Reconnecting';
  fixture.environment.snapshot = makeSnapshot({
    control: workspaceControl({
      operationEpoch: '182',
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase: 'reconnecting', reason: 'manual_retry', attempt: 0, maxAttempts: 6 },
    }),
  });
  await poll(fixture.environment);
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'workspace-row-W1'));
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root)[0].props.interactionMode, 'cachedReadOnly');
});

test('Workspaces and server-sheet Reconnect retry reconnecting or retry-eligible retained work', async t => {
  const cases = [
    { location: 'workspaces', phase: 'reconnecting', reason: 'manual_retry', epoch: '201', state: 'Reconnecting' },
    { location: 'server', phase: 'reconnecting', reason: 'manual_retry', epoch: '202', state: 'Reconnecting' },
    { location: 'server', phase: 'stopped', reason: 'retry_exhausted', epoch: '203', state: 'Failed' },
  ];
  for (const item of cases) {
    const fixture = await mountWorkspaceRecovery(t, {
      phase: item.phase,
      reason: item.reason,
      operationEpoch: item.epoch,
      connectionState: item.state,
    });
    fixture.environment.recoveryRetryMode = 'pending';
    assert.ok(findTestId(fixture.root, 'recovery-rail'));

    if (item.location === 'server') {
      await press(fixture.root, findLabel(fixture.root, 'Server connection'));
    }
    const reconnectId = item.location === 'server' ? 'server-reconnect' : 'workspaces-reconnect';
    assert.ok(findTestId(fixture.root, reconnectId), `${item.location} Reconnect should be visible`);
    await press(fixture.root, findTestId(fixture.root, reconnectId));
    await settleAsync();
    assert.equal(findTestId(fixture.root, reconnectId).props.disabled, true);
    await press(fixture.root, findTestId(fixture.root, reconnectId));
    await settleAsync();

    assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
      { method: 'retryRecovery', operationEpoch: item.epoch },
    ]);
    assert.equal(fixture.environment.nativeCalls.includes('reconnect'), false);
    assert.equal(fixture.environment.calls.some(call => call.method === 'selectRuntime'), false);
    assert.equal(all(fixture.root, node => typeof node.props?.testID === 'string' && node.props.testID.startsWith('runtime-row-')).length, 0);
    assert.equal(all(fixture.root, node => node.type === 'Text' && textContent(node) === 'Choose a runtime for fixture.example').length, 0);
    fixture.environment.resolvePendingRecoveryRetry();
  }
});

test('Workspaces and server-sheet Reconnect tolerate a same-intent no-op retry through Ready', async t => {
  for (const location of ['workspaces', 'server']) {
    const initialEpoch = location === 'workspaces' ? '231' : '241';
    const reconnectingEpoch = String(Number(initialEpoch) + 1);
    const fixture = await mountWorkspaceRecovery(t, {
      phase: 'stopped',
      reason: 'retry_exhausted',
      operationEpoch: initialEpoch,
      connectionState: 'Failed',
    });
    if (location === 'server') {
      await press(fixture.root, findLabel(fixture.root, 'Server connection'));
    }

    const reconnectId = location === 'server' ? 'server-reconnect' : 'workspaces-reconnect';
    fixture.environment.recoveryRetryAttempt = 1;
    await press(fixture.root, findTestId(fixture.root, reconnectId));
    await settleAsync();
    assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
      { method: 'retryRecovery', operationEpoch: initialEpoch },
    ]);
    assert.equal(fixture.environment.connection.state, 'Reconnecting');
    assert.equal(fixture.environment.snapshot.control.operationEpoch, reconnectingEpoch);
    assert.equal(fixture.environment.snapshot.control.recovery.phase, 'reconnecting');
    assert.equal(fixture.environment.snapshot.control.recovery.attempt, 1);

    // Observe the accepted retry snapshot before the next real button press.
    await poll(fixture.environment);
    await settleAsync();
    assert.equal(findTestId(fixture.root, reconnectId).props.disabled, false,
      'the accepted epoch/phase/attempt change releases the previous pending guard');
    fixture.environment.recoveryRetryMode = 'noop';
    await press(fixture.root, findTestId(fixture.root, reconnectId));
    await settleAsync();
    assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
      { method: 'retryRecovery', operationEpoch: initialEpoch },
      { method: 'retryRecovery', operationEpoch: reconnectingEpoch },
    ]);
    assert.equal(fixture.environment.snapshot.control.operationEpoch, reconnectingEpoch,
      'a same-epoch no-op leaves native recovery state unchanged');
    assert.equal(fixture.environment.snapshot.control.recovery.phase, 'reconnecting');
    await poll(fixture.environment);
    await settleAsync();
    assert.equal(findTestId(fixture.root, reconnectId).props.disabled, true,
      'the no-op remains pending until a later authoritative snapshot');

    fixture.environment.connection.state = 'Ready';
    await updateSnapshot(fixture.environment, makeSnapshot({
      control: workspaceControl({
        operationEpoch: reconnectingEpoch,
        runtimeOperationsReady: true,
        terminalInputReady: true,
        recovery: { phase: 'none', reason: '', attempt: 0, maxAttempts: 6 },
      }),
    }));
    await settleAsync();
    assert.equal(all(fixture.root, node => node.type === 'Text'
      && textContent(node).includes('Recovery could not be started')).length, 0,
    'a native Ok no-op must not show a retry failure notice');
    assert.equal(all(fixture.root, node => typeof node.props?.testID === 'string'
      && node.props.testID.startsWith('runtime-row-')).length, 0,
    'retained recovery must not open the runtime picker');
    assert.equal(all(fixture.root, node => node.type === 'Text'
      && textContent(node) === 'Choose a runtime for fixture.example').length, 0);
    assert.equal(fixture.environment.nativeCalls.includes('reconnect'), false);
    assert.equal(fixture.environment.calls.some(call => call.method === 'selectRuntime'), false);

    // A later retry-eligible snapshot makes a stuck pending/disabled action
    // observable even though the reconnect button is correctly hidden at Ready.
    const postReadyEpoch = String(Number(reconnectingEpoch) + 1);
    fixture.environment.connection.state = 'Failed';
    await updateSnapshot(fixture.environment, makeSnapshot({
      control: workspaceControl({
        operationEpoch: postReadyEpoch,
        runtimeOperationsReady: false,
        terminalInputReady: false,
        recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
      }),
    }));
    await settleAsync();
    assert.equal(findTestId(fixture.root, reconnectId).props.disabled, false,
      'Ready must release the no-op request pending guard');
    assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
      { method: 'retryRecovery', operationEpoch: initialEpoch },
      { method: 'retryRecovery', operationEpoch: reconnectingEpoch },
    ], 'the no-op path must not create extra retries');
  }
});

test('a rejected Workspaces Reconnect shows its failure notice and becomes usable again', async t => {
  const fixture = await mountWorkspaceRecovery(t, {
    phase: 'stopped',
    reason: 'retry_exhausted',
    operationEpoch: '251',
    connectionState: 'Failed',
  });
  fixture.environment.recoveryRetryShouldFail = true;

  await press(fixture.root, findTestId(fixture.root, 'workspaces-reconnect'));
  await settleAsync();

  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
    { method: 'retryRecovery', operationEpoch: '251' },
  ]);
  assert.ok(all(fixture.root, node => node.type === 'Text'
    && textContent(node).includes('Recovery could not be started. Try again or change the destination.')).length > 0,
  'a genuine native rejection should be surfaced');
  assert.equal(findTestId(fixture.root, 'workspaces-reconnect').props.disabled, false,
    'a rejected native call clears pending so the retry button can be used again');
  assert.equal(fixture.environment.nativeCalls.includes('reconnect'), false);
  assert.equal(fixture.environment.calls.some(call => call.method === 'selectRuntime'), false);
});

test('retained Reconnect is hidden while resynchronizing or stopped without rail Retry', async t => {
  const cases = [
    { phase: 'resynchronizing', reason: 'screen_resync', state: 'Reconnecting' },
    { phase: 'stopped', reason: 'runtime_missing', state: 'Failed' },
    { phase: 'stopped', reason: 'terminal_missing', state: 'Failed' },
    { phase: 'stopped', reason: 'incompatible', state: 'Failed' },
    { phase: 'stopped', reason: 'authentication_failed', state: 'Failed' },
    { phase: 'stopped', reason: 'host_key_changed', state: 'Failed', errorCode: 'host_key_changed' },
  ];
  for (const item of cases) {
    const fixture = await mountWorkspaceRecovery(t, {
      phase: item.phase,
      reason: item.reason,
      operationEpoch: '220',
      connectionState: item.state,
      errorCode: item.errorCode,
    });
    assert.ok(findTestId(fixture.root, 'recovery-rail'));
    assert.equal(all(fixture.root, node => node.props?.testID === 'workspaces-reconnect').length, 0, item.reason);
    await press(fixture.root, findLabel(fixture.root, 'Server connection'));
    assert.equal(all(fixture.root, node => node.props?.testID === 'server-reconnect').length, 0, item.reason);
    assert.equal(fixture.environment.calls.some(call => call.method === 'retryRecovery'), false);
    assert.equal(fixture.environment.nativeCalls.includes('reconnect'), false);
  }
});

test('Workspaces Reconnect without retained work starts the fresh picker path', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));

  fixture.environment.connection.state = 'Failed';
  await updateSnapshot(fixture.environment, makeSnapshot({
    control: workspaceControl({
      operationEpoch: '191',
      hasRetainedWork: false,
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase: 'none', reason: '', attempt: 0, maxAttempts: 6 },
    }),
  }));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'workspaces-reconnect'));
  await settleAsync();

  assert.deepEqual(fixture.environment.nativeCalls.filter(call => call === 'reconnect'), ['reconnect']);
  assert.equal(fixture.environment.calls.some(call => call.method === 'retryRecovery'), false);
  assert.ok(findText(fixture.root, 'Choose a runtime for fixture.example'));
});

test('retained recovery keeps the cached native terminal and disables remote navigation', async t => {
  const control = workspaceControl({
    operationEpoch: '41',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'resynchronizing', reason: 'runtime_validation', attempt: 2, maxAttempts: 6 },
  });
  const fixture = await mountRecovering(t, control);
  await updateSnapshot(fixture.environment, makeSnapshot({
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G2', 'W1', 'Group Two', false),
      group('G3', 'W2', 'Group Three', true),
    ],
    terminals: [
      pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1'),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
      pane('P3', 'W2', 'G3', 'native:P3', false, true, 'P3'),
    ],
    control,
  }));

  assert.equal(findTestId(fixture.root, 'recovery-title').children[0], 'Verifying this workspace…');
  assert.equal(findTestId(fixture.root, 'recovery-meta').children[0], 'Last received output · Input paused');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root)[0].props.interactionMode, 'cachedReadOnly');
  assert.equal(all(fixture.root, node => typeof node.props?.testID === 'string' && node.props.testID.startsWith('runtime-row-')).length, 0);
  assert.equal(findLabel(fixture.root, 'Switch workspace').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Switch terminal group').props.disabled, true);
  assert.equal(findTestId(fixture.root, 'terminal-tab-P2').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Create terminal').props.disabled, true);
  assert.equal(fixture.environment.visibility.at(-1), true, 'cached surface visibility is independent; native recovery gates keep it input-inert');

  fixture.environment.connection.state = 'AwaitingRuntimeSelection';
  await poll(fixture.environment);
  assert.equal(all(fixture.root, node => typeof node.props?.testID === 'string' && node.props.testID.startsWith('runtime-row-')).length, 0);
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);

  const callsBeforeTabPress = fixture.environment.calls.length;
  await press(fixture.root, findTestId(fixture.root, 'terminal-tab-P2'));
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeTabPress), []);

  await press(fixture.root, findLabel(fixture.root, 'Terminal menu'));
  assert.equal(findLabel(fixture.root, 'Refresh terminal').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Rename terminal').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Close terminal').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Workspace options Workspace One').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Create group').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Rename group').props.disabled, true);
  assert.equal(findLabel(fixture.root, 'Close group').props.disabled, true);
  await press(fixture.root, findLabel(fixture.root, 'Close sheet'));
});

test('explicit disconnect keeps the layout-restore warning and does not restore Ready', async t => {
  const warning = 'The desktop layout could not be confirmed after disconnect.';
  const fixture = await mountForTest(t, makeSnapshot(), environment => {
    environment.disconnectRelease = {
      errorCode: 'layout_restore_unconfirmed',
      errorMessage: warning,
    };
  });
  await openWorkspace(fixture.root, 'W1');

  await press(fixture.root, findLabel(fixture.root, 'Terminal menu'));
  await press(fixture.root, findLabel(fixture.root, 'PC handoff help'));
  await press(fixture.root, findLabel(fixture.root, 'Disconnect'));
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(terminalViews(fixture.root).length, 0, 'disconnect must not leave a stale Ready terminal mounted');
  assert.ok(findLabel(fixture.root, 'Dismiss desktop layout warning'));
  assert.ok(all(fixture.root, node => textContent(node).includes(warning)).length > 0);
});

test('polling a disconnected connection keeps the layout-restore warning visible', async t => {
  const warning = 'The desktop layout could not be confirmed during polling.';
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');

  fixture.environment.connection = {
    ...fixture.environment.connection,
    state: 'Disconnected',
    errorCode: 'layout_restore_unconfirmed',
    errorMessage: warning,
  };
  await poll(fixture.environment);
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'Disconnected');
  assert.equal(terminalViews(fixture.root).length, 1, 'polling may retain the cached surface while disconnected');
  assert.equal(terminalViews(fixture.root)[0].props.interactionMode, 'cachedReadOnly');
  assert.ok(findLabel(fixture.root, 'Dismiss desktop layout warning'));
  assert.ok(all(fixture.root, node => textContent(node).includes(warning)).length > 0);
  await press(fixture.root, findLabel(fixture.root, 'Dismiss desktop layout warning'));
  await poll(fixture.environment);
  assert.equal(all(fixture.root, node => node.props?.testID === 'cleanup-warning').length, 0);
});

test('recovery Change keeps the layout-restore warning after the release read', async t => {
  const warning = 'The previous desktop layout could not be confirmed before switching runtime.';
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '115',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }), 'Failed', environment => {
    environment.changeRuntimeRelease = {
      errorCode: 'layout_restore_unconfirmed',
      errorMessage: warning,
    };
  });

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  await press(fixture.root, findTestId(fixture.root, 'recovery-change-runtime'));
  await settleAsync();

  assert.equal(fixture.environment.connection.state, 'AwaitingRuntimeSelection');
  assert.equal(terminalViews(fixture.root).length, 0, 'runtime switch must not restore the retired Ready terminal');
  assert.ok(findTestId(fixture.root, 'runtime-refresh'), 'the switch result must remain visible in the runtime picker');
  assert.ok(findLabel(fixture.root, 'Dismiss desktop layout warning'));
  assert.ok(all(fixture.root, node => textContent(node).includes(warning)).length > 0);
});

test('cleanup warning is independent from auth errors, dismissal, command feedback, and new results', async t => {
  const firstWarning = {
    id: '501',
    code: 'layout_restore_unconfirmed',
    message: 'The old connection desktop layout was not confirmed.',
  };
  const fixture = await mountForTest(t, makeSnapshot({
    control: workspaceControl({ cleanupWarning: firstWarning }),
  }), environment => {
    environment.connection = {
      ...environment.connection,
      state: 'Failed',
      errorCode: 'auth_failed',
      errorMessage: 'SSH authentication failed.',
    };
  });
  await poll(fixture.environment);

  assert.ok(findText(fixture.root, 'Authentication failed. Check your username and the password or private key for your chosen sign-in method.'));
  assert.ok(findTestId(fixture.root, 'cleanup-warning'));
  await press(fixture.root, findLabel(fixture.root, 'Dismiss desktop layout warning'));
  assert.equal(all(fixture.root, node => node.props?.testID === 'cleanup-warning').length, 0);
  assert.ok(findText(fixture.root, 'Authentication failed. Check your username and the password or private key for your chosen sign-in method.'));

  // A slow/repeated read of the same native result must not re-latch the
  // locally dismissed warning through either the canonical or legacy path.
  await poll(fixture.environment);
  assert.equal(all(fixture.root, node => node.props?.testID === 'cleanup-warning').length, 0);

  fixture.environment.snapshot = makeSnapshot({
    control: workspaceControl({
      cleanupWarning: { ...firstWarning, id: '502', message: 'A newer old-connection layout result needs review.' },
    }),
  });
  await poll(fixture.environment);
  assert.ok(findTestId(fixture.root, 'cleanup-warning'));
  assert.ok(findText(fixture.root, 'A newer old-connection layout result needs review.'));

  // The warning is not stored in the generic command-feedback slot. A
  // failing terminal command may publish feedback while the warning remains
  // dismissible independently.
  const live = await mountForTest(t, makeSnapshot({
    control: workspaceControl({
      cleanupWarning: { id: '503', code: 'layout_restore_unconfirmed', message: 'Desktop layout restore needs review.' },
    }),
  }));
  await poll(live.environment);
  await openWorkspace(live.root, 'W1');
  await press(live.root, findLabel(live.root, 'Terminal menu'));
  await press(live.root, findLabel(live.root, 'Refresh terminal'));
  assert.ok(findLabel(live.root, 'Dismiss message'));
  assert.ok(findLabel(live.root, 'Dismiss desktop layout warning'));
});

test('recovery Retry uses the current epoch once and waits for a native snapshot transition', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '51',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }), 'Failed');
  fixture.environment.recoveryRetryMode = 'pending';

  assert.ok(findTestId(fixture.root, 'recovery-change'));
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-review').length, 0);
  await press(fixture.root, findTestId(fixture.root, 'recovery-retry'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'recovery-retry'));
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'retryRecovery'), [
    { method: 'retryRecovery', operationEpoch: '51' },
  ]);
  assert.equal(findTestId(fixture.root, 'recovery-retry').props.disabled, true);

  fixture.environment.resolvePendingRecoveryRetry();
  fixture.environment.connection.state = 'Reconnecting';
  await updateSnapshot(fixture.environment, makeSnapshot({
    control: workspaceControl({
      operationEpoch: '52',
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase: 'reconnecting', reason: 'manual_retry', attempt: 0, maxAttempts: 6 },
    }),
  }));
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-retry').length, 0);
  assert.equal(findTestId(fixture.root, 'recovery-title').children[0], 'Reconnecting…');
});

test('stopped authentication and host-key recovery keep their existing actions', async t => {
  const authentication = await mountRecovering(t, workspaceControl({
    operationEpoch: '66',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'authentication_failed', attempt: 1, maxAttempts: 6 },
  }), 'Failed');
  assert.ok(findTestId(authentication.root, 'recovery-connection-details'));
  assert.equal(all(authentication.root, node => node.props?.testID === 'recovery-retry').length, 0);

  const hostKey = await mountRecovering(t, workspaceControl({
    operationEpoch: '67',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'host_key_changed', attempt: 1, maxAttempts: 6 },
  }), 'Failed');
  assert.equal(findTestId(hostKey.root, 'recovery-review').props.accessibilityLabel, 'Review key change');
});

test('recovery confirmation state and Expo API are absent', () => {
  const retiredMethod = ['confirm', 'Recovery'].join('');
  const retiredPhase = ['awaiting', 'Confirmation'].join('');
  const retiredToken = ['confirmation', 'Token'].join('');
  const sources = [
    'App.tsx',
    'modules/meeterm-terminal/src/MeetermTerminal.types.ts',
    'modules/meeterm-terminal/src/MeetermTerminalModule.ts',
    'modules/meeterm-terminal/src/MeetermTerminalModule.web.ts',
    'modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/MeetermTerminalModule.kt',
    'modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/MeetermNative.kt',
    'modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/InputSession.kt',
    'modules/meeterm-terminal/ios/MeetermTerminalModule.swift',
    'modules/meeterm-terminal/ios/MeetermCore.swift',
    'modules/meeterm-terminal/ios/TerminalInputView.swift',
  ];
  for (const source of sources) {
    const contents = fs.readFileSync(path.join(REPO_ROOT, source), 'utf8');
    assert.equal(contents.includes(retiredMethod), false, `${source} must not expose the retired bridge method`);
    assert.equal(contents.includes(retiredPhase), false, `${source} must not retain the retired phase`);
    assert.equal(contents.includes(retiredToken), false, `${source} must not retain confirmation data`);
  }
});

test('runtime identity mismatch keeps the last native terminal instead of binding a replacement', async t => {
  const control = workspaceControl({
    operationEpoch: '71',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'runtime_identity_mismatch', attempt: 1, maxAttempts: 6 },
  });
  const fixture = await mountRecovering(t, control, 'Failed');
  const callsBefore = fixture.environment.calls.length;

  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: 'P2',
    control,
  }));
  assert.equal(findTestId(fixture.root, 'recovery-title').children[0], 'This runtime can’t be restored');
  assert.match(findTestId(fixture.root, 'recovery-detail').children[0], /not the same instance as before/);
  assert.ok(findTestId(fixture.root, 'recovery-retry'));
  assert.ok(findTestId(fixture.root, 'recovery-change'));
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-review').length, 0);
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root).some(view => view.props.terminalId === 'native:P2'), false);
  await press(fixture.root, findTestId(fixture.root, 'terminal-tab-P2'));
  assert.deepEqual(fixture.environment.calls.slice(callsBefore).filter(call => call.method === 'selectPane'), []);
});

test('Change sheet preserves recovery on dismiss and invalidates it only on destination selection', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '81',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }), 'Failed');

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  assert.equal(findTestId(fixture.root, 'recovery-change-intro').children[0], 'Choosing another destination stops recovery for this workspace. Remote work will not be closed.');
  await press(fixture.root, findLabel(fixture.root, 'Cancel'));
  assert.ok(findTestId(fixture.root, 'recovery-rail'));

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  await press(fixture.root, findTestId(fixture.root, 'recovery-change-runtime'));
  await settleAsync();
  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'changeRuntime'), [
    { method: 'changeRuntime', operationEpoch: '81' },
  ]);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-rail').length, 0);
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-change').length, 0);
});

test('recovery Change keeps the cached terminal pending native acceptance', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '101',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }), 'Failed');
  fixture.environment.changeRuntimeMode = 'pending';

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  const destination = findTestId(fixture.root, 'recovery-change-runtime');
  await act(async () => { destination.props.onPress(); });
  await settleAsync();

  assert.ok(fixture.environment.pendingChangeRuntime, 'native acceptance should still be pending');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.ok(findLabel(fixture.root, 'Back to workspaces'));
  assert.equal(findTestId(fixture.root, 'recovery-change').props.disabled, true);
  assert.equal(destination.props.accessibilityState.disabled, true);
  assert.equal(all(fixture.root, node => node.props?.testID === 'runtime-refresh').length, 0);

  fixture.environment.resolvePendingChangeRuntime('accept');
  await settleAsync();
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-rail').length, 0);
  assert.ok(findTestId(fixture.root, 'runtime-refresh'), 'the runtime picker should bind only after native acceptance');
});

test('stale recovery Change rejection retains the latest cached snapshot and clears busy', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '111',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }));
  fixture.environment.changeRuntimeMode = 'pending';

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  const destination = findTestId(fixture.root, 'recovery-change-runtime');
  await act(async () => { destination.props.onPress(); });
  await settleAsync();

  const latestControl = workspaceControl({
    operationEpoch: '112',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  });
  await updateSnapshot(fixture.environment, makeSnapshot({
    control: latestControl,
    terminals: [
      pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1 latest', { name: 'Latest agent', status: 'working' }),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
      pane('P3', 'W2', 'G2', 'native:P3', false, true, 'P3'),
    ],
  }));
  assert.equal(findTestId(fixture.root, 'selected-agent-line').props.accessibilityLabel, 'Latest agent, Agent status unavailable');

  fixture.environment.resolvePendingChangeRuntime('reject');
  await settleAsync();

  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(workspaceTitle(fixture.root), 'Workspace One');
  assert.ok(findLabel(fixture.root, 'Back to workspaces'));
  assert.ok(findTestId(fixture.root, 'recovery-rail'));
  assert.equal(findTestId(fixture.root, 'recovery-change').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'recovery-change-runtime').props.accessibilityState.disabled, false);
  assert.equal(findTestId(fixture.root, 'selected-agent-line').props.accessibilityLabel, 'Latest agent, Agent status unavailable');
  assert.equal(fixture.environment.snapshot.control.operationEpoch, '112');
  assert.ok(all(fixture.root, node => textContent(node).includes('Could not start the destination change. Check the connection and try again.')).length > 0);
});

test('rapid repeated recovery Change taps enqueue only one native request', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '121',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }), 'Failed');
  fixture.environment.changeRuntimeMode = 'pending';

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  const destination = findTestId(fixture.root, 'recovery-change-runtime');
  await act(async () => {
    destination.props.onPress();
    destination.props.onPress();
  });
  await settleAsync();

  assert.deepEqual(fixture.environment.calls.filter(call => call.method === 'changeRuntime'), [
    { method: 'changeRuntime', operationEpoch: '121' },
  ]);
  assert.ok(fixture.environment.pendingChangeRuntime);
  assert.equal(findTestId(fixture.root, 'recovery-change').props.disabled, true);
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);

  fixture.environment.resolvePendingChangeRuntime('reject');
  await settleAsync();
  assert.equal(findTestId(fixture.root, 'recovery-change').props.disabled, false);
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
});

test('accepted recovery Change ignores a late old Ready snapshot', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '131',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  }));
  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));

  const oldSnapshot = clone(fixture.environment.snapshot);
  let releaseSnapshot;
  const snapshotGate = new Promise(resolve => { releaseSnapshot = resolve; });
  const originalGetWorkspaceState = fixture.native.getWorkspaceState;
  let holdingOldSnapshot = true;
  fixture.native.getWorkspaceState = async (...args) => {
    if (holdingOldSnapshot) {
      holdingOldSnapshot = false;
      await snapshotGate;
      return oldSnapshot;
    }
    return originalGetWorkspaceState(...args);
  };
  fixture.environment.connection.state = 'Ready';
  // Start the interval callback directly so the intentionally held native
  // snapshot does not keep the test helper's act() scope open.
  fixture.environment.intervalCallbacks[0]();
  await settleAsync();
  assert.equal(holdingOldSnapshot, false, 'the old Ready poll should be paused at workspace snapshot read');

  fixture.environment.changeRuntimeMode = 'pending';
  const destination = findTestId(fixture.root, 'recovery-change-runtime');
  await act(async () => { destination.props.onPress(); });
  await settleAsync();
  fixture.environment.resolvePendingChangeRuntime('accept');
  await settleAsync();
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-rail').length, 0);
  assert.ok(findTestId(fixture.root, 'runtime-refresh'));

  releaseSnapshot();
  await settleAsync();
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-rail').length, 0);
  assert.ok(findTestId(fixture.root, 'runtime-refresh'));
});

test('Herdr recovery progress reaches Ready without a confirmation dialog', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '91',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'resynchronizing', reason: 'screen_resync', attempt: 2, maxAttempts: 6 },
  }));
  const recoveringTerminals = [
    pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1', { name: 'Claude Code', status: 'working' }),
    pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
    pane('P3', 'W2', 'G2', 'native:P3', false, true, 'P3'),
  ];
  await updateSnapshot(fixture.environment, makeSnapshot({
    terminals: recoveringTerminals,
    control: workspaceControl({
      operationEpoch: '91',
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase: 'resynchronizing', reason: 'screen_resync', attempt: 2, maxAttempts: 6 },
    }),
  }));

  fixture.environment.connection.state = 'Ready';
  await updateSnapshot(fixture.environment, makeSnapshot({
    terminals: recoveringTerminals,
    control: workspaceControl({ operationEpoch: '92' }),
  }));
  await settleAsync();
  assert.equal(fixture.environment.alert, null);
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-review').length, 0);
  assert.equal(findTestId(fixture.root, 'recovery-title').children[0], 'Back online');
  assert.equal(findTestId(fixture.root, 'recovery-detail').children[0], 'Terminal is live · Input available');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root)[0].props.interactionMode, 'live');
  assert.equal(terminalViews(fixture.root)[0].props.autoFocus, undefined);
  assert.equal(findTestId(fixture.root, 'selected-agent-line').props.accessibilityLabel, 'Claude Code, Agent status: working');
});

test('retained recovery rail stays reachable from Workspaces and reopens only the cached terminal', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));

  const control = workspaceControl({
    operationEpoch: '141',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'retry_exhausted', attempt: 6, maxAttempts: 6 },
  });
  fixture.environment.connection.state = 'Failed';
  await updateSnapshot(fixture.environment, makeSnapshot({ control }));

  assert.ok(findTestId(fixture.root, 'recovery-rail'));
  assert.equal(findTestId(fixture.root, 'recovery-retry').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'recovery-change').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'workspace-row-W1').props.disabled, false);
  assert.equal(findTestId(fixture.root, 'workspace-row-W2').props.disabled, true);

  const callsBeforeOtherWorkspace = fixture.environment.calls.length;
  await press(fixture.root, findTestId(fixture.root, 'workspace-row-W2'));
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeOtherWorkspace), []);
  assert.equal(all(fixture.root, node => node.props?.accessibilityLabel === 'Back to workspaces').length, 0);

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  assert.ok(findTestId(fixture.root, 'recovery-change-intro'));
  await press(fixture.root, findLabel(fixture.root, 'Cancel'));
  assert.ok(findTestId(fixture.root, 'recovery-rail'));

  const callsBeforeRetainedWorkspace = fixture.environment.calls.length;
  await press(fixture.root, findTestId(fixture.root, 'workspace-row-W1'));
  await settleAsync();
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root)[0].props.interactionMode, 'cachedReadOnly');
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeRetainedWorkspace).filter(call => call.method === 'selectPane'), []);
});

test('healthy visibility and foreground cycles never announce Back online', async t => {
  const fixture = await mountForTest(t, makeSnapshot(), environment => { environment.fakeTimers = true; });
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await act(async () => { fixture.environment.emitAppState('background'); fixture.environment.emitAppState('active'); });
  await press(fixture.root, findLabel(fixture.root, 'Back to workspaces'));
  await press(fixture.root, findLabel(fixture.root, 'Switch server or session'));
  await press(fixture.root, findLabel(fixture.root, 'Cancel server or session switch'));
  await act(async () => { fixture.environment.emitAppState('inactive'); fixture.environment.emitAppState('active'); });

  const gatesClosed = workspaceControl({
    operationEpoch: '151',
    runtimeOperationsReady: true,
    terminalInputReady: false,
    recovery: { phase: 'none', reason: '', attempt: 0, maxAttempts: 6 },
  });
  await updateSnapshot(fixture.environment, makeSnapshot({ control: gatesClosed }));
  await updateSnapshot(fixture.environment, makeSnapshot({ control: workspaceControl({ operationEpoch: '151' }) }));
  await settleAsync();

  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-title' && textContent(node) === 'Back online').length, 0);
  assert.equal(fixture.environment.accessibilityAnnouncements.filter(message => message.startsWith('Back online.')).length, 0);
});

test('a real recovery announces Back online once, expires after two seconds, and ignores an old epoch', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '161',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'resynchronizing', reason: 'screen_resync', attempt: 2, maxAttempts: 6 },
  }), 'Reconnecting', environment => { environment.fakeTimers = true; });

  fixture.environment.connection.state = 'Ready';
  await updateSnapshot(fixture.environment, makeSnapshot({ control: workspaceControl({ operationEpoch: '162' }) }));
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-title' && textContent(node) === 'Back online').length, 1);
  assert.equal(fixture.environment.accessibilityAnnouncements.filter(message => message.startsWith('Back online.')).length, 1);
  assert.deepEqual(fixture.environment.timeoutCallbacks.filter(timer => !timer.canceled).map(timer => timer.delay).sort((a, b) => a - b), [2000]);

  const healthyHidden = workspaceControl({
    operationEpoch: '162',
    runtimeOperationsReady: true,
    terminalInputReady: false,
    recovery: { phase: 'none', reason: '', attempt: 0, maxAttempts: 6 },
  });
  await updateSnapshot(fixture.environment, makeSnapshot({ control: healthyHidden }));
  await updateSnapshot(fixture.environment, makeSnapshot({ control: workspaceControl({ operationEpoch: '162' }) }));
  await settleAsync();
  assert.equal(fixture.environment.accessibilityAnnouncements.filter(message => message.startsWith('Back online.')).length, 1);
  assert.deepEqual(fixture.environment.timeoutCallbacks.filter(timer => !timer.canceled).map(timer => timer.delay), [2000]);

  await act(async () => { fixture.environment.runFakeTimers(2000); });
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-title' && textContent(node) === 'Back online').length, 0);

  await updateSnapshot(fixture.environment, makeSnapshot({ control: workspaceControl({
    operationEpoch: '161',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'resynchronizing', reason: 'late_old_epoch', attempt: 2, maxAttempts: 6 },
  }) }));
  await updateSnapshot(fixture.environment, makeSnapshot({ control: workspaceControl({ operationEpoch: '161' }) }));
  await settleAsync();
  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-title' && textContent(node) === 'Back online').length, 0);
  assert.equal(fixture.environment.accessibilityAnnouncements.filter(message => message.startsWith('Back online.')).length, 1);
});

test('terminal_missing recovery offers Change instead of a fake terminal picker', async t => {
  const fixture = await mountRecovering(t, workspaceControl({
    operationEpoch: '171',
    runtimeOperationsReady: false,
    terminalInputReady: false,
    recovery: { phase: 'stopped', reason: 'terminal_missing', attempt: 1, maxAttempts: 6 },
  }), 'Failed');

  assert.equal(all(fixture.root, node => node.props?.testID === 'recovery-choose-terminal').length, 0);
  assert.ok(findTestId(fixture.root, 'recovery-change'));
  assert.match(findTestId(fixture.root, 'recovery-detail').children[0], /Change/);
  assert.match(findTestId(fixture.root, 'recovery-meta').children[0], /Change/);

  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  assert.ok(findTestId(fixture.root, 'recovery-change-intro'));
  await press(fixture.root, findLabel(fixture.root, 'Cancel'));
  assert.ok(findTestId(fixture.root, 'recovery-rail'));
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);

  fixture.environment.changeRuntimeMode = 'pending';
  await press(fixture.root, findTestId(fixture.root, 'recovery-change'));
  await press(fixture.root, findTestId(fixture.root, 'recovery-change-runtime'));
  await settleAsync();
  assert.ok(fixture.environment.pendingChangeRuntime);
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);

  fixture.environment.resolvePendingChangeRuntime('accept');
  await settleAsync();
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.ok(findTestId(fixture.root, 'runtime-refresh'));
});

test('external cross-workspace move follows selected stable native terminal', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(fixture.environment.visibility.at(-1), true);
  const rendersBeforeMove = fixture.environment.renderedTerminalIds.length;
  const callsBeforeMove = fixture.environment.calls.length;
  const visibilityBeforeMove = fixture.environment.visibility.length;

  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: 'P1',
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G2', 'W2', 'Group Two', true),
      group('G3', 'W2', 'Other Group', false),
    ],
    terminals: [
      pane('P1', 'W2', 'G2', 'native:P1', true, true, 'P1 moved'),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
      pane('P3', 'W2', 'G2', 'native:P3', false, false, 'P3'),
    ],
  }));

  assert.equal(workspaceTitle(fixture.root), 'Workspace Two');
  assert.equal(groupTitle(fixture.root), 'Group Two');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root).some(view => view.props.terminalId === 'native:P2'), false);
  assert.equal(fixture.environment.renderedTerminalIds.slice(rendersBeforeMove).includes('native:P2'), false);
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeMove), []);
  assert.equal(fixture.environment.visibility.slice(visibilityBeforeMove).includes(false), false);
  assert.equal(fixture.environment.visibility.at(-1), true);
});

test('selected terminal remains visible when its origin workspace disappears', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  assert.equal(fixture.environment.visibility.at(-1), true);
  const callsBeforeMove = fixture.environment.calls.length;
  const visibilityBeforeMove = fixture.environment.visibility.length;
  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: 'P1',
    workspaces: [workspace('W2', 'Workspace Two')],
    groups: [group('G2', 'W2', 'Group Two', true)],
    terminals: [pane('P1', 'W2', 'G2', 'native:P1', true, true, 'P1 moved')],
  }));

  assert.equal(workspaceTitle(fixture.root), 'Workspace Two');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(all(fixture.root, node => node.props && node.props.accessibilityLabel === 'Terminal unavailable').length, 0);
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeMove), []);
  assert.equal(fixture.environment.visibility.slice(visibilityBeforeMove).includes(false), false);
  assert.equal(fixture.environment.visibility.at(-1), true);
});

test('same-workspace group move follows selected terminal without stale pane fallback', async t => {
  const snapshot = makeSnapshot({
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G2', 'W1', 'Group Two', false),
      group('G3', 'W2', 'Group Three', true),
    ],
    terminals: [
      pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1'),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
      pane('P3', 'W1', 'G2', 'native:P3', false, false, 'P3'),
    ],
  });
  const fixture = await mountForTest(t, snapshot);
  await openWorkspace(fixture.root, 'W1');
  assert.equal(fixture.environment.visibility.at(-1), true);
  const callsBeforeMove = fixture.environment.calls.length;
  const visibilityBeforeMove = fixture.environment.visibility.length;
  await updateSnapshot(fixture.environment, makeSnapshot({
    groups: [
      group('G1', 'W1', 'Group One', false),
      group('G2', 'W1', 'Group Two', true),
      group('G3', 'W2', 'Group Three', true),
    ],
    terminals: [
      pane('P1', 'W1', 'G2', 'native:P1', true, true, 'P1 moved'),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
      pane('P3', 'W1', 'G2', 'native:P3', false, false, 'P3'),
    ],
  }));

  assert.equal(workspaceTitle(fixture.root), 'Workspace One');
  assert.equal(groupTitle(fixture.root), 'Group Two');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(terminalViews(fixture.root).some(view => view.props.terminalId === 'native:P2'), false);
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeMove), []);
  assert.equal(fixture.environment.visibility.slice(visibilityBeforeMove).includes(false), false);
  assert.equal(fixture.environment.visibility.at(-1), true);
});

test('manual empty-group navigation preserves workspace with per-workspace selected groups', async t => {
  const fixture = await mountForTest(t, makeSnapshot({
    selectedPane: null,
    groups: [
      group('G-empty', 'W1', 'Empty Group', true),
      group('G2', 'W2', 'Other Group', true),
    ],
    terminals: [pane('P3', 'W2', 'G2', 'native:P3', false, true, 'P3')],
  }));
  await openWorkspace(fixture.root, 'W1');

  assert.equal(workspaceTitle(fixture.root), 'Workspace One');
  assert.equal(groupTitle(fixture.root), '');
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.ok(fixture.environment.calls.some(call => call.method === 'selectGroup' && call.groupId === 'G-empty'));
});

test('manual empty-workspace navigation survives a stale native selection snapshot', async t => {
  const fixture = await mountForTest(t, makeSnapshot({
    workspaces: [workspace('W1', 'Workspace One'), workspace('W2', 'Workspace Two')],
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G-empty', 'W2', 'Empty Group', true),
    ],
    terminals: [pane('P1', 'W1', 'G1', 'native:P1', true, true, 'P1')],
  }));
  fixture.native.selectGroup = async (_connectionId, groupId) => {
    fixture.environment.calls.push({ method: 'selectGroup', groupId });
  };
  let resolveCommandSnapshot;
  const commandSnapshot = new Promise(resolve => { resolveCommandSnapshot = resolve; });
  const getWorkspaceState = fixture.native.getWorkspaceState;
  fixture.native.getWorkspaceState = async (...args) => {
    const state = await getWorkspaceState(...args);
    resolveCommandSnapshot();
    return state;
  };

  await press(fixture.root, findTestId(fixture.root, 'workspace-row-W2'));
  assert.ok(fixture.environment.calls.some(call => call.method === 'selectGroup' && call.groupId === 'G-empty'));
  await act(async () => { await commandSnapshot; });
  const visibilityBeforeSnapshot = fixture.environment.visibility.length;

  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: null,
    workspaces: [workspace('W1', 'Workspace One'), workspace('W2', 'Workspace Two')],
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G-empty', 'W2', 'Empty Group', true),
    ],
    terminals: [pane('P1', 'W1', 'G1', 'native:P1', false, true, 'P1')],
  }));

  assert.equal(workspaceTitle(fixture.root), 'Workspace Two');
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(fixture.environment.visibility.slice(visibilityBeforeSnapshot).includes(false), true);
  assert.equal(fixture.environment.visibility.at(-1), false);
});

test('empty selected group after a moved terminal keeps the last native workspace', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: 'P1',
    terminals: [
      pane('P1', 'W2', 'G2', 'native:P1', true, true, 'P1 moved'),
      pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2'),
    ],
  }));
  assert.equal(workspaceTitle(fixture.root), 'Workspace Two');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);

  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: null,
    groups: [
      group('G1', 'W1', 'Group One', true),
      group('G2', 'W2', 'Group Two', true),
    ],
    terminals: [pane('P2', 'W1', 'G1', 'native:P2', false, false, 'P2')],
  }));

  assert.equal(workspaceTitle(fixture.root), 'Workspace Two');
  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(all(fixture.root, node => node.props && node.props.accessibilityLabel === 'Terminal unavailable').length, 1);
});

test('workspace list stays visible during native selection changes and manual navigation remains explicit', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  assert.equal(all(fixture.root, node => node.props && node.props.accessibilityLabel === 'Switch workspace').length, 0);
  const callsBeforeNativeChange = fixture.environment.calls.length;
  await updateSnapshot(fixture.environment, makeSnapshot({ selectedPane: 'P3' }));
  assert.equal(all(fixture.root, node => node.props && node.props.accessibilityLabel === 'Switch workspace').length, 0);
  assert.deepEqual(fixture.environment.calls.slice(callsBeforeNativeChange), []);
  const tools = findTestId(fixture.root, 'workspace-row-W2');
  assert.equal(tools.props.accessibilityState.selected, true);

  await openWorkspace(fixture.root, 'W1');
  assert.equal(workspaceTitle(fixture.root), 'Workspace One');
  assert.ok(fixture.environment.calls.some(call => call.method === 'selectPane' && call.paneId === 'P1'));
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
});

test('closing selected pane follows native fallback pane instead of an unavailable terminal', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  await closeCurrentPane(fixture.root, fixture.environment);

  assert.equal(workspaceTitle(fixture.root), 'Workspace One');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P2']);
  assert.equal(fixture.environment.calls.at(-1).method, 'closePane');
});

test('absence of a selected pane hides and releases the native terminal view', async t => {
  const fixture = await mountForTest(t, makeSnapshot());
  await openWorkspace(fixture.root, 'W1');
  assert.deepEqual(terminalViews(fixture.root).map(view => view.props.terminalId), ['native:P1']);
  assert.equal(fixture.environment.visibility.at(-1), true);
  const visibilityBeforeRelease = fixture.environment.visibility.length;
  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: null,
    groups: [group('G1', 'W1', 'Empty Group', true), group('G2', 'W2', 'Group Two', true)],
    terminals: [
      pane('P2', 'W1', 'G1', 'native:P2', false, true, 'P2 active'),
      pane('P3', 'W2', 'G2', 'native:P3', false, false, 'P3'),
    ],
  }));

  assert.equal(terminalViews(fixture.root).length, 0);
  assert.equal(fixture.environment.visibility.at(visibilityBeforeRelease - 1), true);
  assert.equal(fixture.environment.visibility.slice(visibilityBeforeRelease).includes(false), true, 'native visibility should be released when no terminal is selected');
  assert.equal(fixture.environment.visibility.at(-1), false);
  assert.equal(all(fixture.root, node => node.props && node.props.accessibilityLabel === 'Terminal unavailable').length, 1);
});

// ---- Issue #28 attachment UI flow (review-round cases, adapted) ----

const ATTACHMENT_PREPARED = {
  status: 'prepared',
  fileId: 'att_review001.png',
  previewUri: 'file:///review.png',
  format: 'png',
  width: 100,
  height: 100,
  byteCount: 1000,
  sourceByteCount: 1000,
};

const ATTACHMENT_UPLOADED = {
  phase: 'uploaded',
  attachmentId: '42',
  bytesUploaded: 1000,
  sizeBytes: 1000,
  remotePath: '/home/u/meeterm-review.png',
  displayName: 'review',
  errorCode: '',
  errorMessage: '',
  insertUnconfirmed: false,
  remoteRemoved: false,
};

/**
 * Double the native attachment surface. `environment.attachmentOp` is the
 * live core record — tests mutate it to drive snapshot-poll transitions.
 */
function configureAttachment(environment, native, initialOp) {
  environment.attachmentOp = initialOp;
  native.attachmentCompositionStatus = async () => environment.compositionHeld
    ? { status: 'held', reason: 'composing' }
    : { status: 'ok' };
  native.getAttachmentState = async () => ({
    status: 'idle',
    fileId: '',
    previewUri: '',
    format: '',
    width: 0,
    height: 0,
    byteCount: 0,
    sourceByteCount: 0,
    target: null,
    operation: null,
    errorCode: '',
    errorMessage: '',
  });
  native.beginAttachment = async (id, target) => {
    environment.attachmentTarget = target;
    return { status: 'ready' };
  };
  native.pickAttachmentImage = async () => ({ status: 'picked', token: 'att_review001.bin', byteCount: 1000 });
  native.prepareAttachmentImage = async () => ATTACHMENT_PREPARED;
  native.uploadAttachment = async (terminalId, remoteDirectory) => {
    environment.attachmentOwner = terminalId;
    environment.attachmentRemoteDirectory = remoteDirectory;
    environment.attachmentOp = environment.attachmentOp && environment.attachmentOp.phase !== 'cancelled'
      ? environment.attachmentOp
      : { ...ATTACHMENT_UPLOADED, phase: 'uploading', bytesUploaded: 0, remotePath: '' };
    return { status: 'accepted', attachmentId: '42' };
  };
  native.attachmentSnapshot = async () => environment.attachmentOp
    ? { status: 'snapshot', operation: environment.attachmentOp }
    : { status: 'idle' };
  native.retryAttachmentUpload = async () => {
    environment.attachmentRetries = (environment.attachmentRetries || 0) + 1;
    return { status: 'accepted', attachmentId: '42' };
  };
  native.cancelAttachment = async () => ({ status: 'accepted', attachmentId: '42' });
  native.insertAttachment = async terminalId => {
    environment.insertedInto = terminalId;
    if (environment.attachmentOp) environment.attachmentOp = { ...environment.attachmentOp, phase: 'inserted' };
    return { status: 'inserted' };
  };
  native.deleteRemoteAttachment = async () => {
    environment.deleteCalls = (environment.deleteCalls || 0) + 1;
    return { status: 'accepted', attachmentId: '42' };
  };
}

/** Mount → open W1 terminal → open the attachment sheet → pick → ready. */
async function openPrepared(t, op) {
  const fixture = await mountForTest(t, makeSnapshot(), (e, n) => configureAttachment(e, n, op));
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findTestId(fixture.root, 'attach-image'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'attachment-pick-files'));
  await settleAsync();
  return fixture;
}

test('attachment: opening the sheet keeps the terminal input surface alive', async t => {
  const fixture = await openPrepared(t, null);
  assert.equal(fixture.environment.visibility.at(-1), true, 'the input surface must stay reported visible under the attachment sheet');
  assert.equal(terminalViews(fixture.root).length, 1, 'the terminal view stays mounted while the sheet is open');
  assert.ok(findTestId(fixture.root, 'attachment-remote-dir'), 'the ready-phase sheet should render');
});

test('attachment: a composing IME holds the sheet open request', async t => {
  const fixture = await mountForTest(t, makeSnapshot(), (e, n) => {
    configureAttachment(e, n, null);
    e.compositionHeld = true;
  });
  await settleAsync();
  await openWorkspace(fixture.root, 'W1');
  await press(fixture.root, findTestId(fixture.root, 'attach-image'));
  await settleAsync();
  assert.equal(fixture.environment.attachmentTarget, undefined, 'beginAttachment must not run while composition is held');
  assert.ok(findText(fixture.root, 'Finish IME composition before attaching.'), 'the hold notice should explain the block');
  assert.equal(fixture.environment.visibility.at(-1), true, 'the terminal surface stays live while composition is held');
});

test('attachment: upload uses the captured destination, not the selected pane', async t => {
  const fixture = await openPrepared(t, null);
  // Switch the selected pane under the open sheet: the captured target must
  // still own the upload — it never follows the current selection.
  await updateSnapshot(fixture.environment, makeSnapshot({ selectedPane: 'P2' }));
  await press(fixture.root, findTestId(fixture.root, 'attachment-upload'));
  await settleAsync();
  assert.equal(fixture.environment.attachmentOwner, 'native:P1');
});

test('attachment: an accepted delete stays pending until remoteRemoved', async t => {
  const fixture = await openPrepared(t, null);
  await press(fixture.root, findTestId(fixture.root, 'attachment-upload'));
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  const timersBefore = fixture.environment.intervalCallbacks.length;
  await press(fixture.root, findTestId(fixture.root, 'attachment-delete-remote'));
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'attachment-deleting'), 'an accepted delete shows the pending Deleting state');
  assert.ok(fixture.environment.intervalCallbacks.length > timersBefore, 'deletion keeps a snapshot poll running');
  assert.equal(findTestId(fixture.root, 'attachment-insert').props.disabled, true, 'insert stays disabled while deletion is pending');
  // The flag arrives through the poll — only then does the removed state show.
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED, remoteRemoved: true };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  assert.ok(findTestId(fixture.root, 'attachment-deleted'), 'verified removal shows the deleted state');
});

test('attachment: Upload again after verified removal invokes transfer retry', async t => {
  const fixture = await openPrepared(t, null);
  await press(fixture.root, findTestId(fixture.root, 'attachment-upload'));
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'attachment-delete-remote'));
  await settleAsync();
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED, remoteRemoved: true };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'attachment-retry-upload'));
  await settleAsync();
  assert.equal(fixture.environment.attachmentRetries, 1, 'Upload again after verified removal must call retryAttachmentUpload');
});

test('attachment: toolbar insert appears only for the captured terminal and taps through the input gate', async t => {
  const fixture = await openPrepared(t, null);
  await press(fixture.root, findTestId(fixture.root, 'attachment-upload'));
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  // Uploaded guidance replaces the in-sheet Insert action.
  assert.ok(findTestId(fixture.root, 'attachment-insert-guidance'), 'the uploaded sheet should point at the terminal toolbar');
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'attachment-insert').length, 1, 'the toolbar insert action is rendered for the captured terminal');
  // Close the sheet — the toolbar action is the only insert path.
  await press(fixture.root, findLabel(fixture.root, 'Close attachment sheet'));
  await settleAsync();
  await press(fixture.root, findTestId(fixture.root, 'attachment-insert'));
  await settleAsync();
  assert.equal(fixture.environment.insertedInto, 'native:P1', 'insert targets the captured destination terminal');
  assert.ok(findText(fixture.root, 'Inserted into terminal input. Review it before sending.'), 'the toolbar result is announced');
});

test('attachment: toolbar insert is hidden on other terminals and gated when input is not ready', async t => {
  const fixture = await openPrepared(t, null);
  await press(fixture.root, findTestId(fixture.root, 'attachment-upload'));
  fixture.environment.attachmentOp = { ...ATTACHMENT_UPLOADED };
  await act(async () => {
    fixture.environment.intervalCallbacks.at(-1)();
  });
  await settleAsync();
  await press(fixture.root, findLabel(fixture.root, 'Close attachment sheet'));
  await settleAsync();
  // Stopped recovery while the destination terminal is retained: the surface
  // stays up in cached mode, the input gate is down, and the tap explains
  // itself without reaching the core.
  await updateSnapshot(fixture.environment, makeSnapshot({
    selectedPane: 'P1',
    control: workspaceControl({
      operationEpoch: '2',
      hasRetainedWork: true,
      runtimeOperationsReady: false,
      terminalInputReady: false,
      recovery: { phase: 'stopped', reason: 'runtime_missing', attempt: 6, maxAttempts: 6 },
    }),
  }));
  await press(fixture.root, findTestId(fixture.root, 'attachment-insert'));
  await settleAsync();
  assert.equal(fixture.environment.insertedInto, undefined, 'a blocked input gate must not reach the core insert');
  assert.ok(findText(fixture.root, 'The terminal input is not ready. Finish recovery and keep this terminal open before inserting.'), 'the blocked tap should explain itself');
  // A different selected pane hides the action — never retargets it.
  await updateSnapshot(fixture.environment, makeSnapshot({ selectedPane: 'P2' }));
  assert.equal(all(fixture.root, node => node.props && node.props.testID === 'attachment-insert').length, 0, 'insert must not appear for a non-destination terminal');
});
