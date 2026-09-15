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

function Modal({ visible, children }) {
  return visible ? React.createElement(React.Fragment, null, children) : null;
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
    visibility: [],
    renderedTerminalIds: [],
    intervalCallbacks: [],
    alert: null,
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
      environment.connection.state = 'DiscoveringRuntimes';
    },
    async connectProfileHost(_connectionId, profileId) {
      environment.nativeCalls.push({ method: 'connectProfileHost', profileId });
      environment.connection.state = 'DiscoveringRuntimes';
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
      environment.connection.state = 'Disconnected';
    },
    async setForeground(_connectionId, foreground) { environment.foregroundCalls.push(foreground); },
    async getConnectionState() { return { ...environment.connection }; },
    async getWorkspaceState() { return clone(environment.snapshot); },
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
  return { environment, native };
}

function completeSelection(environment, candidate) {
  environment.snapshot.backend = candidate.backend;
  environment.snapshot.runtime = candidate.name;
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
    addEventListener() { return noOpSubscription; },
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
  const Platform = { OS: 'ios' };
  function StatusBar(props) { return React.createElement('StatusBar', props); }
  StatusBar.setBarStyle = () => {};
  const StyleSheet = {
    hairlineWidth: 1,
    create(styles) { return styles; },
  };

  return {
    ActivityIndicator: 'ActivityIndicator',
    Alert,
    AppState,
    BackHandler,
    FlatList,
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
  return {
    __esModule: true,
    ConnectionForm: hidden,
    DEFAULT_PREFERENCES: { ...PREFERENCES },
    itemActions() {},
    NameForm: hidden,
    ProfileList: hidden,
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
    setTimeout,
    clearTimeout,
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
  for (const screen of ['welcome', 'empty', 'search-empty', 'disconnected', 'reconnecting', 'connection-error', 'long-workspaces',
    'runtime-picker', 'runtime-partial-error', 'runtime-empty', 'runtime-create']) {
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
  const herdrPicker = smoke.smokeFixture('herdr-connection');
  assert.equal(herdrPicker.connection.state, 'AwaitingRuntimeSelection');
  assert.equal(herdrPicker.formVisible, false);
  assert.equal(herdrPicker.runtimeDiscovery.backends[0].candidates[0].lastUsed, false);
  assert.equal(herdrPicker.runtimeDiscovery.backends[1].candidates[0].lastUsed, true);
  assert.equal(environment.calls.length, 0);
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
  assert.equal(resolveAgentStatus(null, true), null);
  assert.equal(resolveAgentStatus('blocked', true), 'blocked');
  assert.equal(resolveAgentStatus('blocked', false), 'unavailable');
  assert.equal(agentStatusPhrase('done', true), 'Agent status: finished, not yet viewed');
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
        React.createElement(AgentStatusIndicator, { status: 'blocked', live: true, colors: LIGHT, testID: 'blocked' }),
        React.createElement(AgentStatusIndicator, { status: 'idle', live: true, colors: LIGHT, testID: 'idle' }),
        React.createElement(AgentStatusIndicator, { status: 'unknown', live: false, colors: DARK, showLabel: true, testID: 'unavailable' }),
        React.createElement(AgentStatusIndicator, { status: null, live: true, colors: LIGHT, testID: 'none' }),
      ));
    });
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
} = {}) {
  return {
    backend: 'herdr',
    runtime: 'default',
    groupsSupported: true,
    workspaces,
    groups,
    terminals: terminals.map(item => ({ ...item, selected: item.id === selectedPane })),
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

async function mountApp(snapshot) {
  const { environment, native } = makeNativeEnvironment();
  environment.snapshot = clone(snapshot);
  const App = loadApp(environment, native);
  const root = createRoot();
  await act(async () => {
    root.render(React.createElement(App));
  });
  return { root, environment, native };
}

async function mountForTest(t, snapshot) {
  const fixture = await mountApp(snapshot);
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

async function poll(env) {
  assert.equal(env.intervalCallbacks.length, 1, 'App should register one metadata polling interval');
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

test('verified runtime reconnect skips the picker but a lost identity reopens it with an explanation', async t => {
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
