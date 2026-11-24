package app.ichioshi

import android.app.Activity
import android.content.Intent
import android.os.Handler
import android.os.Looper
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/** What to play (from the app's core, through the web UI). */
@InvokeArg
class PlayArgs {
  var url: String = ""
  var title: String = ""
  var name: String = ""
  var start: Double = 0.0
  var alang: String = ""
  var slang: String = ""
  var skipOpenings: Boolean = false
  var skipEndings: Boolean = false
  var hasNext: Boolean = false
  var hasPrev: Boolean = false
  /** The series and episode (for the watch party). */
  var id: Long = 0
  var episode: Int = 0
  /** "member": following a watch party's host, who controls playback. */
  var party: String = ""
  /** Start paused (the party's player is). */
  var paused: Boolean = false
  /** The party's speed (none: the speed you play at). */
  var speed: Double? = null
}

/** What the watch party wants the player to do. */
@InvokeArg
class ControlArgs {
  var seek: Double? = null
  var paused: Boolean? = null
  var speed: Double? = null
  var close: Boolean = false
}

/** A downloaded update, for the system's installer. */
@InvokeArg
class InstallArgs {
  var path: String = ""
}

/**
 * The phone app's own player (PlayerActivity, libmpv), sending the app to
 * the background (back on its first page), and installing updates.
 */
@TauriPlugin
class PlayerPlugin(private val activity: Activity) : Plugin(activity) {
  companion object {
    /** The players' plays, waiting for them to say how far they got. */
    private val waiting = java.util.concurrent.ConcurrentHashMap<Long, Invoke>()
    private val plays = java.util.concurrent.atomic.AtomicLong()

    /**
     * Play [token] is done: how far it got, and what next. Straight back to
     * the app's core (not through the activity's result, which waits until
     * the app's own window is back: a player in picture in picture would
     * keep it waiting).
     */
    fun finished(token: Long, result: JSObject) {
      waiting.remove(token)?.resolve(result)
    }
  }

  /** Play an episode: what the player says when it's done. */
  @Command
  fun play(invoke: Invoke) {
    val args = invoke.parseArgs(PlayArgs::class.java)
    val token = plays.incrementAndGet()
    waiting[token] = invoke
    // In a task of its own (its affinity): picture in picture takes the
    // task whole, and back in full it's still there to be focused (Android
    // moving it back into the app's task left it without focus).
    val intent = Intent(activity, PlayerActivity::class.java).apply {
      addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      putExtra("token", token)
      putExtra("url", args.url)
      putExtra("title", args.title)
      putExtra("name", args.name)
      putExtra("start", args.start)
      putExtra("alang", args.alang)
      putExtra("slang", args.slang)
      putExtra("skipOpenings", args.skipOpenings)
      putExtra("skipEndings", args.skipEndings)
      putExtra("hasNext", args.hasNext)
      putExtra("hasPrev", args.hasPrev)
      putExtra("id", args.id)
      putExtra("episode", args.episode)
      putExtra("party", args.party)
      putExtra("paused", args.paused)
      args.speed?.let { putExtra("speed", it) }
    }
    Handler(Looper.getMainLooper()).post {
      // The player in picture in picture, at the end and waiting for the
      // next episode: it plays it there.
      if (PlayerActivity.current?.takeNext(intent) == true) return@post
      // One player at a time: one already open closes (saying how far it got).
      PlayerActivity.current?.replaced()
      try {
        activity.startActivity(intent)
      } catch (e: Exception) {
        waiting.remove(token)
        invoke.reject("The player couldn't start: ${e.message}")
      }
    }
  }

  /**
   * Hand a downloaded APK to the system's installer, which asks first (and,
   * the first time, has you allow Ichioshi to install apps).
   */
  @Command
  fun installApk(invoke: Invoke) {
    val args = invoke.parseArgs(InstallArgs::class.java)
    try {
      val file = java.io.File(args.path)
      val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
      val intent = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(uri, "application/vnd.android.package-archive")
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      }
      activity.startActivity(intent)
      invoke.resolve()
    } catch (e: Exception) {
      invoke.reject("Couldn't open the installer: ${e.message}")
    }
  }

  /** The player, for the watch party: open, what, where, paused, asked. */
  @Command
  fun partyState(invoke: Invoke) {
    val p = PlayerActivity.current
    invoke.resolve(p?.state() ?: JSObject().apply { put("open", false) })
  }

  /** Do what the watch party says: seek, pause or play, the speed, close. */
  @Command
  fun partyControl(invoke: Invoke) {
    val args = invoke.parseArgs(ControlArgs::class.java)
    PlayerActivity.current?.control(args.seek, args.paused, args.speed, args.close)
    invoke.resolve()
  }

  @Command
  fun toBackground(invoke: Invoke) {
    activity.moveTaskToBack(true)
    invoke.resolve()
  }
}
