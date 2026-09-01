// Re-export the native module. On web, it will be resolved to MeetermTerminalModule.web.ts
// and on native platforms to MeetermTerminalModule.ts
export { default } from './src/MeetermTerminalModule';
export { default as MeetermTerminalView } from './src/MeetermTerminalView';
export { default as TerminalView } from './src/MeetermTerminalView';
export * from './src/MeetermTerminal.types';
