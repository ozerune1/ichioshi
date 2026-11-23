//! Android: logging to logcat, and opening videos and links in other apps
//! (through Java, from the activity the app runs in).

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;

use iced_winit::android::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn keep(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}

/// How much of the window's bottom Android's own bars cover (the gesture
/// bar, the keyboard), in pixels: what the app keeps clear.
pub fn bottom_covered() -> f32 {
    let Some(app) = APP.get() else { return 0.0 };
    let rect = app.content_rect();
    let height = app.native_window().map_or(0, |w| w.height());
    (height - rect.bottom).max(0) as f32
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// One line to logcat (tag "SeaDex").
pub fn log(text: &str) {
    let tag = c"SeaDex";
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
    use jni::objects::{JObject, JValue};
    // MediaStore knows files by their real paths (/sdcard is a link).
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let path = real.to_str()?;
    let ctx = ndk_context::android_context();
    // SAFETY: the VM and activity android-activity set up for this app.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let find = |env: &mut jni::JNIEnv| -> jni::errors::Result<Option<String>> {
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
                &activity,
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
            id = Some(env.call_method(&cursor, "getLong", "(I)J", &[JValue::Int(0)])?.j()?);
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
        Ok(Some(env.get_string(&text.into())?.into()))
    };
    let attempt = |env: &mut jni::JNIEnv| {
        let found = find(env);
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        match found {
            Ok(link) => link,
            Err(e) => {
                log(&format!("couldn't look the video up in MediaStore: {e}"));
                None
            }
        }
    };
    if let Some(link) = attempt(&mut env) {
        return Some(link);
    }
    // Not there yet: have it scanned.
    let scan = (|| -> jni::errors::Result<()> {
        let value = env.new_string(path)?;
        let paths = env.new_object_array(1, "java/lang/String", &value)?;
        env.call_static_method(
            "android/media/MediaScannerConnection",
            "scanFile",
            "(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;\
             Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&paths),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        )?;
        Ok(())
    })();
    if scan.is_err() || env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
        return None;
    }
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some(link) = attempt(&mut env) {
            return Some(link);
        }
    }
    None
}

/// Open `uri` in the app that shows it (a video's player app, the browser
/// for a link): Android's ACTION_VIEW, with a MIME type if given.
pub fn view(uri: &str, mime: Option<&str>) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    let ctx = ndk_context::android_context();
    // SAFETY: the VM and activity android-activity set up for this app.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result = (|| -> jni::errors::Result<()> {
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
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })();
    // A Java exception (no app to open it, say) is cleared, and said.
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
        return Err("no app on this phone opens it".into());
    }
    result.map_err(|e| e.to_string())
}
