import { StatusBar, StyleSheet, View } from 'react-native';

import { TerminalView } from './modules/meeterm-terminal';

export default function App() {
  return (
    <View style={styles.container}>
      <StatusBar hidden />
      <TerminalView terminalId="poc-main" style={styles.terminal} />
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    backgroundColor: '#090b0f',
    flex: 1,
  },
  terminal: {
    backgroundColor: '#090b0f',
    flex: 1,
  },
});
