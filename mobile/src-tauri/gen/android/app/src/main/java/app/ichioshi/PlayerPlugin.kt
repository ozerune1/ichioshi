package app.ichioshi

import android.app.Activity
import android.content.Intent
import androidx.core.content.FileProvider
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
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
}

/** What the watch party wants the player to do. */
@InvokeArg
class ControlArgs {
  var seek: Double? = null
  var paused: Boolean? = null
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
  @Command
  fun play(invoke: Invoke) {
    val args = invoke.parseArgs(PlayArgs::class.java)
    // One player at a time: one already open closes (saying how far it got).
    PlayerActivity.current?.replaced()
    val intent = Intent(activity, PlayerActivity::class.java).apply {
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
    }
    startActivityForResult(invoke, intent, "played")
  }

  /** How far it got, and what next ("next", "prev" or nothing). */
  @ActivityCallback
  fun played(invoke: Invoke, result: ActivityResult) {
    val data = result.data
    val out = JSObject()
    out.put("position", data?.getDoubleExtra("position", 0.0) ?: 0.0)
    out.put("duration", data?.getDoubleExtra("duration", 0.0) ?: 0.0)
    out.put("ended", data?.getBooleanExtra("ended", false) ?: false)
    out.put("then", data?.getStringExtra("then"))
    out.put("error", data?.getStringExtra("error"))
    invoke.resolve(out)
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

  /** Do what the watch party says: seek, pause or play, close. */
  @Command
  fun partyControl(invoke: Invoke) {
    val args = invoke.parseArgs(ControlArgs::class.java)
    PlayerActivity.current?.control(args.seek, args.paused, args.close)
    invoke.resolve()
  }

  @Command
  fun toBackground(invoke: Invoke) {
    activity.moveTaskToBack(true)
    invoke.resolve()
  }
}
