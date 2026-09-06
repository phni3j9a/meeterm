package dev.meeterm.terminal

import android.content.Context
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.opengl.GLSurfaceView
import android.text.Editable
import android.text.InputType
import android.text.SpannableStringBuilder
import android.util.Log
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.LinearLayout
import android.widget.TextView
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.viewevent.EventDispatcher
import expo.modules.kotlin.views.ExpoView
import kotlin.math.max

/**
 * Native terminal surface exported through Expo Modules API.
 *
 * This class owns only the view and input bridge. The durable terminal state
 * lives in Rust and is retained by TerminalRegistry when this view is
 * recreated.
 */
class MeetermTerminalView(
  context: Context,
  appContext: AppContext,
) : ExpoView(context, appContext) {
  private companion object {
    const val TAG = "MeetermTerminalView"
    const val DEFAULT_TERMINAL_ID = "poc-main"
    const val DEFAULT_COLUMNS = 80
    const val DEFAULT_ROWS = 24
    const val REVISION_POLL_INTERVAL_MS = 33L
  }

  private val surface: GLSurfaceView = GLSurfaceView(context)
  private val content: LinearLayout = LinearLayout(context)
  private val renderer = TerminalRenderer(context)
  private lateinit var specialKeyRow: LinearLayout
  private var terminalId: String = DEFAULT_TERMINAL_ID
  private var inputGeneration = 0L
  @Volatile private var terminalHandle: Long = 0L
  private var lastColumns = 0
  private var lastRows = 0
  private var attached = false
  private var occludedInsetBottom = 0
  private var systemInsetLeft = 0
  private var systemInsetRight = 0
  private var lastTerminalRevision = -1L
  private val editable = SpannableStringBuilder()
  // Keep one post-resize draw after EGL settles. This is separate from the
  // revision poll: it repairs a surface timing race even when terminal
  // content did not change.
  private val settledFrameRequest = Runnable {
    if (attached && terminalHandle != 0L && windowVisibility == View.VISIBLE) {
      surface.requestRender()
    }
  }
  private val revisionPoll = object : Runnable {
    override fun run() {
      if (!attached || terminalHandle == 0L || windowVisibility != View.VISIBLE) {
        return
      }

      val revision = MeetermNative.terminalRevision(terminalHandle)
      if (revision != lastTerminalRevision) {
        lastTerminalRevision = revision
        // Snapshot bytes are pulled by the renderer only after this native
        // revision check reports a change. JavaScript never participates.
        surface.requestRender()
      }
      postDelayed(this, REVISION_POLL_INTERVAL_MS)
    }
  }

  private val inputSession = InputSession(
    sink = RustInputSink { terminalHandle },
    onPreeditChanged = { value ->
      renderer.setPreedit(value)
      surface.requestRender()
    },
  )

  private val onNativeReady by EventDispatcher<Map<String, Any>>()
  private val onMetrics by EventDispatcher<Map<String, Any>>()

  init {
    setBackgroundColor(Color.rgb(9, 11, 15))
    isFocusable = true
    isFocusableInTouchMode = true
    descendantFocusability = ViewGroup.FOCUS_BEFORE_DESCENDANTS

    content.orientation = LinearLayout.VERTICAL
    content.setBackgroundColor(Color.rgb(9, 11, 15))
    addView(
      content,
      ViewGroup.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.MATCH_PARENT,
      ),
  )

    surface.setEGLContextClientVersion(2)
    surface.setRenderer(renderer)
    surface.renderMode = GLSurfaceView.RENDERMODE_WHEN_DIRTY
    surface.setPreserveEGLContextOnPause(true)
    content.addView(
      surface,
      LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        0,
        1f,
      ),
    )
    setOnApplyWindowInsetsListener { _, insets ->
      val (leftInset, rightInset, bottomInset) = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        val ime = insets.getInsets(WindowInsets.Type.ime())
        val systemBars = insets.getInsets(WindowInsets.Type.systemBars())
        Triple(systemBars.left, systemBars.right, max(ime.bottom, systemBars.bottom))
      } else {
        @Suppress("DEPRECATION")
        Triple(
          insets.systemWindowInsetLeft,
          insets.systemWindowInsetRight,
          max(insets.systemWindowInsetBottom, insets.stableInsetBottom),
        )
      }
      if (systemInsetLeft != leftInset ||
        systemInsetRight != rightInset ||
        occludedInsetBottom != bottomInset
      ) {
        systemInsetLeft = leftInset
        systemInsetRight = rightInset
        occludedInsetBottom = bottomInset
        Log.i(TAG, "window insets left=$leftInset right=$rightInset bottom=$bottomInset")
        layoutTerminalChildren()
      }
      insets
    }
    surface.addOnLayoutChangeListener { _, left, top, right, bottom, _, _, _, _ ->
      val surfaceWidth = right - left
      val surfaceHeight = bottom - top
      renderer.updateSurfaceSize(surfaceWidth, surfaceHeight)
      reconcileResize(surfaceWidth, surfaceHeight)
      if (attached && windowVisibility == View.VISIBLE) {
        surface.requestRender()
      }
      scheduleSettledFrame()
    }

    specialKeyRow = createSpecialKeyRow(context)
    content.addView(specialKeyRow, LinearLayout.LayoutParams(
      ViewGroup.LayoutParams.MATCH_PARENT,
      dp(48),
    ))
  }

  fun bindTerminal(value: String?) {
    val nextId = value?.takeIf { it.isNotBlank() } ?: DEFAULT_TERMINAL_ID
    if (nextId == terminalId && terminalHandle != 0L) return

    releaseBinding()
    terminalId = nextId
    terminalHandle = TerminalRegistry.acquire(nextId, DEFAULT_COLUMNS, DEFAULT_ROWS)
    Log.i(TAG, "bound terminalId=$terminalId handle=$terminalHandle")
    renderer.attachTerminal(terminalHandle)
    (context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager)?.restartInput(this)
    lastTerminalRevision = MeetermNative.terminalRevision(terminalHandle)
    post {
      emitReady()
      reconcileResize(surface.width, surface.height)
      surface.requestRender()
      startRevisionPolling()
      scheduleSettledFrame()
    }
  }

  override fun onAttachedToWindow() {
    super.onAttachedToWindow()
    attached = true
    Log.i(TAG, "attached")
    if (terminalHandle == 0L) {
      bindTerminal(terminalId)
    } else {
      post {
        emitReady()
        reconcileResize(surface.width, surface.height)
        surface.requestRender()
        startRevisionPolling()
        scheduleSettledFrame()
      }
    }
    surface.onResume()
  }

  override fun onDetachedFromWindow() {
    Log.i(TAG, "detached")
    attached = false
    stopRevisionPolling()
    removeCallbacks(settledFrameRequest)
    surface.onPause()
    renderer.attachTerminal(0L)
    releaseBinding()
    super.onDetachedFromWindow()
  }

  override fun onWindowVisibilityChanged(visibility: Int) {
    super.onWindowVisibilityChanged(visibility)
    if (!attached) return
    if (visibility == View.VISIBLE) {
      surface.onResume()
      surface.requestRender()
      startRevisionPolling()
      scheduleSettledFrame()
    } else {
      stopRevisionPolling()
      removeCallbacks(settledFrameRequest)
      surface.onPause()
    }
  }

  override fun onSizeChanged(width: Int, height: Int, oldWidth: Int, oldHeight: Int) {
    super.onSizeChanged(width, height, oldWidth, oldHeight)
    layoutTerminalChildren()
  }

  override fun onLayout(changed: Boolean, left: Int, top: Int, right: Int, bottom: Int) {
    super.onLayout(changed, left, top, right, bottom)
    content.layout(0, 0, width, height)
    layoutTerminalChildren()
  }

  private fun layoutTerminalChildren() {
    if (width <= 0 || height <= 0 || !::specialKeyRow.isInitialized) return
    val desiredHeight = max(renderer.cellHeightPx, height - dp(48) - occludedInsetBottom)
    val contentLeft = systemInsetLeft
    val contentRight = max(contentLeft + renderer.cellWidthPx, width - systemInsetRight)
    surface.layout(contentLeft, 0, contentRight, desiredHeight)
    specialKeyRow.layout(contentLeft, desiredHeight, contentRight, desiredHeight + dp(48))
  }

  private fun startRevisionPolling() {
    removeCallbacks(revisionPoll)
    if (attached && terminalHandle != 0L && windowVisibility == View.VISIBLE) {
      postDelayed(revisionPoll, REVISION_POLL_INTERVAL_MS)
    }
  }

  private fun stopRevisionPolling() {
    removeCallbacks(revisionPoll)
  }

  private fun scheduleSettledFrame() {
    removeCallbacks(settledFrameRequest)
    if (attached && terminalHandle != 0L && windowVisibility == View.VISIBLE) {
      postDelayed(settledFrameRequest, 500L)
    }
  }

  override fun dispatchTouchEvent(event: MotionEvent): Boolean {
    if (event.actionMasked == MotionEvent.ACTION_DOWN) {
      if (event.y < surface.bottom) {
        requestFocusFromTouch()
        post {
          val inputManager = context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
          inputManager?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        }
      }
    }
    return super.dispatchTouchEvent(event)
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    val handled = inputSession.handleKeyEvent(event.action, event.keyCode, event.unicodeChar)
    if (handled) {
      if (event.action != KeyEvent.ACTION_UP) surface.requestRender()
      return true
    }
    return super.dispatchKeyEvent(event)
  }

  override fun onCheckIsTextEditor(): Boolean = true

  override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
    outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
    outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE or EditorInfo.IME_FLAG_NO_EXTRACT_UI
    outAttrs.initialSelStart = 0
    outAttrs.initialSelEnd = 0

    val generation = inputGeneration
    return object : BaseInputConnection(this@MeetermTerminalView, true) {
      override fun getEditable(): Editable = this@MeetermTerminalView.editable

      override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        if (generation != inputGeneration) return false
        val result = super.setComposingText(text ?: "", newCursorPosition)
        inputSession.setComposingText(text)
        surface.requestRender()
        return result
      }

      override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        if (generation != inputGeneration) return false
        val editorResult = super.commitText(text ?: "", newCursorPosition)
        val result = inputSession.commitText(text)
        if (result) surface.requestRender()
        return editorResult && result
      }

      override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        if (generation != inputGeneration) return false
        val result = inputSession.deleteSurroundingText(beforeLength, afterLength)
        if (result) surface.requestRender()
        return result
      }

      override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int): Boolean {
        return deleteSurroundingText(beforeLength, afterLength)
      }

      override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (generation != inputGeneration) return false
        val result = inputSession.handleKeyEvent(event.action, event.keyCode, event.unicodeChar)
        if (result && event.action != KeyEvent.ACTION_UP) surface.requestRender()
        return result
      }

      override fun setComposingRegion(start: Int, end: Int): Boolean = true

      override fun finishComposingText(): Boolean {
        if (generation != inputGeneration) return false
        val editorResult = super.finishComposingText()
        val result = inputSession.finishComposingText()
        if (result) surface.requestRender()
        return editorResult && result
      }
    }
  }

  private fun reconcileResize(width: Int, height: Int) {
    val handle = terminalHandle
    val cellWidth = renderer.cellWidthPx
    val cellHeight = renderer.cellHeightPx
    if (handle == 0L || width <= 0 || height <= 0 || cellWidth <= 0 || cellHeight <= 0) return

    val columns = max(2, width / cellWidth)
    val rows = max(1, height / cellHeight)
    if (columns == lastColumns && rows == lastRows) return

    if (MeetermNative.resize(handle, columns, rows) == 0) {
      lastColumns = columns
      lastRows = rows
      emitMetrics(columns, rows, cellWidth, cellHeight)
      Log.i(TAG, "resized columns=$columns rows=$rows cell=${cellWidth}x$cellHeight")
      surface.requestRender()
    }
  }

  private fun emitReady() {
    if (!attached || terminalHandle == 0L) return
    Log.i(TAG, "ready terminalId=$terminalId handle=$terminalHandle")
    Log.i(TAG, "MEETERM_SMOKE_NATIVE_READY")
    onNativeReady(
      mapOf(
        "terminalId" to terminalId,
        "native" to true,
      ),
    )
  }

  private fun emitMetrics(columns: Int, rows: Int, cellWidth: Int, cellHeight: Int) {
    if (!attached) return
    onMetrics(
      mapOf(
        "terminalId" to terminalId,
        "columns" to columns,
        "rows" to rows,
        "cellWidthPx" to cellWidth,
        "cellHeightPx" to cellHeight,
      ),
    )
  }

  private fun createSpecialKeyRow(context: Context): LinearLayout {
    val row = LinearLayout(context).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = android.view.Gravity.CENTER_VERTICAL
      setBackgroundColor(Color.rgb(16, 20, 27))
      importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
    }
    listOf(
      "Esc" to TerminalSpecialKey.Escape,
      "Tab" to TerminalSpecialKey.Tab,
      "↑" to TerminalSpecialKey.Up,
      "↓" to TerminalSpecialKey.Down,
      "←" to TerminalSpecialKey.Left,
      "→" to TerminalSpecialKey.Right,
    ).forEach { (label, key) ->
      val button = TextView(context).apply {
        text = label
        textSize = 12f
        gravity = android.view.Gravity.CENTER
        minHeight = dp(44)
        minimumHeight = dp(44)
        minWidth = 0
        minimumWidth = 0
        setPadding(0, 0, 0, 0)
        setTextColor(Color.rgb(218, 224, 234))
        background = GradientDrawable().apply {
          setColor(Color.rgb(29, 35, 46))
          cornerRadius = dp(5).toFloat()
        }
        isClickable = true
        isFocusable = true
        contentDescription = label
        setOnClickListener {
          requestFocusFromTouch()
          if (inputSession.sendSpecial(key)) surface.requestRender()
        }
      }
      row.addView(button, LinearLayout.LayoutParams(0, dp(44), 1f).apply {
        marginStart = dp(1)
        marginEnd = dp(1)
      })
    }
    return row
  }

  private fun dp(value: Int): Int =
    (value * resources.displayMetrics.density).toInt().coerceAtLeast(value)

  private fun releaseBinding() {
    inputGeneration += 1
    inputSession.setComposingText("")
    editable.clear()
    if (terminalHandle == 0L) return
    TerminalRegistry.release(terminalId, terminalHandle)
    terminalHandle = 0L
    lastTerminalRevision = -1L
    lastColumns = 0
    lastRows = 0
  }

  internal fun releaseBindingForLifecycle() {
    renderer.attachTerminal(0L)
    releaseBinding()
  }

  val cellWidthPx: Int
    get() = renderer.cellWidthPx

  val cellHeightPx: Int
    get() = renderer.cellHeightPx
}
