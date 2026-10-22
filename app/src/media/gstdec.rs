// Linux viewer video: VA-API hardware decoding through GStreamer
// (appsrc → h264parse → vah264dec / vaapih264dec → RGBA appsink). Without GStreamer's
// VA plugins or a VA driver the caller falls back to OpenH264.
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

pub struct GstDecoder {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    name: &'static str,
    rgba: Vec<u8>,
    ts: u64,
}

impl GstDecoder {
    pub fn new() -> Result<Self, String> {
        gst::init().map_err(|e| e.to_string())?;
        // vah264dec (gst ≥ 1.22, "va" plugin) first, then the older gstreamer-vaapi element.
        let (dec, name) = [("vah264dec", "VA-API"), ("vaapih264dec", "VA-API (vaapi)")]
            .into_iter()
            .find(|(e, _)| gst::ElementFactory::find(e).is_some())
            .ok_or("no VA-API GStreamer decoder (gstreamer1.0-plugins-bad / gstreamer1.0-vaapi)")?;
        let desc = format!(
            "appsrc name=src is-live=true do-timestamp=false format=time \
             caps=video/x-h264,stream-format=byte-stream,alignment=au ! \
             h264parse ! {dec} ! videoconvert ! video/x-raw,format=RGBA ! \
             appsink name=sink sync=false max-buffers=2 drop=true"
        );
        let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>().map_err(|_| "pipeline")?;
        let src = pipeline.by_name("src").and_then(|e| e.downcast::<gst_app::AppSrc>().ok()).ok_or("appsrc")?;
        let sink = pipeline.by_name("sink").and_then(|e| e.downcast::<gst_app::AppSink>().ok()).ok_or("appsink")?;
        pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        // A VA element that exists but has no working driver errors out on the bus at start.
        if let Some(bus) = pipeline.bus() {
            if let Some(msg) = bus.timed_pop_filtered(gst::ClockTime::from_mseconds(300), &[gst::MessageType::Error]) {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(format!("{msg:?}"));
            }
        }
        Ok(GstDecoder { pipeline, src, sink, name, rgba: Vec::new(), ts: 0 })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn reset(&mut self) {
        let _ = self.pipeline.send_event(gst::event::FlushStart::new());
        let _ = self.pipeline.send_event(gst::event::FlushStop::new(true));
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Option<(&[u8], usize, usize)>, String> {
        if let Some(bus) = self.pipeline.bus() {
            if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
                return Err(format!("{msg:?}"));
            }
        }
        let mut buf = gst::Buffer::from_slice(au.to_vec());
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_mseconds(self.ts));
        }
        self.ts += 16;
        self.src.push_buffer(buf).map_err(|e| e.to_string())?;
        // The decoder works asynchronously; a frame usually comes out within a few ms.
        let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::from_mseconds(40)) else { return Ok(None) };
        let caps = sample.caps().ok_or("caps")?;
        let s = caps.structure(0).ok_or("caps")?;
        let (w, h) = (s.get::<i32>("width").map_err(|e| e.to_string())? as usize, s.get::<i32>("height").map_err(|e| e.to_string())? as usize);
        let buffer = sample.buffer().ok_or("buffer")?;
        let map = buffer.map_readable().map_err(|e| e.to_string())?;
        // Rows may be padded; RGBA stride is rounded to 4 bytes, so w*4 is exact.
        let data = map.as_slice();
        let stride = data.len() / h.max(1);
        self.rgba.resize(w * h * 4, 0);
        for y in 0..h {
            self.rgba[y * w * 4..(y + 1) * w * 4].copy_from_slice(&data[y * stride..y * stride + w * 4]);
        }
        Ok(Some((self.rgba.as_slice(), w, h)))
    }
}

impl Drop for GstDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
