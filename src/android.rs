//! Android: logging to logcat; opening videos and links in other apps;
//! the clipboard; what the system's bars cover (all through Java, from
//! the activity the app runs in).

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;

use iced_winit::android::AndroidApp;
use jni::JNIEnv;
use jni::objects::{JObject, JValue};

static APP: OnceLock<AndroidApp> = OnceLock::new();
static VM: OnceLock<Option<jni::JavaVM>> = OnceLock::new();

pub fn keep(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// One line to logcat (tag "Ichioshi").
pub fn log(text: &str) {
    let tag = c"Ichioshi";
    let Ok(text) = CString::new(text.replace('\0', " ")) else {
        return;
    };
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        __android_log_write(4, tag.as_ptr(), text.as_ptr());
    }
}

struct Logcat;

impl std::io::Write for Logcat {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        log(String::from_utf8_lossy(buf).trim_end());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Logs (and panics) go to logcat.
pub fn start_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_ansi(false)
        .without_time()
        .with_writer(|| Logcat)
        .try_init();
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
}

/// Call into Java with the activity: `f`'s Java objects are let go after
/// (the app's thread stays attached to Java, so they'd pile up), and a
/// Java exception is cleared (`f` fails instead).
fn java<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Option<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: the VM android-activity set up for this app; it lives as
    // long as the app.
    let vm = VM
        .get_or_init(|| unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok())
        .as_ref()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    // The activity itself (the context ndk-context has is the app's
    // Application, which has no window).
    let activity = APP.get().map_or(ctx.context(), |app| app.activity_as_ptr());
    // SAFETY: the activity (a global reference) lives as long as the app.
    let activity = unsafe { JObject::from_raw(activity.cast()) };
    let result = env.with_local_frame(32, |env| f(env, &activity));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    result.ok()
}

/// A Java string's text.
fn string(env: &mut JNIEnv, s: JObject) -> jni::errors::Result<String> {
    Ok(env.get_string(&s.into())?.into())
}

/// A video on the phone, for a player app: its MediaStore link (which the
/// player is let read), or failing that its file:// link (players that may
/// read the phone's videos open those).
pub fn play_file(path: &std::path::Path) -> Result<(), String> {
    let link = match media_link(path) {
        Some(link) => link,
        None => format!("file://{}", path.display()),
    };
    view(&link, Some("video/*"))
}

/// The file's content:// link in MediaStore: files written to shared
/// storage are added as they're made; others are scanned (briefly waited
/// for).
fn media_link(path: &std::path::Path) -> Option<String> {
    // MediaStore knows files by their real paths (/sdcard is a link).
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let path = real.to_str()?.to_owned();
    let find = |path: &str| {
        java(|env, activity| {
            let volume = env.new_string("external")?;
            let files = env
                .call_static_method(
                    "android/provider/MediaStore$Files",
                    "getContentUri",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValue::Object(&volume)],
                )?
                .l()?;
            let resolver = env
                .call_method(
                    activity,
                    "getContentResolver",
                    "()Landroid/content/ContentResolver;",
                    &[],
                )?
                .l()?;
            let id_col = env.new_string("_id")?;
            let columns = env.new_object_array(1, "java/lang/String", &id_col)?;
            let selection = env.new_string("_data=?")?;
            let value = env.new_string(path)?;
            let args = env.new_object_array(1, "java/lang/String", &value)?;
            let cursor = env
                .call_method(
                    &resolver,
                    "query",
                    "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;\
                     [Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                    &[
                        JValue::Object(&files),
                        JValue::Object(&columns),
                        JValue::Object(&selection),
                        JValue::Object(&args),
                        JValue::Object(&JObject::null()),
                    ],
                )?
                .l()?;
            if cursor.is_null() {
                return Ok(None);
            }
            let mut id = None;
            if env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()? {
                id = Some(
                    env.call_method(&cursor, "getLong", "(I)J", &[JValue::Int(0)])?
                        .j()?,
                );
            }
            env.call_method(&cursor, "close", "()V", &[])?;
            let Some(id) = id else { return Ok(None) };
            let link = env
                .call_static_method(
                    "android/content/ContentUris",
                    "withAppendedId",
                    "(Landroid/net/Uri;J)Landroid/net/Uri;",
                    &[JValue::Object(&files), JValue::Long(id)],
                )?
                .l()?;
            let text = env
                .call_method(&link, "toString", "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(Some(string(env, text)?))
        })
        .flatten()
    };
    if let Some(link) = find(&path) {
        return Some(link);
    }
    // Not there yet: have it scanned.
    java(|env, activity| {
        let value = env.new_string(&path)?;
        let paths = env.new_object_array(1, "java/lang/String", &value)?;
        env.call_static_method(
            "android/media/MediaScannerConnection",
            "scanFile",
            "(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;\
             Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V",
            &[
                JValue::Object(activity),
                JValue::Object(&paths),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        )?;
        Ok(())
    })?;
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some(link) = find(&path) {
            return Some(link);
        }
    }
    None
}

/// Open `uri` in the app that shows it (a video's player app, the browser
/// for a link): Android's ACTION_VIEW, with a MIME type if given.
pub fn view(uri: &str, mime: Option<&str>) -> Result<(), String> {
    java(|env, activity| {
        // file:// links are fine with player apps that may read the phone's
        // videos; Android only objects to them by default.
        if uri.starts_with("file://") {
            let builder = env.new_object("android/os/StrictMode$VmPolicy$Builder", "()V", &[])?;
            let policy = env
                .call_method(&builder, "build", "()Landroid/os/StrictMode$VmPolicy;", &[])?
                .l()?;
            env.call_static_method(
                "android/os/StrictMode",
                "setVmPolicy",
                "(Landroid/os/StrictMode$VmPolicy;)V",
                &[JValue::Object(&policy)],
            )?;
        }
        let jstr = env.new_string(uri)?;
        let parsed = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&jstr)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action)],
        )?;
        match mime {
            Some(m) => {
                let m = env.new_string(m)?;
                env.call_method(
                    &intent,
                    "setDataAndType",
                    "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
                    &[JValue::Object(&parsed), JValue::Object(&m)],
                )?;
            }
            None => {
                env.call_method(
                    &intent,
                    "setData",
                    "(Landroid/net/Uri;)Landroid/content/Intent;",
                    &[JValue::Object(&parsed)],
                )?;
            }
        }
        // FLAG_ACTIVITY_NEW_TASK | FLAG_GRANT_READ_URI_PERMISSION.
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(0x1000_0000 | 0x1)],
        )?;
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
    // (A Java exception: no app to open it, say.)
    .ok_or_else(|| "no app on this phone opens it".to_owned())
}

/// Android's clipboard service.
fn clipboard_manager<'l>(
    env: &mut JNIEnv<'l>,
    activity: &JObject,
) -> jni::errors::Result<JObject<'l>> {
    let name = env.new_string("clipboard")?;
    env.call_method(
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&name)],
    )?
    .l()
}

/// The clipboard's text, if it has some.
pub fn clipboard() -> Option<String> {
    java(|env, activity| {
        let manager = clipboard_manager(env, activity)?;
        let clip = env
            .call_method(
                &manager,
                "getPrimaryClip",
                "()Landroid/content/ClipData;",
                &[],
            )?
            .l()?;
        if clip.is_null() || env.call_method(&clip, "getItemCount", "()I", &[])?.i()? == 0 {
            return Ok(None);
        }
        let item = env
            .call_method(
                &clip,
                "getItemAt",
                "(I)Landroid/content/ClipData$Item;",
                &[JValue::Int(0)],
            )?
            .l()?;
        let chars = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(activity)],
            )?
            .l()?;
        if chars.is_null() {
            return Ok(None);
        }
        let text = env
            .call_method(&chars, "toString", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(Some(string(env, text)?))
    })
    .flatten()
}

/// Put text on the clipboard.
pub fn set_clipboard(text: &str) {
    java(|env, activity| {
        let manager = clipboard_manager(env, activity)?;
        let label = env.new_string("Ichioshi")?;
        let value = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&value)],
            )?
            .l()?;
        env.call_method(
            &manager,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    });
}

/// What Android's bars, the screen's cutout and the keyboard cover of the
/// window: (top, bottom) in pixels. From the window's insets; failing
/// that, the bottom from the activity's content rectangle.
pub fn covered() -> (f32, f32) {
    insets().unwrap_or_else(|| (0.0, bottom_covered()))
}

fn insets() -> Option<(f32, f32)> {
    java(|env, activity| {
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
            .l()?;
        let insets = env
            .call_method(
                &decor,
                "getRootWindowInsets",
                "()Landroid/view/WindowInsets;",
                &[],
            )?
            .l()?;
        if insets.is_null() {
            return Ok(None);
        }
        let mut kinds = 0;
        for kind in ["systemBars", "displayCutout", "ime"] {
            kinds |= env
                .call_static_method("android/view/WindowInsets$Type", kind, "()I", &[])?
                .i()?;
        }
        let covered = env
            .call_method(
                &insets,
                "getInsets",
                "(I)Landroid/graphics/Insets;",
                &[JValue::Int(kinds)],
            )?
            .l()?;
        let top = env.get_field(&covered, "top", "I")?.i()?;
        let bottom = env.get_field(&covered, "bottom", "I")?.i()?;
        Ok(Some((top.max(0) as f32, bottom.max(0) as f32)))
    })
    .flatten()
}

/// How much of the window's bottom Android's own bars cover (the gesture
/// bar, the keyboard), in pixels, from the activity's content rectangle.
fn bottom_covered() -> f32 {
    let Some(app) = APP.get() else { return 0.0 };
    let rect = app.content_rect();
    let height = app.native_window().map_or(0, |w| w.height());
    (height - rect.bottom).max(0) as f32
}

/// Send the app to the background, as back does on Android's home screens.
pub fn to_background() {
    java(|env, activity| {
        env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])?;
        Ok(())
    });
}
