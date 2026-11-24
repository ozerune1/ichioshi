package app.ichioshi

import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.res.Configuration
import android.media.AudioManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.SurfaceHolder
import android.view.View
import android.view.WindowManager
import android.view.accessibility.AccessibilityManager
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import app.tauri.plugin.JSObject
import dev.jdtech.mpv.MPVLib
import java.io.File
import java.util.Locale
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.abs
import kotlin.math.max

/**
 * The app's player: libmpv (mpv's engine: every codec, ASS subtitles as
 * they're meant to look, chapters), full screen, with its controls: play
 * and pause, seeking, audio and subtitles, skipping openings and endings,
 * the next episode. It says how far it got when it closes.
 */
class PlayerActivity : AppCompatActivity(), MPVLib.EventObserver, SurfaceHolder.Callback,
  OverlayActions, TapLayer.Host, Timeline.Listener, TrackControl {
  companion object {
    /** The player open now (the watch party asks it, and tells it). */
    @Volatile
    var current: PlayerActivity? = null

    private const val TAG = "Ichioshi"
  }

  private lateinit var mpv: MPVLib
  /** mpv is set up (and not destroyed yet). */
  @Volatile private var mpvReady = false
  private val main = Handler(Looper.getMainLooper())
  /** Where the player tells mpv things: mpv can be busy for a second, and the controls mustn't wait. */
  private val worker: ExecutorService = Executors.newSingleThreadExecutor()
  private lateinit var kit: Kit
  private lateinit var ui: PlayerOverlay
  private var prefs: SharedPreferences? = null
  private var access: AccessibilityManager? = null

  // Read by state(), off the main thread.
  @Volatile private var position = 0.0
  @Volatile private var duration = 0.0
  @Volatile private var paused = false
  @Volatile private var ended = false
  /** What you asked the host to do (pause, play), not yet passed on. */
  @Volatile private var asked: String? = null
  @Volatile private var closed = false
  /** mpv has the file open (time-pos counts). */
  @Volatile private var loaded = false
  /**
   * Not playing though mpv isn't paused: the error card is up, or it's
   * reconnecting or loading again. The watch party hears "paused", so a
   * host's party waits instead of being moved back again and again.
   */
  @Volatile private var stalled = false

  /** Following a watch party's host: only the host controls playback. */
  private var member = false
  private var hasNext = false
  private var hasPrev = false
  private var skipOpenings = false
  private var skipEndings = false
  private var url = ""
  /** It's streamed (http or https), not a file on the phone. */
  private var stream = false
  private var start = 0.0
  /** Where the file being loaded starts (the start, or where a retry picks up). */
  private var loadAt = 0.0
  private var fileStarted = false
  /** The main thread's view of [loaded]: the file loaded, and its views are set up. */
  private var ready = false
  private var everLoaded = false

  private var chapters = Chapters(emptyList(), 0.0)
  private var tracks = listOf<Track>()
  private var curChapter = -1
  private var chapterEnteredAt = 0L
  /** Openings and endings that won't auto-skip (again). */
  private val skipped = mutableSetOf<Int>()
  private var lastUserSeekAt = -100_000L
  /** The chapter counting down to an auto-skip (-1: none). */
  private var countdownChapter = -1
  private var momentNow: Moment? = null
  private var momentLabel: String? = null

  private var pausedForCache = false
  private var seeking = false
  private var seekingSince = 0L
  private var cacheSince = 0L
  private var bufferPct = 0
  private var lastSubPos = 100
  private var fill = false
  private var talkback = false
  /** The text size and display density the views were made for. */
  private var fontScale = 1f
  private var densityDpi = 0
  private var touchingControl = false
  /** The error card is up; [cardError] is the result's error (null once the episode played). */
  private var cardUp = false
  private var cardError: String? = null
  private var reconnecting = false
  private var lastAutoRetryAt = -100_000L
  private var loadedAt = 0L
  private var resumedAt = 0L
  private var lastHostPauseMsg = -100_000L
  private var lastHostJumpMsg = -100_000L
  private var lastMemberMsg = -100_000L
  private var askBlockedUntil = 0L
  private var hintChecked = false
  private var resumeShown = false
  private var restartSeen = false
  /** The picture margins mpv has now (right, top, bottom), for the panel. */
  private var margins: Triple<Double, Double, Double>? = null
  private val refreshPending = AtomicBoolean(false)
  private var lastRefresh = 0L

  // A drag on a file on the phone seeks as it goes, one seek at a time.
  private var liveInFlight = false
  private var liveSent = -1.0
  private var liveWanted = -1.0
  /** The end came during a drag: it waits for the drag's seek. */
  private var endDuringDrag = false
  /**
   * Where a drag that seeked as it went was let go (or put back), until its
   * seeks have landed (-1: none). A chapter its earlier seeks report
   * meanwhile was only passed through.
   */
  private var settleAt = -1.0

  private val now get() = SystemClock.uptimeMillis()

  private fun log(msg: String) = Log.i(TAG, "player: $msg")

  private val hideControls = Runnable { if (canAutoHide()) setControls(false) }
  private val refreshTick = Runnable {
    refreshPending.set(false)
    lastRefresh = now
    refresh()
  }
  private val momentTick = Runnable { updateMoment() }
  private val busyTick = Runnable { updateBusy() }
  private val liveTimeout = Runnable {
    liveInFlight = false
    flushLive()
  }
  private val settled = Runnable { settleAt = -1.0 }
  /** After a drag that met the end: mpv still at the end means it's the end. */
  private val endCheck = Runnable {
    mpvDo { m ->
      if (m.getPropertyBoolean("eof-reached") == true) main.post { if (!closed && loaded) eofReached() }
    }
  }
  private val loadingCaption = Runnable { if (ui.mode == Mode.LOADING && !reconnecting) ui.setLoading("Loading") }
  private val talkbackListener = AccessibilityManager.TouchExplorationStateChangeListener { on ->
    talkback = on
    syncTaps()
    if (on) setControls(true) else scheduleHide()
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    if (Build.VERSION.SDK_INT < 35) {
      window.attributes = window.attributes.apply {
        layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
      }
    }
    WindowCompat.setDecorFitsSystemWindows(window, false)
    hideBars()
    volumeControlStream = AudioManager.STREAM_MUSIC
    if (Build.VERSION.SDK_INT >= 34) {
      overrideActivityTransition(OVERRIDE_TRANSITION_CLOSE, 0, android.R.anim.fade_out)
    }

    member = intent.getStringExtra("party") == "member"
    skipOpenings = intent.getBooleanExtra("skipOpenings", false) && !member
    skipEndings = intent.getBooleanExtra("skipEndings", false) && !member
    hasNext = intent.getBooleanExtra("hasNext", false)
    hasPrev = intent.getBooleanExtra("hasPrev", false)
    url = intent.getStringExtra("url") ?: ""
    stream = url.startsWith("http://", true) || url.startsWith("https://", true)
    start = intent.getDoubleExtra("start", 0.0)
    loadAt = start
    fontScale = resources.configuration.fontScale
    densityDpi = resources.configuration.densityDpi
    prefs = try { getSharedPreferences("player", Context.MODE_PRIVATE) } catch (e: Exception) { null }
    fill = pref { it.getBoolean("fill", false) } ?: false
    val subScale = (pref { it.getInt("subScale", 100) } ?: 100).coerceIn(80, 150)
    current = this

    kit = Kit(this)
    ui = PlayerOverlay(
      this, kit, this, member, hasPrev, hasNext,
      intent.getStringExtra("title") ?: "",
      episodeLine(intent.getStringExtra("name") ?: "", intent.getIntExtra("episode", 0)),
      this,
    )
    ui.timeline.listener = this
    ui.taps.host = this
    ui.panel.onClose = { closePanel() }
    ui.panel.setScale(subScale, tell = false)
    ui.moments.onPress = { momentPressed() }
    ui.moments.onWatch = { watchPressed() }
    ui.moments.onDone = { countdownDone() }
    ui.setFillIcon(fill)
    ui.setPaused(intent.getBooleanExtra("paused", false))
    setContentView(ui.root)
    ui.surface.holder.addCallback(this)
    ViewCompat.setOnApplyWindowInsetsListener(ui.root) { _, insets ->
      val cut = insets.getInsets(WindowInsetsCompat.Type.displayCutout())
      val mand = insets.getInsets(WindowInsetsCompat.Type.mandatorySystemGestures())
      ui.setPads(
        max(kit.px(16), cut.left + kit.px(8)),
        max(kit.px(16), cut.right + kit.px(8)),
        max(kit.px(8), cut.top),
        maxOf(kit.px(12), cut.bottom + kit.px(8), mand.bottom - kit.px(12)),
        mand.bottom,
      )
      // The panel moved (the cutout changed sides): so does the picture beside it.
      if (ui.panelOpen) applyMargins()
      insets
    }
    ui.root.addOnLayoutChangeListener { _, l, t, r, b, oldL, oldT, oldR, oldB ->
      if (r - l != oldR - oldL || b - t != oldB - oldT) {
        ui.root.post { relayout() }
      }
    }
    access = getSystemService(AccessibilityManager::class.java)
    talkback = access?.isTouchExplorationEnabled == true
    access?.addTouchExplorationStateChangeListener(talkbackListener)
    main.postDelayed(loadingCaption, 2000)

    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        when {
          ui.panelOpen -> closePanel()
          cardUp -> cardClose()
          else -> close(null)
        }
      }
    })

    mpv = MPVLib.create(this) ?: run {
      close(null, "The player couldn't start")
      return
    }
    mpv.setOptionString("vo", "gpu")
    mpv.setOptionString("gpu-context", "android")
    mpv.setOptionString("opengl-es", "yes")
    mpv.setOptionString("hwdec", "mediacodec,mediacodec-copy")
    mpv.setOptionString("hwdec-codecs", "all")
    mpv.setOptionString("ao", "audiotrack,opensles")
    mpv.setOptionString("force-window", "no")
    mpv.setOptionString("keep-open", "yes")
    // The end doesn't pause: a stream that drops plays on once it reconnects
    // (and a host's drop doesn't pause the party). A file's end closes the player.
    mpv.setOptionString("keep-open-pause", "no")
    mpv.setOptionString("save-position-on-quit", "no")
    mpv.setOptionString("input-default-bindings", "no")
    // Direct urls only. When a url fails, mpv's ytdl hook would start yt-dlp,
    // and on Android that subprocess's failure aborts the whole app (fdsan).
    mpv.setOptionString("ytdl", "no")
    mpv.setOptionString("cache", "yes")
    mpv.setOptionString("demuxer-max-bytes", "64MiB")
    // Servers' certificates: checked against the phone's own CAs.
    caFile()?.let {
      mpv.setOptionString("tls-ca-file", it.path)
      mpv.setOptionString("tls-verify", "yes")
    }
    // The audio and subtitles you prefer (mpv picks the tracks).
    intent.getStringExtra("alang")?.takeIf { it.isNotEmpty() }?.let { mpv.setOptionString("alang", it) }
    intent.getStringExtra("slang")?.takeIf { it.isNotEmpty() }?.let { mpv.setOptionString("slang", it) }
    if (intent.getStringExtra("slang").isNullOrEmpty()) mpv.setOptionString("sid", "no")
    if (start > 0) mpv.setOptionString("start", fmt(start))
    if (intent.getBooleanExtra("paused", false)) mpv.setOptionString("pause", "yes")
    // Fill or Fit, and the subtitle size: as last time.
    mpv.setOptionString("panscan", if (fill) "1.0" else "0.0")
    mpv.setOptionString("sub-ass-force-margins", if (fill) "yes" else "no")
    mpv.setOptionString("sub-scale", (subScale / 100.0).toString())
    log("sub-scale ${subScale / 100.0}")
    mpv.init()
    mpvReady = true
    mpv.addObserver(this)
    mpv.observeProperty("time-pos", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    mpv.observeProperty("duration", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    mpv.observeProperty("pause", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("eof-reached", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("paused-for-cache", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("seeking", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("chapter", MPVLib.MpvFormat.MPV_FORMAT_INT64)
    mpv.observeProperty("chapter-list", MPVLib.MpvFormat.MPV_FORMAT_NONE)
    mpv.observeProperty("track-list", MPVLib.MpvFormat.MPV_FORMAT_NONE)
    mpv.observeProperty("sid", MPVLib.MpvFormat.MPV_FORMAT_STRING)
    mpv.observeProperty("aid", MPVLib.MpvFormat.MPV_FORMAT_STRING)
    mpv.observeProperty("cache-buffering-state", MPVLib.MpvFormat.MPV_FORMAT_INT64)
    if (stream) mpv.observeProperty("demuxer-cache-time", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    mpv.observeProperty("dwidth", MPVLib.MpvFormat.MPV_FORMAT_INT64)
    mpv.observeProperty("dheight", MPVLib.MpvFormat.MPV_FORMAT_INT64)
  }

  /** "Episode 2 · Name", or the name alone when it says as much. */
  private fun episodeLine(name: String, episode: Int): String = when {
    name.isBlank() -> "Episode $episode"
    name == "Episode $episode" || episode <= 0 -> name
    else -> "Episode $episode · $name"
  }

  /** Everything in place again for the window's size, and the picture beside the panel. */
  private fun relayout() {
    if (closed) return
    // The time labels' widths, for the text size now.
    if (duration > 0) ui.setDuration(duration)
    ui.layout()
    if (ui.panelOpen) applyMargins()
    pictureChanged()
  }

  /**
   * The display size, the text size, the language or the keys changed: the
   * player stays open (it would start the episode again) and lays out again.
   * A new text size reaches the texts; after a new display size, the parts
   * keep their sizes until the next episode, but go where they should.
   */
  override fun onConfigurationChanged(newConfig: Configuration) {
    super.onConfigurationChanged(newConfig)
    if (!::ui.isInitialized) return
    if (newConfig.fontScale != fontScale && newConfig.densityDpi == densityDpi) kit.rescaleText()
    fontScale = newConfig.fontScale
    densityDpi = newConfig.densityDpi
    log("configuration changed: font scale $fontScale, density $densityDpi")
    ViewCompat.requestApplyInsets(ui.root)
    ui.root.post { relayout() }
  }

  private fun <T> pref(read: (SharedPreferences) -> T): T? = try {
    prefs?.let(read)
  } catch (e: Exception) {
    null
  }

  private fun savePref(write: (SharedPreferences.Editor) -> Unit) {
    try {
      prefs?.edit()?.let {
        write(it)
        it.apply()
      }
    } catch (e: Exception) {
      // Not saved; it's only a preference.
    }
  }

  /** A number for mpv, whatever the phone's language. */
  private fun fmt(v: Double) = String.format(Locale.ROOT, "%.3f", v)

  /** Tell mpv something, in order, without waiting for it. */
  private fun mpvDo(block: (MPVLib) -> Unit) {
    if (!mpvReady) return
    worker.execute {
      if (mpvReady) {
        try {
          block(mpv)
        } catch (e: Exception) {
          Log.w(TAG, "player: mpv call failed", e)
        }
      }
    }
  }

  /**
   * Tell mpv something and wait for it, at most [ms] (false: it took longer,
   * and still happens, in order). For the surface, which goes when its
   * callback returns.
   */
  private fun mpvWait(ms: Long, block: (MPVLib) -> Unit): Boolean {
    if (!mpvReady) return true
    val done = CountDownLatch(1)
    mpvDo {
      try {
        block(it)
      } finally {
        done.countDown()
      }
    }
    return done.await(ms, TimeUnit.MILLISECONDS)
  }

  private fun hideBars() {
    WindowInsetsControllerCompat(window, window.decorView).apply {
      hide(WindowInsetsCompat.Type.systemBars())
      systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }
  }

  /** Android's CAs in one file, for mpv's TLS (made once). */
  private fun caFile(): File? {
    val out = File(cacheDir, "cacerts.pem")
    if (out.exists() && out.length() > 0) return out
    val dirs = listOf(File("/apex/com.android.conscrypt/cacerts"), File("/system/etc/security/cacerts"))
    val certs = dirs.firstOrNull { it.isDirectory }?.listFiles() ?: return null
    return try {
      out.bufferedWriter().use { w ->
        for (f in certs) {
          val text = f.readText()
          val begin = text.indexOf("-----BEGIN CERTIFICATE-----")
          val end = text.indexOf("-----END CERTIFICATE-----")
          if (begin >= 0 && end > begin) {
            w.write(text.substring(begin, end + "-----END CERTIFICATE-----".length))
            w.write("\n")
          }
        }
      }
      out
    } catch (e: Exception) {
      null
    }
  }

  // ── Showing the controls ────────────────────────────────────────────

  private fun setControls(on: Boolean) {
    val show = on || talkback
    if (ui.controlsShown != show) {
      ui.controlsShown = show
      if (show) refreshNow()
      updateMoment()
      ui.sync()
    }
    scheduleHide()
  }

  private fun canAutoHide() = ready && ui.mode == Mode.PLAYING && !paused && !ui.timeline.dragging &&
    !ui.panelOpen && !touchingControl && !talkback

  private fun scheduleHide() {
    main.removeCallbacks(hideControls)
    if (ui.controlsShown && canAutoHide()) {
      main.postDelayed(hideControls, kit.timeout(3500,
        AccessibilityManager.FLAG_CONTENT_ICONS or AccessibilityManager.FLAG_CONTENT_CONTROLS or AccessibilityManager.FLAG_CONTENT_TEXT))
    }
  }

  /** A control was used: show the controls a while longer. */
  private fun poke() {
    if (ready && ui.mode == Mode.PLAYING) setControls(true)
  }

  private fun syncTaps() {
    ui.taps.active = ready && ui.mode == Mode.PLAYING && !ui.panelOpen && !talkback
  }

  // ── The overlay's buttons ───────────────────────────────────────────

  /** The top bar's Back: as the system's Back, it closes the panel first. */
  override fun back() {
    when {
      ui.panelOpen -> closePanel()
      cardUp -> cardClose()
      else -> close(null)
    }
  }

  override fun previous() = close("prev")

  override fun next() = close("next")

  override fun tracks() {
    if (ui.panelOpen) closePanel() else openPanel()
  }

  override fun fitFill() {
    setFill(!fill)
    poke()
  }

  override fun step(side: Int) {
    if (member) memberSeek() else ui.taps.step(side)
  }

  override fun playPause() {
    val p = !paused
    mpvDo { it.setPropertyBoolean("pause", p) }
    poke()
  }

  override fun ask() {
    if (!member) return
    val t = now
    if (t < askBlockedUntil) return
    askBlockedUntil = t + 2000
    val what = if (paused) "play" else "pause"
    asked = what
    log("asked $what")
    message(if (what == "pause") "Asked the host to pause" else "Asked the host to play", R.drawable.ic_users)
    ui.asked(3000)
    ui.playDisc.view.performHapticFeedback(HapticFeedbackConstants.CONFIRM)
    poke()
  }

  override fun touching(down: Boolean) {
    touchingControl = down
    if (down) main.removeCallbacks(hideControls) else scheduleHide()
  }

  override fun subPos(pos: Int) {
    if (pos == lastSubPos || !mpvReady) return
    lastSubPos = pos
    mpvDo { it.setPropertyString("sub-pos", pos.toString()) }
    log("sub-pos $pos")
  }

  /** A message at the top: plain ones stay 2 s, ones with an action 4 s. */
  private fun message(text: String, icon: Int = 0, action: String? = null, onAction: (() -> Unit)? = null, base: Int = 0) {
    val flags = if (action != null) {
      AccessibilityManager.FLAG_CONTENT_TEXT or AccessibilityManager.FLAG_CONTENT_CONTROLS
    } else {
      AccessibilityManager.FLAG_CONTENT_TEXT
    }
    val ms = kit.timeout(if (base > 0) base else if (action != null) 4000 else 2000, flags)
    ui.pill.show(text, icon, action, onAction, ms)
  }

  override fun memberSeek() {
    val t = now
    if (t - lastMemberMsg < 3000) return
    lastMemberMsg = t
    message("The host controls playback", R.drawable.ic_users)
  }

  // ── Seeking ─────────────────────────────────────────────────────────

  private fun seek(t: Double, mode: String) {
    val s = fmt(t)
    mpvDo { it.command(arrayOf("seek", s, mode)) }
    log("seek $s $mode")
  }

  override fun userSeek(t: Double) {
    seek(t, "absolute+exact")
    lastUserSeekAt = now
    // Right after a drag: this seek's chapter is the one that counts.
    if (settleAt >= 0) settleAt = t
  }

  override fun position() = position

  override fun duration() = duration

  override fun toggleControls() = setControls(!ui.controlsShown)

  override fun stepped() {
    if (ui.controlsShown) scheduleHide()
  }

  override fun pinch(fill: Boolean) {
    if (ui.panelOpen || aspectDiff() <= 0.05) return
    if (this.fill != fill) setFill(fill)
  }

  // The timeline.

  override fun scrubStart(t: Double) {
    main.removeCallbacks(hideControls)
    main.removeCallbacks(settled)
    settleAt = -1.0
    ui.scrubbing = true
    // The controls may be fading out (the band still takes touches then): they come back under the finger.
    poke()
    ui.sync()
    ui.moveBubble(t, chapters.title(chapters.at(t)), isAmber(chapters.at(t)))
    ui.showBubble(true)
    ui.setTimes(t, duration)
    if (!stream) liveSeek(t)
  }

  override fun scrub(t: Double) {
    ui.moveBubble(t, chapters.title(chapters.at(t)), isAmber(chapters.at(t)))
    ui.setTimes(t, duration)
    if (!stream) liveSeek(t)
  }

  override fun scrubEnd(t: Double) {
    main.removeCallbacks(liveTimeout)
    val moved = liveSent >= 0
    liveWanted = -1.0
    liveSent = -1.0
    liveInFlight = false
    userSeek(t)
    if (moved) settle(t)
    // Let go in the chapter the drag's seeks already reached (so no chapter change comes):
    // judged now, as a chapter you seeked into. It won't count down or auto-skip.
    if (ready && !member && chapters.at(t) == curChapter) autoSkip(curChapter)
    // The exact seek leaves the end; reaching it again ends the episode as usual.
    checkEndLater()
    restartSeen = false
    ui.setTimes(t, duration)
    ui.scrubbing = false
    ui.showBubble(false)
    ui.sync()
    ui.updateSpacer()
    updateBusy()
    poke()
  }

  override fun scrubCancel(before: Double) {
    main.removeCallbacks(liveTimeout)
    val moved = liveSent >= 0
    liveWanted = -1.0
    liveSent = -1.0
    liveInFlight = false
    // Only a file on the phone moved during the drag: put it back.
    if (moved) {
      seek(before, "absolute+exact")
      settle(before)
    }
    log("scrub cancelled")
    checkEndLater()
    restartSeen = false
    ui.setTimes(before, duration)
    ui.scrubbing = false
    ui.showBubble(false)
    ui.sync()
    ui.updateSpacer()
    updateBusy()
    poke()
  }

  override fun seekTo(t: Double) {
    userSeek(t)
    poke()
  }

  override fun memberTouch() = memberSeek()

  private fun isAmber(i: Int) = chapters.kind(i).let { it == Part.OPENING || it == Part.ENDING }

  /** The end came during the drag just over: check it again once the drag's seek has had time. */
  private fun checkEndLater() {
    if (!endDuringDrag) return
    endDuringDrag = false
    main.removeCallbacks(endCheck)
    main.postDelayed(endCheck, 3000)
  }

  private fun liveSeek(t: Double) {
    // Never the very end, which would end the episode under the finger.
    liveWanted = t.coerceIn(0.0, max(0.0, duration - 2))
    if (!liveInFlight) flushLive()
  }

  private fun flushLive() {
    if (liveWanted < 0 || liveWanted == liveSent || !ui.timeline.dragging) return
    liveSent = liveWanted
    liveInFlight = true
    seek(liveWanted, "absolute+keyframes")
    main.removeCallbacks(liveTimeout)
    main.postDelayed(liveTimeout, 150)
  }

  /**
   * The drag just let go seeked as it went: its last seek goes to [t], and
   * the ones before it may still report chapters on the way.
   */
  private fun settle(t: Double) {
    settleAt = t
    main.removeCallbacks(settled)
    // In case no restart comes (the end, an error).
    main.postDelayed(settled, 3000)
  }

  private fun playbackRestart() {
    if (liveInFlight) {
      main.removeCallbacks(liveTimeout)
      liveInFlight = false
      flushLive()
    }
    if (!ui.timeline.dragging) restartSeen = true
    // A seek landed: maybe one of the drag's last few, whose picture can
    // still move it to another chapter. Settled a moment after the last one.
    if (settleAt >= 0 && !ui.timeline.dragging) {
      main.removeCallbacks(settled)
      main.postDelayed(settled, 250)
    }
  }

  // ── Fit and Fill ────────────────────────────────────────────────────

  /** How far the picture's shape is from the screen's (0.25: 25%). */
  private fun aspectDiff(): Double {
    val vw = ui.videoW
    val vh = ui.videoH
    val w = ui.root.width
    val h = ui.root.height
    if (vw <= 0 || vh <= 0 || w <= 0 || h <= 0) return 0.0
    return abs((vw.toDouble() / vh) / (w.toDouble() / h) - 1)
  }

  private fun setFill(on: Boolean) {
    fill = on
    savePref { it.putBoolean("fill", on) }
    applyFill()
    ui.setFillIcon(on)
    if (on) message("Zoomed to fill", R.drawable.ic_expand) else message("Fit to screen", R.drawable.ic_shrink)
  }

  /** Fill (unless the panel's open: the picture fits beside it) or Fit. */
  private fun applyFill() {
    if (!mpvReady) return
    val on = fill && !ui.panelOpen
    mpvDo {
      it.setPropertyString("panscan", if (on) "1.0" else "0.0")
      it.setPropertyString("sub-ass-force-margins", if (on) "yes" else "no")
    }
    log("panscan ${if (on) "1.0" else "0.0"}")
    ui.fill = on
    ui.lift()
  }

  private fun pictureChanged() {
    ui.fitAvailable = aspectDiff() > 0.05
    ui.sync()
    checkHint()
  }

  /** Once: "Pinch to fill the screen", when the picture's shape isn't the screen's. */
  private fun checkHint() {
    if (hintChecked || !ready || ui.videoW <= 0 || ui.videoH <= 0 || ui.root.width <= 0) return
    hintChecked = true
    if (aspectDiff() <= 0.10 || resumeShown) return
    if (pref { it.contains("pinchHint") } != false) return
    message("Pinch to fill the screen", R.drawable.ic_expand, base = 4000)
    savePref { it.putBoolean("pinchHint", true) }
  }

  // ── The tracks panel ────────────────────────────────────────────────

  private fun openPanel() {
    if (!ready || ui.panelOpen || !mpvReady) return
    main.removeCallbacks(hideControls)
    ui.panel.setTracks(tracks)
    ui.panelOpen = true
    ui.controlsShown = true
    applyFill()
    applyMargins()
    ui.panel.open()
    syncTaps()
    updateMoment()
    ui.sync()
    syncCountdown()
  }

  /** The picture in the room the panel leaves (set again when the panel moves). */
  private fun applyMargins() {
    if (!mpvReady) return
    val m = ui.panel.margins()
    if (m == margins) return
    margins = m
    val (r, t, b) = m
    mpvDo {
      it.setPropertyString("video-margin-ratio-right", String.format(Locale.ROOT, "%.4f", r))
      it.setPropertyString("video-margin-ratio-top", String.format(Locale.ROOT, "%.4f", t))
      it.setPropertyString("video-margin-ratio-bottom", String.format(Locale.ROOT, "%.4f", b))
    }
    log(String.format(Locale.ROOT, "margins set right=%.4f top=%.4f bottom=%.4f", r, t, b))
  }

  private fun closePanel() {
    if (!ui.panelOpen) return
    ui.panel.close {
      mpvDo {
        it.setPropertyString("video-margin-ratio-right", "0")
        it.setPropertyString("video-margin-ratio-top", "0")
        it.setPropertyString("video-margin-ratio-bottom", "0")
      }
      margins = null
      log("margins reset")
      ui.panelOpen = false
      applyFill()
      syncTaps()
      ui.sync()
      updateMoment()
      syncCountdown()
      poke()
    }
  }

  override fun chooseAudio(id: Int) {
    mpvDo { it.setPropertyInt("aid", id) }
    log("aid $id")
  }

  override fun chooseSubtitles(id: Int?) {
    mpvDo { if (id == null) it.setPropertyString("sid", "no") else it.setPropertyInt("sid", id) }
    log("sid ${id ?: "no"}")
  }

  override fun subDelay(seconds: Double) {
    val s = String.format(Locale.ROOT, "%.1f", seconds)
    mpvDo { it.setPropertyString("sub-delay", s) }
    log("sub-delay $s")
  }

  override fun subScale(percent: Int) {
    val s = (percent / 100.0).toString()
    mpvDo { it.setPropertyString("sub-scale", s) }
    savePref { it.putInt("subScale", percent) }
    log("sub-scale $s")
  }

  /** mpv's tracks, read on its event thread. */
  private fun gotTracks(list: List<Track>) {
    tracks = list
    ui.panel.setTracks(list)
    ui.setSubsOff(list.none { it.type == "sub" && it.selected })
  }

  // ── Chapters, the moment button, auto-skip ──────────────────────────

  /** mpv's chapters, read on its event thread. */
  private fun gotChapters(list: Chapters) {
    chapters = Chapters(list.list, duration)
    ui.timeline.setChapters(chapters)
    ui.setChapterTitle(chapters.title(curChapter))
    updateMoment()
  }

  private fun chapterChanged(i: Int, force: Boolean = false) {
    if (!force && i == curChapter) return
    curChapter = i
    chapterEnteredAt = now
    if (countdownChapter >= 0 && countdownChapter != i) {
      // Left the chapter: the countdown just stops.
      ui.moments.stopCountdown()
      countdownChapter = -1
      log("countdown cancel (left the chapter)")
    }
    ui.setChapterTitle(chapters.title(i))
    // A drag's seeks only pass through chapters, even the ones that land after
    // it's let go: auto-skip judges the one it ends in (and scrubEnd).
    val passing = ui.timeline.dragging || (settleAt >= 0 && i != chapters.at(settleAt))
    if (ready && !member && !passing) autoSkip(i)
    updateMoment()
    main.removeCallbacks(momentTick)
    main.postDelayed(momentTick, 8100)
  }

  private fun autoSkip(i: Int) {
    val kind = chapters.kind(i)
    val want = (kind == Part.OPENING && skipOpenings) || (kind == Part.ENDING && skipEndings)
    if (!want || i in skipped) return
    skipped += i
    val pos = position
    // Seeked into it: no countdown.
    if (now - lastUserSeekAt < 1500 || pos - chapters.start(i) > 2) return
    countdownChapter = i
    val opening = kind == Part.OPENING
    // With TalkBack, long enough to hear it and find the Watch button.
    val ms = kit.timeout(if (talkback) 10_000 else 4000, AccessibilityManager.FLAG_CONTENT_CONTROLS)
    val secs = Math.round(ms / 1000.0)
    val what = if (opening) "opening" else "ending"
    ui.moments.startCountdown(
      ms,
      if (opening) "Skipping opening" else "Skipping ending",
      // A live region: said wherever TalkBack is, so it says how to cancel, not "double tap".
      "Skipping the $what in $secs seconds. To cancel, use Watch the $what.",
      if (opening) "Watch opening" else "Watch ending",
      if (opening) "Watch the opening, don't skip" else "Watch the ending, don't skip",
    )
    log("countdown start chapter=$i ms=$ms")
    syncCountdown()
  }

  /** The countdown runs on playback time: not while paused, buffering, seeking, or in the panel. */
  private fun syncCountdown() {
    if (countdownChapter < 0) return
    ui.moments.runCountdown(!paused && !pausedForCache && !seeking && !ui.panelOpen)
  }

  private fun countdownDone() {
    val i = countdownChapter
    if (i < 0) return
    countdownChapter = -1
    log("countdown fire")
    skipNow(i)
    updateMoment()
  }

  /** Do what the moment button says for chapter [i] after a countdown, then offer Undo. */
  private fun skipNow(i: Int) {
    val m = moment(chapters, curChapter, position, hasNext, ui.controlsShown)
    when (m?.rule) {
      1, 4 -> {
        userSeek(chapters.end(curChapter))
        message(if (m.rule == 1) "Skipped opening" else "Skipped ending", action = "Undo", onAction = {
          userSeek(chapters.start(i))
        })
      }
      2, 3 -> eofPath()
    }
  }

  private fun watchPressed() {
    if (countdownChapter < 0) return
    countdownChapter = -1
    ui.moments.stopCountdown()
    log("countdown cancel")
    momentLabel = null
    updateMoment()
    poke()
  }

  private fun momentPressed() {
    if (countdownChapter >= 0) {
      val i = countdownChapter
      countdownChapter = -1
      ui.moments.stopCountdown()
      log("countdown fire (pressed)")
      skipNow(i)
      updateMoment()
      return
    }
    val m = momentNow ?: return
    when (m.rule) {
      1, 4 -> userSeek(chapters.end(curChapter))
      2, 5 -> {
        close("next")
        return
      }
      3 -> {
        eofPath()
        return
      }
      6 -> userSeek(position + 85)
    }
    poke()
  }

  /** What reaching the end does: the app's autoplay setting decides what's next. */
  private fun eofPath() {
    ended = true
    position = duration
    close(if (hasNext && !member) "auto" else null)
  }

  private fun updateMoment() {
    if (!::ui.isInitialized) return
    val counting = countdownChapter >= 0
    val m = if (member || !ready) null else moment(chapters, curChapter, position, hasNext, ui.controlsShown)
    momentNow = m
    val label = if (counting) "countdown" else m?.label
    val changed = label != momentLabel
    if (changed) {
      log("moment ${if (counting) "countdown" else label ?: "none"}")
      momentLabel = label
      if (m != null) ui.moments.setLabel(m.label, m.spoken)
    }
    val rule = m?.rule ?: 0
    val show = (m != null || counting) && !member && ready && ui.mode == Mode.PLAYING && !ui.panelOpen &&
      (ui.controlsShown || counting || ((rule == 1 || rule == 4) && now - chapterEnteredAt < 8000) ||
        rule == 2 || rule == 3)
    ui.setMoment(show, changed)
  }

  // ── Loading, buffering, errors ──────────────────────────────────────

  /** What mpv says about the file just loaded (read on its event thread). */
  private class Loaded(
    val duration: Double,
    val chapters: Chapters,
    val tracks: List<Track>,
    val chapter: Int,
    val time: Double,
  )

  private fun fileLoaded(l: Loaded) {
    if (closed || !mpvReady) return
    val first = !everLoaded
    everLoaded = true
    ready = true
    loadedAt = now
    reconnecting = false
    main.removeCallbacks(settled)
    settleAt = -1.0
    cardUp = false
    stalled = false
    main.removeCallbacks(loadingCaption)
    if (l.duration > 0) duration = l.duration
    position = l.time
    ui.setDuration(duration)
    gotChapters(l.chapters)
    gotTracks(l.tracks)
    val at = loadAt
    if (at > 1) {
      for (i in chapters.list.indices) {
        val k = chapters.kind(i)
        if ((k == Part.OPENING || k == Part.ENDING) && at > chapters.start(i) + 1 && at < chapters.end(i)) skipped += i
      }
    }
    ui.hideCard()
    ui.mode = Mode.PLAYING
    ui.setLoading(null)
    syncTaps()
    chapterChanged(l.chapter, force = true)
    log("loaded duration=$duration chapters=${chapters.list.size}")
    if (first && !member && start > 30 && start < duration - 30) {
      resumeShown = true
      message("Resuming at ${clock(start)}", action = "Start over", onAction = { userSeek(0.0) }, base = 5000)
    }
    ui.controlsShown = false
    setControls(true)
    ui.sync()
    pictureChanged()
  }

  private fun updateBusy() {
    if (!::ui.isInitialized) return
    main.removeCallbacks(busyTick)
    val t = now
    var next = Long.MAX_VALUE
    val cacheBusy = pausedForCache && t - cacheSince >= 250
    if (pausedForCache && !cacheBusy) next = cacheSince + 250
    val dragging = ui.timeline.dragging
    val seekBusy = seeking && !dragging && t - seekingSince > 400
    if (seeking && !dragging && !seekBusy) next = minOf(next, seekingSince + 401)
    // "Buffering N%" after 1.5 s.
    val pct = if (pausedForCache && t - cacheSince >= 1500) bufferPct else null
    if (pausedForCache && pct == null) next = minOf(next, cacheSince + 1500)
    ui.setBusy(ready && (cacheBusy || seekBusy), pct)
    if (next != Long.MAX_VALUE) main.postAtTime(busyTick, next)
  }

  private fun eofReached() {
    if (!ready || closed) return
    // A stream that stops well before the end lost its connection: it's not the end.
    if (stream && duration > 0 && duration - position > 10) {
      lost()
      return
    }
    // A drag never ends the episode: its seek on release leaves the end.
    if (ui.timeline.dragging) {
      endDuringDrag = true
      log("end during a drag: waits")
      return
    }
    ended = true
    close(if (hasNext && !member) "auto" else null)
  }

  private fun endFileError(error: Int) {
    if (closed) return
    if (!everLoaded) {
      val detail = when (error) {
        -13 -> if (stream) "The server didn't send the video." else "The file couldn't be opened."
        -17 -> "This kind of file isn't supported."
        -16 -> "There's no video or audio in it."
        else -> "mpv error $error."
      }
      showCard("Couldn't play this episode", detail, true)
    } else {
      lost()
    }
  }

  /** Playback stopped after it had started: reconnect once (a stream), else the card. */
  private fun lost() {
    if (closed) return
    ready = false
    loaded = false
    stalled = true
    if (countdownChapter >= 0) {
      countdownChapter = -1
      ui.moments.stopCountdown()
    }
    if (ui.panelOpen) closePanel()
    if (stream && !reconnecting && now - lastAutoRetryAt > 60_000) {
      reconnecting = true
      lastAutoRetryAt = now
      log("reconnect at ${fmt(position)}")
      ui.mode = Mode.LOADING
      ui.setLoading("Reconnecting…")
      syncTaps()
      updateMoment()
      ui.sync()
      main.postDelayed({ if (!closed && reconnecting) retry(position) }, 2000)
    } else {
      reconnecting = false
      if (stream) showCard("Lost the connection", "The stream stopped before the end.", false)
      else showCard("Couldn't keep playing", "The file couldn't be read.", false)
    }
  }

  private fun showCard(title: String, detail: String, beforePlay: Boolean) {
    cardUp = true
    stalled = true
    cardError = if (beforePlay) "$title. $detail" else null
    ready = false
    main.removeCallbacks(loadingCaption)
    if (countdownChapter >= 0) {
      countdownChapter = -1
      ui.moments.stopCountdown()
    }
    ui.mode = Mode.CARD
    ui.setLoading(null)
    syncTaps()
    updateMoment()
    ui.sync()
    ui.showCard(title, detail, { cardClose() }, { retry(if (beforePlay) start else position) })
    log("card $title: $detail")
  }

  private fun cardClose() {
    if (!cardUp) return
    close(null, cardError)
  }

  /** Load the file again, from [at]. */
  private fun retry(at: Double) {
    if (closed || !mpvReady) return
    cardUp = false
    ui.hideCard()
    ui.mode = Mode.LOADING
    if (!reconnecting) {
      ui.setLoading(null)
      main.removeCallbacks(loadingCaption)
      main.postDelayed(loadingCaption, 2000)
    }
    ui.sync()
    loadAt = at
    loaded = false
    ready = false
    stalled = true
    val from = fmt(at)
    // Not loadfile's options argument: its place changed in mpv 0.38.
    mpvDo {
      it.setPropertyString("start", from)
      it.command(arrayOf("loadfile", url, "replace"))
    }
    log("retry at ${fmt(at)}")
  }

  // ── Refreshing what the controls say ────────────────────────────────

  /** At most every 250 ms (from mpv's thread). */
  private fun scheduleRefresh() {
    if (refreshPending.compareAndSet(false, true)) {
      main.postDelayed(refreshTick, max(0L, lastRefresh + 250 - now))
    }
  }

  private fun refresh() {
    if (closed || !ready) return
    if (restartSeen) {
      restartSeen = false
      ui.timeline.releaseHold()
    }
    refreshNow()
    updateMoment()
  }

  private fun refreshNow() {
    if (!ready) return
    val visible = ui.controlsShown || ui.band.visibility == View.VISIBLE
    ui.timeline.setPosition(position, visible)
    if (visible && !ui.timeline.dragging) ui.setTimes(ui.timeline.shown, duration)
  }

  // ── mpv's events (on its own thread) ────────────────────────────────

  override fun eventProperty(property: String) {
    // The JNI has no node values: read the JSON here, on mpv's event thread.
    if (!mpvReady) return
    when (property) {
      "chapter-list" -> {
        val c = Chapters.parse(mpv.getPropertyString("chapter-list"), duration)
        main.post { if (!closed) gotChapters(c) }
      }
      "track-list" -> readTracksLater()
    }
  }

  override fun eventProperty(property: String, value: Long) {
    main.post {
      if (closed) return@post
      when (property) {
        // With the surface gone there's no video, and paused before the audio
        // began, mpv doesn't know where it is for a while: not a new chapter.
        "chapter" -> if (value >= 0 || !surfaceGone) chapterChanged(value.toInt())
        "cache-buffering-state" -> {
          bufferPct = value.toInt()
          updateBusy()
        }
        "dwidth" -> {
          ui.videoW = value.toInt()
          pictureChanged()
        }
        "dheight" -> {
          ui.videoH = value.toInt()
          pictureChanged()
        }
      }
    }
  }

  override fun eventProperty(property: String, value: Double) {
    when (property) {
      "time-pos" -> if (loaded) {
        position = value
        scheduleRefresh()
      }
      "duration" -> {
        duration = value
        main.post {
          if (closed) return@post
          ui.setDuration(value)
          chapters = Chapters(chapters.list, value)
          ui.timeline.setChapters(chapters)
        }
      }
      "demuxer-cache-time" -> main.post { if (!closed) ui.timeline.setCache(value) }
    }
  }

  override fun eventProperty(property: String, value: Boolean) {
    if (property == "pause") paused = value
    main.post {
      if (closed) return@post
      when (property) {
        "pause" -> {
          ui.setPaused(value)
          // Paused (by you or the host): the controls stay up.
          if (ready) setControls(true)
          syncCountdown()
        }
        "eof-reached" -> if (value && loaded) eofReached()
        "paused-for-cache" -> {
          pausedForCache = value
          if (value) cacheSince = now
          updateBusy()
          syncCountdown()
        }
        "seeking" -> {
          seeking = value
          if (value) seekingSince = now
          updateBusy()
          syncCountdown()
        }
      }
    }
  }

  override fun eventProperty(property: String, value: String) {
    if (property == "sid") main.post { if (!closed) ui.setSubsOff(value == "no") }
    if (property == "sid" || property == "aid") readTracksLater()
  }

  /** Read the tracks (on mpv's event thread), then show them. */
  private fun readTracksLater() {
    if (!mpvReady) return
    val t = Track.parse(mpv.getPropertyString("track-list"))
    main.post { if (!closed) gotTracks(t) }
  }

  override fun event(eventId: Int) {
    when (eventId) {
      MPVLib.MpvEvent.MPV_EVENT_FILE_LOADED -> {
        if (!mpvReady) return
        // A retry's start applies once.
        mpv.setPropertyString("start", "none")
        val d = mpv.getPropertyDouble("duration") ?: 0.0
        val l = Loaded(
          d,
          Chapters.parse(mpv.getPropertyString("chapter-list"), d),
          Track.parse(mpv.getPropertyString("track-list")),
          mpv.getPropertyInt("chapter") ?: -1,
          mpv.getPropertyDouble("time-pos") ?: 0.0,
        )
        loaded = true
        main.post { fileLoaded(l) }
      }
      MPVLib.MpvEvent.MPV_EVENT_PLAYBACK_RESTART -> main.post { if (!closed) playbackRestart() }
    }
  }

  override fun eventEndFile(reason: Int, error: Int) {
    // error >= 0: it stopped (or a retry replaced it).
    if (error < 0) main.post { endFileError(error) }
  }

  // ── The surface ─────────────────────────────────────────────────────
  // mpv is told on the worker: when mpv's core is stuck, the app isn't.

  /** The video track while the surface is gone (only the worker uses it). */
  private var hiddenVid: String? = null
  /** The surface is gone (Home, or another app on top): mpv plays without video. */
  private var surfaceGone = false

  override fun surfaceCreated(holder: SurfaceHolder) {
    surfaceGone = false
    if (!mpvReady) return
    val surface = holder.surface
    val first = !fileStarted
    fileStarted = true
    val u = url
    mpvDo { m ->
      m.attachSurface(surface)
      if (first) {
        m.setOptionString("force-window", "yes")
        m.command(arrayOf("loadfile", u))
      } else {
        m.setPropertyString("vo", "gpu")
        m.setOptionString("force-window", "yes")
        // The video again, from where it is (paused, it shows that frame).
        m.setPropertyString("vid", hiddenVid ?: "auto")
        hiddenVid = null
      }
    }
  }

  override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
    if (!mpvReady) return
    mpvDo { it.setPropertyString("android-surface-size", "${width}x$height") }
    ui.lift()
  }

  override fun surfaceDestroyed(holder: SurfaceHolder) {
    if (!mpvReady) return
    surfaceGone = true
    val t = now
    // mpv lets go of the surface before it goes (when this returns), but the
    // main thread doesn't wait long for it.
    val inTime = mpvWait(2000) { m ->
      // No video without a surface. mpv would open its decoder again with
      // none, and MediaCodec can stall for good that way (it does on the
      // emulator), mpv's core with it.
      hiddenVid = m.getPropertyString("vid") ?: "auto"
      m.setPropertyString("vid", "no")
      m.setOptionString("force-window", "no")
      m.setPropertyString("vo", "null")
      m.detachSurface()
    }
    log(if (inTime) "surface gone (${now - t} ms)" else "surface gone, mpv still busy after ${now - t} ms")
  }

  // ── Keys ────────────────────────────────────────────────────────────

  /** Play, pause and the other media keys: the player's, whatever has focus. */
  private fun playKey(code: Int) = when (code) {
    KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_HEADSETHOOK, KeyEvent.KEYCODE_SPACE, KeyEvent.KEYCODE_K,
    KeyEvent.KEYCODE_MEDIA_PLAY, KeyEvent.KEYCODE_MEDIA_PAUSE,
    KeyEvent.KEYCODE_MEDIA_REWIND, KeyEvent.KEYCODE_MEDIA_FAST_FORWARD,
    KeyEvent.KEYCODE_MEDIA_NEXT, KeyEvent.KEYCODE_MEDIA_PREVIOUS -> true
    else -> false
  }

  /**
   * The play and media keys reach the player before the views: a focused
   * button takes Space as a press (on Back, it would close the episode).
   * Not on the error card, whose buttons Space presses as usual.
   */
  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    if (!playKey(event.keyCode) || !::ui.isInitialized || ui.mode == Mode.CARD) return super.dispatchKeyEvent(event)
    if (event.action == KeyEvent.ACTION_DOWN) playKeyDown(event)
    return true
  }

  private fun playKeyDown(event: KeyEvent) {
    val code = event.keyCode
    // A stray headset press would throw you out of the episode.
    if (code == KeyEvent.KEYCODE_MEDIA_NEXT || code == KeyEvent.KEYCODE_MEDIA_PREVIOUS) return
    if (!ready || ui.mode != Mode.PLAYING) return
    // Held down: one toggle, not one per repeat.
    val first = event.repeatCount == 0
    when (code) {
      KeyEvent.KEYCODE_MEDIA_REWIND, KeyEvent.KEYCODE_MEDIA_FAST_FORWARD ->
        if (!ui.panelOpen) step(if (code == KeyEvent.KEYCODE_MEDIA_REWIND) -1 else 1)
      KeyEvent.KEYCODE_MEDIA_PLAY -> if (first) {
        if (member) {
          if (paused) ask()
        } else {
          mpvDo { it.setPropertyBoolean("pause", false) }
          poke()
        }
      }
      KeyEvent.KEYCODE_MEDIA_PAUSE -> if (first) {
        if (member) {
          if (!paused) ask()
        } else {
          mpvDo { it.setPropertyBoolean("pause", true) }
          poke()
        }
      }
      else -> if (first) {
        if (member) ask() else playPause()
      }
    }
  }

  override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
    when (keyCode) {
      // A stray headset press would throw you out of the episode (on the error card too).
      KeyEvent.KEYCODE_MEDIA_NEXT, KeyEvent.KEYCODE_MEDIA_PREVIOUS -> return true
    }
    if (!ready || ui.mode != Mode.PLAYING || ui.panelOpen) return super.onKeyDown(keyCode, event)
    val up = ui.controlsShown
    when (keyCode) {
      KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER -> if (!up) {
        setControls(true)
        ui.playDisc.view.requestFocus()
        return true
      }
      KeyEvent.KEYCODE_DPAD_LEFT, KeyEvent.KEYCODE_DPAD_RIGHT -> if (!up) {
        step(if (keyCode == KeyEvent.KEYCODE_DPAD_LEFT) -1 else 1)
        return true
      }
      KeyEvent.KEYCODE_DPAD_UP, KeyEvent.KEYCODE_DPAD_DOWN -> if (!up) {
        setControls(true)
        return true
      }
    }
    if (up) scheduleHide()
    return super.onKeyDown(keyCode, event)
  }

  // ── Lifecycle ───────────────────────────────────────────────────────

  override fun onResume() {
    super.onResume()
    hideBars()
    resumedAt = now
  }

  override fun onPause() {
    super.onPause()
    if (!isFinishing) mpvDo { it.setPropertyBoolean("pause", true) }
  }

  // ── The watch party ─────────────────────────────────────────────────

  /** What's playing, where, and whether you asked the host for something. */
  fun state(): JSObject = JSObject().apply {
    put("open", !closed)
    put("id", intent.getLongExtra("id", 0))
    put("episode", intent.getIntExtra("episode", 0))
    put("position", position)
    put("paused", paused || stalled)
    put("ended", ended)
    put("asked", asked)
    asked = null
  }

  /** Seek, pause or play, or close: as the party's host's player is. */
  fun control(seekTo: Double?, pause: Boolean?, quit: Boolean) {
    main.post {
      if (closed || !mpvReady) return@post
      if (quit) {
        close(null)
        return@post
      }
      // The card, or reconnecting: nothing to seek or pause.
      if (cardUp || reconnecting) return@post
      if (member) hostMessages(seekTo, pause)
      mpvDo { m ->
        seekTo?.let { m.setPropertyDouble("time-pos", it) }
        pause?.let { m.setPropertyBoolean("pause", it) }
      }
    }
  }

  /** "The host paused", "The host jumped to 12:34" (not the small corrections). */
  private fun hostMessages(seekTo: Double?, pause: Boolean?) {
    val t = now
    if (!ready || t - loadedAt < 6000 || t - resumedAt < 3000) return
    if (pause != null && pause != paused) {
      ui.resetAsked()
      if (t - lastHostPauseMsg >= 3000) {
        lastHostPauseMsg = t
        message(if (pause) "The host paused" else "The host resumed", R.drawable.ic_users, base = 2500)
      }
    }
    if (seekTo != null && abs(seekTo - position) > 10 && t - lastHostJumpMsg >= 5000) {
      lastHostJumpMsg = t
      message("The host jumped to ${clock(seekTo)}", R.drawable.ic_users, base = 2500)
    }
  }

  /** Another episode's starting: this one closes. */
  fun replaced() {
    main.post { close(null) }
  }

  /** Done: how far it got, and what next. */
  private fun close(then: String?, error: String? = null) {
    if (closed) return
    closed = true
    log("close then=$then ended=$ended position=$position duration=$duration error=$error")
    val result = Intent().apply {
      putExtra("position", position)
      putExtra("duration", duration)
      putExtra("ended", ended)
      putExtra("then", then)
      putExtra("error", error)
    }
    setResult(RESULT_OK, result)
    finish()
    if (Build.VERSION.SDK_INT < 34) {
      @Suppress("DEPRECATION")
      overridePendingTransition(0, android.R.anim.fade_out)
    }
  }

  override fun onDestroy() {
    if (current === this) current = null
    access?.removeTouchExplorationStateChangeListener(talkbackListener)
    if (mpvReady) {
      mpvReady = false
      mpv.removeObserver(this)
      // After whatever the worker is still telling it.
      val m = mpv
      worker.execute { m.destroy() }
    }
    worker.shutdown()
    main.removeCallbacksAndMessages(null)
    if (::ui.isInitialized) ui.release()
    super.onDestroy()
  }
}
