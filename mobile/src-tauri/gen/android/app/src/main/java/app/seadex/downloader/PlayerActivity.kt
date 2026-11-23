package app.seadex.downloader

import android.annotation.SuppressLint
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.SeekBar
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.jdtech.mpv.MPVLib
import java.io.File

/**
 * The app's player: libmpv (mpv's engine: every codec, ASS subtitles as
 * they're meant to look, chapters), full screen, with its controls: play
 * and pause, seeking, audio and subtitles, skipping openings and endings,
 * the next episode. It says how far it got when it closes.
 */
class PlayerActivity : AppCompatActivity(), MPVLib.EventObserver, SurfaceHolder.Callback {
  private lateinit var mpv: MPVLib
  private val main = Handler(Looper.getMainLooper())

  private var position = 0.0
  private var duration = 0.0
  private var paused = false
  private var ended = false
  private var loaded = false
  private var dragging = false
  private var skipOpenings = false
  private var skipEndings = false
  private val skipped = mutableSetOf<Int>()

  private lateinit var controls: View
  private lateinit var playButton: TextView
  private lateinit var seek: SeekBar
  private lateinit var time: TextView
  private lateinit var left: TextView
  private lateinit var skipButton: TextView
  private lateinit var spinner: ProgressBar
  private lateinit var chapterName: TextView

  private val hide = Runnable { showControls(false) }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    WindowCompat.setDecorFitsSystemWindows(window, false)
    WindowInsetsControllerCompat(window, window.decorView).apply {
      hide(WindowInsetsCompat.Type.systemBars())
      systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }
    skipOpenings = intent.getBooleanExtra("skipOpenings", false)
    skipEndings = intent.getBooleanExtra("skipEndings", false)
    buildViews()

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
    mpv.setOptionString("save-position-on-quit", "no")
    mpv.setOptionString("input-default-bindings", "no")
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
    val start = intent.getDoubleExtra("start", 0.0)
    if (start > 0) mpv.setOptionString("start", start.toString())
    mpv.init()
    mpv.addObserver(this)
    mpv.observeProperty("time-pos", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    mpv.observeProperty("duration", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    mpv.observeProperty("pause", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("eof-reached", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("paused-for-cache", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
    mpv.observeProperty("chapter", MPVLib.MpvFormat.MPV_FORMAT_INT64)

    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() = close(null)
    })
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

  // ── The views ───────────────────────────────────────────────────────

  private fun dp(n: Int) = (n * resources.displayMetrics.density).toInt()

  private fun label(text: String, size: Float, bold: Boolean = false) = TextView(this).apply {
    this.text = text
    textSize = size
    setTextColor(Color.WHITE)
    if (bold) typeface = Typeface.DEFAULT_BOLD
  }

  private fun pill(text: String, onClick: () -> Unit) = label(text, 14f, true).apply {
    setPadding(dp(14), dp(8), dp(14), dp(8))
    background = GradientDrawable().apply {
      cornerRadius = dp(20).toFloat()
      setColor(Color.argb(170, 23, 27, 36))
      setStroke(dp(1), Color.argb(90, 255, 255, 255))
    }
    setOnClickListener { onClick() }
  }

  @SuppressLint("ClickableViewAccessibility")
  private fun buildViews() {
    val root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
    val surface = SurfaceView(this)
    surface.holder.addCallback(this)
    root.addView(surface, FrameLayout.LayoutParams(-1, -1))

    spinner = ProgressBar(this).apply { visibility = View.GONE }
    root.addView(spinner, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.CENTER))

    val overlay = FrameLayout(this).apply { setBackgroundColor(Color.argb(110, 0, 0, 0)) }
    controls = overlay

    // Top: back, what's playing, tracks.
    val top = LinearLayout(this).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
      setPadding(dp(16), dp(14), dp(16), dp(8))
    }
    top.addView(pill("←") { close(null) })
    val names = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      setPadding(dp(14), 0, dp(14), 0)
    }
    names.addView(label(intent.getStringExtra("title") ?: "", 17f, true).apply { maxLines = 1 })
    names.addView(label(intent.getStringExtra("name") ?: "", 13f).apply {
      setTextColor(Color.argb(200, 255, 255, 255)); maxLines = 1
    })
    top.addView(names, LinearLayout.LayoutParams(0, -2, 1f))
    top.addView(pill("Audio") { pickTrack("audio") })
    top.addView(View(this), LinearLayout.LayoutParams(dp(8), 1))
    top.addView(pill("Subtitles") { pickTrack("sub") })
    overlay.addView(top, FrameLayout.LayoutParams(-1, -2, Gravity.TOP))

    // Middle: back 10 s, play or pause, on 10 s.
    val middle = LinearLayout(this).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER
    }
    val back10 = label("⟲ 10", 18f, true).apply { setPadding(dp(24), dp(12), dp(24), dp(12)); setOnClickListener { seekBy(-10.0) } }
    playButton = label("❚❚", 34f, true).apply {
      gravity = Gravity.CENTER
      background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Color.argb(150, 139, 124, 255)) }
      setOnClickListener { mpv.setPropertyBoolean("pause", !paused); poke() }
    }
    val on10 = label("10 ⟳", 18f, true).apply { setPadding(dp(24), dp(12), dp(24), dp(12)); setOnClickListener { seekBy(10.0) } }
    middle.addView(back10)
    middle.addView(playButton, LinearLayout.LayoutParams(dp(76), dp(76)))
    middle.addView(on10)
    overlay.addView(middle, FrameLayout.LayoutParams(-2, -2, Gravity.CENTER))

    // Bottom: chapter, skip, next; time, the bar, time left.
    val bottom = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      setPadding(dp(16), dp(8), dp(16), dp(14))
    }
    val actions = LinearLayout(this).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL or Gravity.END
    }
    chapterName = label("", 13f).apply { setTextColor(Color.argb(200, 255, 255, 255)) }
    actions.addView(chapterName, LinearLayout.LayoutParams(0, -2, 1f))
    skipButton = pill("Skip opening ⏭") { skipChapter() }.apply { visibility = View.GONE }
    actions.addView(skipButton)
    if (intent.getBooleanExtra("hasPrev", false)) {
      actions.addView(View(this), LinearLayout.LayoutParams(dp(8), 1))
      actions.addView(pill("⏮ Previous") { close("prev") })
    }
    if (intent.getBooleanExtra("hasNext", false)) {
      actions.addView(View(this), LinearLayout.LayoutParams(dp(8), 1))
      actions.addView(pill("Next ⏭") { close("next") })
    }
    bottom.addView(actions)
    val bar = LinearLayout(this).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
    }
    time = label("0:00", 13f)
    left = label("-0:00", 13f)
    seek = SeekBar(this).apply {
      max = 1000
      setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
        override fun onProgressChanged(s: SeekBar, value: Int, fromUser: Boolean) {
          if (fromUser && duration > 0) time.text = clock(duration * value / 1000)
        }
        override fun onStartTrackingTouch(s: SeekBar) { dragging = true; main.removeCallbacks(hide) }
        override fun onStopTrackingTouch(s: SeekBar) {
          dragging = false
          if (duration > 0) mpv.setPropertyDouble("time-pos", duration * s.progress / 1000)
          poke()
        }
      })
    }
    bar.addView(time)
    bar.addView(seek, LinearLayout.LayoutParams(0, -2, 1f))
    bar.addView(left)
    bottom.addView(bar)
    overlay.addView(bottom, FrameLayout.LayoutParams(-1, -2, Gravity.BOTTOM))
    root.addView(overlay, FrameLayout.LayoutParams(-1, -1))

    // Tap: the controls; double tap a side: 10 s back or on.
    val gestures = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
      override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
        showControls(controls.visibility != View.VISIBLE); return true
      }
      override fun onDoubleTap(e: MotionEvent): Boolean {
        seekBy(if (e.x < root.width / 2f) -10.0 else 10.0); return true
      }
    })
    surface.setOnTouchListener { _, e -> gestures.onTouchEvent(e); true }
    overlay.setOnTouchListener { _, e -> gestures.onTouchEvent(e); true }
    setContentView(root)
    poke()
  }

  private fun showControls(on: Boolean) {
    controls.visibility = if (on) View.VISIBLE else View.GONE
    main.removeCallbacks(hide)
    if (on && !paused) main.postDelayed(hide, 3500)
  }

  /** Show the controls a while longer. */
  private fun poke() = showControls(true)

  private fun seekBy(seconds: Double) {
    mpv.command(arrayOf("seek", seconds.toString(), "relative"))
    poke()
  }

  private fun clock(s: Double): String {
    val t = s.toInt().coerceAtLeast(0)
    val (h, m, sec) = Triple(t / 3600, t / 60 % 60, t % 60)
    return if (h > 0) "%d:%02d:%02d".format(h, m, sec) else "%d:%02d".format(m, sec)
  }

  // ── Chapters: skipping openings and endings ─────────────────────────

  private val opening = Regex("(?i)^(op|opening|intro)\\b|opening")
  private val ending = Regex("(?i)^(ed|ending|outro|credits)\\b|ending")

  /** "opening", "ending", or null for the chapter now. */
  private fun chapterKind(i: Int): String? {
    val title = mpv.getPropertyString("chapter-list/$i/title") ?: return null
    return when {
      opening.containsMatchIn(title) -> "opening"
      ending.containsMatchIn(title) -> "ending"
      else -> null
    }
  }

  private fun onChapter(i: Int) {
    val title = mpv.getPropertyString("chapter-list/$i/title") ?: ""
    chapterName.text = title
    val kind = chapterKind(i)
    val auto = (kind == "opening" && skipOpenings) || (kind == "ending" && skipEndings)
    if (auto && skipped.add(i)) {
      skipChapter()
      return
    }
    skipButton.visibility = if (kind != null) View.VISIBLE else View.GONE
    skipButton.text = if (kind == "ending") "Skip ending ⏭" else "Skip opening ⏭"
  }

  private fun skipChapter() {
    mpv.command(arrayOf("add", "chapter", "1"))
    skipButton.visibility = View.GONE
  }

  // ── Audio and subtitles ─────────────────────────────────────────────

  private fun pickTrack(type: String) {
    val count = mpv.getPropertyInt("track-list/count") ?: 0
    val ids = mutableListOf<Int>()
    val names = mutableListOf<String>()
    var chosen = -1
    if (type == "sub") {
      ids.add(-1); names.add("None")
    }
    for (i in 0 until count) {
      if (mpv.getPropertyString("track-list/$i/type") != type) continue
      val id = mpv.getPropertyInt("track-list/$i/id") ?: continue
      val lang = mpv.getPropertyString("track-list/$i/lang")
      val title = mpv.getPropertyString("track-list/$i/title")
      val codec = mpv.getPropertyString("track-list/$i/codec")
      names.add(listOfNotNull(title, lang?.uppercase(), codec).joinToString(" · ").ifEmpty { "Track $id" })
      ids.add(id)
      if (mpv.getPropertyBoolean("track-list/$i/selected") == true) chosen = ids.size - 1
    }
    if (type == "sub" && chosen < 0) chosen = 0
    AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
      .setTitle(if (type == "audio") "Audio" else "Subtitles")
      .setSingleChoiceItems(names.toTypedArray(), chosen) { d, which ->
        val prop = if (type == "audio") "aid" else "sid"
        val id = ids[which]
        if (id < 0) mpv.setPropertyString(prop, "no") else mpv.setPropertyInt(prop, id)
        d.dismiss()
      }
      .show()
  }

  // ── mpv's events (on its own thread) ────────────────────────────────

  override fun eventProperty(property: String) {}

  override fun eventProperty(property: String, value: Long) {
    if (property == "chapter") main.post { onChapter(value.toInt()) }
  }

  override fun eventProperty(property: String, value: Double) {
    main.post {
      when (property) {
        "time-pos" -> {
          position = value
          if (!dragging && duration > 0) {
            seek.progress = (value / duration * 1000).toInt()
            time.text = clock(value)
            left.text = "-" + clock(duration - value)
          }
        }
        "duration" -> duration = value
      }
    }
  }

  override fun eventProperty(property: String, value: Boolean) {
    main.post {
      when (property) {
        "pause" -> {
          paused = value
          playButton.text = if (value) "▶" else "❚❚"
          if (value) showControls(true) else poke()
        }
        "paused-for-cache" -> spinner.visibility = if (value) View.VISIBLE else View.GONE
        "eof-reached" -> if (value && loaded) {
          ended = true
          close(if (intent.getBooleanExtra("hasNext", false)) "auto" else null)
        }
      }
    }
  }

  override fun eventProperty(property: String, value: String) {}

  override fun event(eventId: Int) {
    if (eventId == MPVLib.MpvEvent.MPV_EVENT_FILE_LOADED) main.post { loaded = true }
  }

  override fun eventEndFile(reason: Int, error: Int) {
    // Couldn't play it at all.
    if (error < 0 && !loaded) main.post { close(null, "mpv couldn't play it (error $error)") }
  }

  // ── The surface ─────────────────────────────────────────────────────

  override fun surfaceCreated(holder: SurfaceHolder) {
    mpv.attachSurface(holder.surface)
    mpv.setOptionString("force-window", "yes")
    if (!loaded) {
      mpv.command(arrayOf("loadfile", intent.getStringExtra("url") ?: ""))
    } else {
      mpv.setPropertyString("vo", "gpu")
    }
  }

  override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
    mpv.setPropertyString("android-surface-size", "${width}x$height")
  }

  override fun surfaceDestroyed(holder: SurfaceHolder) {
    mpv.setPropertyString("vo", "null")
    mpv.setOptionString("force-window", "no")
    mpv.detachSurface()
  }

  override fun onPause() {
    super.onPause()
    if (!isFinishing) mpv.setPropertyBoolean("pause", true)
  }

  private var closed = false

  /** Done: how far it got, and what next. */
  private fun close(then: String?, error: String? = null) {
    if (closed) return
    closed = true
    val result = Intent().apply {
      putExtra("position", position)
      putExtra("duration", duration)
      putExtra("ended", ended)
      putExtra("then", then)
      putExtra("error", error)
    }
    setResult(RESULT_OK, result)
    finish()
  }

  override fun onDestroy() {
    main.removeCallbacksAndMessages(null)
    if (::mpv.isInitialized) {
      mpv.removeObserver(this)
      mpv.destroy()
    }
    super.onDestroy()
  }
}
