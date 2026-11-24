package app.ichioshi

import android.animation.ValueAnimator
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.DrawableWrapper
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.LayerDrawable
import android.graphics.drawable.RippleDrawable
import android.graphics.drawable.StateListDrawable
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.accessibility.AccessibilityManager
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.TextView
import com.google.android.material.progressindicator.CircularProgressIndicator
import java.lang.ref.WeakReference

/** The player's colors: the app's (mobile/src/app.css), and white and black over video. */
object Colors {
  /** Icons and text over video. */
  const val WHITE = 0xFFFFFFFF.toInt()
  const val BLACK = 0xFF000000.toInt()
  const val TRANSPARENT = 0
  /** Secondary text over video, the time left (80%: 62% washed out on bright frames). */
  const val DIM = 0xCCFFFFFF.toInt()
  /** Disabled over video. */
  const val FAINT = 0x61FFFFFF
  /** The icon and spinner on the white play disc. */
  const val INK = 0xFF0D0F14.toInt()
  /** The played part of the timeline, checks, the active tool, actions in messages. */
  const val ACCENT = 0xFF8B7CFF.toInt()
  /** The accent gradient's end (the primary button). */
  const val ACCENT_2 = 0xFF5B8CFF.toInt()
  /** The chosen track row. */
  const val ACCENT_10 = 0x1A8B7CFF
  /** The active tool's circle. */
  const val ACCENT_16 = 0x298B7CFF
  /**
   * The party chip: the accent at 18% over black, 85% opaque, so its text
   * stays readable on a bright frame.
   */
  const val CHIP = 0xD919162E.toInt()
  /** The party chip's text and icon. */
  const val ACCENT_SOFT = 0xFFC9C2FF.toInt()
  /** Openings and endings, played (the desktop player's color). */
  const val AMBER = 0xFFFFC45C.toInt()
  /** Openings and endings, not played yet. */
  const val AMBER_45 = 0x73FFC45C
  /** Openings and endings, buffered. */
  const val AMBER_60 = 0x99FFC45C.toInt()
  /** The panel and the error card. */
  const val SURFACE = 0xF5171B24.toInt()
  /** Stepper buttons, the secondary button. */
  const val SURFACE2 = 0xFF1F2430.toInt()
  /** The message pill, the scrub bubble. */
  const val SURFACE2_94 = 0xF01F2430.toInt()
  /** Every 1 dp border. */
  const val LINE = 0xFF272D3B.toInt()
  /** Text on surfaces. */
  const val TEXT = 0xFFE9ECF3.toInt()
  /** Secondary text on surfaces. */
  const val MUTED = 0xFF8B94A8.toInt()
  /** Headings and notes on surfaces. */
  const val FAINT_T = 0xFF5A6274.toInt()
  /** The error icon. */
  const val RED = 0xFFF87171.toInt()
  /** The ±10 discs, the Ask disc, the buffering disc. */
  const val GLASS = 0x73000000
  /** The pill behind captions on the bare picture (Ask, Buffering, a double tap's time). */
  const val CAPTION = 0x99000000.toInt()
  /** The ring on glass discs. */
  const val GLASS_RING = 0x40FFFFFF
  /** The ring on the watch party's Ask disc. */
  const val ASK_RING = 0x66FFFFFF
  /** The back button's circle. */
  const val BACK_FILL = 0x1AFFFFFF
  /** The timeline, not played yet. */
  const val TRACK = 0x38FFFFFF
  /** The timeline, buffered. */
  const val BUFFERED = 0x66FFFFFF
  /** The moment button and the Watch button. */
  const val PILL_FILL = 0x8C000000.toInt()
  const val PILL_PRESSED = 0xBF000000.toInt()
  /** The moment button's border. */
  const val PILL_STROKE = 0xD9FFFFFF.toInt()
  /** The auto-skip countdown's fill. */
  const val COUNTDOWN = 0x2EFFFFFF
  /** Ripples on dark. */
  const val PRESS = 0x33FFFFFF
  /** The ripple on the white disc. */
  const val PRESS_DARK = 0x1F000000
  /** The track lists' scroll bar. */
  const val SCROLL = 0x668B94A8
  /** Ripples on track rows. */
  const val PRESS_ROW = 0x14FFFFFF
  /** Text shadows over video. */
  const val SHADOW = 0x99000000.toInt()
}

/**
 * [d] without the focused state: a ripple would tint the whole control when
 * focused (the white disc goes grey); the focus ring shows focus instead.
 */
private class NoFocusTint(d: Drawable) : DrawableWrapper(d) {
  override fun onStateChange(state: IntArray): Boolean =
    drawable?.setState(state.filter { it != android.R.attr.state_focused }.toIntArray()) ?: false

  override fun isStateful() = true
}

/** A round control: [view] takes the touches, [disc] is the circle you see, [shape] its fill. */
class Round(val view: FrameLayout, val disc: FrameLayout, val shape: GradientDrawable)

/** Sizes in dp, the Inter fonts, and the shapes and views the player's parts share. */
class Kit(val context: Context) {
  // Read each time: a display size change updates it while the player stays open.
  private val metrics get() = context.resources.displayMetrics
  private val access = context.getSystemService(AccessibilityManager::class.java)

  val medium: Typeface = context.resources.getFont(R.font.inter_medium)
  val semibold: Typeface = context.resources.getFont(R.font.inter_semibold)
  val bold: Typeface = context.resources.getFont(R.font.inter_bold)

  /** Every text made here and its size in sp, so a new text size setting reaches them. */
  private val texts = mutableListOf<Pair<WeakReference<TextView>, Float>>()

  /** [v] dp in pixels. */
  fun dp(v: Float): Float = v * metrics.density
  fun dp(v: Int): Float = v * metrics.density

  /** [v] dp in whole pixels. */
  fun px(v: Float): Int = Math.round(v * metrics.density)
  fun px(v: Int): Int = Math.round(v * metrics.density)

  /** Pixels in dp. */
  fun toDp(px: Int): Float = px / metrics.density

  /** [v] sp in pixels. */
  fun sp(v: Float): Float = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, v, metrics)

  /** An animation's length: none when the system turns animations off. */
  fun ms(v: Long): Long = if (ValueAnimator.areAnimatorsEnabled()) v else 0

  /** How long something stays up, longer if the user asked for more time (accessibility). */
  fun timeout(base: Int, flags: Int): Long =
    access?.getRecommendedTimeoutMillis(base, flags)?.toLong() ?: base.toLong()

  /**
   * One line of text; shadowed over video; tabular digits for times.
   * [fixed]: [sp] is dp, and the text size setting doesn't change it (a
   * label that's part of an icon stays the icon's size).
   */
  fun text(sp: Float, color: Int, font: Typeface, shadow: Boolean = false, tnum: Boolean = false, fixed: Boolean = false) =
    TextView(context).apply {
      setTextSize(if (fixed) TypedValue.COMPLEX_UNIT_DIP else TypedValue.COMPLEX_UNIT_SP, sp)
      setTextColor(color)
      typeface = font
      includeFontPadding = false
      isSingleLine = true
      ellipsize = TextUtils.TruncateAt.END
      if (shadow) setShadowLayer(dp(3), 0f, dp(1), Colors.SHADOW)
      if (tnum) fontFeatureSettings = "tnum"
      if (!fixed) texts += WeakReference(this) to sp
    }

  /** A caption on the bare picture: white on a dark pill, so it reads on any frame. */
  fun caption(sp: Float) = text(sp, Colors.WHITE, medium).apply {
    gravity = Gravity.CENTER
    background = rounded(Colors.CAPTION, 100f)
    setPadding(px(10), px(4), px(10), px(4))
  }

  /** The text size setting changed (the player stayed open): every text takes it. */
  fun rescaleText() {
    texts.removeAll { it.first.get() == null }
    for ((ref, sp) in texts) ref.get()?.setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
  }

  fun oval(color: Int, strokeDp: Float = 0f, stroke: Int = 0) = GradientDrawable().apply {
    shape = GradientDrawable.OVAL
    setColor(color)
    if (strokeDp > 0) setStroke(px(strokeDp).coerceAtLeast(1), stroke)
  }

  fun rounded(color: Int, radiusDp: Float, strokeDp: Float = 0f, stroke: Int = 0) = GradientDrawable().apply {
    cornerRadius = dp(radiusDp)
    setColor(color)
    if (strokeDp > 0) setStroke(px(strokeDp).coerceAtLeast(1), stroke)
  }

  /**
   * A control's background: [shape], a ripple masked to it, and a ring when
   * the D-pad or a keyboard focuses it. [radiusDp] null: round. [over]:
   * layers drawn over the ripple and under the ring (a border, a fill), so
   * the ring always shows.
   */
  fun pressable(shape: Drawable, radiusDp: Float?, ripple: Int = Colors.PRESS, over: List<Drawable> = emptyList()): Drawable {
    val mask = if (radiusDp == null) oval(Colors.BLACK) else rounded(Colors.BLACK, radiusDp)
    val ring = if (radiusDp == null) oval(Colors.TRANSPARENT, 2f, Colors.ACCENT)
    else rounded(Colors.TRANSPARENT, radiusDp, 2f, Colors.ACCENT)
    val focus = StateListDrawable().apply { addState(intArrayOf(android.R.attr.state_focused), ring) }
    val press: Drawable = NoFocusTint(RippleDrawable(ColorStateList.valueOf(ripple), shape, mask))
    return LayerDrawable((listOf(press) + over + focus).toTypedArray())
  }

  /** A [discDp] circle in a [targetDp] touch area; the circle shows the press and the focus. */
  fun round(
    targetDp: Float,
    discDp: Float,
    fill: Int,
    strokeDp: Float = 0f,
    stroke: Int = 0,
    ripple: Int = Colors.PRESS,
  ): Round {
    val shape = oval(fill, strokeDp, stroke)
    val disc = FrameLayout(context).apply {
      background = pressable(shape, null, ripple)
      isDuplicateParentStateEnabled = true
    }
    val view = FrameLayout(context).apply {
      isClickable = true
      isFocusable = true
      // The disc draws the focus ring; no grey square over the whole target.
      defaultFocusHighlightEnabled = false
      clipChildren = false
      clipToPadding = false
      addView(disc, FrameLayout.LayoutParams(px(discDp), px(discDp), Gravity.CENTER))
    }
    return Round(view, disc, shape)
  }

  /** An icon, [sizeDp] square, tinted. */
  fun icon(res: Int, sizeDp: Float, tint: Int) = ImageView(context).apply {
    if (res != 0) setImageResource(res)
    imageTintList = ColorStateList.valueOf(tint)
    scaleType = ImageView.ScaleType.FIT_CENTER
    importantForAccessibility = ImageView.IMPORTANT_FOR_ACCESSIBILITY_NO
    layoutParams = FrameLayout.LayoutParams(px(sizeDp), px(sizeDp), Gravity.CENTER)
  }

  /** A spinner, [sizeDp] across with a 3 dp line. */
  fun spinner(sizeDp: Float, color: Int) = CircularProgressIndicator(context).apply {
    isIndeterminate = true
    indicatorSize = px(sizeDp)
    trackThickness = px(3)
    setIndicatorColor(color)
    trackColor = Colors.TRANSPARENT
    importantForAccessibility = CircularProgressIndicator.IMPORTANT_FOR_ACCESSIBILITY_NO
    layoutParams = FrameLayout.LayoutParams(px(sizeDp), px(sizeDp), Gravity.CENTER)
  }
}
