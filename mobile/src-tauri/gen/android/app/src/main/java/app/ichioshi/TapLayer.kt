package app.ichioshi

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.os.SystemClock
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.ViewConfiguration
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * The picture's touches: a tap shows or hides the controls, a double tap on
 * a side seeks 10 s (more with each further tap), a pinch fills the screen
 * or fits the picture. It draws the double tap's feedback.
 */
@SuppressLint("ViewConstructor")
class TapLayer(context: Context, private val kit: Kit, private val member: Boolean) : View(context) {
  /** What the touches do (the activity does it). */
  interface Host {
    fun position(): Double
    fun duration(): Double
    /** Seek there, exactly: the user asked. */
    fun userSeek(t: Double)
    fun toggleControls()
    /** A seek step: keep the controls up a while longer if they are. */
    fun stepped()
    /** A member tried to seek. */
    fun memberSeek()
    /** A pinch: fill the screen (true) or fit the picture. */
    fun pinch(fill: Boolean)
  }

  var host: Host? = null

  /** Touches count (loaded, no card, no panel, no TalkBack). */
  var active = false
    set(v) {
      field = v
      if (!v) {
        removeCallbacks(single)
        pendingSide = 0
      }
    }

  private val slop = ViewConfiguration.get(context).scaledTouchSlop
  private val longPress = ViewConfiguration.getLongPressTimeout().toLong()
  private val doubleGap = 250L
  private val runLife = 700L

  private var downX = 0f
  private var downY = 0f
  private var downTime = 0L
  private var consumeUp = false
  private var multi = false
  private var pendingSide = 0
  private var pendingUpAt = 0L
  private var pendingX = 0f
  private var pendingY = 0f
  private val single = Runnable {
    pendingSide = 0
    host?.toggleControls()
  }

  /** A run of seek steps on one side. */
  private class Run(val side: Int, val startPos: Double) {
    var total = 0.0
    var lastStepAt = 0L
  }

  private var run: Run? = null
  private val endRun = Runnable { run = null }

  private var pinchScale = 1f
  private var pinched = false
  private val scale = ScaleGestureDetector(context, object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
    override fun onScaleBegin(detector: ScaleGestureDetector): Boolean {
      pinchScale = 1f
      pinched = false
      return true
    }

    override fun onScale(detector: ScaleGestureDetector): Boolean {
      pinchScale *= detector.scaleFactor
      if (!pinched && pinchScale > 1.08f) {
        pinched = true
        host?.pinch(true)
      } else if (!pinched && pinchScale < 0.92f) {
        pinched = true
        host?.pinch(false)
      }
      return true
    }
  }).apply { isQuickScaleEnabled = false }

  // The feedback.
  private var fbSide = 0
  private var fbLabel = ""
  private var fbTarget = ""
  private var fbAlpha = 0f
  private var fbAnim: ValueAnimator? = null
  private var labelScale = 1f
  private var labelAnim: ValueAnimator? = null
  private val fadeOut = Runnable { fadeFeedback(0f, 300) }
  private val oval = RectF()
  private val areaPaint = Paint(Paint.ANTI_ALIAS_FLAG)
  /** The dark pill behind the chevrons and the times, so they read on a bright frame. */
  private val backPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Colors.CAPTION }
  private val back = RectF()
  private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
    typeface = kit.semibold
    textSize = kit.sp(14f)
    color = Colors.WHITE
    textAlign = Paint.Align.CENTER
    fontFeatureSettings = "tnum"
    setShadowLayer(kit.dp(3), 0f, kit.dp(1), Colors.SHADOW)
  }
  private val targetPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
    typeface = kit.medium
    textSize = kit.sp(12f)
    color = Colors.WHITE
    textAlign = Paint.Align.CENTER
    fontFeatureSettings = "tnum"
    setShadowLayer(kit.dp(3), 0f, kit.dp(1), Colors.SHADOW)
  }
  private val chevron = context.getDrawable(R.drawable.ic_chevron_right)!!.mutate().apply { setTint(Colors.WHITE) }

  init {
    importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
  }

  /** -1 left, 0 center, 1 right. */
  private fun zone(x: Float): Int = when {
    x < width * 0.4f -> -1
    x > width * 0.6f -> 1
    else -> 0
  }

  private fun runAlive(): Boolean {
    val r = run ?: return false
    return SystemClock.uptimeMillis() - r.lastStepAt < runLife
  }

  @SuppressLint("ClickableViewAccessibility")
  override fun onTouchEvent(e: MotionEvent): Boolean {
    if (!active) return false
    scale.onTouchEvent(e)
    when (e.actionMasked) {
      MotionEvent.ACTION_DOWN -> {
        multi = false
        consumeUp = false
        downX = e.x
        downY = e.y
        downTime = e.eventTime
        val z = zone(e.x)
        if (runAlive()) {
          if (z != 0) {
            step(z)
            consumeUp = true
            return true
          }
          // The center ends the run; this is an ordinary tap.
          run = null
        }
        if (pendingSide != 0) {
          val second = z == pendingSide && e.eventTime - pendingUpAt <= doubleGap &&
            hypot(e.x - pendingX, e.y - pendingY) <= kit.dp(100)
          removeCallbacks(single)
          pendingSide = 0
          if (second) {
            if (member) host?.memberSeek() else step(z)
            consumeUp = true
          }
        }
        return true
      }
      MotionEvent.ACTION_POINTER_DOWN -> {
        multi = true
        removeCallbacks(single)
        pendingSide = 0
        run = null
      }
      MotionEvent.ACTION_UP -> {
        if (consumeUp || multi) return true
        val tap = hypot(e.x - downX, e.y - downY) <= slop && e.eventTime - downTime <= longPress
        if (!tap) return true
        val z = zone(e.x)
        if (z == 0) {
          host?.toggleControls()
        } else {
          pendingSide = z
          pendingUpAt = e.eventTime
          pendingX = e.x
          pendingY = e.y
          postDelayed(single, doubleGap)
        }
      }
      MotionEvent.ACTION_CANCEL -> {
        removeCallbacks(single)
        pendingSide = 0
      }
    }
    return true
  }

  /**
   * Seek 10 s back (-1) or on (1); steps on the same side add up, and the
   * seek goes to where the run started plus the total.
   */
  fun step(side: Int) {
    val h = host ?: return
    if (member) {
      h.memberSeek()
      return
    }
    val d = h.duration()
    if (d <= 0) return
    var r = run
    if (r == null || !runAlive() || r.side != side) {
      r = Run(side, h.position())
      run = r
    }
    r.total += 10.0 * side
    val target = (r.startPos + r.total).coerceIn(0.0, max(0.0, d - 2))
    h.userSeek(target)
    r.lastStepAt = SystemClock.uptimeMillis()
    removeCallbacks(endRun)
    postDelayed(endRun, runLife)
    val secs = (target - r.startPos).roundToInt()
    val label = (if (secs < 0) "−" else "+") + abs(secs) + " s"
    showFeedback(side, label, clock(target))
    h.stepped()
  }

  private fun showFeedback(side: Int, label: String, target: String) {
    val changed = fbSide == side && fbLabel != label && fbAlpha > 0f
    fbSide = side
    fbLabel = label
    fbTarget = target
    removeCallbacks(fadeOut)
    if (fbAlpha < 1f) fadeFeedback(1f, 100)
    postDelayed(fadeOut, runLife)
    if (changed) {
      labelAnim?.cancel()
      labelAnim = ValueAnimator.ofFloat(1f, 1.08f, 1f).apply {
        duration = kit.ms(120)
        addUpdateListener {
          labelScale = it.animatedValue as Float
          invalidate()
        }
        start()
      }
    }
    postInvalidateOnAnimation()
  }

  private fun fadeFeedback(to: Float, ms: Long) {
    fbAnim?.cancel()
    fbAnim = ValueAnimator.ofFloat(fbAlpha, to).apply {
      duration = kit.ms(ms)
      addUpdateListener {
        fbAlpha = it.animatedValue as Float
        invalidate()
      }
      start()
    }
  }

  override fun onDraw(c: Canvas) {
    if (fbAlpha <= 0f || fbSide == 0) return
    val w = width.toFloat()
    val h = height.toFloat()
    c.save()
    c.clipRect(0f, 0f, w, h)
    if (fbSide < 0) oval.set(-0.25f * w, -0.15f * h, 0.4f * w, 1.15f * h)
    else oval.set(0.6f * w, -0.15f * h, 1.25f * w, 1.15f * h)
    areaPaint.color = 0x1FFFFFFF
    areaPaint.alpha = (0x1F * fbAlpha).toInt()
    c.drawOval(oval, areaPaint)
    c.restore()

    val cx = if (fbSide < 0) 0.2f * w else 0.8f * w
    val cy = h / 2
    val size = kit.dp(20)
    val gap = kit.dp(2)
    val total = size * 3 + gap * 2
    val labelTop = cy + size / 2 + kit.dp(8)
    val labelBase = labelTop - labelPaint.ascent()
    val targetBase = labelBase + labelPaint.descent() + kit.dp(2) - targetPaint.ascent()
    // Wide enough for the label at the top of its pulse.
    val half = maxOf(total, labelPaint.measureText(fbLabel) * 1.08f, targetPaint.measureText(fbTarget)) / 2 + kit.dp(14)
    back.set(cx - half, cy - size / 2 - kit.dp(12), cx + half, targetBase + targetPaint.descent() + kit.dp(10))
    backPaint.alpha = ((Colors.CAPTION ushr 24) * fbAlpha).toInt()
    c.drawRoundRect(back, kit.dp(16), kit.dp(16), backPaint)
    val now = SystemClock.uptimeMillis() % 750
    for (j in 0..2) {
      // The pulse runs the way the seek goes.
      val k = if (fbSide > 0) j else 2 - j
      val phase = ((now - k * 125) % 750 + 750) % 750
      val pulse = if (phase < 250) {
        val p = phase / 250f
        0.3f + 0.7f * (if (p < 0.5f) p * 2 else (1 - p) * 2)
      } else 0.3f
      val left = cx - total / 2 + j * (size + gap)
      chevron.setBounds(left.toInt(), (cy - size / 2).toInt(), (left + size).toInt(), (cy + size / 2).toInt())
      chevron.alpha = (255 * pulse * fbAlpha).toInt()
      c.save()
      if (fbSide < 0) c.rotate(180f, left + size / 2, cy)
      chevron.draw(c)
      c.restore()
    }

    labelPaint.alpha = (255 * fbAlpha).toInt()
    c.save()
    c.scale(labelScale, labelScale, cx, labelTop + (labelPaint.descent() - labelPaint.ascent()) / 2)
    c.drawText(fbLabel, cx, labelBase, labelPaint)
    c.restore()
    targetPaint.alpha = (255 * fbAlpha).toInt()
    c.drawText(fbTarget, cx, targetBase, targetPaint)
    postInvalidateOnAnimation()
  }

  fun release() {
    removeCallbacks(single)
    removeCallbacks(endRun)
    removeCallbacks(fadeOut)
    fbAnim?.cancel()
    labelAnim?.cancel()
  }
}
