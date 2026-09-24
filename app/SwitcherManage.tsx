import { KeyboardAvoidingView, Platform, ScrollView, StyleSheet, Text, View } from 'react-native';
import type { ReactNode } from 'react';
import { SafeAreaView, useSafeAreaInsets } from 'react-native-safe-area-context';
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
export function SwitcherManage({ profiles, selectedId, loading, error, busy, colors, form, notice, onBack, onClose, onRetry, onConnect, onAdd, onEdit, onDelete, onFormClose, onFormSubmit }: {
  profiles: ServerProfile[];
  selectedId: string;
  loading: boolean;
  error: boolean;
  busy: boolean;
  colors: Palette;
  form: { visible: boolean; profile?: ServerProfile };
  /** Shared control feedback (e.g. a failed delete) rendered like NativeSheet. */
  notice?: ReactNode;
  onBack: () => void;
  onClose: () => void;
  onRetry: () => void;
  onConnect: (profile: ServerProfile) => void;
  onAdd: () => void;
  onEdit: (profile: ServerProfile) => void;
  onDelete: (profile: ServerProfile) => void;
  onFormClose: () => void;
  onFormSubmit: (submission: ConnectionSubmission) => Promise<boolean>;
}) {
  const insets = useSafeAreaInsets();
  return <SafeAreaView edges={['top', 'bottom', 'left', 'right']} style={[styles.safeArea, { backgroundColor: colors.background }]}>
    <KeyboardAvoidingView style={styles.flex} behavior={Platform.OS === 'ios' ? 'padding' : 'height'}>
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        automaticallyAdjustKeyboardInsets
        keyboardShouldPersistTaps="handled"
        keyboardDismissMode="on-drag"
        contentContainerStyle={[styles.content, { paddingBottom: Math.max(insets.bottom, 24) }]}
      >
        <View style={styles.titleRow}>
          <IconButton icon="back" label="Back to session switcher" colors={colors} disabled={busy} onPress={onBack} />
          <View style={styles.flex}><Text accessibilityRole="header" style={[styles.heading, { color: colors.text }]}>Saved servers</Text><Text style={[styles.intro, { color: colors.muted }]}>Add, edit, or remove servers, or choose one to open.</Text></View>
          <IconButton icon="close" label="Close session switcher" colors={colors} disabled={busy} onPress={onClose} />
        </View>
        {notice}
        {form.visible ? <ConnectionForm
          visible
          embedded
          mode="save"
          initialProfile={form.profile}
          colors={colors}
          onClose={onFormClose}
          onSubmit={onFormSubmit}
        /> : <ProfileList
          profiles={profiles}
          selectedId={selectedId}
          loading={loading}
          error={error}
          busy={busy}
          colors={colors}
          onRetry={onRetry}
          onAdd={onAdd}
          onConnect={onConnect}
          onEdit={onEdit}
          onDelete={onDelete}
        />}
      </ScrollView>
    </KeyboardAvoidingView>
  </SafeAreaView>;
}

const styles = StyleSheet.create({
  flex: { flex: 1, minWidth: 0 },
  safeArea: { flex: 1 },
  content: { paddingHorizontal: 20, paddingTop: 8, gap: 12 },
  titleRow: { minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 12, paddingBottom: 8 },
  heading: { fontSize: 20, lineHeight: 28, fontWeight: '600' },
  intro: { fontSize: 13, lineHeight: 20, marginTop: 2 },
});
