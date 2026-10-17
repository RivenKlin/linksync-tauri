// 屏幕共享骨架：xcap 捕获 -> 缩放 -> openh264 编码 -> webrtc-rs 视频轨道发送。
// 信令在 s4 步骤接入 8766 服务；本模块先打通本机管线并暴露 start/stop。
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::Bytes;
use futures::{select, FutureExt};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use serde::{Deserialize, Serialize};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription, Registry, register_default_interceptors,
};
use webrtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit, RtpSender};
use webrtc::runtime::{channel, default_runtime, Receiver, Runtime, Sender};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ShareConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for ShareConfig {
    fn default() -> Self {
        ShareConfig { width: 1280, height: 720, fps: 15 }
    }
}

impl ShareConfig {
    fn clamp(&mut self) {
        self.width = self.width.clamp(320, 3840);
        self.height = self.height.clamp(240, 2160);
        self.fps = self.fps.clamp(1, 60);
        // 分辨率必须为偶数，I420 编码要求
        self.width &= !1;
        self.height &= !1;
    }
}

enum SessionCmd {
    Answer(String),
    Candidate(String),
    Stop,
}

struct Session {
    cmd_tx: Sender<SessionCmd>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn sessions() -> &'static Mutex<Option<Session>> {
    static S: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

// 收集到的 ICE candidate（s4 信令阶段通过 8766 发给对端）
fn candidates_arc() -> Arc<Mutex<Vec<String>>> {
    static C: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    Arc::clone(C.get_or_init(Default::default))
}

/// 取走并清空已收集的 ICE candidate，供信令发送
pub fn take_candidates() -> Vec<String> {
    let c = candidates_arc();
    let mut g = c.lock().unwrap();
    std::mem::take(&mut *g)
}

/// 提交对端 SDP answer
pub fn submit_answer(sdp: String) -> Result<(), String> {
    let g = sessions().lock().unwrap();
    let s = g.as_ref().ok_or("共享未运行")?;
    s.cmd_tx.try_send(SessionCmd::Answer(sdp)).map_err(|_| "会话通道已满".to_string())
}

/// 提交对端 ICE candidate
pub fn submit_candidate(candidate: String) -> Result<(), String> {
    let g = sessions().lock().unwrap();
    let s = g.as_ref().ok_or("共享未运行")?;
    s.cmd_tx.try_send(SessionCmd::Candidate(candidate)).map_err(|_| "会话通道已满".to_string())
}

/// 共享会话是否在运行（信令轮询线程据此判断退出）
pub fn is_running() -> bool {
    let g = sessions().lock().unwrap();
    g.as_ref().map(|s| s.running.load(Ordering::SeqCst)).unwrap_or(false)
}

// RGBA -> I420（BT.601 限定范围），输出写入 buf（w*h*3/2）
fn rgba_to_i420(src: &[u8], w: usize, h: usize, buf: &mut Vec<u8>) {
    buf.resize(w * h * 3 / 2, 0);
    let (y_plane, rest) = buf.split_at_mut(w * h);
    let (u_plane, v_plane) = rest.split_at_mut(w * h / 4);
    for yy in 0..h {
        for xx in 0..w {
            let i = (yy * w + xx) * 4;
            let (r, g, b) = (src[i] as i32, src[i + 1] as i32, src[i + 2] as i32);
            y_plane[yy * w + xx] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16)
                .clamp(16, 235) as u8;
            if yy % 2 == 0 && xx % 2 == 0 {
                let cu = yy / 2 * (w / 2) + xx / 2;
                u_plane[cu] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
                v_plane[cu] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
            }
        }
    }
}

/// 启动共享：返回本端 SDP offer（JSON），供信令发给对端
pub fn start(cfg: ShareConfig) -> Result<serde_json::Value, String> {
    let mut cfg = cfg;
    cfg.clamp();
    stop();
    let running = Arc::new(AtomicBool::new(true));
    let (cmd_tx, cmd_rx) = channel::<SessionCmd>(8);
    let (offer_tx, offer_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let r2 = Arc::clone(&running);
    let r3 = Arc::clone(&running);
    let handle = std::thread::spawn(move || {
        let rt = match default_runtime() {
            Some(rt) => rt,
            None => {
                let _ = offer_tx.send(Err("WebRTC 运行时创建失败".into()));
                return;
            }
        };
        let mut out: Option<Result<(), String>> = None;
        {
            let slot = &mut out;
            let rt2 = Arc::clone(&rt);
            rt.block_on(Box::pin(async move {
                *slot = Some(run_session(cfg, cmd_rx, r2, rt2, offer_tx).await);
            }));
        }
        // 会话线程退出即置 false，is_running() 据此结束信令轮询
        r3.store(false, Ordering::SeqCst);
        if let Err(e) = out.unwrap_or_else(|| Err("会话未运行".into())) {
            eprintln!("screen share session ended: {}", e);
        }
    });
    *sessions().lock().unwrap() = Some(Session {
        cmd_tx,
        running: Arc::clone(&running),
        thread: Some(handle),
    });
    match offer_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(sdp)) => Ok(serde_json::json!({ "sdp": sdp })),
        Ok(Err(e)) => {
            stop();
            Err(e)
        }
        Err(_) => {
            stop();
            Err("等待 offer 超时".into())
        }
    }
}

/// 停止共享
pub fn stop() {
    let session = sessions().lock().unwrap().take();
    if let Some(mut s) = session {
        s.running.store(false, Ordering::SeqCst);
        let _ = s.cmd_tx.try_send(SessionCmd::Stop);
        if let Some(h) = s.thread.take() {
            let _ = h.join();
        }
    }
}

#[derive(Clone)]
struct ShareHandler {
    done_tx: Sender<()>,
    connected: Arc<AtomicBool>,
    candidates: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ShareHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            self.candidates.lock().unwrap().push(init.candidate);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => {
                self.connected.store(true, Ordering::SeqCst);
            }
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                let _ = self.done_tx.try_send(());
            }
            _ => {}
        }
    }
}

async fn run_session(
    cfg: ShareConfig,
    mut cmd_rx: Receiver<SessionCmd>,
    running: Arc<AtomicBool>,
    rt: Arc<dyn Runtime>,
    offer_tx: std::sync::mpsc::Sender<Result<String, String>>,
) -> Result<(), String> {
    // 1. 媒体引擎：注册 H264（与 openh264 输出的 42e01f profile 对齐）
    let mut me = MediaEngine::default();
    let video_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: "video/H264".to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 102,
        ..Default::default()
    };
    me.register_codec(video_codec.clone(), RtpCodecKind::Video)
        .map_err(|e| e.to_string())?;
    let registry = register_default_interceptors(Registry::new(), &mut me)
        .map_err(|e| e.to_string())?;

    let config = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: vec!["stun:39.172.137.182:41789".to_string()],
            ..Default::default()
        }])
        .build();

    let (done_tx, mut done_rx) = channel::<()>(1);
    let connected = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(ShareHandler {
        done_tx,
        connected: Arc::clone(&connected),
        candidates: candidates_arc(),
    });

    // 2. PeerConnection
    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(me)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(Arc::clone(&rt))
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await
        .map_err(|e| {
            let e = e.to_string();
            let _ = offer_tx.send(Err(e.clone()));
            e
        })?;

    // 3. 视频轨道
    let ssrc = {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        (d.as_millis() as u32) ^ (d.subsec_nanos() as u32)
    };
    let track = Arc::new(
        TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "linksync-screen-stream".to_string(),
                "linksync-screen-track".to_string(),
                "linksync-screen".to_string(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: video_codec.rtp_codec.clone(),
                    ..Default::default()
                }],
            ),
        )
        .map_err(|e| {
            let e = e.to_string();
            let _ = offer_tx.send(Err(e.clone()));
            e
        })?,
    );
    let sender = pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await
        .map_err(|e| e.to_string())?;

    // 4. 生成 offer 并回传给调用方
    let offer = pc.create_offer(None).await.map_err(|e| e.to_string())?;
    let sdp = offer.sdp.clone();
    pc.set_local_description(offer).await.map_err(|e| e.to_string())?;
    let _ = offer_tx.send(Ok(sdp));

    // 5. 捕获编码发送任务（connected 后开始出帧）
    {
        let cfg2 = cfg;
        let track2 = Arc::clone(&track);
        let sender2 = Arc::clone(&sender);
        let connected2 = Arc::clone(&connected);
        let running2 = Arc::clone(&running);
        let rt2 = Arc::clone(&rt);
        rt.spawn(Box::pin(async move {
            if let Err(e) = stream_loop(cfg2, track2, sender2, connected2, running2, rt2).await {
                eprintln!("screen share stream loop ended: {}", e);
            }
        }));
    }

    // 6. 信令事件循环：answer / candidate / stop
    loop {
        select! {
            cmd = cmd_rx.recv().fuse() => {
                match cmd {
                    Some(SessionCmd::Answer(sdp)) => {
                        let desc = RTCSessionDescription::answer(sdp).map_err(|e| e.to_string())?;
                        pc.set_remote_description(desc).await.map_err(|e| e.to_string())?;
                    }
                    Some(SessionCmd::Candidate(c)) => {
                        let init = RTCIceCandidateInit { candidate: c, ..Default::default() };
                        let _ = pc.add_ice_candidate(init).await;
                    }
                    Some(SessionCmd::Stop) | None => break,
                }
            }
            _ = done_rx.recv().fuse() => break,
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
    }
    let _ = pc.close().await;
    Ok(())
}

async fn stream_loop(
    cfg: ShareConfig,
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    connected: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    rt: Arc<dyn Runtime>,
) -> Result<(), String> {
    // 等待连接建立（或停止）
    while running.load(Ordering::SeqCst) && !connected.load(Ordering::SeqCst) {
        rt.sleep(Duration::from_millis(100)).await;
    }
    if !running.load(Ordering::SeqCst) {
        return Ok(());
    }

    let payload_type = sender
        .get_parameters()
        .await
        .map_err(|e| e.to_string())?
        .rtp_parameters
        .codecs
        .first()
        .map(|c| c.payload_type)
        .ok_or("无协商视频编码")?;
    let ssrc = *track.ssrcs().await.first().ok_or("轨道无 SSRC")?;

    let mut enc = openh264::encoder::Encoder::new().map_err(|e| e.to_string())?;
    enc.force_intra_frame();
    let w = cfg.width as usize;
    let h = cfg.height as usize;
    let y_size = w * h;
    let c_size = (w / 2) * (h / 2);
    let frame_dur = Duration::from_secs_f64(1.0 / cfg.fps as f64);
    let mut yuv: Vec<u8> = Vec::new();
    let mut next_tick = Instant::now();

    while running.load(Ordering::SeqCst) && connected.load(Ordering::SeqCst) {
        // 捕获+编码放在独立作用域：Monitor 不是 Send，必须在 await 前释放
        let bytes = {
            let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
            let monitor = monitors
                .iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .or_else(|| monitors.first())
                .ok_or("无可用显示器")?;
            let img = monitor.capture_image().map_err(|e| e.to_string())?;
            let scaled = if img.width() == cfg.width && img.height() == cfg.height {
                img
            } else {
                image::imageops::resize(
                    &img,
                    cfg.width,
                    cfg.height,
                    image::imageops::FilterType::Triangle,
                )
            };
            rgba_to_i420(scaled.as_raw(), w, h, &mut yuv);
            let frame = openh264::formats::YUVSlices::new(
                (&yuv[..y_size], &yuv[y_size..y_size + c_size], &yuv[y_size + c_size..]),
                (w, h),
                (w, w / 2, w / 2),
            );
            enc.encode(&frame).map_err(|e| e.to_string())?.to_vec()
        };
        if !bytes.is_empty() {
            track
                .sample_writer(ssrc, payload_type)
                .write_sample(&Sample {
                    data: Bytes::from(bytes),
                    duration: frame_dur,
                    ..Sample::new(Instant::now())
                })
                .await
                .map_err(|e| e.to_string())?;
        }
        // 帧率节流
        next_tick += frame_dur;
        let now = Instant::now();
        if next_tick > now {
            rt.sleep(next_tick - now).await;
        } else {
            next_tick = now;
        }
    }
    Ok(())
}

// ==================== 观看端 ====================
// 收到对端 offer 后创建 RecvOnly 会话，on_track 拿到远端视频轨道，
// 逐包 depacketize（单NAL/STAP-A/FU-A）重组 Annex-B 帧入队列，
// marker 位标记帧尾；前端轮询 viewer_take_frames 取帧后用 WebCodecs 渲染。

enum ViewerCmd {
    Candidate(String),
    Stop,
}

struct ViewerSession {
    cmd_tx: Sender<ViewerCmd>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn viewer_sessions() -> &'static Mutex<Option<ViewerSession>> {
    static S: OnceLock<Mutex<Option<ViewerSession>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn viewer_candidates_arc() -> Arc<Mutex<Vec<String>>> {
    static C: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    Arc::clone(C.get_or_init(Default::default))
}

/// 取走并清空观看端已收集的 ICE candidate
pub fn viewer_take_candidates() -> Vec<String> {
    let c = viewer_candidates_arc();
    let mut g = c.lock().unwrap();
    std::mem::take(&mut *g)
}

/// 提交对端（共享方）ICE candidate
pub fn viewer_submit_candidate(candidate: String) -> Result<(), String> {
    let g = viewer_sessions().lock().unwrap();
    let s = g.as_ref().ok_or("观看会话未运行")?;
    s.cmd_tx.try_send(ViewerCmd::Candidate(candidate)).map_err(|_| "观看会话通道已满".to_string())
}

fn frames_queue() -> &'static Mutex<VecDeque<(Vec<u8>, bool)>> {
    static F: OnceLock<Mutex<VecDeque<(Vec<u8>, bool)>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// 取走已重组的视频帧（Annex-B 字节 + 是否关键帧），上限 30 帧
pub fn viewer_take_frames() -> Vec<(Vec<u8>, bool)> {
    let q = frames_queue();
    let mut g = q.lock().unwrap();
    std::mem::take(&mut *g).into_iter().collect()
}

/// 观看会话是否在运行
pub fn viewer_is_running() -> bool {
    let g = viewer_sessions().lock().unwrap();
    g.as_ref().map(|s| s.running.load(Ordering::SeqCst)).unwrap_or(false)
}

/// 停止观看会话
pub fn viewer_stop() {
    let session = viewer_sessions().lock().unwrap().take();
    if let Some(mut s) = session {
        s.running.store(false, Ordering::SeqCst);
        let _ = s.cmd_tx.try_send(ViewerCmd::Stop);
        if let Some(h) = s.thread.take() {
            let _ = h.join();
        }
    }
}

fn h264_codec_params() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: "video/H264".to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 102,
        ..Default::default()
    }
}

/// 将单个 RTP payload 解包追加到当前帧（Annex-B）。
/// fu/fu_open 保存 FU-A 跨包重组状态。
fn depacketize(payload: &[u8], cur: &mut Vec<u8>, fu: &mut Vec<u8>, fu_open: &mut bool) {
    if payload.is_empty() {
        return;
    }
    let b0 = payload[0];
    let t = b0 & 0x1F;
    match t {
        24 => {
            // STAP-A：1 字节头 + 若干 [2 字节长度 + NAL]
            let mut i = 1usize;
            while i + 2 <= payload.len() {
                let n = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                i += 2;
                let end = (i + n).min(payload.len());
                cur.extend_from_slice(&[0, 0, 0, 1]);
                cur.extend_from_slice(&payload[i..end]);
                i = end;
            }
        }
        28 => {
            // FU-A：byte0=FU indicator, byte1=FU header(S/E/type)
            if payload.len() < 2 {
                return;
            }
            let fh = payload[1];
            let start = (fh & 0x80) != 0;
            let end = (fh & 0x40) != 0;
            if start {
                fu.clear();
                fu.push((b0 & 0xE0) | (fh & 0x1F));
                fu.extend_from_slice(&payload[2..]);
                *fu_open = true;
            } else if *fu_open {
                fu.extend_from_slice(&payload[2..]);
            }
            if end && *fu_open {
                cur.extend_from_slice(&[0, 0, 0, 1]);
                cur.extend_from_slice(fu);
                fu.clear();
                *fu_open = false;
            }
        }
        1..=23 => {
            cur.extend_from_slice(&[0, 0, 0, 1]);
            cur.extend_from_slice(payload);
        }
        _ => {}
    }
}

/// 帧内是否含 IDR(5) 或 SPS(7)，据此标记关键帧
fn au_is_key(annexb: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 4 < annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 0 && annexb[i + 3] == 1 {
            let t = annexb[i + 4] & 0x1F;
            if t == 5 || t == 7 {
                return true;
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    false
}

#[derive(Clone)]
struct ViewerHandler {
    done_tx: Sender<()>,
    track_tx: Sender<Arc<dyn TrackRemote>>,
    candidates: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ViewerHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            self.candidates.lock().unwrap().push(init.candidate);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => {}
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                let _ = self.done_tx.try_send(());
            }
            _ => {}
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.track_tx.try_send(track);
    }
}

/// 观看端启动：传入对端 offer SDP，返回本端 answer（JSON）
pub fn viewer_start(offer_sdp: String) -> Result<serde_json::Value, String> {
    viewer_stop();
    let running = Arc::new(AtomicBool::new(true));
    let (cmd_tx, cmd_rx) = channel::<ViewerCmd>(8);
    let (answer_tx, answer_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let r2 = Arc::clone(&running);
    let r3 = Arc::clone(&running);
    let handle = std::thread::spawn(move || {
        let rt = match default_runtime() {
            Some(rt) => rt,
            None => {
                let _ = answer_tx.send(Err("WebRTC 运行时创建失败".into()));
                return;
            }
        };
        let mut out: Option<Result<(), String>> = None;
        {
            let slot = &mut out;
            let rt2 = Arc::clone(&rt);
            rt.block_on(Box::pin(async move {
                *slot = Some(run_viewer(offer_sdp, cmd_rx, r2, rt2, answer_tx).await);
            }));
        }
        r3.store(false, Ordering::SeqCst);
        if let Err(e) = out.unwrap_or_else(|| Err("观看会话未运行".into())) {
            eprintln!("screen viewer session ended: {}", e);
        }
    });
    *viewer_sessions().lock().unwrap() = Some(ViewerSession {
        cmd_tx,
        running: Arc::clone(&running),
        thread: Some(handle),
    });
    frames_queue().lock().unwrap().clear();
    match answer_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(sdp)) => Ok(serde_json::json!({ "sdp": sdp })),
        Ok(Err(e)) => {
            viewer_stop();
            Err(e)
        }
        Err(_) => {
            viewer_stop();
            Err("等待 answer 超时".into())
        }
    }
}

async fn run_viewer(
    offer_sdp: String,
    mut cmd_rx: Receiver<ViewerCmd>,
    running: Arc<AtomicBool>,
    rt: Arc<dyn Runtime>,
    answer_tx: std::sync::mpsc::Sender<Result<String, String>>,
) -> Result<(), String> {
    let fail = |e: String| {
        let _ = answer_tx.send(Err(e.clone()));
        e
    };

    let mut me = MediaEngine::default();
    me.register_codec(h264_codec_params(), RtpCodecKind::Video)
        .map_err(|e| fail(e.to_string()))?;
    let registry = register_default_interceptors(Registry::new(), &mut me)
        .map_err(|e| fail(e.to_string()))?;

    let config = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: vec!["stun:39.172.137.182:41789".to_string()],
            ..Default::default()
        }])
        .build();

    let (done_tx, mut done_rx) = channel::<()>(1);
    let (track_tx, mut track_rx) = channel::<Arc<dyn TrackRemote>>(1);
    let handler = Arc::new(ViewerHandler {
        done_tx,
        track_tx,
        candidates: viewer_candidates_arc(),
    });

    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(me)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(Arc::clone(&rt))
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await
        .map_err(|e| fail(e.to_string()))?;

    let _ = pc
        .add_transceiver_from_kind(
            RtpCodecKind::Video,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| fail(e.to_string()))?;

    let offer = RTCSessionDescription::offer(offer_sdp).map_err(|e| fail(e.to_string()))?;
    pc.set_remote_description(offer).await.map_err(|e| fail(e.to_string()))?;
    let answer = pc.create_answer(None).await.map_err(|e| fail(e.to_string()))?;
    let sdp = answer.sdp.clone();
    pc.set_local_description(answer).await.map_err(|e| fail(e.to_string()))?;
    let _ = answer_tx.send(Ok(sdp));

    // 帧重组任务：收到 track 后持续 poll，marker 处切帧入队
    {
        let running2 = Arc::clone(&running);
        rt.spawn(Box::pin(async move {
            let track = match track_rx.recv().await {
                Some(t) => t,
                None => return,
            };
            let mut cur: Vec<u8> = Vec::new();
            let mut fu: Vec<u8> = Vec::new();
            let mut fu_open = false;
            loop {
                if !running2.load(Ordering::SeqCst) {
                    break;
                }
                let ev = match track.poll().await {
                    Some(e) => e,
                    None => break,
                };
                match ev {
                    TrackRemoteEvent::OnRtpPacket(pkt) => {
                        depacketize(&pkt.payload, &mut cur, &mut fu, &mut fu_open);
                        if pkt.header.marker && !cur.is_empty() {
                            let key = au_is_key(&cur);
                            let frame = std::mem::take(&mut cur);
                            let mut q = frames_queue().lock().unwrap();
                            if q.len() >= 30 {
                                q.pop_front();
                            }
                            q.push_back((frame, key));
                        }
                    }
                    TrackRemoteEvent::OnEnded => break,
                    _ => {}
                }
            }
        }));
    }

    // 信令事件循环：candidate / stop
    loop {
        select! {
            cmd = cmd_rx.recv().fuse() => {
                match cmd {
                    Some(ViewerCmd::Candidate(c)) => {
                        let init = RTCIceCandidateInit { candidate: c, ..Default::default() };
                        let _ = pc.add_ice_candidate(init).await;
                    }
                    Some(ViewerCmd::Stop) | None => break,
                }
            }
            _ = done_rx.recv().fuse() => break,
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
    }
    let _ = pc.close().await;
    Ok(())
}

/// 供 main.rs 使用的帧 base64 编码
pub fn frames_base64(frames: Vec<(Vec<u8>, bool)>) -> Vec<(String, bool)> {
    frames.into_iter().map(|(f, k)| (B64.encode(&f), k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::rtp::codec::h264::H264Payloader;
    use rtc::rtp::packetizer::Payloader;

    fn annexb(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    /// 按 4 字节起始码拆分 Annex-B，返回各 NAL（含头部字节）
    fn split_annexb(data: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 4 <= data.len() {
            if data[i..i + 4] == [0, 0, 0, 1] {
                let s = i + 4;
                let mut e = data.len();
                let mut j = s;
                while j + 4 <= data.len() {
                    if data[j..j + 4] == [0, 0, 0, 1] {
                        e = j;
                        break;
                    }
                    j += 1;
                }
                out.push(data[s..e].to_vec());
                i = e;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn roundtrip_payloader_depacketize() {
        // SPS/PPS 小、IDR 大：触发 STAP-A 聚合 + FU-A 分片
        let sps = vec![0x67, 0x42, 0xE0, 0x1F, 0x11, 0x22];
        let pps = vec![0x68, 0xCE, 0x3C, 0x80];
        let mut idr = vec![0x65];
        for i in 0..3000u32 {
            idr.push((i % 251) as u8 + 1); // 避开 0 字节，防止起始码仿真问题
        }
        let frame = annexb(&[sps.clone(), pps.clone(), idr.clone()]);

        let mut pl = H264Payloader::default();
        let payloads = pl.payload(1200, &Bytes::from(frame)).unwrap();
        assert!(payloads.len() > 2, "应拆分为多包");

        let mut cur = Vec::new();
        let mut fu = Vec::new();
        let mut fu_open = false;
        for p in &payloads {
            depacketize(p, &mut cur, &mut fu, &mut fu_open);
        }
        assert!(!fu_open, "FU-A 应完整重组");
        let nals = split_annexb(&cur);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], sps);
        assert_eq!(nals[1], pps);
        assert_eq!(nals[2], idr);
        assert!(au_is_key(&cur));
    }

    #[test]
    fn single_nal_frame_not_key() {
        let p = vec![0x41, 0x9A, 0x00, 0x11]; // 非 IDR 条带
        let mut cur = Vec::new();
        let mut fu = Vec::new();
        let mut open = false;
        depacketize(&p, &mut cur, &mut fu, &mut open);
        assert_eq!(cur, annexb(&[p]));
        assert!(!au_is_key(&cur));
    }
}
