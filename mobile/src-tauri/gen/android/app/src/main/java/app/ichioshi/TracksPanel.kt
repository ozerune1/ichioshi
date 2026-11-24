package app.ichioshi

import android.annotation.SuppressLint
import android.content.Context
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityManager
import android.view.accessibility.AccessibilityNodeInfo
import android.view.animation.AccelerateInterpolator
import android.view.animation.DecelerateInterpolator
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.util.Locale
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** What the tracks panel changes (the activity tells mpv). */
interface TrackControl {
  fun chooseAudio(id: Int)
  /** [id] null: subtitles off. */
  fun chooseSubtitles(id: Int?)
  fun subDelay(seconds: Double)
  fun subScale(percent: Int)
}

/**
 * The Audio & subtitles panel, inside the player (not a dialog): the audio
 * and subtitle tracks side by side, and subtitle delay and size below. A tap
 * outside it ([onClose]) closes it.
 */
@SuppressLint("ViewConstructor")
class TracksPanel(context: Context, private val kit: Kit, private val control: TrackControl) : FrameLayout(context) {
  var onClose: () -> Unit = {}

  /** A column that's no taller than [maxH]. */
  private class Box(context: Context) : LinearLayout(context) {
    var maxH = 0

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
      var spec = heightMeasureSpec
      if (maxH > 0) {
        val mode = MeasureSpec.getMode(spec)
        val size = MeasureSpec.getSize(spec)
        spec = if (mode == MeasureSpec.UNSPECIFIED) MeasureSpec.makeMeasureSpec(maxH, MeasureSpec.AT_MOST)
        else MeasureSpec.makeMeasureSpec(min(size, maxH), mode)
      }
      super.onMeasure(widthMeasureSpec, spec)
    }
  }

  /**
   * A track list's scroll: when there's more than fits, a thin bar shows it
   * scrolls, and the last row in view is cut clearly (about two thirds of it
   * shows, never a sliver that looks like nothing, nor nearly all of it).
   */
  private class ListScroll(context: Context, private val kit: Kit) : ScrollView(context) {
    init {
      isFillViewport = false
      isVerticalScrollBarEnabled = true
      isScrollbarFadingEnabled = false
      scrollBarStyle = SCROLLBARS_OUTSIDE_OVERLAY
      scrollBarSize = kit.px(3)
      verticalScrollbarThumbDrawable = kit.rounded(Colors.SCROLL, 1.5f)
      setPadding(0, 0, kit.px(6), 0)
      isVerticalFadingEdgeEnabled = true
      setFadingEdgeLength(kit.px(8))
      overScrollMode = OVER_SCROLL_IF_CONTENT_SCROLLS
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
      super.onMeasure(widthMeasureSpec, heightMeasureSpec)
      val content = getChildAt(0) ?: return
      val room = measuredHeight - paddingTop - paddingBottom
      if (content.measuredHeight <= room) return
      val rows = mutableListOf<IntArray>()
      rowsOf(content, 0, rows)
      val k = rows.indexOfFirst { it[1] > room }
      if (k < 0) return
      val h = rows[k][1] - rows[k][0]
      val shown = room - rows[k][0]
      // Cut the row that's barely in view's neighbour above, or the one nearly all in view.
      var j = when {
        shown < 0.35 * h -> k - 1
        shown > 0.8 * h -> k
        else -> return
      }
      // Not a heading or a line: a row.
      while (j >= 0 && rows[j][1] - rows[j][0] < kit.px(40)) j--
      if (j < 0) return
      val cut = rows[j][0] + (0.62 * (rows[j][1] - rows[j][0])).toInt()
      if (cut < kit.px(40) || cut >= room) return
      setMeasuredDimension(measuredWidth, cut + paddingTop + paddingBottom)
    }

    /** Each row's top and bottom (px from [v]'s top, plus [y]), the rows of nested lists included. */
    private fun rowsOf(v: View, y: Int, out: MutableList<IntArray>) {
      val group = v as? LinearLayout ?: return
      var at = y + group.paddingTop
      for (i in 0 until group.childCount) {
        val c = group.getChildAt(i)
        if (c.visibility == GONE) continue
        val lp = c.layoutParams as? MarginLayoutParams
        at += lp?.topMargin ?: 0
        if (c is LinearLayout && c.orientation == LinearLayout.VERTICAL && c.childCount > 0) {
          rowsOf(c, at, out)
        } else {
          out += intArrayOf(at, at + c.measuredHeight)
        }
        at += c.measuredHeight + (lp?.bottomMargin ?: 0)
      }
    }
  }

  private val catcher = View(context)
  private val panel = Box(context)
  private val body = FrameLayout(context)
  private val audioList = LinearLayout(context)
  private val subList = LinearLayout(context)
  private val audioHead = heading("AUDIO")
  private val subHead = heading("SUBTITLES")
  private val footer = LinearLayout(context)
  private lateinit var delayValue: TextView
  private lateinit var scaleValue: TextView
  private val steppers = mutableListOf<View>()
  private val audioRows = mutableListOf<TrackRow>()
  private val subRows = mutableListOf<TrackRow>()
  /** The tracks the rows show (their ids, names and details): rows are made again only when these change. */
  private var shownTracks: String? = null
  private val access = context.getSystemService(AccessibilityManager::class.java)

  private companion object {
    /** The subtitles' Off row. */
    const val OFF = "sub off"
  }

  /** Subtitle delay (seconds, this playback only). */
  var delay = 0.0
    private set

  /** Subtitle size (percent). */
  var scale = 100
    private set

  /** It's open (or opening). */
  var isOpen = false
    private set
  private var closing = false
  private var docked = false
  private var stacked: Boolean? = null
  private var subsOff = true
  private var lastW = 0
  private var lastH = 0
  private var padR = 0
  private var top = 0
  private var padB = 0

  init {
    visibility = INVISIBLE
    clipChildren = false
    catcher.isClickable = true
    catcher.importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    catcher.setOnClickListener { onClose() }
    // Touches only: the D-pad and keyboards go to the rows, not to the whole picture.
    catcher.isFocusable = false
    addView(catcher, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))

    panel.orientation = LinearLayout.VERTICAL
    panel.background = kit.rounded(Colors.SURFACE, 16f, 1f, Colors.LINE)
    panel.setPadding(kit.px(16), kit.px(12), kit.px(16), kit.px(12))
    // Takes the touches that miss a row (they'd reach the catcher and close it), but never focus.
    panel.isClickable = true
    panel.isFocusable = false
    panel.accessibilityPaneTitle = "Audio and subtitles"
    audioList.orientation = LinearLayout.VERTICAL
    subList.orientation = LinearLayout.VERTICAL

    footer.orientation = LinearLayout.VERTICAL
    footer.addView(View(context).apply { setBackgroundColor(Colors.LINE) },
      LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, kit.px(1)).apply {
        topMargin = kit.px(4)
        bottomMargin = kit.px(4)
      })
    footer.addView(stepRow("Subtitle delay", "Subtitles earlier", "Subtitles later",
      { stepDelay(-0.1) }, { stepDelay(0.1) }, { setDelay(0.0) }) { delayValue = it })
    footer.addView(stepRow("Subtitle size", "Smaller subtitles", "Larger subtitles",
      { stepScale(-10) }, { stepScale(10) }, { setScale(100) }) { scaleValue = it })

    panel.addView(body, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
    panel.addView(footer, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT))
    addView(panel, LayoutParams(kit.px(400), LayoutParams.MATCH_PARENT, Gravity.TOP or Gravity.END))
    showValues()
    setSubsOff(true)
  }

  private fun heading(text: String) = kit.text(11f, Colors.FAINT_T, kit.bold).apply {
    this.text = text
    letterSpacing = 0.06f
    gravity = Gravity.CENTER_VERTICAL
    importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
  }

  private fun stepRow(
    label: String,
    less: String,
    more: String,
    onLess: () -> Unit,
    onMore: () -> Unit,
    onReset: () -> Unit,
    value: (TextView) -> Unit,
  ): View {
    val row = LinearLayout(context).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
    }
    row.addView(kit.text(13f, Colors.MUTED, kit.medium).apply { text = label },
      LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
    fun button(icon: Int, desc: String, onClick: () -> Unit) = kit.round(48f, 40f, Colors.SURFACE2, 1f, Colors.LINE).also { r ->
      r.disc.addView(kit.icon(icon, 18f, Colors.TEXT))
      r.view.contentDescription = desc
      r.view.setOnClickListener { onClick() }
      steppers += r.view
    }.view
    row.addView(button(R.drawable.ic_minus, less, onLess), LinearLayout.LayoutParams(kit.px(48), kit.px(48)))
    val v = kit.text(14f, Colors.TEXT, kit.semibold, tnum = true).apply {
      gravity = Gravity.CENTER
      minWidth = kit.px(64)
      isLongClickable = true
      setOnLongClickListener {
        onReset()
        true
      }
    }
    value(v)
    row.addView(v, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT))
    row.addView(button(R.drawable.ic_plus, more, onMore), LinearLayout.LayoutParams(kit.px(48), kit.px(48)))
    row.layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, kit.px(44))
    return row
  }

  private fun stepDelay(by: Double) = setDelay(delay + by)

  private fun setDelay(v: Double) {
    val d = ((v * 10).roundToInt() / 10.0).coerceIn(-30.0, 30.0)
    if (d == delay) return
    delay = d
    control.subDelay(d)
    showValues()
  }

  private fun stepScale(by: Int) = setScale(scale + by)

  /** Subtitle size, [v] percent (80 to 150). */
  fun setScale(v: Int, tell: Boolean = true) {
    val s = v.coerceIn(80, 150)
    if (s == scale && tell) return
    scale = s
    if (tell) control.subScale(s)
    showValues()
  }

  private fun showValues() {
    val tenths = (delay * 10).roundToInt()
    val abs = String.format(Locale.ROOT, "%.1f", abs(tenths) / 10.0)
    delayValue.text = when {
      tenths > 0 -> "+$abs s"
      tenths < 0 -> "−$abs s"
      else -> "0.0 s"
    }
    delayValue.contentDescription = when {
      tenths > 0 -> "Subtitle delay plus $abs seconds"
      tenths < 0 -> "Subtitle delay minus $abs seconds"
      else -> "Subtitle delay 0 seconds"
    }
    scaleValue.text = "$scale%"
    scaleValue.contentDescription = "Subtitle size $scale percent"
  }

  private fun setSubsOff(off: Boolean) {
    subsOff = off
    footer.getChildAt(1)?.alpha = if (off) 0.45f else 1f
    footer.getChildAt(2)?.alpha = if (off) 0.45f else 1f
    for (s in steppers) {
      s.isEnabled = !off
      s.isClickable = !off
    }
    delayValue.isLongClickable = !off
    scaleValue.isLongClickable = !off
  }

  /**
   * The tracks to choose from (mpv's track-list). A new choice changes the
   * rows in place, so the row you chose keeps the D-pad's and TalkBack's
   * focus; they're made again only when the tracks themselves change.
   */
  fun setTracks(tracks: List<Track>) {
    val audio = tracks.filter { it.type == "audio" }
    val subs = tracks.filter { it.type == "sub" }
    val shown = (audio + subs).joinToString("\n") { "${it.type} ${it.id} ${it.name} ${it.detail}" }
    if (shown != shownTracks) {
      shownTracks = shown
      makeRows(audio, subs)
    }
    val chosen = tracks.filter { it.selected }.map { key(it) }.toSet()
    val off = subs.none { it.selected }
    for (r in audioRows) r.setChosen(r.key in chosen)
    for (r in subRows) r.setChosen(if (r.key == OFF) off else r.key in chosen)
    setSubsOff(off)
  }

  private fun key(t: Track) = "${t.type} ${t.id}"

  private fun makeRows(audio: List<Track>, subs: List<Track>) {
    val old = audioRows + subRows
    // Focus on a row goes to its new row.
    val focused = old.firstOrNull { it.view.isFocused }?.key
    val spoken = if (access?.isEnabled == true) {
      old.firstOrNull { it.view.createAccessibilityNodeInfo().isAccessibilityFocused }?.key
    } else null
    audioList.removeAllViews()
    subList.removeAllViews()
    audioRows.clear()
    subRows.clear()
    if (audio.isEmpty()) audioRows += TrackRow("audio none", "No audio", "", "audio track", false) {}
    for (t in audio) {
      audioRows += TrackRow(key(t), t.name, t.detail, "audio track", true) { r ->
        choose(audioRows, r)
        control.chooseAudio(t.id)
      }
    }
    subRows += TrackRow(OFF, "Off", "", "subtitles", true) { r ->
      choose(subRows, r)
      setSubsOff(true)
      control.chooseSubtitles(null)
    }
    for (t in subs) {
      subRows += TrackRow(key(t), t.name, t.detail, "subtitles", true) { r ->
        choose(subRows, r)
        setSubsOff(false)
        control.chooseSubtitles(t.id)
      }
    }
    for (r in audioRows) audioList.addView(r.view)
    for (r in subRows) subList.addView(r.view)
    val rows = audioRows + subRows
    focused?.let { k -> rows.firstOrNull { it.key == k }?.view?.requestFocus() }
    spoken?.let { k ->
      rows.firstOrNull { it.key == k }?.view?.let { v ->
        v.post { v.performAccessibilityAction(AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS, null) }
      }
    }
  }

  /** [row] is chosen in its list now (before mpv says so), so TalkBack's click says "checked". */
  private fun choose(list: List<TrackRow>, row: TrackRow) {
    for (r in list) r.setChosen(r === row)
  }

  /** A track row: a check, the track's name and its details. [setChosen] changes it in place. */
  private inner class TrackRow(
    val key: String,
    line1: String,
    line2: String,
    kind: String,
    enabled: Boolean,
    onTap: (TrackRow) -> Unit,
  ) {
    val view = LinearLayout(context)
    private val fill = kit.rounded(Colors.TRANSPARENT, 10f)
    private val check = kit.icon(R.drawable.ic_check, 18f, Colors.ACCENT)
    private val name = kit.text(14f, Colors.MUTED, kit.medium).apply { text = line1 }

    var chosen = false
      private set

    init {
      view.apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        minimumHeight = kit.px(48)
        setPadding(kit.px(10), kit.px(6), kit.px(10), kit.px(6))
        background = kit.pressable(fill, 10f, Colors.PRESS_ROW)
        isClickable = enabled
        isFocusable = enabled
        if (!enabled) alpha = 0.45f
        setOnClickListener { if (enabled) onTap(this@TrackRow) }
        contentDescription = (listOf(line1) + line2.split(" · ").filter { it.isNotBlank() } + kind).joinToString(", ")
        accessibilityDelegate = object : View.AccessibilityDelegate() {
          override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
            super.onInitializeAccessibilityNodeInfo(host, info)
            info.className = "android.widget.RadioButton"
            info.isCheckable = true
            @Suppress("DEPRECATION")
            info.isChecked = chosen
          }

          override fun onInitializeAccessibilityEvent(host: View, event: AccessibilityEvent) {
            super.onInitializeAccessibilityEvent(host, event)
            event.className = "android.widget.RadioButton"
            event.isChecked = chosen
          }
        }
      }
      view.addView(check, LinearLayout.LayoutParams(kit.px(18), kit.px(18)))
      val lines = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
      lines.addView(name)
      if (line2.isNotEmpty()) {
        lines.addView(kit.text(12f, Colors.FAINT_T, kit.medium).apply { text = line2 },
          LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
            topMargin = kit.px(2)
          })
      }
      view.addView(lines, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply { leftMargin = kit.px(10) })
      view.layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
        bottomMargin = kit.px(2)
      }
      show()
    }

    fun setChosen(on: Boolean) {
      if (on == chosen) return
      chosen = on
      show()
      // TalkBack reads the row again (checked or not).
      if (access?.isEnabled == true) view.sendAccessibilityEvent(AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED)
    }

    private fun show() {
      fill.setColor(if (chosen) Colors.ACCENT_10 else Colors.TRANSPARENT)
      check.visibility = if (chosen) View.VISIBLE else View.INVISIBLE
      name.setTextColor(if (chosen) Colors.TEXT else Colors.MUTED)
      name.typeface = if (chosen) kit.semibold else kit.medium
    }
  }

  private fun detach(v: View) {
    (v.parent as? ViewGroup)?.removeView(v)
  }

  private fun scroll(content: View) = ListScroll(context, kit).apply {
    addView(content, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
  }

  /** The columns side by side, or (a narrow panel) one after the other in one scroll. */
  private fun arrange(stack: Boolean) {
    if (stacked == stack) return
    stacked = stack
    for (v in listOf(audioList, subList, audioHead, subHead, footer)) detach(v)
    body.removeAllViews()
    panel.removeView(footer)
    val headH = kit.px(28)
    if (!stack) {
      val row = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL }
      for ((i, pair) in listOf(audioHead to audioList, subHead to subList).withIndex()) {
        val col = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        col.addView(pair.first, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, headH))
        col.addView(scroll(pair.second), LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        row.addView(col, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.MATCH_PARENT, 1f).apply {
          if (i == 1) leftMargin = kit.px(12)
        })
      }
      body.addView(row, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
      panel.addView(footer, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT))
    } else {
      val all = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
      all.addView(audioHead, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, headH))
      all.addView(audioList)
      all.addView(subHead, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, headH))
      all.addView(subList)
      all.addView(footer)
      body.addView(scroll(all), LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
    }
  }

  /**
   * Where it goes in a [w] x [h] px window with these edge paddings (px).
   * [top]: where the room under the top bar starts (the bar's bottom + 8 dp).
   */
  fun place(w: Int, h: Int, padL: Int, padR: Int, top: Int, padB: Int) {
    lastW = w
    lastH = h
    fitValues()
    this.padR = padR
    this.top = top
    this.padB = padB
    docked = kit.toDp(w) < 600
    catcher.layoutParams = LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT).apply {
      topMargin = top
    }
    if (!docked) {
      panel.maxH = 0
      panel.layoutParams = LayoutParams(panelWidth(), LayoutParams.MATCH_PARENT, Gravity.TOP or Gravity.END).apply {
        rightMargin = padR
        topMargin = top
        bottomMargin = padB
      }
      arrange(false)
    } else {
      panel.maxH = (0.6 * h).toInt()
      val width = w - padL - padR
      panel.layoutParams = LayoutParams(width, LayoutParams.WRAP_CONTENT, Gravity.BOTTOM or Gravity.START).apply {
        leftMargin = padL
        bottomMargin = padB
      }
      // Side by side only when each list gets room for two and a half rows; else one scroll.
      footer.measure(MeasureSpec.makeMeasureSpec(width, MeasureSpec.AT_MOST), MeasureSpec.makeMeasureSpec(0, MeasureSpec.UNSPECIFIED))
      val listRoom = panel.maxH - panel.paddingTop - panel.paddingBottom - kit.px(28) - footer.measuredHeight
      arrange(kit.toDp(width - kit.px(32)) < 400 || listRoom < kit.px(125))
    }
  }

  /**
   * Both values as wide as the widest either gets ("−30.0 s", "150%") at
   * the text size now, so the two rows' buttons line up and never move.
   */
  private fun fitValues() {
    val widest = max(delayValue.paint.measureText("−30.0 s"), scaleValue.paint.measureText("150%"))
    val w = max(kit.px(64), ceil(widest).toInt() + kit.px(8))
    for (v in listOf(delayValue, scaleValue)) {
      val lp = v.layoutParams as LinearLayout.LayoutParams
      if (lp.width != w) v.layoutParams = lp.apply { width = w }
    }
  }

  /** Its width on a wide screen (px). */
  fun panelWidth(): Int = (0.48 * lastW).toInt().coerceIn(kit.px(360), kit.px(480))

  /** It sits at the bottom (a narrow window). */
  val isDocked get() = docked

  /** mpv's video-margin-ratio right, top and bottom: the picture goes in the room left. */
  fun margins(): Triple<Double, Double, Double> {
    if (lastW <= 0 || lastH <= 0) return Triple(0.0, 0.0, 0.0)
    val topRatio = top.toDouble() / lastH
    return if (!docked) {
      Triple((panelWidth() + padR + kit.px(8)).toDouble() / lastW, topRatio, padB.toDouble() / lastH)
    } else {
      val lp = panel.layoutParams
      panel.measure(
        MeasureSpec.makeMeasureSpec(lp.width, MeasureSpec.EXACTLY),
        MeasureSpec.makeMeasureSpec(panel.maxH, MeasureSpec.AT_MOST),
      )
      Triple(0.0, topRatio, (panel.measuredHeight + padB + kit.px(8)).toDouble() / lastH)
    }
  }

  fun open() {
    if (isOpen && !closing) return
    isOpen = true
    closing = false
    visibility = VISIBLE
    panel.animate().cancel()
    panel.alpha = 0f
    if (docked) panel.translationY = kit.dp(24) else panel.translationX = kit.dp(24)
    panel.animate().alpha(1f).translationX(0f).translationY(0f).setStartDelay(0)
      .setDuration(kit.ms(220)).setInterpolator(DecelerateInterpolator()).withEndAction(null).start()
  }

  /** Slide it out, then [after]. */
  fun close(after: () -> Unit) {
    if (!isOpen || closing) return
    closing = true
    panel.animate().cancel()
    panel.animate().alpha(0f).translationX(if (docked) 0f else kit.dp(24)).translationY(if (docked) kit.dp(24) else 0f)
      .setStartDelay(0).setDuration(kit.ms(180)).setInterpolator(AccelerateInterpolator())
      .withEndAction {
        visibility = INVISIBLE
        isOpen = false
        closing = false
        after()
      }.start()
  }

  fun release() {
    panel.animate().cancel()
  }
}
