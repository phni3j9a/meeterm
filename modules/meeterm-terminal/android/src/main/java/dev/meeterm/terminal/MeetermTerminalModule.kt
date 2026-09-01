package dev.meeterm.terminal

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class MeetermTerminalModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("MeetermTerminal")

    View(MeetermTerminalView::class) {
      Prop("terminalId", "poc-main") { view: MeetermTerminalView, terminalId: String ->
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")

      OnViewDestroys { view: MeetermTerminalView ->
        view.releaseBindingForLifecycle()
      }
    }
  }
}
