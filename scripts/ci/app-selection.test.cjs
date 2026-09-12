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
};

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
    snapshot: null,
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
    visibility: [],
    renderedTerminalIds: [],
    intervalCallbacks: [],
    alert: null,
  };

  const native = {
    async getProfiles() { return []; },
    async getPreferences() { return { ...PREFERENCES }; },
    async setPreferences() {},
    async setAutomaticReconnect() {},
    async setForeground() {},
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
  return { environment, native };
}

function makeReactNativeMocks(environment) {
  const noOpSubscription = { remove() {} };
  const AppState = {
    currentState: 'active',
    addEventListener() { return noOpSubscription; },
  };
  const BackHandler = {
    addEventListener() { return noOpSubscription; },
  };
  const Keyboard = { dismiss() {} };
  const Linking = {
    async getInitialURL() { return null; },
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

function loadApp(environment, native) {
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
  ]);
  function localRequire(request) {
    if (moduleMap.has(request)) return moduleMap.get(request);
    return require(request);
  }
  const processForApp = { ...process, env: { ...process.env } };
  processForApp.env.EXPO_PUBLIC_MEETERM_SMOKE = '0';
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
  return appModule.exports.default;
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function pane(id, workspaceId, groupId, terminalId, selected = false, active = false, name = id) {
  return {
    id,
    workspaceId,
    groupId,
    terminalId,
    name,
    active,
    selected,
    agent: null,
  };
}

function group(id, workspaceId, name, selected) {
  return { id, workspaceId, name, selected };
}

function workspace(id, name) {
  return { id, name };
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
