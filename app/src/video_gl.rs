// Draws the remote screen with OpenGL straight from the decoder's YCbCr planes. The planes
// go up as one-channel (Y, Cb, Cr) or two-channel (CbCr) textures and a fragment shader
// turns them into RGB with the stream's matrix — no RGBA conversion on the CPU and 1.5
// bytes a pixel to upload instead of 4. RGBA pictures (Linux's GStreamer path) are drawn
// by the same shader. Runs inside egui's frame through a paint callback (egui_glow sets
// the viewport and scissor before it and restores its own state after).
#![cfg(not(target_os = "ios"))]
use std::sync::{Arc, Mutex};

use eframe::egui_glow;
use eframe::glow::{self, HasContext};

use crate::video::{Color, Picture, Planes};

/// One picture on screen. Shared by the UI (which hands it new pictures) and egui's paint
/// callbacks (which upload and draw on the GL thread).
pub struct GlVideo {
    inner: Mutex<Inner>,
}

struct Inner {
    /// the newest picture, uploaded at the next paint (and kept: the screenshot reads it)
    pending: Option<Arc<Picture>>,
    /// counts the pictures handed over, so a paint knows whether `pending` is new
    generation: u64,
    gl: Option<State>,
    /// the shader would not build: the caller draws through egui textures instead
    failed: bool,
}

struct State {
    gl: Arc<glow::Context>,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    tex: [glow::Texture; 3],
    /// what the textures hold: the picture's generation, layout kind (0 NV12, 1 I420,
    /// 2 RGBA), sizes and colour
    shown: u64,
    kind: i32,
    sizes: [(i32, i32); 3],
    color: Color,
    u_kind: Option<glow::UniformLocation>,
    u_matrix: Option<glow::UniformLocation>,
    u_offset: Option<glow::UniformLocation>,
    u_scale: Option<glow::UniformLocation>,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_program(self.program);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_buffer(self.vbo);
            for t in self.tex {
                self.gl.delete_texture(t);
            }
        }
    }
}

const VERTEX: &str = r#"
#ifdef NEW_SHADER_INTERFACE
#define I in
#define O out
#else
#define I attribute
#define O varying
#endif
I vec2 a_pos;
I vec2 a_tc;
O vec2 v_tc;
void main() {
    v_tc = a_tc;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
#ifdef NEW_SHADER_INTERFACE
#define I in
#define TEX texture
out vec4 out_color;
#else
#define I varying
#define TEX texture2D
#define out_color gl_FragColor
#endif
#ifdef GL_ES
precision mediump float;
#endif
I vec2 v_tc;
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
uniform int u_kind;
uniform mat3 u_matrix;
uniform vec3 u_offset;
uniform vec3 u_scale;
void main() {
    if (u_kind == 2) {
        out_color = vec4(TEX(u_y, v_tc).rgb, 1.0);
        return;
    }
    float y = TEX(u_y, v_tc).r;
    vec2 c = u_kind == 0 ? TEX(u_u, v_tc).rg : vec2(TEX(u_u, v_tc).r, TEX(u_v, v_tc).r);
    vec3 yuv = (vec3(y, c) - u_offset) * u_scale;
    out_color = vec4(clamp(u_matrix * yuv, 0.0, 1.0), 1.0);
}
"#;

/// (matrix, offset, scale): normalized YCbCr → RGB for a colour signalling.
/// The matrix is column-major (GL) for (Y, Cb, Cr).
fn conversion(c: Color) -> ([f32; 9], [f32; 3], [f32; 3]) {
    let (rv, gu, gv, bu) = if c.bt709 { (1.5748, 0.187324, 0.468124, 1.8556) } else { (1.402, 0.344136, 0.714136, 1.772) };
    let matrix = [1.0, 1.0, 1.0, 0.0, -gu, bu, rv, -gv, 0.0];
    if c.full {
        (matrix, [0.0, 128.0 / 255.0, 128.0 / 255.0], [1.0, 1.0, 1.0])
    } else {
        (matrix, [16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0], [255.0 / 219.0, 255.0 / 224.0, 255.0 / 224.0])
    }
}

impl GlVideo {
    pub fn new() -> Arc<Self> {
        Arc::new(GlVideo { inner: Mutex::new(Inner { pending: None, generation: 0, gl: None, failed: false }) })
    }

    /// The shader could not be built on this GL: draw through egui instead.
    pub fn failed(&self) -> bool {
        self.inner.lock().unwrap().failed
    }

    /// The newest picture; returns the one it replaces when nothing else holds it (its
    /// buffer goes back to the decoder).
    pub fn set(&self, pic: Arc<Picture>) -> Option<Picture> {
        let mut inner = self.inner.lock().unwrap();
        inner.generation += 1;
        let old = inner.pending.replace(pic);
        old.and_then(|p| Arc::try_unwrap(p).ok())
    }

    /// A paint callback drawing the picture into `rect`.
    pub fn callback(self: &Arc<Self>, rect: egui::Rect) -> egui::PaintCallback {
        let me = self.clone();
        egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                me.paint(painter.gl());
            })),
        }
    }

    fn paint(&self, gl: &Arc<glow::Context>) {
        let mut inner = self.inner.lock().unwrap();
        if inner.failed {
            return;
        }
        if inner.gl.is_none() {
            // MANYDESK_NO_GPU_VIDEO=1: draw through egui textures (comparison, a broken driver)
            if std::env::var_os("MANYDESK_NO_GPU_VIDEO").is_some() {
                inner.failed = true;
                return;
            }
            match unsafe { State::new(gl) } {
                Ok(s) => inner.gl = Some(s),
                Err(e) => {
                    eprintln!("[video] GPU drawing unavailable ({e}); drawing through textures");
                    inner.failed = true;
                    return;
                }
            }
        }
        let (pending, generation) = (inner.pending.clone(), inner.generation);
        let Some(state) = inner.gl.as_mut() else { return };
        unsafe {
            // upload once per new picture; repaints for other reasons only draw
            if let Some(pic) = pending.filter(|_| state.shown != generation) {
                let t0 = std::time::Instant::now();
                state.upload(&pic);
                state.shown = generation;
                crate::perf::add("gl-upload", t0.elapsed());
            }
            state.draw();
        }
    }
}

impl State {
    unsafe fn new(gl: &Arc<glow::Context>) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        // one- and two-channel textures (R8, RG8) need GL 3 / GLES 3
        if !version.is_new_shader_interface() {
            return Err(format!("{version:?} has no R8/RG8 textures"));
        }
        let header = format!("{}\n{}\n", version.version_declaration(), if version.is_new_shader_interface() { "#define NEW_SHADER_INTERFACE" } else { "" });
        let program = gl.create_program()?;
        let mut shaders = vec![];
        for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
            let sh = gl.create_shader(kind)?;
            gl.shader_source(sh, &format!("{header}{src}"));
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                let log = gl.get_shader_info_log(sh);
                gl.delete_shader(sh);
                gl.delete_program(program);
                return Err(format!("shader: {log}"));
            }
            gl.attach_shader(program, sh);
            shaders.push(sh);
        }
        gl.bind_attrib_location(program, 0, "a_pos");
        gl.bind_attrib_location(program, 1, "a_tc");
        gl.link_program(program);
        for sh in shaders {
            gl.detach_shader(program, sh);
            gl.delete_shader(sh);
        }
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(format!("link: {log}"));
        }
        // a quad over the whole viewport (egui's callback sets the viewport to the rect)
        let vao = gl.create_vertex_array()?;
        let vbo = gl.create_buffer()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let quad: [f32; 16] = [-1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
        let bytes = std::slice::from_raw_parts(quad.as_ptr() as *const u8, std::mem::size_of_val(&quad));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
        gl.bind_vertex_array(None);
        let mut tex = [gl.create_texture()?, gl.create_texture()?, gl.create_texture()?];
        for t in &mut tex {
            gl.bind_texture(glow::TEXTURE_2D, Some(*t));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        }
        gl.use_program(Some(program));
        for (i, name) in ["u_y", "u_u", "u_v"].iter().enumerate() {
            gl.uniform_1_i32(gl.get_uniform_location(program, name).as_ref(), i as i32);
        }
        let state = State {
            gl: gl.clone(),
            program,
            vao,
            vbo,
            tex,
            shown: 0,
            kind: -1,
            sizes: [(0, 0); 3],
            color: Color::default(),
            u_kind: gl.get_uniform_location(program, "u_kind"),
            u_matrix: gl.get_uniform_location(program, "u_matrix"),
            u_offset: gl.get_uniform_location(program, "u_offset"),
            u_scale: gl.get_uniform_location(program, "u_scale"),
        };
        if let Some(e) = gl_error(gl) {
            return Err(format!("setup: GL error {e:#x}"));
        }
        Ok(state)
    }

    /// One plane into texture `i`: `channels` bytes a texel, rows `stride` bytes apart.
    unsafe fn plane(&mut self, i: usize, data: &[u8], w: usize, h: usize, stride: usize, channels: usize) {
        let gl = &self.gl;
        let (internal, format) = match channels {
            1 => (glow::R8, glow::RED),
            2 => (glow::RG8, glow::RG),
            _ => (glow::RGBA8, glow::RGBA),
        };
        gl.active_texture(glow::TEXTURE0 + i as u32);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.tex[i]));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, (stride / channels) as i32);
        let bytes = &data[..(h - 1) * stride + w * channels];
        let size = (w as i32, h as i32);
        if self.sizes[i] != size {
            gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, size.0, size.1, 0, format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(bytes)));
            self.sizes[i] = size;
        } else {
            gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, size.0, size.1, format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(bytes)));
        }
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
    }

    unsafe fn upload(&mut self, pic: &Picture) {
        let (w, h) = (pic.width, pic.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let d = &pic.data;
        match pic.planes {
            Planes::Nv12 { y_stride, uv_offset, uv_stride } => {
                self.plane(0, d, w, h, y_stride, 1);
                self.plane(1, &d[uv_offset..], cw, ch, uv_stride, 2);
                self.kind = 0;
            }
            Planes::I420 { y_stride, u_offset, v_offset, c_stride } => {
                self.plane(0, d, w, h, y_stride, 1);
                self.plane(1, &d[u_offset..], cw, ch, c_stride, 1);
                self.plane(2, &d[v_offset..], cw, ch, c_stride, 1);
                self.kind = 1;
            }
            Planes::Rgba => {
                self.plane(0, d, w, h, w * 4, 4);
                self.kind = 2;
            }
        }
        self.color = pic.color;
        self.gl.active_texture(glow::TEXTURE0);
    }

    unsafe fn draw(&self) {
        if self.kind < 0 {
            return;
        }
        let gl = &self.gl;
        gl.use_program(Some(self.program));
        let (matrix, offset, scale) = conversion(self.color);
        gl.uniform_1_i32(self.u_kind.as_ref(), self.kind);
        gl.uniform_matrix_3_f32_slice(self.u_matrix.as_ref(), false, &matrix);
        gl.uniform_3_f32_slice(self.u_offset.as_ref(), &offset);
        gl.uniform_3_f32_slice(self.u_scale.as_ref(), &scale);
        for i in 0..3 {
            gl.active_texture(glow::TEXTURE0 + i as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex[i]));
        }
        // opaque: no blending with what egui drew under it
        gl.disable(glow::BLEND);
        gl.bind_vertex_array(Some(self.vao));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
        gl.active_texture(glow::TEXTURE0);
    }
}

fn gl_error(gl: &glow::Context) -> Option<u32> {
    let e = unsafe { gl.get_error() };
    (e != glow::NO_ERROR).then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shader's matrix gives the same RGB as the CPU conversion (video.rs).
    #[test]
    fn shader_matrix_matches_the_cpu() {
        for color in [Color { bt709: false, full: false }, Color { bt709: true, full: false }, Color { bt709: false, full: true }, Color { bt709: true, full: true }] {
            let (m, off, sc) = conversion(color);
            for (y, u, v) in [(81u8, 90u8, 240u8), (145, 54, 34), (41, 240, 110), (126, 128, 128), (200, 100, 150)] {
                let n = [(y as f32 / 255.0 - off[0]) * sc[0], (u as f32 / 255.0 - off[1]) * sc[1], (v as f32 / 255.0 - off[2]) * sc[2]];
                // column-major mat3 × vec3
                let rgb: Vec<u8> = (0..3).map(|r| ((m[r] * n[0] + m[3 + r] * n[1] + m[6 + r] * n[2]).clamp(0.0, 1.0) * 255.0).round() as u8).collect();
                let pic = Picture { width: 2, height: 2, planes: Planes::Nv12 { y_stride: 2, uv_offset: 4, uv_stride: 2 }, data: vec![y, y, y, y, u, v], color };
                let cpu = pic.rgba();
                for c in 0..3 {
                    assert!((rgb[c] as i32 - cpu[c] as i32).abs() <= 2, "{color:?} yuv ({y},{u},{v}): shader {rgb:?} cpu {:?}", &cpu[..3]);
                }
            }
        }
    }
}
