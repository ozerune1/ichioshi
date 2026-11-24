package app.ichioshi

import org.json.JSONArray
import java.util.Locale
import kotlin.math.abs

/** A part of an episode, by its chapter name. */
enum class Part { OPENING, ENDING, PREVIEW, OTHER }

private val OPENING_NAME =
  Regex("""(?i)^\s*(op\s*\d*|opening(\s+(song|theme|credits))?|intro)\s*\d*\s*$""")
private val ENDING_NAME =
  Regex("""(?i)^\s*(ed\s*\d*|ending(\s+(song|theme|credits))?|outro|credits|end\s+credits)\s*\d*\s*$""")
private val PREVIEW_NAME = Regex("""(?i)\bpreview\b""")

/** "Opening", "OP", "Intro", "OP1"; "Ending", "ED", "Credits"; "Preview" (the desktop player's rules). */
fun partOf(title: String): Part = when {
  OPENING_NAME.containsMatchIn(title) -> Part.OPENING
  ENDING_NAME.containsMatchIn(title) -> Part.ENDING
  PREVIEW_NAME.containsMatchIn(title) -> Part.PREVIEW
  else -> Part.OTHER
}

class Chapter(val title: String, val start: Double, val part: Part)

/** An episode's chapters, and what the player asks of them. */
class Chapters(val list: List<Chapter>, val duration: Double) {
  /** Some chapter is an opening or an ending. */
  val named = list.any { it.part == Part.OPENING || it.part == Part.ENDING }

  fun kind(i: Int): Part = list.getOrNull(i)?.part ?: Part.OTHER
  fun title(i: Int): String = list.getOrNull(i)?.title ?: ""
  fun start(i: Int): Double = list.getOrNull(i)?.start ?: 0.0
  fun end(i: Int): Double = list.getOrNull(i + 1)?.start ?: duration

  /** Chapter [i] runs to the end of the episode. */
  fun last(i: Int) = abs(end(i) - duration) < 0.5

  /** Where the endings and previews at the end start (the list's size: there are none). */
  val tailStart: Int = run {
    var k = list.size
    if (list.isNotEmpty() && last(list.size - 1)) {
      while (k > 0 && (list[k - 1].part == Part.ENDING || list[k - 1].part == Part.PREVIEW)) k--
    }
    k
  }

  fun inTail(i: Int) = i >= tailStart && i < list.size

  /** The chapter at [t] (-1: before the first). */
  fun at(t: Double): Int {
    var found = -1
    for ((i, c) in list.withIndex()) if (c.start <= t) found = i
    return found
  }

  companion object {
    /** mpv's chapter-list, as JSON. */
    fun parse(json: String?, duration: Double): Chapters {
      val out = mutableListOf<Chapter>()
      try {
        val a = JSONArray(json ?: "[]")
        for (i in 0 until a.length()) {
          val o = a.getJSONObject(i)
          val title = o.optString("title", "")
          out += Chapter(title, o.optDouble("time", 0.0), partOf(title))
        }
      } catch (e: Exception) {
        // No chapters, then.
      }
      return Chapters(out, duration)
    }
  }
}

/** An audio or subtitle track, from mpv's track-list. */
class Track(
  val id: Int,
  val type: String,
  val title: String,
  val lang: String,
  val codec: String,
  val channels: Int,
  val forced: Boolean,
  val external: Boolean,
  val hearingImpaired: Boolean,
  val selected: Boolean,
) {
  /** Line one: the language, else the title, else "Track N". */
  val name: String
  /** Line two: the title (unless it's line one), codec and channels, forced, SDH, external. */
  val detail: String

  init {
    val language = lang.trim().takeIf { it.isNotEmpty() && it != "und" }?.let { languageName(normalizeLanguage(it)) }
    val t = title.trim()
    name = language ?: t.ifEmpty { "Track $id" }
    val parts = mutableListOf<String>()
    if (t.isNotEmpty() && language != null && t != name) parts += t
    if (type == "audio") {
      val ch = when (channels) {
        1 -> " 1.0"
        2 -> " 2.0"
        6 -> " 5.1"
        8 -> " 7.1"
        else -> ""
      }
      if (codec.isNotEmpty()) parts += codecName(codec) + ch
    } else if (codec.isNotEmpty()) {
      parts += codecName(codec)
    }
    if (forced && parts.none { it.lowercase().contains("forced") } && !name.lowercase().contains("forced")) parts += "Forced"
    if (hearingImpaired) parts += "SDH"
    if (external) parts += "External"
    detail = parts.joinToString(" · ")
  }

  companion object {
    /** mpv's track-list, as JSON. */
    fun parse(json: String?): List<Track> {
      val out = mutableListOf<Track>()
      try {
        val a = JSONArray(json ?: "[]")
        for (i in 0 until a.length()) {
          val o = a.getJSONObject(i)
          out += Track(
            id = o.optInt("id", 0),
            type = o.optString("type", ""),
            title = o.optString("title", ""),
            lang = o.optString("lang", ""),
            codec = o.optString("codec", ""),
            channels = o.optInt("demux-channel-count", o.optInt("audio-channels", 0)),
            forced = o.optBoolean("forced", false),
            external = o.optBoolean("external", false),
            hearingImpaired = o.optBoolean("hearing-impaired", false),
            selected = o.optBoolean("selected", false),
          )
        }
      } catch (e: Exception) {
        // No tracks, then.
      }
      return out
    }
  }
}

/** A codec's usual name ("AAC", "ASS"). */
fun codecName(codec: String): String = when (codec) {
  "flac" -> "FLAC"
  "aac" -> "AAC"
  "opus" -> "Opus"
  "ac3" -> "AC-3"
  "eac3" -> "E-AC-3"
  "truehd" -> "TrueHD"
  "dts" -> "DTS"
  "mp3" -> "MP3"
  "vorbis" -> "Vorbis"
  "pcm_s16le", "pcm_s24le" -> "PCM"
  "ass", "ssa" -> "ASS"
  "subrip" -> "SRT"
  "hdmv_pgs_subtitle" -> "PGS"
  "dvd_subtitle" -> "VobSub"
  else -> codec.uppercase(Locale.ROOT)
}

/**
 * Matroska's ISO 639-2 code for a language tag: two-letter and
 * "terminology" codes become the bibliographic codes Matroska uses
 * ("en" is "eng", "deu" is "ger"). The desktop app's table (src/remux.rs).
 */
fun normalizeLanguage(code: String): String {
  val c = code.trim().lowercase(Locale.ROOT)
  return when (val primary = c.split('-', '_').firstOrNull() ?: "") {
    "" -> "und"
    "ja" -> "jpn"
    "en" -> "eng"
    "es" -> "spa"
    "pt" -> "por"
    "fr", "fra" -> "fre"
    "de", "deu" -> "ger"
    "it" -> "ita"
    "ru" -> "rus"
    "ar" -> "ara"
    "zh", "zho" -> "chi"
    "ko" -> "kor"
    "th" -> "tha"
    "vi" -> "vie"
    "id" -> "ind"
    "ms", "msa" -> "may"
    "hi" -> "hin"
    "pl" -> "pol"
    "tr" -> "tur"
    "uk" -> "ukr"
    "he" -> "heb"
    "nl", "nld" -> "dut"
    "sv" -> "swe"
    "no" -> "nor"
    "nb" -> "nob"
    "nn" -> "nno"
    "da" -> "dan"
    "fi" -> "fin"
    "cs", "ces" -> "cze"
    "sk", "slk" -> "slo"
    "hu" -> "hun"
    "ro", "ron" -> "rum"
    "el", "ell" -> "gre"
    "bg" -> "bul"
    "hr" -> "hrv"
    "sr" -> "srp"
    "sl" -> "slv"
    "ca" -> "cat"
    "gl" -> "glg"
    "eu", "eus" -> "baq"
    "fa", "fas" -> "per"
    "is", "isl" -> "ice"
    "tl" -> "tgl"
    else -> primary
  }
}

/** The English name of a (normalized) language code. */
fun languageName(code: String): String = when (code) {
  "jpn" -> "Japanese"
  "eng" -> "English"
  "spa" -> "Spanish"
  "por" -> "Portuguese"
  "fre" -> "French"
  "ger" -> "German"
  "ita" -> "Italian"
  "rus" -> "Russian"
  "ara" -> "Arabic"
  "chi" -> "Chinese"
  "kor" -> "Korean"
  "tha" -> "Thai"
  "vie" -> "Vietnamese"
  "ind" -> "Indonesian"
  "may" -> "Malay"
  "hin" -> "Hindi"
  "pol" -> "Polish"
  "tur" -> "Turkish"
  "ukr" -> "Ukrainian"
  "heb" -> "Hebrew"
  "dut" -> "Dutch"
  "swe" -> "Swedish"
  "nor" -> "Norwegian"
  "nob" -> "Norwegian Bokmål"
  "nno" -> "Norwegian Nynorsk"
  "dan" -> "Danish"
  "fin" -> "Finnish"
  "cze" -> "Czech"
  "slo" -> "Slovak"
  "hun" -> "Hungarian"
  "rum" -> "Romanian"
  "gre" -> "Greek"
  "bul" -> "Bulgarian"
  "hrv" -> "Croatian"
  "srp" -> "Serbian"
  "slv" -> "Slovenian"
  "cat" -> "Catalan"
  "glg" -> "Galician"
  "baq" -> "Basque"
  "fil" -> "Filipino"
  "tgl" -> "Tagalog"
  "per" -> "Persian"
  "ben" -> "Bengali"
  "tam" -> "Tamil"
  "tel" -> "Telugu"
  "mal" -> "Malayalam"
  "kan" -> "Kannada"
  "mar" -> "Marathi"
  "urd" -> "Urdu"
  "lit" -> "Lithuanian"
  "lav" -> "Latvian"
  "est" -> "Estonian"
  "ice" -> "Icelandic"
  "und" -> "Unknown language"
  "zxx" -> "No language"
  "mul" -> "Multiple languages"
  else -> code
}

/** "1:05", "1:02:03". */
fun clock(s: Double): String {
  val t = if (s.isNaN()) 0L else s.toLong().coerceAtLeast(0)
  val h = t / 3600
  val m = t / 60 % 60
  val sec = t % 60
  return if (h > 0) String.format(Locale.ROOT, "%d:%02d:%02d", h, m, sec)
  else String.format(Locale.ROOT, "%d:%02d", m, sec)
}

/** A time as TalkBack says it: "1 minute 5 seconds", "0 seconds". */
fun spoken(s: Double): String {
  val t = if (s.isNaN()) 0L else s.toLong().coerceAtLeast(0)
  val parts = mutableListOf<String>()
  fun add(n: Long, unit: String) {
    if (n > 0) parts += if (n == 1L) "1 $unit" else "$n ${unit}s"
  }
  add(t / 3600, "hour")
  add(t / 60 % 60, "minute")
  add(t % 60, "second")
  return if (parts.isEmpty()) "0 seconds" else parts.joinToString(" ")
}
