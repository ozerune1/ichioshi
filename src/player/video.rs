//! mpv's picture, drawn with OpenGL into a texture the app shows like any
//! image. Everything here runs on the thread with the GL context current.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

use anyhow::{Result, bail};
use eframe::glow::{self, HasContext};

use super::{Mpv, ffi};

/// Looks up OpenGL functions (eframe's, or a test's).
pub type GetProc = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

type Wake = Box<dyn Fn() + Send + Sync>;

pub struct Video {
    api: &'static ffi::Api,
    ctx: *mut ffi::RenderContext,
    gl: Arc<glow::Context>,
    fbo: glow::Framebuffer,
    pub texture: glow::Texture,
    /// The size the picture was last drawn at.
    size: [i32; 2],
    /// The texture once the app has registered it with egui.
    pub id: Option<egui::TextureId>,
    _get_proc: Box<GetProc>,
    _on_frame: Box<Wake>,
}

unsafe extern "C" fn get_proc(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: `ctx` is the boxed GetProc from `Video::new`; `name` is a C string.
    let (f, name) = unsafe { (&*ctx.cast::<GetProc>(), CStr::from_ptr(name)) };
    f(name).cast_mut()
}

unsafe extern "C" fn on_frame(data: *mut c_void) {
    // SAFETY: `data` is the boxed closure from `Video::new`.
    let f = unsafe { &*data.cast::<Wake>() };
    f();
}

impl Video {
    /// Have mpv draw with this GL context (current on this thread).
    /// `frame_ready` is called, from any thread, when there's a new frame.
    pub fn new(
        mpv: &Mpv,
        gl: Arc<glow::Context>,
        proc_address: GetProc,
        frame_ready: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let api = mpv.api();
        let get_proc_box: Box<GetProc> = Box::new(proc_address);
        let mut init = ffi::OpenGlInitParams {
            get_proc_address: Some(get_proc),
            get_proc_address_ctx: (&raw const *get_proc_box).cast_mut().cast(),
        };
        let mut params = [
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_API_TYPE,
                data: c"opengl".as_ptr().cast_mut().cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx = std::ptr::null_mut();
        // SAFETY: valid handle; the params live for the call.
        let r = unsafe { (api.render_context_create)(&mut ctx, mpv.raw(), params.as_mut_ptr()) };
        if r < 0 {
            bail!("mpv can't draw here: {}", api.error(r));
        }
        let on_frame_box: Box<Wake> = Box::new(Box::new(frame_ready));
        // SAFETY: the closure is kept until the context is freed.
        unsafe {
            (api.render_context_set_update_callback)(
                ctx,
                Some(on_frame),
                (&raw const *on_frame_box).cast_mut().cast(),
            );
        }
        // SAFETY: plain GL calls with the context current.
        let (fbo, texture) = unsafe {
            let texture = gl.create_texture().map_err(anyhow::Error::msg)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            alloc(&gl, 16, 16);
            let fbo = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                (api.render_context_free)(ctx);
                bail!("the video framebuffer isn't usable ({status:#x})");
            }
            (fbo, texture)
        };
        Ok(Self {
            api,
            ctx,
            gl,
            fbo,
            texture,
            size: [16, 16],
            id: None,
            _get_proc: get_proc_box,
            _on_frame: on_frame_box,
        })
    }

    /// Draw the current frame at `size` pixels, if there's a new one or the
    /// size changed. Returns whether it drew.
    pub fn render(&mut self, size: [i32; 2]) -> bool {
        let size = [size[0].clamp(16, 8192), size[1].clamp(16, 8192)];
        // SAFETY: valid context, GL current.
        let flags = unsafe { (self.api.render_context_update)(self.ctx) };
        let resized = size != self.size;
        if flags & ffi::RENDER_UPDATE_FRAME == 0 && !resized {
            return false;
        }
        let gl = &self.gl;
        // SAFETY: GL current; the params live for the call.
        unsafe {
            if resized {
                gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
                alloc(gl, size[0], size[1]);
                gl.bind_texture(glow::TEXTURE_2D, None);
                self.size = size;
            }
            let mut fbo = ffi::OpenGlFbo {
                fbo: self.fbo.0.get() as c_int,
                w: size[0],
                h: size[1],
                internal_format: glow::RGBA8 as c_int,
            };
            // Row 0 of the texture is the picture's top, as egui draws it.
            let mut flip: c_int = 0;
            let mut params = [
                ffi::RenderParam {
                    kind: ffi::RENDER_PARAM_OPENGL_FBO,
                    data: (&raw mut fbo).cast(),
                },
                ffi::RenderParam {
                    kind: ffi::RENDER_PARAM_FLIP_Y,
                    data: (&raw mut flip).cast(),
                },
                ffi::RenderParam {
                    kind: ffi::RENDER_PARAM_INVALID,
                    data: std::ptr::null_mut(),
                },
            ];
            let r = (self.api.render_context_render)(self.ctx, params.as_mut_ptr());
            if r < 0 {
                tracing::warn!("mpv couldn't draw: {}", self.api.error(r));
            }
            // Leave GL as egui expects it.
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        true
    }

    /// The frame was shown (helps mpv's timing).
    pub fn shown(&self) {
        // SAFETY: valid context.
        unsafe { (self.api.render_context_report_swap)(self.ctx) };
    }

    #[cfg(test)]
    pub fn size(&self) -> [i32; 2] {
        self.size
    }

    /// The texture's pixels, row 0 first (the top, as egui shows it).
    #[cfg(test)]
    pub fn pixels(&self) -> Vec<u8> {
        let [w, h] = self.size;
        let mut out = vec![0u8; (w * h * 4) as usize];
        // SAFETY: GL current; the buffer fits the framebuffer.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl.read_pixels(
                0,
                0,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut out)),
            );
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        out
    }
}

/// (Re)size the bound texture.
unsafe fn alloc(gl: &glow::Context, w: i32, h: i32) {
    // SAFETY: a texture is bound, GL current.
    unsafe {
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        // SAFETY: GL current (the app frees the player while drawing, or on
        // exit, before the context goes). A registered texture is egui's.
        unsafe {
            (self.api.render_context_free)(self.ctx);
            self.gl.delete_framebuffer(self.fbo);
            if self.id.is_none() {
                self.gl.delete_texture(self.texture);
            }
        }
    }
}
