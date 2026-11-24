package app.ichioshi

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Outline
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.InsetDrawable
import android.os.SystemClock
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.ViewOutlineProvider
import android.view.accessibility.AccessibilityNodeInfo
import android.view.animation.AccelerateInterpolator
import android.view.animation.DecelerateInterpolator
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

/** What the player shows: the episode loading, playing, or an error card. */
enum class Mode { LOADING, PLAYING, CARD }

/** What the overlay's controls do (the activity does it). */
interface OverlayActions {
  fun back()
  fun previous()
  fun next()
  fun tracks()
  fun fitFill()
  /** 10 s back (-1) or on (1). */
  fun step(side: Int)
  fun playPause()
  /** A member asks the host to pause or play. */
  fun ask()
  /** A finger went down on a control (true), or came up. */
  fun touching(down: Boolean)
  /** Where subtitles go (mpv's sub-pos), so nothing covers them. */
  fun subPos(pos: Int)
}

private const val MATCH = ViewGroup.LayoutParams.MATCH_PARENT
private const val WRAP = ViewGroup.LayoutParams.WRAP_CONTENT

/** A short message at the top (instead of a Toast): an icon, the text, maybe an action. */
@SuppressLint("ViewConstructor")
class MessagePill(context: Context, private val kit: Kit) : LinearLayout(context) {
  private val icon = kit.icon(0, 16f, Colors.TEXT)
  private val text = kit.text(13f, Colors.TEXT, kit.medium)
  private val action = kit.text(13f, Colors.ACCENT, kit.semibold)
  private var onAction: (() -> Unit)? = null
  private val hideLater = Runnable { hide() }
  private var showing = false

  /** The widest it gets (px). */
  var maxW = 0

  init {
    orientation = HORIZONTAL
    gravity = Gravity.CENTER_VERTICAL
    minimumHeight = kit.px(48)
    // 40 dp you see; 48 dp for the action's touch.
    background = InsetDrawable(kit.rounded(Colors.SURFACE2_94, 12f, 1f, Colors.LINE), 0, kit.px(4), 0, kit.px(4))
    setPadding(kit.px(14), 0, kit.px(14), 0)
    accessibilityLiveRegion = ACCESSIBILITY_LIVE_REGION_POLITE
    visibility = INVISIBLE
    addView(icon, LayoutParams(kit.px(16), kit.px(16)).apply { rightMargin = kit.px(8) })
    addView(text, LayoutParams(WRAP, WRAP, 1f))
    action.apply {
      gravity = Gravity.CENTER
      setPadding(kit.px(12), 0, kit.px(12), 0)
      background = kit.pressable(kit.rounded(Colors.TRANSPARENT, 8f), 8f)
      isClickable = true
      isFocusable = true
      setOnClickListener {
        val a = onAction
        hide()
        a?.invoke()
      }
    }
    addView(action, LayoutParams(WRAP, kit.px(48)))
  }

  override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
    var spec = widthMeasureSpec
    if (maxW > 0) spec = MeasureSpec.makeMeasureSpec(min(MeasureSpec.getSize(spec), maxW), MeasureSpec.AT_MOST)
    super.onMeasure(spec, heightMeasureSpec)
  }

  /** Show [message] for [ms]; a new one replaces the one showing. */
  fun show(message: String, iconRes: Int = 0, actionText: String? = null, onAction: (() -> Unit)? = null, ms: Long) {
    removeCallbacks(hideLater)
    val fill = {
      text.text = message
      if (iconRes != 0) {
        icon.setImageResource(iconRes)
        icon.visibility = VISIBLE
      } else {
        icon.visibility = GONE
      }
      this.onAction = onAction
      if (actionText != null) {
        action.text = actionText
        action.visibility = VISIBLE
        setPadding(kit.px(14), 0, kit.px(2), 0)
      } else {
        action.visibility = GONE
        setPadding(kit.px(14), 0, kit.px(14), 0)
      }
    }
    animate().cancel()
    if (showing && visibility == VISIBLE) {
      animate().alpha(0f).translationY(0f).setStartDelay(0).setDuration(kit.ms(60))
        .setInterpolator(AccelerateInterpolator()).withEndAction {
          fill()
          animate().alpha(1f).setStartDelay(0).setDuration(kit.ms(60)).setInterpolator(DecelerateInterpolator()).start()
        }.start()
    } else {
      fill()
      alpha = 0f
      translationY = -kit.dp(6)
      visibility = VISIBLE
      animate().alpha(1f).translationY(0f).setStartDelay(0).setDuration(kit.ms(150))
        .setInterpolator(DecelerateInterpolator()).start()
    }
    showing = true
    postDelayed(hideLater, ms)
  }

  fun hide() {
    removeCallbacks(hideLater)
    if (!showing) return
    showing = false
    animate().cancel()
    animate().alpha(0f).setStartDelay(0).setDuration(kit.ms(200)).setInterpolator(AccelerateInterpolator())
      .withEndAction { if (!showing) visibility = INVISIBLE }.start()
  }

  fun release() {
    removeCallbacks(hideLater)
    animate().cancel()
  }
}

/**
 * The player's views over the picture, where they go on this screen, and
 * showing and hiding them. From the bottom: the picture, the tap layer, the
 * controls, the buffering disc, the moment button, the tracks panel,
 * messages, the error card. The activity sets the state fields, then [sync].
 */
@SuppressLint("ClickableViewAccessibility")
class PlayerOverlay(
  context: Context,
  private val kit: Kit,
  private val actions: OverlayActions,
  val member: Boolean,
  hasPrev: Boolean,
  hasNext: Boolean,
  title: String,
  episode: String,
  trackControl: TrackControl,
) {
  val root = FrameLayout(context)
  val surface = SurfaceView(context)
  val taps = TapLayer(context, kit, member)
  private val controls = FrameLayout(context)
  private val scrimTop = View(context)
  private val scrimBottom = View(context)

  // The top bar.
  private val topBar = LinearLayout(context)
  private val backButton = kit.round(48f, 40f, Colors.BACK_FILL)
  private val titleText = kit.text(16f, Colors.WHITE, kit.semibold, shadow = true).apply { text = title }
  private val episodeText = kit.text(13f, Colors.DIM, kit.medium, shadow = true).apply { text = episode }
  private val chapterText = kit.text(13f, Colors.DIM, kit.medium, shadow = true).apply {
    maxWidth = kit.px(220)
    visibility = View.GONE
  }
  private val cluster = LinearLayout(context)
  private val prevTool = if (hasPrev && !member) tool(R.drawable.ic_skip_back, 20f, "Previous episode") { actions.previous() } else null
  private val nextTool = if (hasNext && !member) tool(R.drawable.ic_skip_forward, 20f, "Next episode") { actions.next() } else null
  private val tracksIcon = kit.icon(R.drawable.ic_captions, 22f, Colors.WHITE)
  private val tracksTool = tool(0, 22f, "Audio and subtitles", tracksIcon) { actions.tracks() }
  private val fitIcon = kit.icon(R.drawable.ic_expand, 22f, Colors.WHITE)
  private val fitTool = tool(0, 22f, "Fill the screen", fitIcon) { actions.fitFill() }
  private val chipDetail = kit.text(13f, Colors.ACCENT_SOFT, kit.medium).apply { text = " · The host controls playback" }
  private val chip: View? = if (member) LinearLayout(context).apply {
    orientation = LinearLayout.HORIZONTAL
    gravity = Gravity.CENTER_VERTICAL
    background = kit.rounded(Colors.CHIP, 16f)
    setPadding(kit.px(12), 0, kit.px(12), 0)
    addView(kit.icon(R.drawable.ic_users, 16f, Colors.ACCENT_SOFT),
      LinearLayout.LayoutParams(kit.px(16), kit.px(16)).apply { rightMargin = kit.px(8) })
    addView(kit.text(13f, Colors.ACCENT_SOFT, kit.semibold).apply { text = "Watch party" })
    addView(chipDetail)
    contentDescription = "Watch party. The host controls playback."
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
    isScreenReaderFocusable = true
  } else null

  // The transport.
  private val transport = FrameLayout(context)
  private val back10 = if (!member) seekDisc(R.drawable.ic_rotate_ccw, "Back 10 seconds", -1) else null
  private val fwd10 = if (!member) seekDisc(R.drawable.ic_rotate_cw, "Forward 10 seconds", 1) else null
  private val playIcon = kit.icon(R.drawable.ic_pause, 28f, if (member) Colors.WHITE else Colors.INK).apply { tag = R.drawable.ic_pause }
  private val playSpinner = kit.spinner(28f, if (member) Colors.WHITE else Colors.INK).apply { visibility = View.INVISIBLE }

  /** The white play disc, or for a member, the Ask disc. */
  val playDisc: Round = if (member) kit.round(72f, 64f, Colors.GLASS, 1.5f, Colors.ASK_RING)
  else kit.round(72f, 64f, Colors.WHITE, ripple = Colors.PRESS_DARK)
  private val askCaption = if (member) kit.caption(12f).apply {
    text = "Ask to pause"
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
  } else null

  // The bottom band.
  val band = LinearLayout(context)
  private val elapsed = kit.text(13f, Colors.WHITE, kit.semibold, shadow = true, tnum = true).apply {
    gravity = Gravity.START or Gravity.CENTER_VERTICAL
    text = "0:00"
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
  }
  private val remaining = kit.text(13f, Colors.DIM, kit.semibold, shadow = true, tnum = true).apply {
    gravity = Gravity.END or Gravity.CENTER_VERTICAL
    text = "−0:00"
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
  }
  val timeline = Timeline(context, kit, member)
  private val spacer = View(context)
  private val bubble = LinearLayout(context)
  private val bubbleTime = kit.text(16f, Colors.TEXT, kit.semibold, tnum = true).apply { gravity = Gravity.CENTER_HORIZONTAL }
  private val bubbleChapter = kit.text(12f, Colors.DIM, kit.medium).apply {
    gravity = Gravity.CENTER_HORIZONTAL
    maxWidth = kit.px(180)
  }

  // Loading and buffering.
  private val status = FrameLayout(context)
  private val statusDisc = FrameLayout(context)
  /** "Loading", "Reconnecting…" or "Buffering 45%", on a pill under the spinner. */
  private val statusCaption = LinearLayout(context)
  /**
   * The caption's word: a live region, said when it appears or the word
   * changes. The percent beside it isn't, so TalkBack doesn't say every step.
   */
  private val statusWord = kit.text(12f, Colors.WHITE, kit.medium).apply {
    accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
  }
  private val statusPct = kit.text(12f, Colors.WHITE, kit.medium, tnum = true).apply {
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
  }

  val moments = MomentLayer(context, kit)
  private val pillLayer = FrameLayout(context)
  val pill = MessagePill(context, kit)
  val panel = TracksPanel(context, kit, trackControl)

  // The error card.
  private val cardLayer = FrameLayout(context)
  private val card = LinearLayout(context)
  private val cardTitle = kit.text(17f, Colors.TEXT, kit.semibold).apply {
    isSingleLine = false
    maxLines = 2
  }
  private val cardDetail = kit.text(13f, Colors.MUTED, kit.medium).apply {
    isSingleLine = false
    maxLines = 3
  }
  private val cardWords = LinearLayout(context)
  private val cardClose = cardButton("Close", false)
  private val cardRetry = cardButton("Try again", true)

  /** What's showing. */
  var mode = Mode.LOADING
  var controlsShown = false
  var panelOpen = false
  var scrubbing = false
  /** The moment button should show. */
  var momentOn = false
    private set
  /** The picture's shape isn't the screen's (Fit and Fill differ). */
  var fitAvailable = false
    set(v) {
      if (field != v) {
        field = v
        fitTopBar()
      }
    }
  /** Previous and Next have room in the top bar (a narrow window leaves them out). */
  private var prevRoom = true
  private var nextRoom = true
  /**
   * The moment button sits above the band, not at its end: a narrow window,
   * or text so large that a countdown would leave the timeline under 160 dp.
   */
  private var momentAbove = false
  /** The picture fills the screen now. */
  var fill = false
  /** The picture's size (mpv's dwidth and dheight). */
  var videoW = 0
  var videoH = 0
  private var busy = false
  /** Buffering's percent, once it's taken a while (null: no caption yet). */
  private var busyPct: Int? = null
  /** The status caption shows (or is fading in). */
  private var captionShown = false
  private var loadingCaption: String? = null
  private var askText = "Ask to pause"
  private var askedUntil = 0L

  /** The window is narrow (under 600 dp). */
  var compact = false
    private set
  private var padL = 0
  private var padR = 0
  private var padT = 0
  private var padB = 0
  /** The bottom edge's system gesture area (px): a swipe up starts there. */
  private var gestureB = 0
  /** The top bar's height (px): 48 dp, more with large text. */
  private var barH = 0
  private var spacerWidth = 0
  private var spacerTarget = 0
  private var spacerAnim: ValueAnimator? = null
  private var pillX = Float.NaN
  private var pillY = Float.NaN
  private val targets = HashMap<View, Float>()

  init {
    root.setBackgroundColor(Color.BLACK)
    root.addView(surface, FrameLayout.LayoutParams(MATCH, MATCH))
    root.addView(taps, FrameLayout.LayoutParams(MATCH, MATCH))

    scrimTop.background = GradientDrawable().apply { orientation = GradientDrawable.Orientation.TOP_BOTTOM }
    scrimBottom.background = GradientDrawable().apply { orientation = GradientDrawable.Orientation.BOTTOM_TOP }
    shade(scrimTop, kit.px(104), kit.px(48))
    shade(scrimBottom, kit.px(112), kit.px(48))
    controls.addView(scrimTop, FrameLayout.LayoutParams(MATCH, kit.px(104), Gravity.TOP))
    controls.addView(scrimBottom, FrameLayout.LayoutParams(MATCH, kit.px(112), Gravity.BOTTOM))

    // The top bar: back, the titles, the tools.
    topBar.orientation = LinearLayout.HORIZONTAL
    topBar.gravity = Gravity.CENTER_VERTICAL
    // 48 dp, or taller when large text needs it.
    topBar.minimumHeight = kit.px(48)
    backButton.disc.addView(kit.icon(R.drawable.ic_arrow_left, 22f, Colors.WHITE))
    backButton.view.contentDescription = "Back"
    backButton.view.setOnClickListener { actions.back() }
    hold(backButton.view)
    topBar.addView(backButton.view, LinearLayout.LayoutParams(kit.px(48), kit.px(48)))
    val titles = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
    titles.addView(titleText, LinearLayout.LayoutParams(WRAP, WRAP))
    val line = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL }
    line.addView(episodeText, LinearLayout.LayoutParams(WRAP, WRAP, 1f))
    line.addView(chapterText, LinearLayout.LayoutParams(WRAP, WRAP))
    titles.addView(line, LinearLayout.LayoutParams(WRAP, WRAP).apply { topMargin = kit.px(2) })
    topBar.addView(titles, LinearLayout.LayoutParams(0, WRAP, 1f).apply {
      leftMargin = kit.px(12)
      rightMargin = kit.px(12)
    })
    cluster.orientation = LinearLayout.HORIZONTAL
    cluster.gravity = Gravity.CENTER_VERTICAL
    chip?.let { cluster.addView(it, LinearLayout.LayoutParams(WRAP, kit.px(32)).apply { rightMargin = kit.px(12) }) }
    prevTool?.let {
      cluster.addView(it.view, LinearLayout.LayoutParams(kit.px(48), kit.px(48)).apply {
        rightMargin = kit.px(if (nextTool != null) 8 else 16)
      })
    }
    nextTool?.let {
      cluster.addView(it.view, LinearLayout.LayoutParams(kit.px(48), kit.px(48)).apply { rightMargin = kit.px(16) })
    }
    cluster.addView(tracksTool.view, LinearLayout.LayoutParams(kit.px(48), kit.px(48)))
    cluster.addView(fitTool.view, LinearLayout.LayoutParams(kit.px(48), kit.px(48)).apply { leftMargin = kit.px(8) })
    topBar.addView(cluster, LinearLayout.LayoutParams(WRAP, kit.px(48)))
    controls.addView(topBar, FrameLayout.LayoutParams(MATCH, WRAP, Gravity.TOP))

    // The transport: -10, play (or Ask), +10.
    transport.clipChildren = false
    playDisc.disc.addView(playIcon)
    playDisc.disc.addView(playSpinner)
    playDisc.view.id = View.generateViewId()
    playDisc.view.contentDescription = if (member) "Ask the host to pause" else "Pause"
    playDisc.view.setOnClickListener { if (member) actions.ask() else actions.playPause() }
    hold(playDisc.view)
    if (!member) {
      playDisc.disc.elevation = kit.dp(6)
      playDisc.disc.outlineProvider = object : ViewOutlineProvider() {
        override fun getOutline(view: View, outline: Outline) = outline.setOval(0, 0, view.width, view.height)
      }
    }
    back10?.let { transport.addView(it.view, FrameLayout.LayoutParams(kit.px(56), kit.px(56))) }
    transport.addView(playDisc.view, FrameLayout.LayoutParams(kit.px(72), kit.px(72)))
    fwd10?.let { transport.addView(it.view, FrameLayout.LayoutParams(kit.px(56), kit.px(56))) }
    askCaption?.let { transport.addView(it, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL)) }
    controls.addView(transport, FrameLayout.LayoutParams(WRAP, WRAP))

    // The bottom band: elapsed, the timeline, time left, room for the moment button.
    band.orientation = LinearLayout.HORIZONTAL
    band.gravity = Gravity.CENTER_VERTICAL
    band.addView(elapsed, LinearLayout.LayoutParams(kit.px(40), MATCH))
    band.addView(timeline, LinearLayout.LayoutParams(0, kit.px(48), 1f).apply {
      leftMargin = kit.px(12)
      rightMargin = kit.px(12)
    })
    band.addView(remaining, LinearLayout.LayoutParams(kit.px(40), MATCH))
    band.addView(spacer, LinearLayout.LayoutParams(0, 1))
    controls.addView(band, FrameLayout.LayoutParams(MATCH, kit.px(48), Gravity.BOTTOM))

    bubble.orientation = LinearLayout.VERTICAL
    bubble.gravity = Gravity.CENTER_HORIZONTAL
    bubble.background = kit.rounded(Colors.SURFACE2_94, 10f, 1f, Colors.LINE)
    bubble.setPadding(kit.px(10), kit.px(6), kit.px(10), kit.px(6))
    bubble.importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
    bubble.addView(bubbleTime, LinearLayout.LayoutParams(WRAP, WRAP))
    bubble.addView(bubbleChapter, LinearLayout.LayoutParams(WRAP, WRAP).apply { topMargin = kit.px(2) })
    controls.addView(bubble, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.LEFT))
    root.addView(controls, FrameLayout.LayoutParams(MATCH, MATCH))

    // Loading and buffering.
    statusDisc.background = kit.oval(Colors.GLASS, 1f, Colors.GLASS_RING)
    statusDisc.addView(kit.spinner(36f, Colors.WHITE))
    statusDisc.contentDescription = "Loading"
    status.addView(statusDisc, FrameLayout.LayoutParams(kit.px(64), kit.px(64)))
    statusCaption.orientation = LinearLayout.HORIZONTAL
    statusCaption.gravity = Gravity.CENTER_VERTICAL
    statusCaption.background = kit.rounded(Colors.CAPTION, 100f)
    statusCaption.setPadding(kit.px(10), kit.px(4), kit.px(10), kit.px(4))
    statusCaption.addView(statusWord, LinearLayout.LayoutParams(WRAP, WRAP))
    statusCaption.addView(statusPct, LinearLayout.LayoutParams(WRAP, WRAP))
    status.addView(statusCaption, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL))
    root.addView(status, FrameLayout.LayoutParams(MATCH, MATCH))

    root.addView(moments, FrameLayout.LayoutParams(WRAP, kit.px(48), Gravity.BOTTOM or Gravity.RIGHT))
    root.addView(panel, FrameLayout.LayoutParams(MATCH, MATCH))
    // Over the panel's catcher, so a message's action (Undo, Start over) still works with the panel open.
    pillLayer.addView(pill, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL))
    root.addView(pillLayer, FrameLayout.LayoutParams(MATCH, MATCH))

    buildCard(context)
    cardLayer.addView(card, FrameLayout.LayoutParams(kit.px(420), WRAP, Gravity.CENTER))
    cardLayer.visibility = View.INVISIBLE
    root.addView(cardLayer, FrameLayout.LayoutParams(MATCH, MATCH))

    // Hidden until there's something to show (the top bar shows while loading).
    for (v in listOfNotNull(scrimTop, scrimBottom, transport, band, bubble, moments, statusDisc, statusCaption,
      tracksTool.view, fitTool.view, prevTool?.view, nextTool?.view)) {
      v.visibility = View.INVISIBLE
      v.alpha = 0f
      targets[v] = -1f
    }
    targets[topBar] = 1f
    moments.button.accessibilityTraversalAfter = playDisc.view.id
    moments.watch.accessibilityTraversalAfter = playDisc.view.id
    sync()
  }

  /** A top-bar tool: a 22 dp icon in a 40 dp circle, 48 dp to touch. */
  private fun tool(icon: Int, size: Float, desc: String, image: ImageView? = null, onClick: () -> Unit): Round =
    kit.round(48f, 40f, Colors.TRANSPARENT).apply {
      disc.addView(image ?: kit.icon(icon, size, Colors.WHITE))
      view.contentDescription = desc
      view.setOnClickListener { onClick() }
      hold(view)
    }

  /** A ±10 disc. */
  private fun seekDisc(icon: Int, desc: String, side: Int): Round =
    kit.round(56f, 52f, Colors.GLASS, 1f, Colors.GLASS_RING).apply {
      disc.addView(kit.icon(icon, 28f, Colors.WHITE))
      // Part of the icon: dp, so large text doesn't push it over the arrow.
      disc.addView(kit.text(10f, Colors.WHITE, kit.bold, fixed = true).apply {
        text = "10"
        gravity = Gravity.CENTER
        translationY = kit.dp(0.5f)
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
      }, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.CENTER))
      view.contentDescription = desc
      view.setOnClickListener { actions.step(side) }
      hold(view)
    }

  /** Keep the controls up while a finger is on [v]. */
  private fun hold(v: View) {
    v.setOnTouchListener { _, e ->
      when (e.actionMasked) {
        MotionEvent.ACTION_DOWN -> actions.touching(true)
        MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> actions.touching(false)
      }
      false
    }
  }

  private fun cardButton(text: String, primary: Boolean) = kit.text(14f, if (primary) Colors.WHITE else Colors.TEXT, kit.semibold).apply {
    this.text = text
    gravity = Gravity.CENTER
    val shape = if (primary) {
      GradientDrawable(GradientDrawable.Orientation.TL_BR, intArrayOf(Colors.ACCENT, Colors.ACCENT_2)).apply { cornerRadius = kit.dp(10) }
    } else {
      kit.rounded(Colors.SURFACE2, 10f, 1f, Colors.LINE)
    }
    background = InsetDrawable(kit.pressable(shape, 10f), 0, kit.px(2), 0, kit.px(2))
    // After the background, whose insets would replace it.
    setPadding(kit.px(16), 0, kit.px(16), 0)
    isClickable = true
    isFocusable = true
  }

  private fun buildCard(context: Context) {
    card.orientation = LinearLayout.VERTICAL
    card.background = kit.rounded(Colors.SURFACE, 16f, 1f, Colors.LINE)
    card.setPadding(kit.px(24), kit.px(24), kit.px(24), kit.px(24))
    // Takes the touches on it, but not focus: the D-pad goes to Close and Try again.
    card.isClickable = true
    card.isFocusable = false
    val top = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL }
    top.addView(kit.icon(R.drawable.ic_circle_x, 28f, Colors.RED), LinearLayout.LayoutParams(kit.px(28), kit.px(28)))
    val words = cardWords.apply {
      orientation = LinearLayout.VERTICAL
      // TalkBack reads the title and the detail together.
      isScreenReaderFocusable = true
    }
    words.addView(cardTitle, LinearLayout.LayoutParams(MATCH, WRAP))
    words.addView(cardDetail, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = kit.px(4) })
    top.addView(words, LinearLayout.LayoutParams(0, WRAP, 1f).apply { leftMargin = kit.px(16) })
    card.addView(top, LinearLayout.LayoutParams(MATCH, WRAP))
    val buttons = LinearLayout(context).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.END
    }
    buttons.addView(cardClose, LinearLayout.LayoutParams(WRAP, kit.px(48)))
    buttons.addView(cardRetry, LinearLayout.LayoutParams(WRAP, kit.px(48)).apply { leftMargin = kit.px(10) })
    card.addView(buttons, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = kit.px(18) })
  }

  // ── Showing and hiding ──────────────────────────────────────────────

  /**
   * Fade [v] to [alpha], or out (it ends INVISIBLE, then [done]). It comes
   * in from [dx], [dy] px away, and goes back there if [slideOut]. [move]
   * false: it doesn't touch the translation (the bubble's place is its
   * translation).
   */
  private fun fade(
    v: View,
    on: Boolean,
    alpha: Float = 1f,
    inMs: Long = 160,
    outMs: Long = 240,
    dx: Float = 0f,
    dy: Float = 0f,
    slideOut: Boolean = true,
    move: Boolean = true,
    done: (() -> Unit)? = null,
  ) {
    val target = if (on) alpha else -1f
    if (targets[v] == target) return
    targets[v] = target
    val a = v.animate()
    a.cancel()
    if (on) {
      if (v.visibility != View.VISIBLE) {
        v.alpha = 0f
        if (move) {
          v.translationX = dx
          v.translationY = dy
        }
        v.visibility = View.VISIBLE
      }
      a.alpha(alpha)
      if (move) a.translationX(0f).translationY(0f)
      a.setStartDelay(0).setDuration(kit.ms(inMs))
        .setInterpolator(DecelerateInterpolator()).withEndAction { done?.invoke() }.start()
    } else {
      a.alpha(0f)
      if (move) a.translationX(if (slideOut) dx else v.translationX).translationY(if (slideOut) dy else v.translationY)
      a.setStartDelay(0).setDuration(kit.ms(outMs)).setInterpolator(AccelerateInterpolator())
        .withEndAction {
          if (targets[v] == -1f) {
            v.visibility = View.INVISIBLE
            done?.invoke()
          }
        }.start()
    }
  }

  /** Show or hide [v] at once: it swaps with another view in the same place. */
  private fun snap(v: View, on: Boolean) {
    val target = if (on) 1f else -1f
    if (targets[v] == target) return
    targets[v] = target
    v.animate().cancel()
    v.alpha = if (on) 1f else 0f
    v.visibility = if (on) View.VISIBLE else View.INVISIBLE
  }

  /** Bring every view to what the state fields say, animated. */
  fun sync() {
    val playing = mode == Mode.PLAYING
    val up = playing && controlsShown
    val outMs = when {
      scrubbing -> 120L
      panelOpen -> 160L
      else -> 240L
    }
    fade(scrimTop, up, outMs = outMs)
    fade(scrimBottom, up && !panelOpen, outMs = outMs)
    fade(topBar, !playing || panelOpen || (up && !scrubbing), outMs = if (scrubbing) 120L else 240L,
      dy = if (scrubbing) 0f else -kit.dp(8))
    fade(tracksTool.view, playing)
    prevTool?.let { fadeTool(it.view, prevRoom, playing && !panelOpen) }
    nextTool?.let { fadeTool(it.view, nextRoom, playing && !panelOpen) }
    fadeTool(fitTool.view, fitAvailable, playing && !panelOpen)
    fade(transport, up && !panelOpen && !scrubbing, outMs = outMs)
    fade(band, up && !panelOpen, outMs = outMs, dy = kit.dp(8), done = {
      updateSpacer()
      lift()
    })
    fade(moments, momentOn, alpha = if (scrubbing) 0f else 1f, inMs = if (scrubbing) 120L else 200L, outMs = 200L,
      dx = kit.dp(16), slideOut = false, done = { lift() })
    timeline.thumb = up
    // With the controls hidden, a D-pad press seeks; it mustn't land on the moment button first.
    moments.button.isFocusable = up
    moments.watch.isFocusable = up
    syncTracksTool()
    syncStatus()
    placePill()
    lift()
  }

  /** A top-bar tool that's left out ([room] false: GONE), or faded in or out. */
  private fun fadeTool(v: View, room: Boolean, on: Boolean) {
    if (room) {
      fade(v, on, outMs = 160)
    } else {
      v.animate().cancel()
      v.visibility = View.GONE
      targets.remove(v)
    }
  }

  /** The moment button should show ([on]); [changed]: its words changed. */
  fun setMoment(on: Boolean, changed: Boolean) {
    if (momentOn != on) {
      momentOn = on
      sync()
      updateSpacer()
    } else if (changed) {
      updateSpacer()
    }
  }

  /**
   * The timeline makes room at the band's right end for the moment button.
   * Not during a drag. It changes in view only while the band shows: while
   * the band fades in from nothing it changes at once, and while it fades
   * out it waits for the band to go.
   */
  fun updateSpacer() {
    if (timeline.dragging) return
    val want = if (!momentAbove && momentOn) moments.wantedWidth() + kit.px(12) else 0
    if (want == spacerTarget) return
    val hidden = band.visibility != View.VISIBLE
    val goingOut = targets[band] == -1f
    if (goingOut && !hidden) return
    spacerTarget = want
    spacerAnim?.cancel()
    spacerAnim = null
    if (hidden || band.alpha < 0.05f) {
      setSpacer(want)
      return
    }
    spacerAnim = ValueAnimator.ofInt(spacerWidth, want).apply {
      duration = kit.ms(200)
      addUpdateListener { setSpacer(it.animatedValue as Int) }
      addListener(object : AnimatorListenerAdapter() {
        override fun onAnimationEnd(animation: Animator) {
          if (spacerAnim === animation) spacerAnim = null
        }
      })
      start()
    }
  }

  private fun setSpacer(w: Int) {
    spacerWidth = w
    spacer.layoutParams = (spacer.layoutParams as LinearLayout.LayoutParams).apply { width = w }
  }

  private fun syncTracksTool() {
    tracksTool.shape.setColor(if (panelOpen) Colors.ACCENT_16 else Colors.TRANSPARENT)
    tracksIcon.imageTintList = ColorStateList.valueOf(if (panelOpen) Colors.ACCENT else Colors.WHITE)
    tracksTool.view.contentDescription = if (panelOpen) "Close audio and subtitles" else "Audio and subtitles"
  }

  /** The spinner and its caption: in the play disc with the controls up, else in a disc of its own. */
  private fun syncStatus() {
    val loading = mode == Mode.LOADING
    val playing = mode == Mode.PLAYING
    val inDisc = playing && busy && controlsShown && !panelOpen
    val discOn = loading || (playing && busy && !controlsShown && !panelOpen)
    fade(statusDisc, discOn, inMs = 100, outMs = 150)
    statusDisc.contentDescription = if (loading) "Loading" else "Buffering"
    playSpinner.visibility = if (inDisc) View.VISIBLE else View.INVISIBLE
    playIcon.visibility = if (inDisc) View.INVISIBLE else View.VISIBLE
    val pct = if (loading) null else busyPct
    val word = if (loading) loadingCaption else pct?.let { "Buffering" }
    // In the Ask disc too, in the Ask caption's place.
    val on = word != null && (discOn || inDisc)
    // Shown first, then the words: a live region is said when it changes while it shows.
    // Where the Ask caption shows, the two swap at once, as if its words changed.
    if (member && playing && controlsShown && !panelOpen) snap(statusCaption, on)
    else fade(statusCaption, on, inMs = 100, outMs = 150)
    if (on) {
      val rest = pct?.let { " $it%" } ?: ""
      if (statusPct.text.toString() != rest) statusPct.text = rest
      statusPct.visibility = if (rest.isEmpty()) View.GONE else View.VISIBLE
      // Said again when it comes back (a live region is said when its words
      // change, and the same words again aren't a change); not for a new percent.
      if (!captionShown) statusWord.text = ""
      if (statusWord.text.toString() != word) statusWord.text = word
    }
    captionShown = on
    syncAskCaption()
  }

  private fun syncAskCaption() {
    val c = askCaption ?: return
    // "Buffering 45%" replaces it: the status caption, the same pill in the
    // same place, and a live region (so TalkBack says it to a member too).
    snap(c, !captionShown)
    val text = if (SystemClock.uptimeMillis() < askedUntil) "Asked" else askText
    if (c.text.toString() != text) c.text = text
  }

  /** Buffering ([on]), with "Buffering 45%" ([percent]) once it's taken a while. */
  fun setBusy(on: Boolean, percent: Int?) {
    if (busy == on && busyPct == percent) return
    busy = on
    busyPct = percent
    syncStatus()
  }

  /** "Loading" or "Reconnecting…" under the spinner (null: none yet). */
  fun setLoading(caption: String?) {
    loadingCaption = caption
    syncStatus()
  }

  /** Messages sit under the top bar while it shows, and beside the panel while that's open. */
  private fun placePill() {
    val w = root.width
    val up = panelOpen || mode != Mode.PLAYING || controlsShown
    val y = if (up) max(barH, kit.px(48)).toFloat() else 0f
    val beside = panelOpen && !panel.isDocked && w > 0
    val room = w - panel.panelWidth() - padR - kit.px(8)
    val x = if (beside) room / 2f - w / 2f else 0f
    // Beside the panel, not over it.
    val maxW = if (beside) min((0.7 * w).toInt(), room - kit.px(16)) else (0.7 * w).toInt()
    if (maxW != pill.maxW) {
      pill.maxW = maxW
      pill.requestLayout()
    }
    if (x == pillX && y == pillY) return
    val first = pillX.isNaN()
    pillX = x
    pillY = y
    pillLayer.animate().cancel()
    if (first) {
      pillLayer.translationX = x
      pillLayer.translationY = y
    } else {
      pillLayer.animate().translationX(x).translationY(y).setStartDelay(0).setDuration(kit.ms(200))
        .setInterpolator(DecelerateInterpolator()).start()
    }
  }

  // ── Subtitles stay above whatever covers the picture's bottom ───────

  /** Tell mpv where subtitles go now. */
  fun lift() = actions.subPos(subPos())

  private fun subPos(): Int {
    val w = root.width
    val h = root.height
    if (w <= 0 || h <= 0 || mode != Mode.PLAYING || panelOpen) return 100
    val bandUsed = band.visibility == View.VISIBLE
    val momentUsed = moments.visibility == View.VISIBLE
    if (!bandUsed && !momentUsed) return 100
    var top = 0.0
    var bottom = h.toDouble()
    if (!fill && videoW > 0 && videoH > 0) {
      val s = min(w.toDouble() / videoW, h.toDouble() / videoH)
      val ph = videoH * s
      top = (h - ph) / 2
      bottom = top + ph
    }
    val ph = bottom - top
    val bandTop = h - padB - kit.px(48)
    // When the moment button sits above the band, subtitles stay above it too.
    val coveredTop = if (momentAbove && momentUsed) bandTop - kit.px(56) - kit.px(4) else bandTop - kit.px(4)
    val covered = max(0.0, bottom - coveredTop)
    if (covered <= 0 || ph <= 0) return 100
    val lift = ceil(covered / ph * 100).toInt() + 1
    return (100 - lift).coerceIn(0, 100)
  }

  // ── Where things go ─────────────────────────────────────────────────

  /**
   * The room the display cutout and gesture areas leave (px). [gesture]: the
   * height of the bottom edge's system gesture area.
   */
  fun setPads(l: Int, r: Int, t: Int, b: Int, gesture: Int) {
    if (l == padL && r == padR && t == padT && b == padB && gesture == gestureB) return
    padL = l
    padR = r
    padT = t
    padB = b
    gestureB = gesture
    layout()
  }

  /**
   * A scrim [height] px tall, darkest at the screen's edge. It stays dark
   * for the [text] px nearest the edge, where the top bar or the band is,
   * then eases out. There it's 66% or more: DIM text reads at 4.5:1 even on
   * a white frame (it needs 62%, white needs 54%).
   */
  private fun shade(scrim: View, height: Int, text: Int) {
    val f = if (height > 0) (text.toFloat() / height).coerceIn(0f, 0.9f) else 0.5f
    val rest = 1 - f
    (scrim.background as GradientDrawable).setColors(
      intArrayOf(0xB3000000.toInt(), 0xA8000000.toInt(), 0x8F000000.toInt(), 0x54000000, 0x19000000, 0),
      floatArrayOf(0f, f, f + rest * 0.25f, f + rest * 0.5f, f + rest * 0.75f, 1f),
    )
  }

  /** Put everything in place for this window's size and insets. */
  fun layout() {
    val w = root.width
    val h = root.height
    if (w <= 0 || h <= 0) return
    compact = kit.toDp(w) < 600
    topBar.layoutParams = FrameLayout.LayoutParams(MATCH, WRAP, Gravity.TOP).apply {
      leftMargin = padL
      rightMargin = padR
      topMargin = padT
    }
    topBar.minimumHeight = kit.px(48)
    fitTopBar()
    topBar.measure(
      View.MeasureSpec.makeMeasureSpec(max(0, w - padL - padR), View.MeasureSpec.EXACTLY),
      View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
    )
    barH = max(kit.px(48), topBar.measuredHeight)
    val topH = max(kit.px(104), padT + barH + kit.px(48))
    scrimTop.layoutParams = FrameLayout.LayoutParams(MATCH, topH, Gravity.TOP)
    shade(scrimTop, topH, padT + barH)
    val bottomH = max(kit.px(112), padB + kit.px(48 + 64))
    scrimBottom.layoutParams = FrameLayout.LayoutParams(MATCH, bottomH, Gravity.BOTTOM)
    shade(scrimBottom, bottomH, padB + kit.px(48))
    val bandTop = h - padB - kit.px(48)
    val cy = (padT + barH + bandTop) / 2
    val cx = w / 2
    val apart = kit.px(if (compact) 96 else 112)
    // Just round the discs (and the Ask caption), so TalkBack reads them after the top bar.
    val tw = max(2 * apart + kit.px(72), kit.px(240))
    val tx = tw / 2
    val ty = kit.px(36)
    // Tall enough for the Ask caption's pill at any text size (the transport clips it).
    val askH = askCaption?.let {
      it.measure(
        View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
        View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
      )
      it.measuredHeight
    } ?: 0
    transport.layoutParams = FrameLayout.LayoutParams(tw, max(kit.px(112), ty + kit.px(40) + askH + kit.px(2)),
      Gravity.TOP or Gravity.LEFT).apply {
      leftMargin = cx - tw / 2
      topMargin = cy - kit.px(36)
    }
    place(playDisc.view, kit.px(72), tx, ty)
    back10?.let { place(it.view, kit.px(56), tx - apart, ty) }
    fwd10?.let { place(it.view, kit.px(56), tx + apart, ty) }
    askCaption?.layoutParams = FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL).apply {
      topMargin = ty + kit.px(40)
    }
    band.layoutParams = FrameLayout.LayoutParams(MATCH, kit.px(48), Gravity.BOTTOM).apply {
      leftMargin = padL
      rightMargin = padR
      bottomMargin = padB
    }
    // The band's lowest part is in the system's swipe-up area: a touch there waits to see which way it goes.
    timeline.waitBelow = if (gestureB > 0) (h - gestureB - bandTop).toFloat() else Float.MAX_VALUE
    place(statusDisc, kit.px(64), cx, cy)
    statusCaption.layoutParams = FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL).apply {
      topMargin = cy + kit.px(40)
    }
    pill.layoutParams = FrameLayout.LayoutParams(WRAP, WRAP, Gravity.TOP or Gravity.CENTER_HORIZONTAL).apply {
      topMargin = padT + kit.px(12) - kit.px(4)
    }
    card.layoutParams = FrameLayout.LayoutParams(min(kit.px(420), (0.8 * w).toInt()), WRAP, Gravity.CENTER)
    panel.place(w, h, padL, padR, padT + barH + kit.px(8), padB)
    placeMoments(force = true)
    pillX = Float.NaN
    placePill()
    lift()
  }

  /**
   * What the top bar's right side holds, so the titles keep at least 160 dp:
   * the party chip says "Watch party" only (and always under 700 dp), and a
   * narrow window leaves out Previous, then Next (near the end, the moment
   * button still offers the next episode).
   */
  private fun fitTopBar() {
    val w = root.width
    if (w <= 0) return
    val room = w - padL - padR - kit.px(48 + 12 + 12)
    val least = kit.px(160)
    fun cluster(long: Boolean, prev: Boolean, next: Boolean): Int {
      var c = kit.px(48)
      if (fitAvailable) c += kit.px(8 + 48)
      val n = next && nextTool != null
      if (n) c += kit.px(48 + 16)
      if (prev && prevTool != null) c += kit.px(48) + kit.px(if (n) 8 else 16)
      chip?.let {
        chipDetail.visibility = if (long) View.VISIBLE else View.GONE
        it.measure(
          View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
          View.MeasureSpec.makeMeasureSpec(kit.px(32), View.MeasureSpec.EXACTLY),
        )
        c += it.measuredWidth + kit.px(12)
      }
      return c
    }
    val long = kit.toDp(w) >= 700 && room - cluster(true, true, true) >= least
    var prev = true
    var next = true
    if (room - cluster(long, prev, next) < least) prev = false
    if (room - cluster(long, prev, next) < least) next = false
    chipDetail.visibility = if (long) View.VISIBLE else View.GONE
    if (prev != prevRoom || next != nextRoom) {
      prevRoom = prev
      nextRoom = next
      sync()
    }
  }

  /**
   * The moment button at the band's end, or above the band ([momentAbove]).
   * Decided for its widest (a countdown), so it doesn't move when its words
   * change. [force]: set it again even if that's the same (a new layout).
   */
  private fun placeMoments(force: Boolean = false) {
    val w = root.width
    if (w <= 0) return
    val labels = elapsed.layoutParams.width + remaining.layoutParams.width + kit.px(12 + 12)
    val timelineRoom = w - padL - padR - labels - moments.widestWidth() - kit.px(12)
    val above = compact || timelineRoom < kit.px(160)
    if (!force && above == momentAbove) return
    momentAbove = above
    moments.layoutParams = FrameLayout.LayoutParams(WRAP, kit.px(48), Gravity.BOTTOM or Gravity.RIGHT).apply {
      rightMargin = padR
      bottomMargin = padB + if (above) kit.px(56) else 0
    }
    spacerTarget = -1
    updateSpacer()
    lift()
  }

  private fun place(v: View, size: Int, cx: Int, cy: Int) {
    v.layoutParams = FrameLayout.LayoutParams(size, size, Gravity.TOP or Gravity.LEFT).apply {
      leftMargin = cx - size / 2
      topMargin = cy - size / 2
    }
  }

  // ── What they say ───────────────────────────────────────────────────

  /** The time labels' widths fit the longest time, so they don't move. */
  fun setDuration(d: Double) {
    val sample = clock(d).replace(Regex("[0-9]"), "0")
    val ew = max(kit.px(40), ceil(elapsed.paint.measureText(sample)).toInt() + kit.px(2))
    val rw = max(kit.px(40), ceil(remaining.paint.measureText("−" + sample)).toInt() + kit.px(2))
    elapsed.layoutParams = (elapsed.layoutParams as LinearLayout.LayoutParams).apply { width = ew }
    remaining.layoutParams = (remaining.layoutParams as LinearLayout.LayoutParams).apply { width = rw }
    placeMoments()
  }

  fun setTimes(t: Double, d: Double) {
    val e = clock(t)
    val r = "−" + clock(max(0.0, d - t))
    if (elapsed.text.toString() != e) elapsed.text = e
    if (remaining.text.toString() != r) remaining.text = r
  }

  /** The chapter after the episode's name (blank: none). */
  fun setChapterTitle(t: String) {
    if (t.isBlank()) {
      chapterText.visibility = View.GONE
    } else {
      chapterText.text = " · $t"
      chapterText.visibility = View.VISIBLE
    }
  }

  fun setSubsOff(off: Boolean) {
    tracksIcon.setImageResource(if (off) R.drawable.ic_captions_off else R.drawable.ic_captions)
  }

  /** The Fit/Fill tool for the saved choice. */
  fun setFillIcon(on: Boolean) {
    fitIcon.setImageResource(if (on) R.drawable.ic_shrink else R.drawable.ic_expand)
    fitTool.view.contentDescription = if (on) "Fit the picture to the screen" else "Fill the screen"
  }

  fun setPaused(p: Boolean) {
    val res = if (p) R.drawable.ic_play else R.drawable.ic_pause
    if (playIcon.tag != res) {
      playIcon.tag = res
      playIcon.animate().cancel()
      playIcon.animate().alpha(0f).setStartDelay(0).setDuration(kit.ms(60)).withEndAction {
        playIcon.setImageResource(res)
        playIcon.translationX = if (p) kit.dp(1.5f) else 0f
        playIcon.animate().alpha(1f).setStartDelay(0).setDuration(kit.ms(60)).start()
      }.start()
    }
    playDisc.view.contentDescription = when {
      member && p -> "Ask the host to play"
      member -> "Ask the host to pause"
      p -> "Play"
      else -> "Pause"
    }
    askText = if (p) "Ask to play" else "Ask to pause"
    syncAskCaption()
  }

  /** The Ask disc says "Asked" for [ms], and pulses. */
  fun asked(ms: Long) {
    askedUntil = SystemClock.uptimeMillis() + ms
    syncAskCaption()
    root.postDelayed({ syncAskCaption() }, ms + 20)
    val d = playDisc.disc
    d.animate().cancel()
    d.scaleX = 0.94f
    d.scaleY = 0.94f
    d.animate().scaleX(1f).scaleY(1f).setStartDelay(0).setDuration(kit.ms(150)).setInterpolator(DecelerateInterpolator()).start()
  }

  /** The host did something: the caption stops saying "Asked". */
  fun resetAsked() {
    askedUntil = 0
    syncAskCaption()
  }

  // ── The scrub bubble ────────────────────────────────────────────────

  fun showBubble(on: Boolean) = fade(bubble, on, inMs = 100, outMs = 150, move = false)

  /** The bubble over [t] on the timeline: the time and the chapter. */
  fun moveBubble(t: Double, chapter: String, amber: Boolean) {
    bubbleTime.text = clock(t)
    if (chapter.isBlank()) {
      bubbleChapter.visibility = View.GONE
    } else {
      bubbleChapter.visibility = View.VISIBLE
      bubbleChapter.text = chapter
      bubbleChapter.setTextColor(if (amber) Colors.AMBER else Colors.DIM)
    }
    bubble.measure(
      View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
      View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
    )
    val bw = bubble.measuredWidth
    val bh = bubble.measuredHeight
    val left = band.left + timeline.left
    val x = left + timeline.xOf(t)
    val lo = left.toFloat()
    val hi = (left + timeline.width - bw).toFloat()
    bubble.translationX = (x - bw / 2f).coerceIn(lo, max(lo, hi))
    bubble.translationY = band.top + band.height / 2f - kit.dp(4) - kit.dp(12) - bh
  }

  // ── The error card ──────────────────────────────────────────────────

  fun showCard(title: String, detail: String, onClose: () -> Unit, onRetry: () -> Unit) {
    cardTitle.text = title
    cardDetail.text = detail
    cardClose.setOnClickListener { onClose() }
    cardRetry.setOnClickListener { onRetry() }
    // TalkBack says it appeared, and goes to it.
    cardLayer.accessibilityPaneTitle = title
    cardLayer.visibility = View.VISIBLE
    // A D-pad or keyboard: the focus ring goes to Try again.
    if (!root.isInTouchMode) cardRetry.requestFocus()
    card.alpha = 0f
    card.animate().alpha(1f).setStartDelay(0).setDuration(kit.ms(160)).setInterpolator(DecelerateInterpolator()).start()
    cardWords.post {
      if (cardLayer.visibility == View.VISIBLE) {
        cardWords.performAccessibilityAction(AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS, null)
      }
    }
  }

  fun hideCard() {
    card.animate().cancel()
    cardLayer.visibility = View.INVISIBLE
  }

  /** Stop every animation (the player is closing). */
  fun release() {
    for (v in targets.keys) v.animate().cancel()
    spacerAnim?.cancel()
    pillLayer.animate().cancel()
    playIcon.animate().cancel()
    playDisc.disc.animate().cancel()
    card.animate().cancel()
    timeline.release()
    taps.release()
    moments.release()
    panel.release()
    pill.release()
  }
}
