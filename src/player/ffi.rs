//! libmpv's C API (client.h, render.h, render_gl.h), loaded at run time so
//! the app starts without it: only the player needs it.

use std::ffi::{c_char, c_double, c_int, c_void};
use std::sync::OnceLock;

use libloading::Library;

/// `mpv_handle`, `mpv_render_context`: opaque.
pub enum Handle {}
pub enum RenderContext {}

#[repr(C)]
pub struct Event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct EventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct EventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[repr(C)]
pub struct RenderParam {
    pub kind: c_int,
    pub data: *mut c_void,
}

pub type GetProcAddress =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;
pub type Callback = unsafe extern "C" fn(data: *mut c_void);

#[repr(C)]
pub struct OpenGlInitParams {
    pub get_proc_address: Option<GetProcAddress>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct OpenGlFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub const EVENT_NONE: c_int = 0;
pub const EVENT_SHUTDOWN: c_int = 1;
pub const EVENT_START_FILE: c_int = 6;
pub const EVENT_END_FILE: c_int = 7;
pub const EVENT_FILE_LOADED: c_int = 8;
pub const EVENT_PLAYBACK_RESTART: c_int = 21;
pub const EVENT_PROPERTY_CHANGE: c_int = 22;

pub const FORMAT_NONE: c_int = 0;
pub const FORMAT_STRING: c_int = 1;
pub const FORMAT_FLAG: c_int = 3;
pub const FORMAT_INT64: c_int = 4;
pub const FORMAT_DOUBLE: c_int = 5;

pub const END_FILE_EOF: c_int = 0;
pub const END_FILE_STOP: c_int = 2;
pub const END_FILE_QUIT: c_int = 3;
pub const END_FILE_ERROR: c_int = 4;

pub const RENDER_PARAM_INVALID: c_int = 0;
pub const RENDER_PARAM_API_TYPE: c_int = 1;
pub const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const RENDER_PARAM_FLIP_Y: c_int = 4;

pub const RENDER_UPDATE_FRAME: u64 = 1;

/// The functions used here. The library stays loaded for the whole run.
pub struct Api {
    _lib: Library,
    pub client_api_version: unsafe extern "C" fn() -> std::ffi::c_ulong,
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub create: unsafe extern "C" fn() -> *mut Handle,
    pub initialize: unsafe extern "C" fn(*mut Handle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut Handle),
    pub set_option_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    pub set_property_string:
        unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    pub get_property: unsafe extern "C" fn(*mut Handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub get_property_string: unsafe extern "C" fn(*mut Handle, *const c_char) -> *mut c_char,
    pub command: unsafe extern "C" fn(*mut Handle, *mut *const c_char) -> c_int,
    pub observe_property: unsafe extern "C" fn(*mut Handle, u64, *const c_char, c_int) -> c_int,
    pub wait_event: unsafe extern "C" fn(*mut Handle, c_double) -> *mut Event,
    pub set_wakeup_callback: unsafe extern "C" fn(*mut Handle, Option<Callback>, *mut c_void),
    pub render_context_create:
        unsafe extern "C" fn(*mut *mut RenderContext, *mut Handle, *mut RenderParam) -> c_int,
    pub render_context_set_update_callback:
        unsafe extern "C" fn(*mut RenderContext, Option<Callback>, *mut c_void),
    pub render_context_update: unsafe extern "C" fn(*mut RenderContext) -> u64,
    pub render_context_render: unsafe extern "C" fn(*mut RenderContext, *mut RenderParam) -> c_int,
    pub render_context_report_swap: unsafe extern "C" fn(*mut RenderContext),
    pub render_context_free: unsafe extern "C" fn(*mut RenderContext),
}

#[cfg(target_os = "linux")]
const NAMES: &[&str] = &["libmpv.so.2", "libmpv.so"];
#[cfg(target_os = "windows")]
const NAMES: &[&str] = &["libmpv-2.dll", "mpv-2.dll"];
#[cfg(target_os = "macos")]
const NAMES: &[&str] = &["libmpv.2.dylib", "libmpv.dylib"];
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
const NAMES: &[&str] = &["libmpv.so.2", "libmpv.so"];

/// libmpv, or why it can't be used.
pub fn api() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(String::as_str)
}

fn load() -> Result<Api, String> {
    let mut last = String::new();
    for name in NAMES {
        // SAFETY: loading libmpv runs its initializers, which have no
        // requirements of the caller.
        match unsafe { Library::new(name) } {
            Ok(lib) => return bind(lib).map_err(|e| format!("{name}: {e}")),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("libmpv isn't installed ({last})"))
}

fn bind(lib: Library) -> Result<Api, libloading::Error> {
    macro_rules! sym {
        ($name:literal) => {{
            // SAFETY: the types above match libmpv's headers (API 2.x,
            // checked by the caller).
            let symbol: libloading::Symbol<_> =
                unsafe { lib.get(concat!($name, "\0").as_bytes())? };
            *symbol
        }};
    }
    let api = Api {
        client_api_version: sym!("mpv_client_api_version"),
        error_string: sym!("mpv_error_string"),
        free: sym!("mpv_free"),
        create: sym!("mpv_create"),
        initialize: sym!("mpv_initialize"),
        terminate_destroy: sym!("mpv_terminate_destroy"),
        set_option_string: sym!("mpv_set_option_string"),
        set_property_string: sym!("mpv_set_property_string"),
        get_property: sym!("mpv_get_property"),
        get_property_string: sym!("mpv_get_property_string"),
        command: sym!("mpv_command"),
        observe_property: sym!("mpv_observe_property"),
        wait_event: sym!("mpv_wait_event"),
        set_wakeup_callback: sym!("mpv_set_wakeup_callback"),
        render_context_create: sym!("mpv_render_context_create"),
        render_context_set_update_callback: sym!("mpv_render_context_set_update_callback"),
        render_context_update: sym!("mpv_render_context_update"),
        render_context_render: sym!("mpv_render_context_render"),
        render_context_report_swap: sym!("mpv_render_context_report_swap"),
        render_context_free: sym!("mpv_render_context_free"),
        _lib: lib,
    };
    Ok(api)
}

impl Api {
    /// The client API's major version (2 for mpv 0.35 on).
    #[allow(clippy::unnecessary_cast, reason = "c_ulong is 32 bits on Windows")]
    pub fn major_version(&self) -> u64 {
        // SAFETY: no arguments, no preconditions.
        (unsafe { (self.client_api_version)() } as u64) >> 16
    }

    pub fn error(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string for any code.
        let text = unsafe { std::ffi::CStr::from_ptr((self.error_string)(code)) };
        text.to_string_lossy().into_owned()
    }
}
