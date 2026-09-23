import { useMemo } from 'react';
import type { ReactNode } from 'react';
import {
  ActivityIndicator,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { SafeAreaView, useSafeAreaInsets } from 'react-native-safe-area-context';

import type {
  RuntimeBackend,
  RuntimeBrowseState,
  RuntimeCandidate,
  RuntimeDiscovery,
  ServerProfile,
} from '../modules/meeterm-terminal';
import { Button, Icon, IconButton } from './ui';
import type { Palette } from './ui';

export type SessionSwitcherServer = Pick<ServerProfile, 'id' | 'name' | 'host' | 'port' | 'username' | 'credentialSaved'>;

type Props = {
  mode: 'switch' | 'fresh';
  currentServer: SessionSwitcherServer;
  profiles: ServerProfile[];
  expandedServerId: string;
  browseTargetId: string;
  browse: RuntimeBrowseState | null;
  freshDiscovery: RuntimeDiscovery | null;
  currentBinding: { backend: RuntimeBackend; runtime: string } | null;
  busy: boolean;
  selectingId: string;
  selectionErrors?: Record<string, string>;
  error: string;
  credentialStep?: ReactNode;
  createVisible: boolean;
  createName: string;
  createError: string;
  cleanupWarning: string;
  onClose: () => void;
  onToggleServer: (server: SessionSwitcherServer) => void;
  onSelect: (candidate: RuntimeCandidate) => void;
  onRefresh: () => void;
  onRetryBackend: (backend: RuntimeBackend) => void;
  onOpenCredential: (server: SessionSwitcherServer) => void;
  onOpenCreate: () => void;
  onCreateNameChange: (name: string) => void;
  onCreate: () => void;
  onCancelCreate: () => void;
  onManageServers: () => void;
  onDisconnect: () => void;
  colors: Palette;
};

const BACKENDS: ReadonlyArray<{ id: RuntimeBackend; title: string }> = [
  { id: 'tmux', title: 'tmux' },
  { id: 'herdr', title: 'Herdr' },
];

function address(server: SessionSwitcherServer): string {
  return `${server.username}@${server.host}:${server.port}`;
}

function sortedProfiles(profiles: ServerProfile[], currentId: string): ServerProfile[] {
  return profiles
    .filter(profile => profile.id !== currentId)
    .slice()
    .sort((a, b) => {
      const left = a.name.toLocaleLowerCase();
      const right = b.name.toLocaleLowerCase();
      return left < right ? -1 : left > right ? 1 : a.id.localeCompare(b.id);
    });
}

function backendDiscovery(discovery: RuntimeDiscovery | null, backend: RuntimeBackend) {
  return discovery?.backends.find(item => item.backend === backend) ?? null;
}

function candidateError(candidate: RuntimeCandidate): string {
  return candidate.errorMessage || candidate.errorCode;
}

export function SessionSwitcher({
  mode,
  currentServer,
  profiles,
  expandedServerId,
  browseTargetId,
  browse,
  freshDiscovery,
  currentBinding,
  busy,
  selectingId,
  selectionErrors = {},
  error,
  credentialStep,
  createVisible,
  createName,
  createError,
  cleanupWarning,
  onClose,
  onToggleServer,
  onSelect,
  onRefresh,
  onRetryBackend,
  onOpenCredential,
  onOpenCreate,
  onCreateNameChange,
  onCreate,
  onCancelCreate,
  onManageServers,
  onDisconnect,
  colors,
}: Props) {
  const insets = useSafeAreaInsets();
  const availableProfiles = useMemo(() => sortedProfiles(profiles, currentServer.id), [currentServer.id, profiles]);
  const activeDiscovery = mode === 'fresh' ? freshDiscovery : browse?.discovery ?? null;
  const discoveryReady = mode === 'fresh'
    ? activeDiscovery?.backends.some(section => section.state !== 'loading') === true
    : browse?.phase === 'ready';
  const hostKeyPending = mode === 'switch' && browse?.hostKey.pending === true;

  const renderCandidate = (candidate: RuntimeCandidate, server: SessionSwitcherServer, fresh = false) => {
    const stopped = candidate.state === 'stopped';
    const unavailable = stopped || !candidate.selectable;
    const selected = Boolean(currentBinding
      && server.id === currentServer.id
      && currentBinding.backend === candidate.backend
      && currentBinding.runtime === candidate.name);
    const sameTmuxBindingCanBeConfirmed = selected && candidate.backend === 'tmux';
    const disabled = unavailable || busy || (selected && !sameTmuxBindingCanBeConfirmed) || hostKeyPending;
    const pending = selectingId === candidate.id;
    const errorText = candidateError(candidate) || selectionErrors[candidate.id] || '';
    const label = `${candidate.backend === 'tmux' ? 'tmux' : 'Herdr'} session ${candidate.name} on ${server.name} (${address(server)})`;
    const hint = stopped && candidate.backend === 'herdr'
      ? 'Start this session in the existing Herdr client, then refresh.'
      : selected ? 'Currently selected session.' : undefined;
    return <View key={`${server.id}:${candidate.backend}:${candidate.id}`} style={[styles.sessionRow, { borderBottomColor: colors.border }]}>
      <Pressable
        testID={`runtime-row-${candidate.backend}-${candidate.id}`}
        accessibilityRole="button"
        accessibilityLabel={label}
        accessibilityHint={hint}
        accessibilityState={{ selected, disabled }}
        disabled={disabled}
        onPress={() => onSelect(candidate)}
        style={({ pressed }) => [styles.sessionChoice, pressed && { backgroundColor: colors.surface }, unavailable && { opacity: .62 }]}
      >
        <View style={styles.sessionCopy}>
          <View style={styles.sessionTitleLine}>
            <Text numberOfLines={2} style={[styles.sessionName, { color: colors.text }]}>{candidate.name}</Text>
            {candidate.lastUsed ? <Text style={[styles.hintBadge, { color: colors.accent, borderColor: colors.accent }]}>Last used</Text> : null}
          </View>
          <Text style={[styles.auxLabel, { color: colors.muted }]}>{candidate.backend === 'tmux' ? 'tmux' : 'Herdr'}{stopped ? ' · Stopped' : ''}{pending ? ' · Switching…' : ''}</Text>
          {stopped && candidate.backend === 'herdr' ? <Text style={[styles.rowDetail, { color: colors.muted }]}>Start it in the existing Herdr client, then Refresh.</Text> : null}
          {!candidate.selectable && !stopped ? <Text style={[styles.rowDetail, { color: colors.muted }]}>This session is unavailable. Refresh to check again.</Text> : null}
          {errorText ? <Text accessibilityRole="alert" style={[styles.rowDetail, { color: colors.danger }]}>{errorText}</Text> : null}
        </View>
        {pending ? <ActivityIndicator color={colors.accent} /> : selected ? <Icon name="check" color={colors.accent} size={20} /> : <View style={styles.checkSlot} />}
      </Pressable>
    </View>;
  };

  const renderSessionSections = (server: SessionSwitcherServer, discovery: RuntimeDiscovery | null, fresh = false) => <View>
    {BACKENDS.map(({ id, title }) => {
      const section = backendDiscovery(discovery, id);
      const candidates = section?.candidates ?? [];
      const isError = section?.state === 'error';
      const isLoading = section?.state === 'loading' || (!section && !discoveryReady);
      return <View key={`${server.id}:${id}`} style={styles.backendSection}>
        <View style={styles.backendHeader}>
          <Text style={[styles.backendTitle, { color: colors.muted }]}>{title}</Text>
          {isLoading ? <ActivityIndicator size="small" color={colors.accent} /> : null}
          {isError ? <Pressable accessibilityRole="button" accessibilityLabel={`Retry ${title} discovery`} disabled={busy} onPress={() => onRetryBackend(id)} style={styles.retryAction}><Text style={[styles.action, { color: colors.accent }]}>Retry</Text></Pressable> : null}
        </View>
        {isError ? <Text accessibilityRole="alert" style={[styles.rowDetail, { color: colors.danger }]}>{section?.errorMessage || `${title} sessions could not be loaded.`}</Text> : null}
        {!isLoading && !isError && candidates.length === 0 ? <Text style={[styles.rowDetail, { color: colors.muted }]}>{title === 'Herdr' ? 'No Herdr sessions found.' : 'No tmux sessions found.'}</Text> : null}
        {candidates.map(candidate => renderCandidate(candidate, server, fresh))}
      </View>;
    })}
  </View>;

  if (credentialStep) return <View style={[styles.credentialStep, { backgroundColor: colors.background }]}>{credentialStep}</View>;

  const renderServer = (server: SessionSwitcherServer, current: boolean) => {
    const expanded = expandedServerId === server.id;
    const hasCredential = server.credentialSaved;
    const serverBusy = busy && browseTargetId === server.id;
    return <View key={server.id} style={[styles.serverGroup, { borderBottomColor: colors.border }]}>
      <Pressable
        testID={`switcher-server-${server.id}`}
        accessibilityRole="button"
        accessibilityLabel={`${current ? 'Current server' : 'Server'} ${server.name}`}
        accessibilityState={{ selected: current, expanded, disabled: serverBusy }}
        disabled={serverBusy}
        onPress={() => hasCredential || current ? onToggleServer(server) : onOpenCredential(server)}
        style={({ pressed }) => [styles.serverChoice, pressed && { backgroundColor: colors.surface }]}
      >
        <Icon name={current ? 'check' : 'server'} color={current ? colors.accent : colors.muted} size={18} />
        <View style={styles.serverCopy}>
          <Text numberOfLines={2} style={[styles.serverTitle, { color: colors.text }]}>{server.name}</Text>
          <Text numberOfLines={1} style={[styles.rowDetail, { color: colors.muted }]}>{address(server)}{current ? ' · Current server' : ''}</Text>
        </View>
        {serverBusy ? <ActivityIndicator color={colors.accent} /> : <Icon name={expanded ? 'down' : 'chevron'} color={colors.muted} size={17} />}
      </Pressable>
      {expanded ? <View style={styles.expandedContent}>
        {browseTargetId === server.id && browse?.phase === 'failed' ? <View style={[styles.inlineMessage, { backgroundColor: colors.surface }]}>
          <Text accessibilityRole="alert" style={[styles.rowDetail, { color: colors.danger }]}>{browse.errorMessage || 'Could not explore this server.'}</Text>
          {browse.errorCode === 'authentication_failed' ? <Text style={[styles.rowDetail, { color: colors.muted }]}>Check the saved credentials or enter them again.</Text> : null}
        </View> : null}
        {browseTargetId === server.id && browse?.hostKey.pending ? <View style={[styles.inlineMessage, { backgroundColor: colors.surface }]}>
          <Text style={[styles.rowDetail, { color: colors.text }]}>Verify the SSH host key for {browse.hostKey.host}:{browse.hostKey.port} in the confirmation prompt.</Text>
        </View> : null}
        {mode === 'fresh' && current ? renderSessionSections(server, freshDiscovery, true) : browseTargetId === server.id ? renderSessionSections(server, browse?.discovery ?? null) : <View style={styles.inlineMessage}>
          <Text style={[styles.rowDetail, { color: colors.muted }]}>{hasCredential ? 'Tap to load real sessions from this server.' : 'Credentials are needed before sessions can be explored.'}</Text>
        </View>}
        {(browseTargetId === server.id && browse?.phase === 'ready') || (mode === 'fresh' && current && discoveryReady)
          ? <Pressable testID="runtime-refresh" accessibilityRole="button" accessibilityLabel="Refresh sessions" disabled={busy} onPress={onRefresh} style={styles.refreshAction}><Text style={[styles.action, { color: colors.accent }]}>Refresh</Text></Pressable>
          : null}
      </View> : null}
    </View>;
  };

  const footerTarget = mode === 'fresh'
    ? currentServer
    : (browseTargetId === currentServer.id ? currentServer : availableProfiles.find(item => item.id === browseTargetId) ?? currentServer);
  const showFooter = mode === 'switch' || discoveryReady || freshDiscovery !== null;

  return <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.safeArea, { backgroundColor: colors.background }]}>
    <KeyboardAvoidingView style={styles.flex} behavior={Platform.OS === 'ios' ? 'padding' : 'height'}>
      {createVisible ? <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        automaticallyAdjustKeyboardInsets
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={[styles.content, { paddingBottom: Math.max(insets.bottom, 24) }]}
      >
        <View style={styles.titleRow}>
          <View style={styles.flex}><Text accessibilityRole="header" style={[styles.heading, { color: colors.text }]}>New tmux session</Text><Text style={[styles.rowDetail, { color: colors.muted }]}>On {footerTarget.name} · {address(footerTarget)}</Text></View>
          <IconButton icon="back" label="Back to sessions" colors={colors} onPress={onCancelCreate} />
        </View>
        <TextInput accessibilityLabel="New tmux session name" testID="switcher-create-name" autoCapitalize="none" autoCorrect={false} value={createName} onChangeText={onCreateNameChange} placeholder="meeterm" placeholderTextColor={colors.placeholder} style={[styles.nameInput, { color: colors.text, backgroundColor: colors.surface, borderColor: colors.border }]} returnKeyType="done" onSubmitEditing={onCreate} />
        {createError ? <Text accessibilityRole="alert" style={[styles.rowDetail, { color: colors.danger }]}>{createError}</Text> : null}
        {error ? <Text accessibilityRole="alert" style={[styles.rowDetail, { color: colors.danger }]}>{error}</Text> : null}
        <Button testID="switcher-create-submit" label="Create tmux session" colors={colors} disabled={busy} onPress={onCreate}>Create session</Button>
        <Text style={[styles.rowDetail, { color: colors.muted }]}>A new detached session will be created. Existing sessions stay available.</Text>
      </ScrollView> : <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        automaticallyAdjustKeyboardInsets
        keyboardShouldPersistTaps="handled"
        keyboardDismissMode="on-drag"
        contentContainerStyle={[styles.content, { paddingBottom: Math.max(insets.bottom, 24) }]}
      >
        <View style={styles.titleRow}>
          <View style={styles.flex}><Text accessibilityRole="header" style={[styles.heading, { color: colors.text }]}>{mode === 'fresh' ? 'Choose a session' : 'Switch session'}</Text><Text style={[styles.intro, { color: colors.muted }]}>{mode === 'fresh' ? 'Choose a real session to open. Nothing is selected automatically.' : 'Choose a server, then select one of its running sessions.'}</Text></View>
          {mode === 'switch' ? <IconButton icon="close" label="Close session switcher" colors={colors} disabled={busy} onPress={onClose} /> : null}
        </View>
        {cleanupWarning ? <View style={[styles.warning, { backgroundColor: colors.surface }]}><Text style={[styles.rowDetail, { color: colors.danger }]}>{cleanupWarning}</Text></View> : null}
        {error ? <Text accessibilityRole="alert" style={[styles.inlineError, { color: colors.danger }]}>{error}</Text> : null}
        {renderServer(currentServer, true)}
        {mode === 'switch' ? availableProfiles.map(profile => renderServer(profile, false)) : null}
        {showFooter ? <View style={[styles.footer, { borderTopColor: colors.border }]}>
          <Pressable testID="switcher-new-tmux" accessibilityRole="button" accessibilityLabel={`New tmux session on ${footerTarget.name}`} disabled={busy} onPress={onOpenCreate} style={({ pressed }) => [styles.footerAction, pressed && { backgroundColor: colors.surface }]}>
            <View style={styles.footerCopy}><Text style={[styles.footerTitle, { color: colors.text }]}>New tmux session</Text><Text style={[styles.rowDetail, { color: colors.muted }]}>On {footerTarget.name}</Text></View><Icon name="plus" color={colors.accent} size={18} />
          </Pressable>
          {mode === 'switch' ? <Pressable testID="switcher-manage-servers" accessibilityRole="button" accessibilityLabel="Manage servers" accessibilityState={{ disabled: busy }} disabled={busy} onPress={onManageServers} style={({ pressed }) => [styles.footerAction, pressed && { backgroundColor: colors.surface }]}>
            <Text style={[styles.footerTitle, { color: colors.text }]}>Manage servers</Text><Icon name="chevron" color={colors.muted} size={18} />
          </Pressable> : null}
          <Pressable testID="switcher-disconnect" accessibilityRole="button" accessibilityLabel={mode === 'fresh' ? 'Cancel connection' : 'Disconnect'} accessibilityState={{ disabled: busy }} disabled={busy} onPress={onDisconnect} style={({ pressed }) => [styles.disconnectAction, pressed && { backgroundColor: colors.surface }]}>
            <Text style={[styles.footerTitle, { color: colors.danger }]}>{mode === 'fresh' ? 'Cancel connection' : 'Disconnect'}</Text><Icon name="server" color={colors.danger} size={18} />
          </Pressable>
        </View> : null}
      </ScrollView>}
    </KeyboardAvoidingView>
  </SafeAreaView>;
}

const styles = StyleSheet.create({
  flex: { flex: 1, minWidth: 0 },
  safeArea: { flex: 1 },
  credentialStep: { flex: 1 },
  content: { paddingHorizontal: 20, paddingTop: 8, gap: 12 },
  titleRow: { minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 12, paddingBottom: 8 },
  heading: { fontSize: 20, lineHeight: 28, fontWeight: '600' },
  intro: { fontSize: 13, lineHeight: 20, marginTop: 2 },
  serverGroup: { borderBottomWidth: StyleSheet.hairlineWidth },
  serverChoice: { minHeight: 66, flexDirection: 'row', alignItems: 'center', gap: 12, paddingVertical: 10 },
  serverCopy: { flex: 1, minWidth: 0, gap: 2 },
  serverTitle: { fontSize: 16, lineHeight: 22, fontWeight: '600' },
  rowDetail: { fontSize: 12, lineHeight: 18 },
  expandedContent: { paddingLeft: 30, paddingBottom: 8 },
  inlineMessage: { paddingVertical: 8, gap: 4 },
  inlineError: { fontSize: 13, lineHeight: 19 },
  warning: { padding: 10, borderRadius: 10 },
  backendSection: { paddingTop: 8 },
  backendHeader: { minHeight: 30, flexDirection: 'row', alignItems: 'center', gap: 10 },
  backendTitle: { flex: 1, fontSize: 12, lineHeight: 18, textTransform: 'uppercase', letterSpacing: .5, fontWeight: '600' },
  retryAction: { minWidth: 44, minHeight: 44, justifyContent: 'center', alignItems: 'flex-end' },
  sessionRow: { borderBottomWidth: StyleSheet.hairlineWidth },
  sessionChoice: { minHeight: 56, paddingVertical: 8, flexDirection: 'row', alignItems: 'center', gap: 10 },
  sessionCopy: { flex: 1, minWidth: 0, gap: 2 },
  sessionTitleLine: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  sessionName: { flexShrink: 1, fontSize: 15, lineHeight: 21, fontWeight: '500' },
  auxLabel: { fontSize: 11, lineHeight: 16 },
  hintBadge: { borderWidth: StyleSheet.hairlineWidth, borderRadius: 6, paddingHorizontal: 6, paddingVertical: 2, fontSize: 10, lineHeight: 14, overflow: 'hidden' },
  checkSlot: { width: 20, height: 20 },
  refreshAction: { minHeight: 44, alignSelf: 'flex-start', justifyContent: 'center', paddingHorizontal: 8 },
  action: { fontSize: 14, fontWeight: '600' },
  footer: { borderTopWidth: StyleSheet.hairlineWidth, paddingTop: 8, marginTop: 8 },
  footerAction: { minHeight: 56, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', gap: 12 },
  footerCopy: { flex: 1 },
  footerTitle: { fontSize: 14, lineHeight: 20, fontWeight: '600' },
  disconnectAction: { minHeight: 56, flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
  nameInput: { minHeight: 50, borderWidth: 1, borderRadius: 10, paddingHorizontal: 14, fontSize: 16 },
});
