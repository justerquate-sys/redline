package app.lumen.tv

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.graphics.Bitmap
import androidx.core.content.FileProvider
import java.io.ByteArrayInputStream
import java.io.File
import java.lang.ref.WeakReference
import java.net.HttpURLConnection
import java.net.URL
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** The Rust side of the shell (`tv/shell/src-tauri/src/host.rs`). */
object Native {
  // The same library wry loads (a second load is a no-op); needed before Tauri touches it.
  init { System.loadLibrary("lumen_tv_lib") }
  @JvmStatic external fun init(filesDir: String, host: Host)
  /** The document-start script: the shim plus the newest verified client bundle. */
  @JvmStatic external fun script(): String
  /** The desktop user agent the client needs (www.youtube.com, not m.youtube.com). */
  @JvmStatic external fun userAgent(webViewDefault: String): String
  /** The client's rules for a request: 0 pass, 1 block, 2 fetch here and allow CORS, 3 a page the client renders
   *  (served [document] instead of YouTube's page), 4 the client's Wasm ([wasm]). */
  @JvmStatic external fun decide(url: String, mainFrame: Boolean, top: String): Int
  /** The document for a page the client renders: it stops YouTube's at the first byte and writes its own. */
  @JvmStatic external fun document(): String
  /** The client's Wasm (decide → 4: `/__lumen/client.wasm`): the page compiles it while it streams in. */
  @JvmStatic external fun wasm(): ByteArray
  /** The owner said yes to a shell update: download, verify, then [Host.installApk]. */
  @JvmStatic external fun acceptUpdate()
  /** An intent's action, URI and `query` extra ("" when absent): validated in Rust, kept for the page (`shell_events`). */
  @JvmStatic external fun incoming(action: String, data: String, query: String)
  /** The speech recognizer's answer: `state` is text, cancel, none, error or missing. */
  @JvmStatic external fun voiceResult(state: String, text: String)
}

/**
 * What Rust asks of Android: the Keystore, the bundle swap, the package installer and the speech recognizer.
 * Called from Rust threads; UI work hops to the main thread.
 */
class Host(activity: MainActivity) {
  private val ref = WeakReference(activity)

  // Refresh token sealing: AES-256-GCM with a key that never leaves the Android Keystore.
  private fun key(): SecretKey {
    val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
    val g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    g.init(
      KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .build()
    )
    return g.generateKey()
  }

  fun seal(plain: ByteArray): ByteArray? = try {
    val c = Cipher.getInstance("AES/GCM/NoPadding")
    c.init(Cipher.ENCRYPT_MODE, key())
    c.iv + c.doFinal(plain)
  } catch (e: Exception) { null }

  fun open(sealed: ByteArray): ByteArray? = try {
    val c = Cipher.getInstance("AES/GCM/NoPadding")
    c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, sealed, 0, 12))
    c.doFinal(sealed, 12, sealed.size - 12)
  } catch (e: Exception) { null }

  fun bundleReady() {
    val a = ref.get() ?: return
    a.runOnUiThread { a.swapBundle() }
  }

  /** The page's script changed (telemetry on/off; Rust → here): in place before this returns, so the page's
   *  reload right after the answer already gets it. */
  fun scriptChanged() {
    val a = ref.get() ?: return
    val done = java.util.concurrent.CountDownLatch(1)
    a.runOnUiThread { try { a.reinstallScript() } finally { done.countDown() } }
    done.await(2, java.util.concurrent.TimeUnit.SECONDS)
  }

  /** Android has a speech recognizer for us (the client shows its mic control only then). */
  fun voiceAvailable(): Boolean = ref.get()?.voiceAvailable() ?: false

  /** Opens the recognizer; its answer goes to [Native.voiceResult]. False when there is none. */
  fun voiceStart(): Boolean {
    val a = ref.get() ?: return false
    if (!a.voiceAvailable()) return false
    a.runOnUiThread { a.startVoice() }
    return true
  }

  /** A downloaded APK waiting for the owner to allow installs from this app. */
  @Volatile private var pendingApk: String? = null

  /** Back from Android's "Install unknown apps" screen: the install goes on if it was allowed. */
  fun resumeInstall() {
    val p = pendingApk ?: return
    val a = ref.get() ?: return
    if (a.packageManager.canRequestPackageInstalls()) {
      pendingApk = null
      installApk(p)
    }
  }

  /** Opens Android's installer for a downloaded, hash-checked APK. */
  fun installApk(path: String) {
    val a = ref.get() ?: return
    a.runOnUiThread {
      if (!a.packageManager.canRequestPackageInstalls()) {
        // First time: Android asks the owner to allow installs from this app (then resumeInstall).
        pendingApk = path
        a.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + a.packageName)))
        return@runOnUiThread
      }
      val uri = FileProvider.getUriForFile(a, a.packageName + ".fileprovider", File(path))
      a.startActivity(Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK))
    }
  }

  companion object { const val ALIAS = "ytc-account" }
}

/**
 * Wraps wry's WebViewClient: every request first goes through the client's rules (Rust). A blocked
 * request fails; a CORS one (googlevideo) is fetched here, redirects followed, with
 * `Access-Control-Allow-Origin: *` added, what the extension's declarativeNetRequest rule does.
 */
class Interceptor(private val inner: WebViewClient) : WebViewClient() {
  @Volatile private var top = "https://www.youtube.com/"
  private val doc by lazy { Native.document().toByteArray() }
  @Volatile private var hasVisitor = false
  private fun visitor(): Boolean {
    if (!hasVisitor) {
      val c = android.webkit.CookieManager.getInstance().getCookie("https://www.youtube.com") ?: ""
      hasVisitor = listOf("VISITOR_INFO1_LIVE=", "VISITOR_PRIVACY_METADATA=", "__Secure-YEC=").any { c.contains(it) }
    }
    return hasVisitor
  }

  init {
    // Keep-alive pool per host (Android's default is 5): video and audio, their mirror hedges, a
    // focus prefetch and the client's /generate_204 warm-up then reuse warm connections.
    System.setProperty("http.maxConnections", "16")
  }

  override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
    val url = request.url.toString()
    if (request.isForMainFrame) top = url
    if (url.startsWith("https://") || url.startsWith("http://")) {
      when (Native.decide(url, request.isForMainFrame, top)) {
        1 -> return WebResourceResponse("text/plain", "utf-8", 403, "Blocked", mapOf(), ByteArrayInputStream(ByteArray(0)))
        2 -> if (request.method == "GET") proxy(request)?.let { return it }
        // App start, reload: the client never reads YouTube's page (it stops it at the first byte), so the shell's
        // own document saves that round trip. Not before this WebView has YouTube's visitor cookies (the very first
        // start gets YouTube's page: its cookies, a consent step if YouTube asks for one).
        3 -> if (request.method == "GET" && visitor()) return WebResourceResponse("text/html", "utf-8", 200, "OK", mapOf("Cache-Control" to "no-store"), ByteArrayInputStream(doc))
        4 -> {
          val w = Native.wasm()
          return if (w.isEmpty()) WebResourceResponse("text/plain", "utf-8", 404, "Not Found", mapOf(), ByteArrayInputStream(ByteArray(0)))
          else WebResourceResponse("application/wasm", null, 200, "OK", mapOf("Cache-Control" to "no-store"), ByteArrayInputStream(w))
        }
      }
    }
    return inner.shouldInterceptRequest(view, request)
  }

  private fun proxy(req: WebResourceRequest): WebResourceResponse? = try {
    val c = URL(req.url.toString()).openConnection() as HttpURLConnection
    c.instanceFollowRedirects = true
    // This call blocks one of the WebView's request threads until the headers arrive, even after
    // the page gave the request up (a hedge won elsewhere): a hung host lets it go after 6 s to
    // connect / 15 s without a byte (the client hedges after 0.7 s and refreshes after 2.5 s).
    c.connectTimeout = 6000
    c.readTimeout = 15000
    for ((k, v) in req.requestHeaders) {
      if (!k.equals("Origin", true) && !k.equals("Accept-Encoding", true)) c.setRequestProperty(k, v)
    }
    val code = c.responseCode
    if (code in 300..399) {
      c.disconnect()
      null // e.g. a redirect to plain http: the WebView's own handling (and the client's fallback)
    } else {
      val headers = HashMap<String, String>()
      for ((k, v) in c.headerFields) {
        if (k != null && v.isNotEmpty() && !k.equals("Content-Encoding", true) && !k.equals("Access-Control-Allow-Origin", true)) headers[k] = v.joinToString(", ")
      }
      headers["Access-Control-Allow-Origin"] = "*"
      val body = (if (code >= 400) c.errorStream else c.inputStream) ?: ByteArrayInputStream(ByteArray(0))
      val mime = (c.contentType ?: "application/octet-stream").substringBefore(';').trim()
      WebResourceResponse(mime, null, code, c.responseMessage?.takeIf { it.isNotEmpty() } ?: "OK", headers, body)
    }
  } catch (e: Exception) { null }

  override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = inner.shouldOverrideUrlLoading(view, request)
  override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) = inner.onPageStarted(view, url, favicon)
  override fun onPageFinished(view: WebView, url: String) = inner.onPageFinished(view, url)
  override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) = inner.onReceivedError(view, request, error)
}
