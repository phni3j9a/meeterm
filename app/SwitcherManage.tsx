import { Alert, StyleSheet, Text, View } from 'react-native';
import type { FlatList } from 'react-native';
import type { ReactNode } from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { SafeAreaView } from 'react-native-safe-area-context';
import { useNavigation, usePreventRemove } from '@react-navigation/native';
import type { ServerProfile } from '../modules/meeterm-terminal';
import type { ConnectionSubmission } from './ConnectionForm';
import { ConnectionForm } from './ConnectionForm';
import { ProfileList } from './DailyUse';
import { IconButton } from './ui';
import type { Palette } from './ui';

/** Server management rendered inside the still-presented session switcher.
 * Stacking a RN Modal over the iOS formSheet route collapses the sheet, so the
 * footer Manage servers action swaps this panel into the sheet instead of
 * presenting another surface on top of it. */
export function SwitcherManage({ profiles, selectedId, loading, error, busy, colors, form, notice, noticeKey = '', onBack, onClose, onRetry, onConnect, onAdd, onEdit, onDelete, onFormClose, onFormSubmit, onFormGuardedChange }: {
  profiles: ServerProfile[];
  selectedId: string;
  loading: boolean;
  error: boolean;
  busy: boolean;
  colors: Palette;
  form: { visible: boolean; profile?: ServerProfile };
  /** Shared control feedback (e.g. a failed delete) rendered like NativeSheet. */
  notice?: ReactNode;
  /** Identifies the current notice; a new value scrolls it into view. */
  noticeKey?: string;
  onBack: () => void;
  onClose: () => void;
  onRetry: () => void;
  onConnect: (profile: ServerProfile) => void;
  onAdd: () => void;
  onEdit: (profile: ServerProfile) => void;
  onDelete: (profile: ServerProfile) => void;
  onFormClose: () => void;
  onFormSubmit: (submission: ConnectionSubmission) => Promise<boolean>;
  /** Forwards the save form's guarded state so the sheet can gate dismissal. */
  onFormGuardedChange: (guarded: boolean) => void;
}) {
  const [formGuard, setFormGuard] = useState({ busy: false, dirty: false });
  const navigation = useNavigation();
  const listRef = useRef<FlatList<ServerProfile>>(null);

  // The notice lives inside the list header, so a fresh message must be
  // scrolled into view for users who are partway down a longer list.
  useEffect(() => {
    if (noticeKey) listRef.current?.scrollToOffset({ offset: 0, animated: true });
  }, [noticeKey]);
  const formGuardedChange = useCallback(({ busy, dirty }: { busy: boolean; dirty: boolean }) => {
    setFormGuard({ busy, dirty });
    onFormGuardedChange(busy || dirty);
  }, [onFormGuardedChange]);

  // The swipe gesture is gated by the route's gestureEnabled flag; hardware
  // back and programmatic pops reach here instead. A submitting form blocks
  // removal outright — the in-flight save is never offered as discardable —
  // while unsaved edits ask before they are destroyed with the sheet.
  usePreventRemove(form.visible && (formGuard.busy || formGuard.dirty), ({ data }) => {
    if (formGuard.busy) return;
    Alert.alert('Discard changes?', 'Your changes have not been saved.', [
      { text: 'Keep editing', style: 'cancel' },
      { text: 'Discard', style: 'destructive', onPress: () => navigation.dispatch(data.action) },
    ]);
  });

  if (form.visible) {
    // The save form owns its header, ScrollView, and keyboard handling —
    // nesting another scroll/keyboard container splits that ownership.
    return <ConnectionForm
      visible
      embedded
      mode="save"
      initialProfile={form.profile}
      colors={colors}
      onClose={onFormClose}
      onSubmit={onFormSubmit}
      onGuardedChange={formGuardedChange}
    />;
  }

  // The list is the panel's only child and owns all vertical scrolling —
  // including the title row. react-native-screens only corrects the Fabric
  // (0,0) scroll-view placement for direct children of the formSheet's
  // content wrapper, so a fixed sibling header gets painted over.
  return <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.safeArea, { backgroundColor: colors.background }]}>
    <ProfileList
      profiles={profiles}
      selectedId={selectedId}
      loading={loading}
      error={error}
      busy={busy}
      colors={colors}
      listRef={listRef}
      header={<>
        <View style={styles.titleRow}>
          <IconButton icon="back" label="Back to session switcher" colors={colors} disabled={busy} onPress={onBack} />
          <View style={styles.flex}><Text accessibilityRole="header" style={[styles.heading, { color: colors.text }]}>Saved servers</Text><Text style={[styles.intro, { color: colors.muted }]}>Add, edit, or remove servers, or choose one to open.</Text></View>
          <IconButton icon="close" label="Close session switcher" colors={colors} disabled={busy} onPress={onClose} />
        </View>
        {notice}
      </>}
      onRetry={onRetry}
      onAdd={onAdd}
      onConnect={onConnect}
      onEdit={onEdit}
      onDelete={onDelete}
    />
  </SafeAreaView>;
}

const styles = StyleSheet.create({
  flex: { flex: 1, minWidth: 0 },
  safeArea: { flex: 1 },
  titleRow: { minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 12, paddingTop: 8, paddingBottom: 8 },
  heading: { fontSize: 20, lineHeight: 28, fontWeight: '600' },
  intro: { fontSize: 13, lineHeight: 20, marginTop: 2 },
});
