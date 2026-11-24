package app.ichioshi

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.accessibility.AccessibilityManager
import android.view.accessibility.AccessibilityNodeInfo
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * The timeline: a segment per chapter (openings and endings in amber), what
 * a stream has buffered, and where playback is. Drag or tap it to seek; a
 * watch party member's is read-only.
 */
@SuppressLint("ViewConstructor")
class Timeline(context: Context, private val kit: Kit, private val member: Boolean) : View(context) {
  /** What dragging it does. */
  interface Listener {
    fun scrubStart(t: Double)
    fun scrub(t: Double)
    fun scrubEnd(t: Double)
    /** The drag was taken away (by a system gesture, say): go back to [before], where playback was. */
    fun scrubCancel(before: Double)
    /** TalkBack asked for a time. */
    fun seekTo(t: Double)
    /** A member touched it. */
    fun memberTouch()
  }

  var listener: Listener? = null

  private class Segment(val start: Double, val end: Double, val amber: Boolean)

  private var chapters = Chapters(emptyList(), 0.0)
  private var segments = listOf<Segment>()
  private var position = 0.0
  private var cacheEnd = -1.0
  private var holdT = -1.0
  private var holdUntil = 0L
  private var grow = 0f
  private var growAnim: ValueAnimator? = null
  private var dragChapter = -1
  private val slop = ViewConfiguration.get(context).scaledTouchSlop
  /** A finger is down below [waitBelow], not dragging yet. */
  private var waiting = false
  private var downX = 0f
  private var downY = 0f
  /** Where playback was when the finger went down. */
  private var before = 0.0
  /** This touch seeks only if it drags: a tap does nothing. */
  private var dragOnly = false
  private val access = context.getSystemService(AccessibilityManager::class.java)

  /**
   * A finger that lands at or below this y (px, in this view) waits until it
   * moves sideways before it seeks: it may be the start of the system's swipe
   * up from the bottom edge (home, or bringing the bars back).
   */
  var waitBelow = Float.MAX_VALUE

  /** The thumb shows (the controls are up). */
  var thumb = false
    set(v) {
      if (field != v) {
        field = v
        invalidate()
      }
    }

  /** A finger is on it. */
  var dragging = false
    private set

  /** The time under the finger. */
  var dragT = 0.0
    private set

  private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
  private val thumbPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
    color = Colors.WHITE
    setShadowLayer(kit.dp(4), 0f, kit.dp(1), 0x4D000000)
  }
  private val ringPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
    style = Paint.Style.STROKE
    strokeWidth = kit.dp(2)
    color = Colors.ACCENT
  }
  private val path = Path()
  private val rect = RectF()
  private val inset = kit.dp(9)

  init {
    contentDescription = if (member) "Timeline. The host controls playback" else "Timeline"
    isFocusable = !member
    // onDraw draws the focus ring; no grey square over the whole view.
    defaultFocusHighlightEnabled = false
    importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
  }

  val duration get() = chapters.duration

  fun setChapters(c: Chapters) {
    chapters = c
    val d = c.duration
    val list = c.list
    segments = if (d <= 0 || list.isEmpty()) {
      listOf(Segment(0.0, max(d, 0.0), false))
    } else {
      val out = mutableListOf<Segment>()
      if (list[0].start > 0.5) out += Segment(0.0, min(list[0].start, d), false)
      for (i in list.indices) {
        val s = list[i].start.coerceIn(0.0, d)
        val e = c.end(i).coerceIn(0.0, d)
        val amber = list[i].part == Part.OPENING || list[i].part == Part.ENDING
        if (e > s) out += Segment(s, e, amber)
      }
      out.ifEmpty { listOf(Segment(0.0, d, false)) }
    }
    invalidate()
  }

  /** Where playback is; [redraw] only while the controls show. */
  fun setPosition(p: Double, redraw: Boolean) {
    position = p
    if (holdT >= 0 && SystemClock.uptimeMillis() > holdUntil) holdT = -1.0
    if (redraw) invalidate()
  }

  /** How far a stream has buffered (seconds). */
  fun setCache(end: Double) {
    cacheEnd = end
    if (thumb) invalidate()
  }

  /** Stop drawing the time a seek went to: playback is there. */
  fun releaseHold() {
    if (holdT >= 0) {
      holdT = -1.0
      invalidate()
    }
  }

  /** The time drawn: the finger's, one just seeked to, or playback's. */
  val shown: Double
    get() = when {
      dragging -> dragT
      holdT >= 0 && SystemClock.uptimeMillis() < holdUntil -> holdT
      else -> position
    }

  /** Where [t] is drawn, in this view. */
  fun xOf(t: Double): Float {
    val d = chapters.duration
    if (d <= 0) return inset
    return inset + ((t / d).coerceIn(0.0, 1.0) * (width - 2 * inset)).toFloat()
  }

  private fun timeAt(x: Float): Double {
    val w = width - 2 * inset
    if (w <= 0) return 0.0
    return ((x - inset) / w).toDouble().coerceIn(0.0, 1.0) * chapters.duration
  }

  /**
   * What TalkBack says for [p]: "1 minute 5 seconds of 3 minutes, Part A".
   * It's read when TalkBack asks (not set as playback goes, which TalkBack
   * would speak every second).
   */
  private fun phrase(p: Double): String? {
    val d = chapters.duration
    if (d <= 0) return null
    val title = chapters.title(chapters.at(p))
    return buildString {
      append(spoken(p)).append(" of ").append(spoken(d))
      if (title.isNotBlank()) append(", ").append(title)
    }
  }

  /** Draw [t] (where a seek went) for up to 1 s. */
  private fun hold(t: Double) {
    holdT = t
    holdUntil = SystemClock.uptimeMillis() + 1000
    invalidate()
  }

  override fun onDraw(c: Canvas) {
    val d = chapters.duration
    val left = inset
    val right = width - inset
    if (right <= left) return
    val cy = height / 2f
    val h = if (member) kit.dp(3) else kit.dp(4) + kit.dp(4) * grow
    val t = shown
    val px = if (d > 0) xOf(t) else left
    val cache = if (d > 0 && cacheEnd > t) xOf(min(cacheEnd, d)) else px
    val gap = kit.dp(1.5f)
    val narrow = kit.dp(6)
    fun x(s: Double) = if (d > 0) xOf(s) else left + ((s / max(d, 1.0)).toFloat() * (right - left))
    val n = segments.size
    for (k in 0 until n) {
      val seg = segments[k]
      var x0 = if (d > 0) x(seg.start) else left
      var x1 = if (d > 0) x(seg.end) else right
      val wide = x1 - x0 >= narrow
      if (k > 0 && wide) {
        val prev = segments[k - 1]
        if (x(prev.end) - x(prev.start) >= narrow) x0 += gap
      }
      if (k < n - 1 && wide) {
        val next = segments[k + 1]
        if (x(next.end) - x(next.start) >= narrow) x1 -= gap
      }
      if (x1 - x0 < 0.5f) continue
      rect.set(x0, cy - h / 2, x1, cy + h / 2)
      path.reset()
      path.addRoundRect(rect, h / 2, h / 2, Path.Direction.CW)
      c.save()
      c.clipPath(path)
      paint.color = if (seg.amber) Colors.AMBER_45 else Colors.TRACK
      c.drawRect(rect, paint)
      val b0 = max(x0, px)
      val b1 = min(x1, cache)
      if (b1 > b0) {
        paint.color = if (seg.amber) Colors.AMBER_60 else Colors.BUFFERED
        c.drawRect(b0, rect.top, b1, rect.bottom, paint)
      }
      if (px > x0) {
        paint.color = if (seg.amber) Colors.AMBER else Colors.ACCENT
        c.drawRect(x0, rect.top, min(x1, px), rect.bottom, paint)
      }
      c.restore()
    }
    if (!member && thumb && d > 0) {
      c.drawCircle(px, cy, kit.dp(6) + kit.dp(3) * grow, thumbPaint)
    }
    if (isFocused) {
      rect.set(left - kit.dp(6), cy - kit.dp(12), right + kit.dp(6), cy + kit.dp(12))
      c.drawRoundRect(rect, kit.dp(12), kit.dp(12), ringPaint)
    }
  }

  override fun onFocusChanged(gainFocus: Boolean, direction: Int, previouslyFocusedRect: Rect?) {
    super.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
    invalidate()
  }

  override fun onLayout(changed: Boolean, left: Int, top: Int, right: Int, bottom: Int) {
    super.onLayout(changed, left, top, right, bottom)
    // A custom view doesn't get SeekBar's opt-out from the back gesture.
    if (!member) systemGestureExclusionRects = listOf(Rect(0, 0, width, height))
  }

  private fun growTo(v: Float) {
    growAnim?.cancel()
    growAnim = ValueAnimator.ofFloat(grow, v).apply {
      duration = kit.ms(100)
      addUpdateListener {
        grow = it.animatedValue as Float
        invalidate()
      }
      start()
    }
  }

  private fun begin(x: Float) {
    parent?.requestDisallowInterceptTouchEvent(true)
    dragging = true
    dragT = timeAt(x)
    dragChapter = chapters.at(dragT)
    growTo(1f)
    listener?.scrubStart(dragT)
  }

  private fun moveTo(x: Float) {
    val t = timeAt(x)
    if (t == dragT) return
    dragT = t
    val ch = chapters.at(t)
    if (ch != dragChapter) {
      dragChapter = ch
      performHapticFeedback(
        if (Build.VERSION.SDK_INT >= 34) HapticFeedbackConstants.SEGMENT_TICK
        else HapticFeedbackConstants.CLOCK_TICK,
      )
    }
    listener?.scrub(t)
  }

  @SuppressLint("ClickableViewAccessibility")
  override fun onTouchEvent(e: MotionEvent): Boolean {
    if (member) {
      if (e.actionMasked == MotionEvent.ACTION_DOWN) listener?.memberTouch()
      return true
    }
    val d = chapters.duration
    if (d <= 0) return false
    when (e.actionMasked) {
      MotionEvent.ACTION_DOWN -> {
        waiting = false
        before = position
        downX = e.x
        downY = e.y
        // With TalkBack, a double tap on the focused timeline arrives as a tap at its middle:
        // it mustn't seek there (a host's party would go too). A double tap and hold, then a drag, still seeks.
        dragOnly = access?.isTouchExplorationEnabled == true
        if (e.y >= waitBelow || dragOnly) {
          parent?.requestDisallowInterceptTouchEvent(true)
          waiting = true
        } else {
          begin(e.x)
        }
      }
      MotionEvent.ACTION_MOVE -> when {
        waiting -> {
          val dx = abs(e.x - downX)
          val dy = abs(e.y - downY)
          if (dy > slop && dy >= dx) {
            // A swipe up: the system's, not a seek (the rest of this touch does nothing).
            waiting = false
          } else if (dx > slop) {
            waiting = false
            begin(downX)
            moveTo(e.x)
          }
        }
        dragging -> moveTo(e.x)
      }
      MotionEvent.ACTION_UP -> {
        if (waiting) {
          // A tap: it jumps there, as it does higher up.
          waiting = false
          if (!dragOnly) begin(e.x)
        }
        if (dragging) {
          dragging = false
          // A drag never ends the episode by itself.
          val t = dragT.coerceIn(0.0, max(0.0, d - 2))
          hold(t)
          growTo(0f)
          listener?.scrubEnd(t)
        }
      }
      MotionEvent.ACTION_CANCEL -> {
        waiting = false
        if (dragging) {
          // Taken away (the system's gesture took the touch): it doesn't seek.
          dragging = false
          hold(before)
          growTo(0f)
          listener?.scrubCancel(before)
        }
      }
    }
    invalidate()
    return true
  }

  override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
    super.onInitializeAccessibilityNodeInfo(info)
    info.className = "android.widget.SeekBar"
    val d = chapters.duration.toFloat().coerceAtLeast(0f)
    val p = shown
    @Suppress("DEPRECATION")
    info.rangeInfo = AccessibilityNodeInfo.RangeInfo.obtain(
      AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_FLOAT, 0f, d, p.toFloat().coerceIn(0f, d),
    )
    // Now's time, when TalkBack asks for it.
    phrase(p)?.let { info.stateDescription = it }
    if (!member && d > 0) {
      info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_FORWARD)
      info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_BACKWARD)
      info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS)
    }
  }

  override fun performAccessibilityAction(action: Int, arguments: Bundle?): Boolean {
    val d = chapters.duration
    if (!member && d > 0) {
      val top = max(0.0, d - 2)
      val t = when (action) {
        AccessibilityNodeInfo.ACTION_SCROLL_FORWARD -> (shown + 10).coerceIn(0.0, top)
        AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD -> (shown - 10).coerceIn(0.0, top)
        // A click (TalkBack's double tap) doesn't seek.
        AccessibilityNodeInfo.ACTION_CLICK -> return true
        android.R.id.accessibilityActionSetProgress -> {
          val v = arguments?.getFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE) ?: return false
          v.toDouble().coerceIn(0.0, top)
        }
        else -> null
      }
      if (t != null) {
        listener?.seekTo(t)
        hold(t)
        // Said once, for the seek just asked for.
        stateDescription = phrase(t)
        return true
      }
    }
    return super.performAccessibilityAction(action, arguments)
  }

  /** Stop its animations (the player is closing). */
  fun release() {
    growAnim?.cancel()
  }
}
