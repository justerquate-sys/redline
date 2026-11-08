package app.lumen.tv

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.speech.RecognizerIntent
import android.util.Log
import android.window.OnBackInvokedDispatcher
import android.view.KeyEvent
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.webkit.ScriptHandler
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature

/**
 * The TV app's activity. Only what Android forces into Kotlin lives here (the decisions are Rust's,
 * `tv/shell/src-tauri/src`, reached through [Native]):
 * - the client bundle as a document-start script on https://www.youtube.com (kept as a handle so a
 *   verified update swaps it; the swap applies on the next page load or when the app comes back);
 * - the WebView's requests through the client's rules ([Interceptor]);
 * - the remote: Back asks the client first (YTC_BACK), media keys reach the page as key events, and so do the search
 *   and voice keys when the system delivers them (`BrowserSearch`; debug builds log every key, see tv/README.md);
 * - the speech recognizer (no RECORD_AUDIO needed: the system's recognizer listens, with the remote's mic) and the
 *   intents that bring a search or a link; both reach Rust as they are, and the page asks Rust ([wakePage]).
 */
class MainActivity : TauriActivity() {
  override val handleBackNavigation: Boolean = false
  private var web: WebView? = null
  private var script: ScriptHandler? = null
  /** The first document-start script, composed off the UI thread while Tauri and the WebView start (1.8 MB:
   *  composing it and making the Java string took 17–79 ms on the emulator, more on a TV, before the first load). */
  private var firstScript: java.util.concurrent.FutureTask<String>? = null
  private var reloadOnStart = false
  private var stoppedAt = 0L
  private lateinit var host: Host
  private val debuggable by lazy { (applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0 }
  private var listening = false
  private val recognizer = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
    voiceDone(r.resultCode, r.data)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Before Tauri starts the Rust side: where its files live and how it reaches Android.
    host = Host(this)
    Native.init(filesDir.absolutePath, host)
    firstScript = java.util.concurrent.FutureTask { Native.script() }.also { Thread(it, "lumen-script").start() }
    // A search or a link that started the app (kept for the page). Not when Android brings the activity back from a
    // saved state or from recents: the same intent would run again.
    if (savedInstanceState == null && (intent?.flags ?: 0) and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY == 0) incoming(intent)
    super.onCreate(savedInstanceState)
    // Back goes to the client first. Android 13+ delivers it as a back invocation, not a key, and the
    // WebView registers its own (history back): an overlay-priority callback comes before it.
    if (Build.VERSION.SDK_INT >= 33) {
      onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_OVERLAY) { back() }
    } else {
      onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
        override fun handleOnBackPressed() = back()
      })
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    incoming(intent)
    wakePage()
  }

  private fun incoming(i: Intent?) {
    val action = i?.action ?: return
    if (debuggable) Log.d(TAG, "intent: $action")
    if (action == Intent.ACTION_MAIN) return
    // (Any app can send this exported activity anything, a bad parcel too.)
    val query = try { i.getStringExtra("query") } catch (e: Exception) { null }
    Native.incoming(action, i.dataString ?: "", query ?: "")
  }

  /** Something waits for the page in the shell: it asks for it (`shell_events`). A page not there yet asks when it starts. */
  private fun wakePage() {
    web?.evaluateJavascript("document.dispatchEvent(new Event('ytc-wake'))", null)
  }

  private fun speechIntent() = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
    .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_WEB_SEARCH)
    .putExtra(RecognizerIntent.EXTRA_LANGUAGE, java.util.Locale.getDefault().toLanguageTag())
    .putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)

  /** The recognizer to open, or null when the TV has none (the manifest's `queries` lets this see it on Android 11+).
   *  Android TV images offer two (the Google app's and the text-to-speech engine's), and an open intent would stop at
   *  Android's "Complete action using" dialog: the Google app's, else any other than text-to-speech. */
  private fun recognizerIntent(): Intent? {
    val i = speechIntent()
    val all = packageManager.queryIntentActivities(i, PackageManager.MATCH_DEFAULT_ONLY).map { it.activityInfo }
    val pick = all.firstOrNull { it.packageName == "com.google.android.katniss" }
      ?: all.firstOrNull { it.packageName != "com.google.android.tts" }
      ?: all.firstOrNull()
      ?: return null
    return i.setClassName(pick.packageName, pick.name)
  }

  fun voiceAvailable(): Boolean = recognizerIntent() != null

  /** Rust → [Host.voiceStart]. */
  fun startVoice() {
    if (listening) return
    val i = recognizerIntent() ?: return voiceEnded("missing", "")
    listening = true
    if (debuggable) Log.d(TAG, "recognizer: ${i.component?.flattenToShortString()}")
    try {
      recognizer.launch(i)
    } catch (e: ActivityNotFoundException) {
      voiceEnded("missing", "")
    } catch (e: SecurityException) {
      Log.w(TAG, "recognizer refused: $e")
      voiceEnded("error", "")
    }
  }

  private fun voiceDone(code: Int, data: Intent?) {
    val text = try { data?.getStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS)?.firstOrNull()?.trim() } catch (e: Exception) { null }
    if (debuggable) Log.d(TAG, "recognizer: result code $code, ${if (text.isNullOrEmpty()) "no text" else "text"}")
    when {
      code == Activity.RESULT_OK && !text.isNullOrEmpty() -> voiceEnded("text", text)
      code == Activity.RESULT_OK || code == RecognizerIntent.RESULT_NO_MATCH -> voiceEnded("none", "")
      code == Activity.RESULT_CANCELED -> voiceEnded("cancel", "")
      else -> voiceEnded("error", "")
    }
  }

  private fun voiceEnded(state: String, text: String) {
    listening = false
    Native.voiceResult(state, text)
    web?.requestFocus()
    wakePage()
  }

  override fun onWindowFocusChanged(hasFocus: Boolean) {
    super.onWindowFocusChanged(hasFocus)
    // What the remote's mic does when no key reaches the app: the system's assistant takes the window (see tv/README.md).
    if (debuggable) Log.d(TAG, "window focus: $hasFocus")
  }

  override fun onWebViewCreate(webView: WebView) {
    web = webView
    webView.settings.userAgentString = Native.userAgent(webView.settings.userAgentString ?: "")
    webView.isFocusable = true
    webView.isFocusableInTouchMode = true
    installScript(webView)
    // Runs after wry has set its own client (same main-thread message), before the first load.
    webView.post { webView.webViewClient = Interceptor(webView.webViewClient) }
    webView.requestFocus()
  }

  private fun installScript(w: WebView) {
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) return
    script?.remove()
    val js = firstScript?.let { f -> firstScript = null; try { f.get() } catch (e: Exception) { null } } ?: Native.script()
    script = WebViewCompat.addDocumentStartJavaScript(w, js, setOf("https://www.youtube.com"))
  }

  /** The script changed for the next load (telemetry on/off: the page reloads itself; Rust → [Host.scriptChanged]). */
  fun reinstallScript() {
    web?.let { installScript(it) }
  }

  /** A newer verified bundle is in place (Rust → [Host.bundleReady]). */
  fun swapBundle() {
    val w = web ?: return
    installScript(w)
    reloadOnStart = true
  }

  override fun onStart() {
    super.onStart()
    // A swapped bundle goes live when the owner comes back to the app (never mid-video).
    if (reloadOnStart && stoppedAt > 0) {
      reloadOnStart = false
      web?.reload()
    }
  }

  override fun onResume() {
    super.onResume()
    if (debuggable) Log.d(TAG, "resumed")
    host.resumeInstall()
  }

  override fun onPause() {
    if (debuggable) Log.d(TAG, "paused")
    super.onPause()
  }

  override fun onStop() {
    stoppedAt = System.currentTimeMillis()
    // Leaving the app pauses playback (a TV app never plays on in the background).
    web?.evaluateJavascript("document.querySelectorAll('video,audio').forEach(m=>m.pause())", null)
    // YouTube's cookies on disk now (the WebView writes them out only now and then): the next start can serve the
    // shell's own document (Host.kt `visitor()`), and nothing is lost if Android ends the process.
    android.webkit.CookieManager.getInstance().flush()
    super.onStop()
  }

  private fun back() {
    val w = web ?: return finish()
    // "h" = the client handled it, "x" = the client asks to leave the app (Back on the rail at Home), "n" = not
    // handled: the WebView goes back, or the app leaves when there is nothing to go back to.
    w.evaluateJavascript("(()=>{try{const h=!!(globalThis.YTC_BACK&&YTC_BACK());return h?'h':globalThis.YTC_EXIT?'x':'n'}catch(e){return 'n'}})()") { r ->
      Log.i(TAG, "back: client answered $r")
      if (r == "\"h\"") return@evaluateJavascript
      if (r != "\"x\"" && w.canGoBack()) w.goBack() else moveTaskToBack(true)
    }
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    // Which keys does this TV's remote deliver? (A release build logs only the two voice keys below.)
    if (debuggable && event.action == KeyEvent.ACTION_DOWN && event.repeatCount == 0) {
      Log.d(TAG, "key ${event.keyCode} ${KeyEvent.keyCodeToString(event.keyCode)} scan ${event.scanCode} device ${event.deviceId}")
    }
    // Back is the client's first (YTC_BACK), before the WebView or any callback sees the key.
    if (event.keyCode == KeyEvent.KEYCODE_BACK && web != null) {
      if (event.action == KeyEvent.ACTION_UP && !event.isCanceled) back()
      return true
    }
    val name = when (event.keyCode) {
      KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_HEADSETHOOK -> "MediaPlayPause"
      KeyEvent.KEYCODE_MEDIA_PLAY -> "MediaPlay"
      KeyEvent.KEYCODE_MEDIA_PAUSE -> "MediaPause"
      KeyEvent.KEYCODE_MEDIA_REWIND -> "MediaRewind"
      KeyEvent.KEYCODE_MEDIA_FAST_FORWARD -> "MediaFastForward"
      KeyEvent.KEYCODE_MEDIA_NEXT -> "MediaTrackNext"
      KeyEvent.KEYCODE_MEDIA_PREVIOUS -> "MediaTrackPrevious"
      KeyEvent.KEYCODE_MEDIA_STOP -> "MediaStop"
      // The client opens the recognizer unless a panel is open. With no recognizer the system keeps these keys.
      KeyEvent.KEYCODE_SEARCH, KeyEvent.KEYCODE_VOICE_ASSIST -> {
        if (event.action == KeyEvent.ACTION_DOWN && event.repeatCount == 0) Log.i(TAG, "voice key ${KeyEvent.keyCodeToString(event.keyCode)}")
        if (voiceAvailable()) "BrowserSearch" else null
      }
      else -> null
    }
    val w = web
    if (name != null && w != null) {
      if (event.action == KeyEvent.ACTION_DOWN && event.repeatCount == 0) {
        w.evaluateJavascript("(()=>{const t=document.activeElement||document.body;t.dispatchEvent(new KeyboardEvent('keydown',{key:'$name',bubbles:true,cancelable:true}))})()", null)
      }
      return true
    }
    return super.dispatchKeyEvent(event)
  }

  private companion object { const val TAG = "LumenTV" }
}
