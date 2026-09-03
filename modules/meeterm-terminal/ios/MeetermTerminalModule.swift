import ExpoModulesCore

public final class MeetermTerminalModule: Module {
  public func definition() -> ModuleDefinition {
    Name("MeetermTerminal")

    View(MeetermTerminalView.self) {
      Prop("terminalId", "poc-main") { (view: MeetermTerminalView, terminalId: String) in
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")
    }
  }
}
