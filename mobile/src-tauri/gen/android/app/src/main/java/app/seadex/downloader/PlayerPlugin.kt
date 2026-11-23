package app.seadex.downloader

import android.app.Activity
import android.content.Intent
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
}

/**
 * The phone app's own player (PlayerActivity, libmpv), and sending the app
 * to the background (back on its first page).
 */
@TauriPlugin
class PlayerPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun play(invoke: Invoke) {
    val args = invoke.parseArgs(PlayArgs::class.java)
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

  @Command
  fun toBackground(invoke: Invoke) {
    activity.moveTaskToBack(true)
    invoke.resolve()
  }
}
