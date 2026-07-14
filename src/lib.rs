use log::info;
use wasm_bindgen::{
    prelude::{wasm_bindgen, Closure},
    JsCast,
};
use web_sys::window;
use web_sys::{
    Blob, BlobPropertyBag, CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement, Url,
};
use yew::prelude::*;

mod function_input;
use function_input::FunctionInput;

mod particle;
use particle::Particle;

mod parser;
use parser::{interpret_field_function, pretty_print};

pub enum Msg {
    Init,
    Render,
    RandomFunction,
    UpdateFunc(String),
    ImageUploaded(web_sys::File),
    ImageLoaded(Vec<u8>, u32, u32),
    RemoveImage,
    UpdateMaxParticles(u32),
    ToggleBugFix,       // real
    ToggleFastImage,   // why does the new one look bad...
    ToggleUiVisibility, 
    StartVideoRecording,
    StopVideoRecording,
    RecordingSaved,
}

const MAX_IMAGE_EDGE: u32 = 524;
const TARGET_FPS: f64 = 40.0; // never set this higher than 60
const FPS_UPDATE_PERIOD: f64 = 500.0;
const STARTING_NUM_PARTICLES: usize = 10_000;
const BACKGROUND_COLOUR: &str = "#000";
const FOREGROUND_COLOUR: &str = "#1ce";
const DEFAULT_FUNCTION: &str = "(
100*abs(sin(r/t))*sin(t)
,
100*sin(r/t)*cos(t)
)";
struct Config {
    width: usize,
    height: usize,
    avg_lifetime: i32,
    max_particles: u32,       //max number of particles to render, used to prevent lag
    fg_colour: String,
    bg_colour: String,
    func: Box<dyn Fn((f64, f64, f64)) -> (f64, f64)>,
    target_fps: f64,
}

struct AnimationCanvas {
    image_data: Option<Vec<u8>>,
    image_width: u32,
    image_height: u32,
    bug_fix_enabled: bool, // real
    render_loop_id: Option<i32>, // browser's loop handle

    offscreen_canvas: Option<HtmlCanvasElement>,
    offscreen_context: Option<CanvasRenderingContext2d>,
    particle_buffer: Vec<u8>,
    fast_image_mode: bool,
    ui_visible: bool,
    recording: bool,
    media_recorder: Option<web_sys::MediaRecorder>,
    recorded_chunks: Option<js_sys::Array>,
    ondata_closure: Option<Closure<dyn FnMut(web_sys::BlobEvent)>>,
    onstop_closure: Option<Closure<dyn FnMut(web_sys::Event)>>,
    beforeunload_closure: Option<Closure<dyn FnMut(web_sys::BeforeUnloadEvent)>>,

    canvas: NodeRef,
    context: Option<CanvasRenderingContext2d>,
    particles: Vec<Particle>,
    callback: Closure<dyn FnMut()>,
    config: Config,

    func_string: String,
    func_error_message: String,

    time_delta: f64,
    average_fps: f64,
    frame_counter: usize,
    start_time: f64,
}

impl Component for AnimationCanvas {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let func = interpret_field_function(&DEFAULT_FUNCTION.to_string()).unwrap();

        ctx.link().send_future(async { Msg::Init });
        let comp_ctx = ctx.link().clone();
        let callback =
            Closure::wrap(Box::new(move || comp_ctx.send_message(Msg::Render)) as Box<dyn FnMut()>);
        let config = Config {
            width: window().unwrap().inner_width().unwrap().as_f64().unwrap() as usize + 100,
            height: window().unwrap().inner_height().unwrap().as_f64().unwrap() as usize + 100,
            avg_lifetime: 200,
            max_particles: 30000,
            fg_colour: FOREGROUND_COLOUR.to_string(),
            bg_colour: BACKGROUND_COLOUR.to_string(),
            target_fps: TARGET_FPS,
            func: func,
        };
        Self {
            image_data: None,
            image_width: 0,
            image_height: 0,
            bug_fix_enabled: false, // real
            render_loop_id: None,
            offscreen_canvas: None,
            offscreen_context: None,
            particle_buffer: vec![],
            fast_image_mode: true,
            ui_visible: true,
            recording: false,
            media_recorder: None,
            recorded_chunks: None,
            ondata_closure: None,
            onstop_closure: None,
            beforeunload_closure: None,

            canvas: NodeRef::default(),
            context: None,
            particles: vec![],
            callback: callback,
            config: config,

            func_string: DEFAULT_FUNCTION.to_string(),
            func_error_message: "".to_string(),

            time_delta: js_sys::Date::now(),
            average_fps: TARGET_FPS,
            frame_counter: 0,
            start_time: js_sys::Date::now(),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::Init => {

                if let Some(id) = self.render_loop_id {
                    if let Some(win) = web_sys::window() {
                        win.cancel_animation_frame(id).unwrap();
                    }
                }
    
                self.update_cached_context();

                let ctx_2d = self.context.as_ref().unwrap();

                // clear canvas background
                ctx_2d.set_fill_style_str(&self.config.bg_colour);
                ctx_2d.fill_rect(0.0, 0.0, self.config.width as f64, self.config.height as f64);

                self.particles = Vec::with_capacity(STARTING_NUM_PARTICLES);
                let w = self.config.width as i32;
                let h = self.config.height as i32;
                for _ in 0..STARTING_NUM_PARTICLES {
                    self.particles
                        .push(Particle::new((-w, w), (-h, h), self.config.avg_lifetime));
                }
                self.frame_counter = 0;
                self.start_time = js_sys::Date::now();
                self.time_delta = js_sys::Date::now();

                ctx.link().send_message(Msg::Render);
                true
            }
            Msg::Render => {
                let delta = 1000.0 / self.average_fps;

                self.update_particles(delta);
                self.render();
                self.frame_counter += 1;

                let time = js_sys::Date::now();
                if time - self.time_delta > FPS_UPDATE_PERIOD {
                    self.time_delta = time;
                    self.average_fps = 1000.0 * self.frame_counter as f64 / FPS_UPDATE_PERIOD;
                    self.frame_counter = 0;

                    let fps_ratio = (self.average_fps / self.config.target_fps)
                        .max(0.90)
                        .min(1.10);
                    let target_num_particles = (1000 + (self.particles.len() as f64 * fps_ratio) as usize)
                        .min(self.config.max_particles as usize);
                    info!(
                        "FPS: {}   {}",
                        self.average_fps as i32, target_num_particles
                    );

                    let w = self.config.width as i32;
                    let h = self.config.height as i32;
                    self.particles.resize(
                        target_num_particles,
                        Particle::new((-w, w), (-h, h), self.config.avg_lifetime),
                    );
                    true
                } else {
                    false
                }
            }
            Msg::UpdateFunc(func_string) => {
                self.func_string = func_string.clone();
                match interpret_field_function(&func_string) {
                    Ok(f) => {
                        self.config.func = f;
                        self.func_error_message = "".to_string();
                        info!("{}", pretty_print(self.func_string.to_string()));
                    }
                    Err(e) => {
                        info!("{}", e);
                        self.func_error_message = e;
                    }
                }
                false
            }
            Msg::RandomFunction => {
                let mut counter = 0;
                loop {
                    let func_string = parser::random_field_function(10);
                    counter += 1;
                    if let Ok(f) = interpret_field_function(&func_string) {
                        // sample 100 random coordinates and make sure at least 30 of them are valid
                        let mut valid_count = 0;
                        for _ in 0..100 {
                            // samples between self.config.width and -self.config.width
                            let x = (js_sys::Math::random() * self.config.width as f64 * 2.0)
                                - self.config.width as f64;
                            let y = (js_sys::Math::random() * self.config.height as f64 * 2.0)
                                - self.config.height as f64;
                            let t = js_sys::Math::random() * 60.0;
                            let (dx, dy) = f((x, y, t));
                            // Check for NaN
                            if dx.is_nan() || dy.is_nan() {
                                continue;
                            }
                            // significant velocity in both x and y direction
                            if dx.abs() < 10.0 || dy.abs() < 10.0 {
                                continue;
                            }
                            // total velocity is not too large
                            if (dx * dx + dy * dy) > 1_000_000.0 {
                                continue;
                            }
                            valid_count += 1;
                        }

                        if valid_count > 30 {
                            self.config.func = f;
                            self.func_string = func_string;
                            info!(
                                "found function after {} attempts\n{}",
                                counter,
                                pretty_print(self.func_string.to_string())
                            );
                            self.func_error_message = "".to_string();
                            return false;
                        }
                    }
                }
            }
            Msg::ToggleFastImage => {
                self.fast_image_mode = !self.fast_image_mode;
                true // re render to change
            }
            Msg::ImageUploaded(file) => {
                info!("ImageUploaded: {}", file.name());
                let link = ctx.link().clone();

                let onload = Closure::wrap(Box::new(move |e: web_sys::Event| {
                    let reader: web_sys::FileReader = e.target_unchecked_into();
                    let data_url = reader.result().unwrap().as_string().unwrap();

                    let img = web_sys::HtmlImageElement::new().unwrap();
                    let link2 = link.clone();
                    let data_url2 = data_url.clone();
                    let img_clone = img.clone();

                    let img_onload = Closure::wrap(Box::new(move || {
                        let document = web_sys::window().unwrap().document().unwrap();
                        let canvas = document
                            .create_element("canvas").unwrap()
                            .dyn_into::<HtmlCanvasElement>().unwrap();

                        let w = img_clone.natural_width(); 
                        let h = img_clone.natural_height();
                        let scale = (MAX_IMAGE_EDGE as f64 / w as f64)
                            .min(MAX_IMAGE_EDGE as f64 / h as f64)
                            .min(1.0);

                        let sw = (w as f64 * scale).round() as u32;
                        let sh = (h as f64 * scale).round() as u32;

                        canvas.set_width(sw);
                        canvas.set_height(sh);

                        let ctx2d = canvas
                            .get_context("2d").unwrap().unwrap()
                            .dyn_into::<CanvasRenderingContext2d>().unwrap();

                        ctx2d.draw_image_with_html_image_element_and_dw_and_dh(&img_clone, 
                            0.0, 
                            0.0, 
                            sw as f64, 
                            sh as f64).unwrap();

                        let image_data = ctx2d
                            .get_image_data(0.0, 0.0, sw as f64, sh as f64).unwrap(); 
                        let raw = image_data.data().0;

                        info!("Pixel buffer length: {} (expected {})", raw.len(), sw * sh * 4);
                        if raw.len() >= 4 {
                            info!("First pixel RGBA: {},{},{},{}", raw[0], raw[1], raw[2], raw[3]);
                        }

                        link2.send_message(Msg::ImageLoaded(raw, sw, sh));
                    }) as Box<dyn FnMut()>);

                    img.set_onload(Some(img_onload.as_ref().unchecked_ref()));
                    img_onload.forget();
                    img.set_src(&data_url2);
                }) as Box<dyn FnMut(_)>);

                let reader = web_sys::FileReader::new().unwrap();
                reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                onload.forget();
                reader.read_as_data_url(&file).unwrap();
                false
            }

            Msg::ImageLoaded(raw, w, h) => {
                info!("ImageLoaded: {}x{}, {} bytes", w, h, raw.len());
                self.image_data = Some(raw);
                self.image_width = w;
                self.image_height = h;
                true
            }

            Msg::RemoveImage => {
                self.image_data = None;
                self.image_width = 0;
                self.image_height = 0;
                true
            }

            Msg::UpdateMaxParticles(max_particles) => {
                self.config.max_particles = max_particles;
                info!("Updated max particles to {}", max_particles);
                true
            }

            Msg::ToggleBugFix => {

                // flipping this remounts the canvas!! which would break the recording mid-stream, so don't allow it
                if self.recording {
                    return false;
                }
                self.bug_fix_enabled = !self.bug_fix_enabled;

                let link = ctx.link().clone();

                // this put it to the browser's event loop microtask queue,
                // allowing Yew to complete its DOM patch and swap the canvas element first
                // else issues happen...
                wasm_bindgen_futures::spawn_local(async move {
                    link.send_message(Msg::Init);
                });             
                true 
            }

            Msg::ToggleUiVisibility => {
                self.ui_visible = !self.ui_visible;
                true
            }

            Msg::StartVideoRecording => {
                self.start_video_recording(ctx.link().clone());
                true
            }

            Msg::StopVideoRecording => {
                self.stop_video_recording();
                true
            }

            Msg::RecordingSaved => {
                // closure has done its thing, now kill it
                // stupid closures causing stupid memory leaks...
                self.ondata_closure = None;
                self.onstop_closure = None;
                self.recorded_chunks = None;
                self.media_recorder = None;
                false
            }
        }
    }
    
    
    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_change = ctx.link().callback(Msg::UpdateFunc);

        let func_error_message_html = if self.func_error_message.len() > 0 {
            html! {
                <div class="error-message">{self.func_error_message.clone()}</div>
            }
        } else {
            html! {}
        };
        let current_max = self.config.max_particles;
        html! {
            <div>
                if self.ui_visible {
                    <div style="position: absolute; color: #1ce;">
                        
                        <FunctionInput {on_change} value={self.func_string.clone()} />
                        {func_error_message_html}
                        
                        // 1st div, Stats & Random Function
                        <div>
                            <button class="button" onclick={ctx.link().callback(|_| Msg::RandomFunction)}> {"🎲"} </button>
                            {" FPS: "} {self.average_fps as usize} {"    Particles: "} {self.particles.len()}
                        </div>

                        // 2nd div, Max Particles Input
                        <div style="display: flex; align-items: center; gap: 8px;">
                            <span style="color: #1ce;">{"Max Particles: "}</span>
                            <input 
                            type="number"
                            style="width: 60px; background-color: rgba(0, 0, 0, 0.4); color: #1ce; border: none; padding: 5px 10px;"
                            value={current_max.to_string()}
                            oninput={ctx.link().callback(move |e: web_sys::InputEvent| {
                                let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                                if let Ok(max_particles) = input.value().parse::<u32>() {
                                    Msg::UpdateMaxParticles(max_particles)
                                } else if input.value().is_empty() {
                                    Msg::UpdateMaxParticles(0)
                                } else {
                                    Msg::UpdateMaxParticles(current_max)
                                }
                            })}
                        />
                        </div>

                        // 3rd div, Upload, Remove Image and Record Video buttons
                        <div>
                            <label 
                                style="display: inline-block; background-color: rgba(0, 0, 0, 0.4); color: #1ce; padding: 5px 10px; cursor: pointer; user-select: none;"
                            > // remove default file input styles and make it look like a button
                                {"Upload Image"}
                                <input 
                                    type="file"
                                    accept="image/*"
                                    style="display: none;"
                                    onchange={ctx.link().callback(|e: Event| {
                                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();

                                        // if the user cancels the file picker
                                        if let Some(files) = input.files() {
                                            if let Some(file) = files.get(0) {
                                                return Msg::ImageUploaded(file);
                                            }
                                        }
                                        // fallback / structural requirement for Yew 
                                        Msg::RemoveImage
                                    })}
                                />
                            </label>
                            <button 
                                class="button" 
                                style="background-color: rgba(0, 0, 0, 0.4); color: #1ce; padding: 5px 10px; cursor: pointer; margin-left: 8px;"
                                onclick={ctx.link().callback(|_| Msg::RemoveImage)}
                            >
                                {"Remove image"}
                            </button>
                            if self.recording {
                                <button 
                                    class="button"
                                    style="background-color: rgba(200, 0, 0, 0.6); color: #fff; border: 1px solid #fff; padding: 3px 8px; cursor: pointer; margin-left: 8px;"
                                    onclick={ctx.link().callback(|_| Msg::StopVideoRecording)}
                                >
                                    { "⏺ Stop Recording" }
                                </button>
                            } else {
                                <button 
                                    class="button"
                                    style="background-color: rgba(0, 0, 0, 0.4); color: #1ce; padding: 5px 10px; cursor: pointer; margin-left: 8px;"
                                    onclick={ctx.link().callback(|_| Msg::StartVideoRecording)}
                                >
                                    { "Record Video" }
                                </button>
                            }
                        </div>

                        // 4th div, Toggles
                        <div>
                            <label class="toggle-control" style="margin-right: 15px;" title={if self.recording { "Can't switch modes mid-recording" } else { "" }}>
                                <input 
                                    type="checkbox" 
                                    checked={!self.bug_fix_enabled} 
                                    disabled={self.recording}
                                    onclick={ctx.link().callback(|_| Msg::ToggleBugFix)} 
                                />
                                { " GPU mode" }
                            </label>

                            <label class="toggle-control">
                                <input 
                                    type="checkbox" 
                                    checked={self.fast_image_mode} 
                                    onclick={ctx.link().callback(|_| Msg::ToggleFastImage)} 
                                />
                                { "pixel buffer (image)" }
                            </label>
                        </div>

                        // 5th div, Hide UI button
                        <div>
                            <button 
                                class="button" 
                                style="background-color: rgba(0, 0, 0, 0.4); color: #1ce; border: 1px solid #1ce; padding: 5px 10px; cursor: pointer;"
                                onclick={ctx.link().callback(|_| Msg::ToggleUiVisibility)}
                            >
                                {"Hide UI"}
                            </button>
                        </div>

                    </div>
                }

                // when hidden
                else {
                    <div style="position: absolute;">
                        <button 
                            class="button" 
                            style="background-color: rgba(0, 0, 0, 0.6); color: #1ce; border: 1px solid #1ce; padding: 5px 10px; cursor: pointer;"
                            onclick={ctx.link().callback(|_| Msg::ToggleUiVisibility)}
                        >
                            {"Show UI"}
                        </button>

                        // shortcut for stop recording
                        if self.recording {
                            <button
                                class="button"
                                style="background-color: rgba(200, 0, 0, 0.6); color: #fff; border: 1px solid #fff; padding: 5px 10px; cursor: pointer;"
                                onclick={ctx.link().callback(|_| Msg::StopVideoRecording)}
                            >
                                {"⏺ Stop Recording"}
                            </button>
                        }
                    </div>
                }
            
                <canvas
                    key={self.bug_fix_enabled.to_string()}
                    id="canvas"
                    class="canvas"
                    style={format!("background-color: {};", self.config.bg_colour)} 
                    ref={self.canvas.clone()}>
                </canvas>
            </div>
        }
    }
}

impl AnimationCanvas {
    fn update_cached_context(&mut self) {
        let canvas: HtmlCanvasElement = self.canvas.cast().unwrap();
        
        canvas.set_width(self.config.width as u32);
        canvas.set_height(self.config.height as u32);

        let ctx: CanvasRenderingContext2d = if self.bug_fix_enabled {
            let options = js_sys::Object::new();
            js_sys::Reflect::set(&options, &"willReadFrequently".into(), &true.into()).unwrap();
            canvas.get_context_with_context_options("2d", &options)
                .unwrap()
                .unwrap()
                .unchecked_into()
        } else {
            canvas.get_context("2d")
                .unwrap()
                .unwrap()
                .unchecked_into()
        };

        self.context = Some(ctx);

        // setup the offscreen canvas and byte buffer
        let document = web_sys::window().unwrap().document().unwrap();
        let off_canvas = document.create_element("canvas").unwrap().dyn_into::<HtmlCanvasElement>().unwrap();
        off_canvas.set_width(self.config.width as u32);
        off_canvas.set_height(self.config.height as u32);
        
        // same bug_fix_enabled logic to the offscreen canvas
        let off_ctx: CanvasRenderingContext2d = if self.bug_fix_enabled {
            let options = js_sys::Object::new();
            js_sys::Reflect::set(&options, &"willReadFrequently".into(), &true.into()).unwrap();
            off_canvas.get_context_with_context_options("2d", &options)
                .unwrap().unwrap().unchecked_into()
        } else {
            off_canvas.get_context("2d")
                .unwrap().unwrap().unchecked_into()
        };
        
        self.offscreen_canvas = Some(off_canvas);
        self.offscreen_context = Some(off_ctx);
        self.particle_buffer = vec![0; (self.config.width * self.config.height * 4) as usize];    
    }

    fn update_particles(&mut self, delta: f64) {
        let t = (js_sys::Date::now() - self.start_time) / 1000.0;
        if t > 60.0 {
            self.start_time = js_sys::Date::now();
        }
        for particle in self.particles.iter_mut() {
            if !particle.update(&self.config.func, delta, t) {
                particle.respawn();
                particle.update(&self.config.func, delta, t);
            }
        }
    }

    fn render(&mut self) {
        let ctx = self.context.as_ref().unwrap();
        // put a black square over canvas to fade old particles
        ctx.set_global_alpha(0.01); // lower values make the trails longer
        ctx.set_fill_style_str(&self.config.bg_colour);
        ctx.fill_rect(
            0.0,
            0.0,
            self.config.width as f64,
            self.config.height as f64,
        );

        // render all updated particles
        ctx.set_global_alpha(1.0);

        let has_image = self.image_data.is_some();

        
        // strongest algorithm
        // optimizations!!! so good, better than before!!
        if !has_image {
            ctx.set_fill_style_str(&self.config.fg_colour);
            ctx.begin_path(); // batch path construction
            for particle in self.particles.iter_mut() {
                let x = particle.pos.0 + (self.config.width as f64 / 2.0);
                let y = (self.config.height as f64 / 2.0) - particle.pos.1;
                ctx.rect(x, y, 1.0, 1.0);
            }
            ctx.fill(); // single draw call to GPU
        }

        // weakest algorithm group (2 algorithms)
        // when an image is uploaded
        else if let Some(ref data) = self.image_data {
            let image_width = self.image_width;
            let image_height = self.image_height;
            let canvas_width = self.config.width as f64;
            let canvas_height = self.config.height as f64;
            let cw = self.config.width as usize;
            let ch = self.config.height as usize;

            let scale = (canvas_width / image_width as f64)
                .min(canvas_height / image_height as f64);
            let draw_w = image_width as f64 * scale;
            let draw_h = image_height as f64 * scale;

            let half_draw_w = draw_w * 0.5;
            let half_draw_h = draw_h * 0.5;
            let half_img_w = image_width as f64 * 0.5;
            let half_img_h = image_height as f64 * 0.5;
            let inv_scale = 1.0 / scale;

            let half_canvas_w = canvas_width * 0.5;
            let half_canvas_h = canvas_height * 0.5;

            // strongest algorithm of the weakest algorithm group
            if self.fast_image_mode {
                // reusable buffer to transparent
                self.particle_buffer.fill(0);

                // actual particle loop
                for particle in self.particles.iter_mut() {
                    let px = particle.pos.0;
                    let py = particle.pos.1;

                    let (r, g, b) = if px.abs() >= half_draw_w || py.abs() >= half_draw_h {
                        (0, 0, 0)
                    } else {
                        let ix = (px * inv_scale + half_img_w) as usize;
                        let iy = (half_img_h - py * inv_scale) as usize;
                        let ix = ix.min(image_width as usize - 1);
                        let iy = iy.min(image_height as usize - 1);
                        let idx = (iy * image_width as usize + ix) * 4;
                        (data[idx], data[idx + 1], data[idx + 2])
                    };

                    particle.color = (r, g, b);

                    // map to integer canvas coordinates
                    let x = (px + half_canvas_w).round() as isize;
                    let y = (half_canvas_h - py).round() as isize;

                    // write to byte array if the particle is on screen
                    if x >= 0 && x < cw as isize && y >= 0 && y < ch as isize {
                        let buf_idx = ((y as usize) * cw + (x as usize)) * 4;
                        self.particle_buffer[buf_idx] = r;
                        self.particle_buffer[buf_idx + 1] = g;
                        self.particle_buffer[buf_idx + 2] = b;
                        self.particle_buffer[buf_idx + 3] = 255;
                    }
                }

                // bytes to the offscreen canvas 
                // JS crossing
                let image_data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
                    wasm_bindgen::Clamped(&self.particle_buffer),
                    cw as u32,
                    ch as u32,
                ).unwrap();

                let off_ctx = self.offscreen_context.as_ref().unwrap();
                off_ctx.put_image_data(&image_data, 0.0, 0.0).unwrap();

                // draw offscreen canvas onto the main canvas (1 JS crossing)
                let off_canvas = self.offscreen_canvas.as_ref().unwrap();
                ctx.draw_image_with_html_canvas_element(off_canvas, 0.0, 0.0).unwrap();
            }

            // ==========================================
            // weakest algorithm of the weakest algorithm group
            else {
                for particle in self.particles.iter_mut() {
                    let px = particle.pos.0;
                    let py = particle.pos.1;

                    let (r, g, b) = if px.abs() >= half_draw_w || py.abs() >= half_draw_h {
                        (0, 0, 0)
                    } else {
                        let ix = (px * inv_scale + half_img_w) as usize;
                        let iy = (half_img_h - py * inv_scale) as usize;
                        let ix = ix.min(image_width as usize - 1);
                        let iy = iy.min(image_height as usize - 1);
                        let idx = (iy * image_width as usize + ix) * 4;
                        (data[idx], data[idx + 1], data[idx + 2])
                    };

                    particle.color = (r, g, b);

                    let x = px + half_canvas_w;
                    let y = half_canvas_h - py;

                    let color_str = format!("rgb({},{},{})", r, g, b);
                    ctx.set_fill_style_str(&color_str);
                    ctx.fill_rect(x, y, 1.0, 1.0);
                }
            }
        }           
        let id = window()
            .unwrap()
            .request_animation_frame(self.callback.as_ref().unchecked_ref())
            .unwrap();

        self.render_loop_id = Some(id);
    }

    fn start_video_recording(&mut self, link: yew::html::Scope<Self>) {
        let canvas: HtmlCanvasElement = self.canvas.cast().unwrap();
        
        // 60.0 not 60...
        let stream = canvas.capture_stream_with_frame_request_rate(60.0).unwrap();

        // ini the native MediaRecorder
        let recorder = web_sys::MediaRecorder::new_with_media_stream(&stream).unwrap();
        
        // JS array to hold the video chunks
        let chunks = js_sys::Array::new();
        self.recorded_chunks = Some(chunks.clone());

        // handle video data
        let chunks_for_stop = chunks.clone();
        let ondata = Closure::wrap(Box::new(move |e: web_sys::BlobEvent| {
            if let Some(data) = e.data() {
                if data.size() > 0.0 {
                    chunks.push(&data);
                }
            }
        }) as Box<dyn FnMut(web_sys::BlobEvent)>);
        recorder.set_ondataavailable(Some(ondata.as_ref().unchecked_ref()));
        self.ondata_closure = Some(ondata); // keep closure alive

        let link_for_stop = link.clone();
        let onstop = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let options = BlobPropertyBag::new();
            options.set_type("video/webm");
            
            let blob = Blob::new_with_u8_array_sequence_and_options(&chunks_for_stop, &options).unwrap();
            let url = Url::create_object_url_with_blob(&blob).unwrap();

            // download webm
            let document = web_sys::window().unwrap().document().unwrap();
            let anchor: HtmlAnchorElement = document
                .create_element("a").unwrap()
                .dyn_into::<HtmlAnchorElement>().unwrap();
            anchor.set_href(&url);
            anchor.set_download("vectorflow.webm");
            anchor.click();

            Url::revoke_object_url(&url).unwrap();

            // KILL CLOSURE
            link_for_stop.send_message(Msg::RecordingSaved);
        }) as Box<dyn FnMut(web_sys::Event)>);  
        
        recorder.set_onstop(Some(onstop.as_ref().unchecked_ref()));
        self.onstop_closure = Some(onstop); // keep closure alive

        // Reload site?
        // Changes that u made may not be saved
        let beforeunload = Closure::wrap(Box::new(move |e: web_sys::BeforeUnloadEvent| {
            e.prevent_default();
            e.set_return_value("A recording is in progress...");
        }) as Box<dyn FnMut(web_sys::BeforeUnloadEvent)>);
        window()
            .unwrap()
            .add_event_listener_with_callback(
                "beforeunload",
                beforeunload.as_ref().unchecked_ref(),
            )
            .unwrap();
        self.beforeunload_closure = Some(beforeunload);

        // actual start record
        recorder.start().unwrap();
        self.media_recorder = Some(recorder);
        self.recording = true;
    }

    fn stop_video_recording(&mut self) {
        if let Some(recorder) = self.media_recorder.take() {
            recorder.stop().unwrap();
        }
        self.recording = false;

        // remove the reload warning
        if let Some(beforeunload) = self.beforeunload_closure.take() {
            let _ = window().unwrap().remove_event_listener_with_callback(
                "beforeunload",
                beforeunload.as_ref().unchecked_ref(),
            );
        }

        // ondata_closure / onstop_closure / recorded_chunks are intentionally NOT cleared here 
        // recorder.stop() is async, so onstop hasn't fired yet
        // killed in Msg::RecordingSaved, once onstop has actually run
    }
}

#[function_component(App)]
fn app_body() -> Html {
    html! {
        <>
            <AnimationCanvas/>
        </>
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}
