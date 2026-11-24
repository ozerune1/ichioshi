package app.ichioshi

import android.annotation.SuppressLint
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.drawable.ClipDrawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.InsetDrawable
import android.os.SystemClock
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import kotlin.math.ceil

/**
 * What the moment button offers: "Skip opening", "Next episode"... [rule]
 * is the rule that gave it (the spec's numbers, 1 to 6).
 */
class Moment(val rule: Int, val label: String) {
  /** What TalkBack says. */
  val spoken: String get() = if (rule == 6) "Skip 85 seconds" else label
}

/** What the moment button offers now (a watch party member's player never asks). */
fun moment(ch: Chapters, cur: Int, position: Double, hasNext: Boolean, controlsUp: Boolean): Moment? {
  val d = ch.duration
  if (ch.named) {
    val kind = ch.kind(cur)
    return when {
      kind == Part.OPENING -> Moment(1, "Skip opening")
      (kind == Part.ENDING || kind == Part.PREVIEW) && ch.inTail(cur) && hasNext -> Moment(2, "Next episode")
      kind == Part.ENDING && ch.last(cur) && !hasNext -> Moment(3, "Skip ending")
      kind == Part.ENDING -> Moment(4, "Skip ending")
      else -> null
    }
  }
  if (!controlsUp || d <= 0) return null
  if (hasNext && d - position <= 120) return Moment(5, "Next episode")
  if (position < d / 2 && d - position > 85) return Moment(6, "Skip 85s")
  return null
}

/**
 * The moment button at the bottom right ("Skip opening ⏭"), and during an
 * auto-skip countdown, "Watch opening" beside it. The countdown runs on
 * playback time: it's paused while playback is.
 */
@SuppressLint("ViewConstructor")
class MomentLayer(context: Context, private val kit: Kit) : LinearLayout(context) {
  var onPress: () -> Unit = {}
  var onWatch: () -> Unit = {}
  var onDone: () -> Unit = {}

  private val label = kit.text(15f, Colors.WHITE, kit.semibold)
  private val fill = ClipDrawable(kit.rounded(Colors.COUNTDOWN, 8f), Gravity.LEFT, ClipDrawable.HORIZONTAL)

  /** The moment button. */
  val button = LinearLayout(context)

  /** "Watch opening": don't skip. */
  val watch: TextView = kit.text(14f, Colors.WHITE, kit.medium)

  private var total = 0L
  private var elapsed = 0L
  private var runningSince = -1L
  private val tick = object : Runnable {
    override fun run() {
      if (runningSince < 0 || total <= 0) return
      val done = elapsed + (SystemClock.uptimeMillis() - runningSince)
      fill.level = (done * 10000 / total).toInt().coerceIn(0, 10000)
      if (done >= total) {
        stopCountdown()
        onDone()
      } else {
        postOnAnimation(this)
      }
    }
  }

  /** A countdown is on (running or paused). */
  val counting: Boolean get() = total > 0

  init {
    orientation = HORIZONTAL
    gravity = Gravity.CENTER_VERTICAL or Gravity.END
    clipChildren = false

    watch.apply {
      gravity = Gravity.CENTER
      val shape = kit.rounded(Colors.PILL_FILL, 8f)
      shape.setColor(
        ColorStateList(
          arrayOf(intArrayOf(android.R.attr.state_pressed), intArrayOf()),
          intArrayOf(Colors.PILL_PRESSED, Colors.PILL_FILL),
        ),
      )
      background = InsetDrawable(kit.pressable(shape, 8f), 0, kit.px(2), 0, kit.px(2))
      // After the background, whose insets would replace it.
      setPadding(kit.px(16), 0, kit.px(16), 0)
      isClickable = true
      isFocusable = true
      visibility = GONE
      setOnClickListener { onWatch() }
    }
    addView(watch, LayoutParams(LayoutParams.WRAP_CONTENT, kit.px(48)).apply { rightMargin = kit.px(8) })

    button.apply {
      orientation = HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
      val shape = GradientDrawable().apply {
        cornerRadius = kit.dp(8)
        setColor(
          ColorStateList(
            arrayOf(intArrayOf(android.R.attr.state_pressed), intArrayOf()),
            intArrayOf(Colors.PILL_PRESSED, Colors.PILL_FILL),
          ),
        )
      }
      val border = kit.rounded(Colors.TRANSPARENT, 8f, 2f, Colors.PILL_STROKE)
      // The focus ring over the border, which would hide it.
      background = InsetDrawable(kit.pressable(shape, 8f, over = listOf(fill, border)), 0, kit.px(2), 0, kit.px(2))
      setPadding(kit.px(18), 0, kit.px(18), 0)
      isClickable = true
      isFocusable = true
      accessibilityLiveRegion = ACCESSIBILITY_LIVE_REGION_POLITE
      addView(label, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
      addView(
        kit.icon(R.drawable.ic_skip_forward, 18f, Colors.WHITE),
        LayoutParams(kit.px(18), kit.px(18)).apply { leftMargin = kit.px(10) },
      )
      setOnClickListener { onPress() }
    }
    addView(button, LayoutParams(LayoutParams.WRAP_CONTENT, kit.px(48)))
    fill.level = 0
  }

  /** What the button says (not during a countdown). */
  fun setLabel(text: String, spoken: String) {
    if (counting) return
    if (label.text.toString() != text) label.text = text
    button.contentDescription = spoken
  }

  /**
   * Count down [ms] of playback, then [onDone]. The button says [text];
   * "Watch" beside it ([watchText]) cancels.
   */
  fun startCountdown(ms: Long, text: String, spoken: String, watchText: String, watchSpoken: String) {
    stopCountdown()
    label.text = text
    button.contentDescription = spoken
    watch.text = watchText
    watch.contentDescription = watchSpoken
    watch.visibility = VISIBLE
    total = ms.coerceAtLeast(1)
    elapsed = 0
    fill.level = 0
    runningSince = SystemClock.uptimeMillis()
    postOnAnimation(tick)
  }

  /** Pause or go on with the countdown (it runs on playback time). */
  fun runCountdown(on: Boolean) {
    if (!counting) return
    if (on && runningSince < 0) {
      runningSince = SystemClock.uptimeMillis()
      postOnAnimation(tick)
    } else if (!on && runningSince >= 0) {
      elapsed += SystemClock.uptimeMillis() - runningSince
      runningSince = -1
      removeCallbacks(tick)
    }
  }

  /** End the countdown (cancelled, or done). */
  fun stopCountdown() {
    removeCallbacks(tick)
    total = 0
    elapsed = 0
    runningSince = -1
    fill.level = 0
    watch.visibility = GONE
  }

  /**
   * Its width at its widest, a countdown ("Watch opening" and "Skipping
   * opening"), at the text size now: where it goes doesn't change with
   * what it says.
   */
  fun widestWidth(): Int {
    val w = watch.paint.measureText("Watch opening") + kit.px(16 + 16 + 8)
    val b = label.paint.measureText("Skipping opening") + kit.px(18 + 10 + 18 + 18)
    return ceil(w + b).toInt()
  }

  /** Its width when shown, for the room the timeline leaves it. */
  fun wantedWidth(): Int {
    measure(
      MeasureSpec.makeMeasureSpec(0, MeasureSpec.UNSPECIFIED),
      MeasureSpec.makeMeasureSpec(kit.px(48), MeasureSpec.EXACTLY),
    )
    return measuredWidth
  }

  fun release() {
    removeCallbacks(tick)
  }

  override fun setVisibility(visibility: Int) {
    super.setVisibility(visibility)
    if (visibility != View.VISIBLE) button.isPressed = false
  }
}
