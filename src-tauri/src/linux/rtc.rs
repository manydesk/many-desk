// Native peer connections for Linux. WebKitGTK ships without WebRTC, so the renderer's
// RTCPeerConnection (src/renderer/rtc-native.js) is backed by webrtc-rs here.
//
// Video is H.264, the codec every browser can both send and receive:
//  - host: the captured screen (capture.rs) is encoded with OpenH264 and sent as samples;
//  - viewer: RTP is reassembled into Annex-B access units and handed to WebCodecs.
// Control/file data channels are proxied as-is. Audio is negotiated but not carried yet.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rtc::data_channel::RTCDataChannelInit;
use rtc::interceptor::{Attribute, Interceptor, Packet as IPacket, Slot, StreamInfo, TaggedPacket};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS};
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind};
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use rtc::sansio::Protocol;
use rtc::statistics::report::RTCStatsReportEntry as Entry;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceCandidateInit,
    RTCIceConnectionState, RTCIceGatheringState, RTCIceServer, RTCIceTransportPolicy, RTCPeerConnectionIceErrorEvent,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription, StatsSelector,
};
use webrtc::rtp_transceiver::RtpSender;

use crate::media::capture;
use crate::media::encode::{EncodedFrame, Sink, Video};

// Binary frames to the renderer: [kind][...]. 1 = video access unit, 2 = data channel bytes.
const MEDIA_VIDEO: u8 = 1;
const MEDIA_DC: u8 = 2;
static STATS_DUMPED: AtomicBool = AtomicBool::new(false);

fn h264_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 102,
        ..Default::default()
    }
}

fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
        ..Default::default()
    }
}

// ---- keyframe requests reach the application only through an interceptor ----

struct KeyframeRequests {
    read: std::collections::VecDeque<TaggedPacket>,
    write: std::collections::VecDeque<TaggedPacket>,
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequests {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = rtc::shared::error::Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let IPacket::Rtcp(packets) = &msg.message.packet {
            let keep: Vec<Box<dyn rtc::rtcp::Packet>> = packets
                .iter()
                .filter(|p| p.as_any().is::<PictureLossIndication>() || p.as_any().is::<FullIntraRequest>())
                .cloned()
                .collect();
            if keep.is_empty() {
                return Ok(());
            }
            msg.message.packet = IPacket::Rtcp(keep);
            msg.message.add(Attribute::DeliverToApplication);
        }
        self.read.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.write.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write.pop_front()
    }
}

impl Interceptor for KeyframeRequests {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}

// ---- host video: the shared encoder (media::encode) feeding a sample track ----

struct TrackSink {
    tx: tokio::sync::mpsc::Sender<(Bytes, Duration)>,
}

impl Sink for TrackSink {
    fn push(&self, f: EncodedFrame) -> bool {
        self.tx.try_send((f.data, f.duration)).is_ok()
    }
    fn closed(&self) -> bool {
        self.tx.is_closed()
    }
}

struct VideoOut {
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    ssrc: u32,
    video: Arc<Video>,
    started: AtomicBool,
}

impl VideoOut {
    fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(Bytes, Duration)>(3);
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let pt = me.sender.get_parameters().await.ok().and_then(|p| p.rtp_parameters.codecs.first().map(|c| c.payload_type)).unwrap_or(102);
            while let Some((data, duration)) = rx.recv().await {
                let sample = Sample { data, duration, ..Sample::new(Instant::now()) };
                let _ = me.track.sample_writer(me.ssrc, pt).write_sample(&sample).await;
            }
        });
        // Keyframe requests (PLI/FIR) from the viewer.
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(ev) = me.track.poll().await {
                if let TrackLocalEvent::OnRtcpPacket(_) = ev {
                    me.video.request_keyframe();
                }
            }
        });
        self.video.start(Arc::new(TrackSink { tx }));
    }
}

// ---- one connection ----

#[derive(Clone)]
struct Shared {
    events: Channel<Value>,
    media: Channel<InvokeResponseBody>,
    channels: Arc<Mutex<HashMap<String, Arc<dyn DataChannel>>>>,
    video: Arc<Mutex<Option<Arc<VideoOut>>>>,
    remote_video: Arc<Mutex<Option<Arc<dyn TrackRemote>>>>,
    decoded: Arc<AtomicU64>,
}

impl Shared {
    fn emit(&self, v: Value) {
        let _ = self.events.send(v);
    }

    fn watch_channel(&self, dc: Arc<dyn DataChannel>, label: String) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let _ = dc.set_buffered_amount_low_threshold(1024 * 1024).await;
            while let Some(ev) = dc.poll().await {
                match ev {
                    DataChannelEvent::OnOpen => me.emit(json!({ "e": "dc_open", "label": label })),
                    DataChannelEvent::OnClose => {
                        me.emit(json!({ "e": "dc_close", "label": label }));
                        break;
                    }
                    DataChannelEvent::OnBufferedAmountLow => me.emit(json!({ "e": "dc_low", "label": label })),
                    DataChannelEvent::OnMessage(m) => {
                        if m.is_string {
                            me.emit(json!({ "e": "dc_msg", "label": label, "text": String::from_utf8_lossy(&m.data) }));
                        } else {
                            let mut out = Vec::with_capacity(m.data.len() + label.len() + 2);
                            out.push(MEDIA_DC);
                            out.push(label.len() as u8);
                            out.extend_from_slice(label.as_bytes());
                            out.extend_from_slice(&m.data);
                            let _ = me.media.send(InvokeResponseBody::Raw(out));
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    // Viewer: RTP → Annex-B access units for the WebCodecs decoder in the page.
    fn receive_video(&self, track: Arc<dyn TrackRemote>) {
        let me = self.clone();
        *self.remote_video.lock().unwrap() = Some(track.clone());
        tauri::async_runtime::spawn(async move {
            let ssrc = track.ssrcs().await.first().copied().unwrap_or(0);
            let pli = |t: Arc<dyn TrackRemote>| async move {
                let _ = t.write_rtcp(vec![Box::new(PictureLossIndication { sender_ssrc: 0, media_ssrc: ssrc })]).await;
            };
            pli(track.clone()).await;
            let mut dep = H264Packet::default();
            let mut buf = BytesMut::new();
            let mut last_seq: Option<u16> = None;
            let mut cur_ts: Option<u32> = None;
            let mut broken = true; // until the first keyframe
            let mut last_pli = Instant::now();
            while let Some(ev) = track.poll().await {
                let TrackRemoteEvent::OnRtpPacket(p) = ev else { continue };
                let seq = p.header.sequence_number;
                if let Some(l) = last_seq {
                    if seq != l.wrapping_add(1) {
                        broken = true;
                        buf.clear();
                    }
                }
                last_seq = Some(seq);
                if cur_ts != Some(p.header.timestamp) {
                    buf.clear();
                    cur_ts = Some(p.header.timestamp);
                }
                if let Ok(b) = dep.depacketize(&p.payload) {
                    buf.extend_from_slice(&b);
                }
                if !p.header.marker || buf.is_empty() {
                    continue;
                }
                let key = has_idr(&buf);
                if broken && !key {
                    buf.clear();
                    if last_pli.elapsed() > Duration::from_millis(500) {
                        last_pli = Instant::now();
                        pli(track.clone()).await;
                    }
                    continue;
                }
                broken = false;
                let mut out = Vec::with_capacity(buf.len() + 6);
                out.push(MEDIA_VIDEO);
                out.push(key as u8);
                out.extend_from_slice(&p.header.timestamp.to_be_bytes());
                out.extend_from_slice(&buf);
                buf.clear();
                me.decoded.fetch_add(1, Ordering::Relaxed);
                let _ = me.media.send(InvokeResponseBody::Raw(out));
            }
        });
    }
}

fn has_idr(annexb: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            let t = annexb[i + 3] & 0x1f;
            if t == 5 || t == 7 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

struct Handler(Shared);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, ev: RTCPeerConnectionIceEvent) {
        let c = &ev.candidate;
        self.0.emit(json!({ "e": "cand", "url": ev.url, "c": {
            "address": c.address, "port": c.port, "protocol": c.protocol.to_string().to_lowercase(),
            "type": c.typ.to_string().to_lowercase(), "relatedAddress": c.related_address, "relatedPort": c.related_port,
            "foundation": c.foundation, "priority": c.priority,
        }}));
    }
    async fn on_ice_candidate_error(&self, ev: RTCPeerConnectionIceErrorEvent) {
        self.0.emit(json!({ "e": "cand_err", "text": format!("{ev:?}") }));
    }
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        self.0.emit(json!({ "e": "gather", "s": s.to_string().to_lowercase() }));
    }
    async fn on_ice_connection_state_change(&self, s: RTCIceConnectionState) {
        self.0.emit(json!({ "e": "ice", "s": s.to_string().to_lowercase() }));
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        if s == RTCPeerConnectionState::Connected {
            if let Some(v) = self.0.video.lock().unwrap().clone() {
                v.start();
            }
        }
        self.0.emit(json!({ "e": "conn", "s": s.to_string().to_lowercase() }));
    }
    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let label = dc.label().await.unwrap_or_default();
        self.0.channels.lock().unwrap().insert(label.clone(), dc.clone());
        self.0.watch_channel(dc, label.clone());
        self.0.emit(json!({ "e": "dc", "label": label }));
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if track.kind().await == RtpCodecKind::Video {
            self.0.receive_video(track);
            self.0.emit(json!({ "e": "track", "kind": "video" }));
        }
    }
}

struct Conn {
    pc: Arc<dyn PeerConnection>,
    shared: Shared,
}

#[derive(Default)]
pub struct NativeRtc {
    conns: Mutex<HashMap<u32, Arc<Conn>>>,
}

impl NativeRtc {
    fn get(&self, id: u32) -> Result<Arc<Conn>, String> {
        self.conns.lock().unwrap().get(&id).cloned().ok_or_else(|| "no such connection".into())
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn ice_servers(cfg: &Value) -> Vec<RTCIceServer> {
    cfg["iceServers"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|s| RTCIceServer {
                    urls: match &s["urls"] {
                        Value::String(u) => vec![u.clone()],
                        Value::Array(a) => a.iter().filter_map(|u| u.as_str().map(String::from)).collect(),
                        _ => vec![],
                    },
                    username: s["username"].as_str().unwrap_or("").into(),
                    credential: s["credential"].as_str().unwrap_or("").into(),
                })
                .filter(|s| !s.urls.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// ---- commands ----

type St<'a> = tauri::State<'a, NativeRtc>;

#[tauri::command]
pub async fn npc_create(st: St<'_>, id: u32, config: Value, events: Channel<Value>, media: Channel<InvokeResponseBody>) -> Result<(), String> {
    let mut engine = MediaEngine::default();
    engine.register_codec(h264_codec(), RtpCodecKind::Video).map_err(err)?;
    engine.register_codec(opus_codec(), RtpCodecKind::Audio).map_err(err)?;
    let registry = register_default_interceptors(rtc::interceptor::Registry::new(), &mut engine).map_err(err)?;
    let registry = registry.with(Slot::from(14_000), KeyframeRequests { read: Default::default(), write: Default::default() });
    let mut builder = RTCConfigurationBuilder::new().with_ice_servers(ice_servers(&config));
    if config["iceTransportPolicy"].as_str() == Some("relay") {
        builder = builder.with_ice_transport_policy(RTCIceTransportPolicy::Relay);
    }
    let shared = Shared {
        events,
        media,
        channels: Default::default(),
        video: Default::default(),
        remote_video: Default::default(),
        decoded: Default::default(),
    };
    let pc = PeerConnectionBuilder::new()
        .with_configuration(builder.build())
        .with_media_engine(engine)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Handler(shared.clone())))
        .with_udp_addrs(vec!["0.0.0.0:0".to_string(), "[::]:0".to_string()])
        .build()
        .await
        .map_err(err)?;
    st.conns.lock().unwrap().insert(id, Arc::new(Conn { pc: Arc::new(pc), shared }));
    Ok(())
}

#[tauri::command]
pub async fn npc_add_video(st: St<'_>, id: u32, screen: Option<u32>) -> Result<Value, String> {
    let c = st.get(id)?;
    let screens = tauri::async_runtime::spawn_blocking(capture::screens).await.unwrap_or_default();
    let screen = screens.iter().find(|s| Some(s.id) == screen).or_else(|| screens.first()).cloned();
    let ssrc = rand::random::<u32>();
    let track = Arc::new(
        TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "manydesk".to_owned(),
                "screen".to_owned(),
                "screen".to_owned(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                    codec: h264_codec().rtp_codec,
                    ..Default::default()
                }],
            ),
        )
        .map_err(err)?,
    );
    let sender = c.pc.add_track(track.clone() as Arc<dyn TrackLocal>).await.map_err(err)?;
    let video = Video::new(screen);
    let (w, h) = video.size();
    let out = Arc::new(VideoOut { track, sender, ssrc, video, started: AtomicBool::new(false) });
    *c.shared.video.lock().unwrap() = Some(out);
    Ok(json!({ "width": w, "height": h }))
}

#[tauri::command]
pub async fn npc_add_audio(st: St<'_>, id: u32) -> Result<(), String> {
    let c = st.get(id)?;
    c.pc
        .add_transceiver_from_kind(RtpCodecKind::Audio, Some(RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Sendrecv, ..Default::default() }))
        .await
        .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub async fn npc_offer(st: St<'_>, id: u32) -> Result<String, String> {
    Ok(st.get(id)?.pc.create_offer(None).await.map_err(err)?.sdp)
}

#[tauri::command]
pub async fn npc_answer(st: St<'_>, id: u32) -> Result<String, String> {
    Ok(st.get(id)?.pc.create_answer(None).await.map_err(err)?.sdp)
}

fn desc(kind: &str, sdp: String) -> Result<RTCSessionDescription, String> {
    if kind == "offer" { RTCSessionDescription::offer(sdp) } else { RTCSessionDescription::answer(sdp) }.map_err(err)
}

#[tauri::command]
pub async fn npc_set_local(st: St<'_>, id: u32, kind: String, sdp: String) -> Result<(), String> {
    st.get(id)?.pc.set_local_description(desc(&kind, sdp)?).await.map_err(err)
}

#[tauri::command]
pub async fn npc_set_remote(st: St<'_>, id: u32, kind: String, sdp: String) -> Result<(), String> {
    st.get(id)?.pc.set_remote_description(desc(&kind, sdp)?).await.map_err(err)
}

#[tauri::command]
pub async fn npc_local(st: St<'_>, id: u32) -> Result<Value, String> {
    Ok(match st.get(id)?.pc.local_description().await {
        Some(d) => json!({ "type": d.sdp_type.to_string().to_lowercase(), "sdp": d.sdp }),
        None => Value::Null,
    })
}

#[tauri::command]
pub async fn npc_add_candidate(st: St<'_>, id: u32, candidate: String, mid: Option<String>) -> Result<(), String> {
    let init = RTCIceCandidateInit { candidate, sdp_mid: mid, sdp_mline_index: Some(0), ..Default::default() };
    st.get(id)?.pc.add_ice_candidate(init).await.map_err(err)
}

#[tauri::command]
pub async fn npc_dc_create(st: St<'_>, id: u32, label: String) -> Result<(), String> {
    let c = st.get(id)?;
    let dc = c.pc.create_data_channel(&label, Some(RTCDataChannelInit { ordered: true, ..Default::default() })).await.map_err(err)?;
    c.shared.channels.lock().unwrap().insert(label.clone(), dc.clone());
    c.shared.watch_channel(dc, label);
    Ok(())
}

fn channel(c: &Conn, label: &str) -> Result<Arc<dyn DataChannel>, String> {
    c.shared.channels.lock().unwrap().get(label).cloned().ok_or_else(|| "no channel".into())
}

#[tauri::command]
pub async fn npc_dc_send(st: St<'_>, id: u32, label: String, text: String) -> Result<usize, String> {
    let c = st.get(id)?;
    let dc = channel(&c, &label)?;
    dc.send_text(&text).await.map_err(err)?;
    Ok(dc.outstanding_bytes().await.unwrap_or(0))
}

/// Binary data channel message: raw request body, `x-id` / `x-label` headers.
#[tauri::command]
pub async fn npc_dc_send_bin(st: St<'_>, request: tauri::ipc::Request<'_>) -> Result<usize, String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else { return Err("expected bytes".into()) };
    let h = request.headers();
    let id: u32 = h.get("x-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or("x-id")?;
    let label = h.get("x-label").and_then(|v| v.to_str().ok()).ok_or("x-label")?.to_string();
    let c = st.get(id)?;
    let dc = channel(&c, &label)?;
    dc.send(BytesMut::from(&body[..])).await.map_err(err)?;
    Ok(dc.outstanding_bytes().await.unwrap_or(0))
}

#[tauri::command]
pub async fn npc_dc_close(st: St<'_>, id: u32, label: String) -> Result<(), String> {
    let c = st.get(id)?;
    if let Ok(dc) = channel(&c, &label) {
        let _ = dc.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn npc_video_params(st: St<'_>, id: u32, scale: f64, fps: f64, bitrate: u32) -> Result<(), String> {
    let c = st.get(id)?;
    if let Some(v) = c.shared.video.lock().unwrap().as_ref() {
        v.video.set_params(scale, fps, bitrate);
    }
    Ok(())
}

/// Viewer: ask the host for a keyframe (the page's decoder lost sync).
#[tauri::command]
pub async fn npc_pli(st: St<'_>, id: u32) -> Result<(), String> {
    let c = st.get(id)?;
    let track = c.shared.remote_video.lock().unwrap().clone();
    if let Some(t) = track {
        let ssrc = t.ssrcs().await.first().copied().unwrap_or(0);
        let _ = t.write_rtcp(vec![Box::new(PictureLossIndication { sender_ssrc: 0, media_ssrc: ssrc })]).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn npc_stats(st: St<'_>, id: u32) -> Result<Vec<Value>, String> {
    let c = st.get(id)?;
    let report = c.pc.get_stats(Instant::now(), StatsSelector::None).await;
    let mut out = vec![];
    for e in report.iter() {
        let v = match e {
            Entry::Transport(s) => serde_json::to_value(s),
            Entry::IceCandidatePair(s) => serde_json::to_value(s),
            Entry::LocalCandidate(s) | Entry::RemoteCandidate(s) => serde_json::to_value(s),
            Entry::Codec(s) => serde_json::to_value(s),
            Entry::InboundRtp(s) => serde_json::to_value(s),
            Entry::OutboundRtp(s) => serde_json::to_value(s),
            Entry::RemoteInboundRtp(s) => serde_json::to_value(s),
            _ => continue,
        };
        let Ok(mut v) = v else { continue };
        if let Some(stats) = v.get("stats").cloned() {
            // Flatten the shared id/type/timestamp like the browser API.
            if let (Some(o), Some(s)) = (v.as_object_mut(), stats.as_object()) {
                o.remove("stats");
                for (k, val) in s {
                    o.insert(k.clone(), val.clone());
                }
            }
        }
        out.push(v);
    }
    if std::env::var_os("MANYDESK_DEBUG").is_some() && !STATS_DUMPED.swap(true, Ordering::Relaxed) {
        eprintln!("[stats] {}", serde_json::to_string(&out).unwrap_or_default());
    }
    if let Some(vo) = c.shared.video.lock().unwrap().as_ref() {
        let mut s = vo.video.stats();
        s["type"] = json!("x-encoder");
        out.push(s);
    }
    Ok(out)
}

#[tauri::command]
pub async fn npc_close(st: St<'_>, id: u32) -> Result<(), String> {
    let c = st.conns.lock().unwrap().remove(&id);
    if let Some(c) = c {
        if let Some(v) = c.shared.video.lock().unwrap().take() {
            v.video.stop();
        }
        let _ = c.pc.close().await;
    }
    Ok(())
}
