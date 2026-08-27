// BlurScreen — 화면공유용 블러 창
//
// 현재 화면을 강하게 블러 처리한 모습을 창 하나로 보여줍니다.
// 디스코드 등에서 "이 창"만 공유하면 실제 화면(원고)은 절대 나가지 않습니다.
//
// 구조:
//   캡처 스레드: OS 공식 캡처 API(macOS: ScreenCaptureKit / Windows: Graphics.Capture, scap 크레이트)
//     → 640px 급으로 축소 수신 → 추가 축소(비가역, 텍스트 정보 소멸) → 소형 가우시안 블러
//     → 공유 슬롯(길이 1, 최신 프레임만 유지)
//   UI 스레드(winit + softbuffer): 슬롯의 작은 이미지를 창 크기로 확대(bilinear) 표시
//
// 단축키:  1/2/3 블러 약/중/강 · F fps 전환(5/10/15) · M 모니터 전환 · Space 일시정지(화면 동결)

// Windows에서 실행 시 검은 콘솔 창이 같이 뜨지 않게 함
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use scap::capturer::{Capturer, Options, Resolution};
use scap::frame::{Frame, FrameType};
use scap::Target;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const WINDOW_TITLE: &str = "Blur Screen";

/// (블러 후 이미지의 목표 가로 픽셀, 가우시안 시그마, 라벨)
/// 폭이 작을수록 정보가 더 많이 소멸함 (원본 4K 기준 192px = 1/20 수준)
const STRENGTHS: [(usize, f32, &str); 3] = [(480, 1.3, "약"), (320, 1.7, "중"), (192, 2.2, "강")];
const FPS_OPTIONS: [u32; 3] = [5, 10, 15];

// ---------------------------------------------------------------------------
// 공유 상태
// ---------------------------------------------------------------------------

/// 블러 처리가 끝난 작은 RGB 이미지
struct Tiny {
    w: usize,
    h: usize,
    rgb: Vec<u8>, // w*h*3
}

struct Shared {
    /// 길이 1의 프레임 슬롯 — 항상 최신 프레임만 유지(큐 누적 없음)
    frame: Mutex<Option<Tiny>>,
    /// 오류/안내 메시지(빈 문자열이면 정상)
    status: Mutex<String>,
    strength: AtomicUsize,
    fps_idx: AtomicUsize,
    display_idx: AtomicUsize,
    display_count: AtomicUsize,
    paused: AtomicBool,
    /// fps/모니터 변경 시 캡처 스트림 재구성 요청
    rebuild: AtomicBool,
    quit: AtomicBool,
    /// 현재 유효한 캡처 세대 번호 — 구세대 펌프 스레드가 스스로 종료하는 기준
    generation: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Shared {
            frame: Mutex::new(None),
            status: Mutex::new(String::new()),
            strength: AtomicUsize::new(1), // 기본: 중
            fps_idx: AtomicUsize::new(1),  // 기본: 10fps
            display_idx: AtomicUsize::new(0),
            display_count: AtomicUsize::new(1),
            paused: AtomicBool::new(false),
            rebuild: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// 설정 저장/복원 (~/.blurscreen.conf, key=value)
// ---------------------------------------------------------------------------

fn config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(std::path::PathBuf::from(home).join(".blurscreen.conf"))
}

fn load_config(shared: &Shared) {
    let Some(path) = config_path() else { return };
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let Ok(n) = v.trim().parse::<usize>() else { continue };
        match k.trim() {
            "strength" => shared.strength.store(n.min(STRENGTHS.len() - 1), Ordering::Relaxed),
            "fps" => shared.fps_idx.store(n.min(FPS_OPTIONS.len() - 1), Ordering::Relaxed),
            "display" => shared.display_idx.store(n, Ordering::Relaxed),
            _ => {}
        }
    }
}

fn save_config(shared: &Shared) {
    let Some(path) = config_path() else { return };
    let text = format!(
        "strength={}\nfps={}\ndisplay={}\n",
        shared.strength.load(Ordering::Relaxed),
        shared.fps_idx.load(Ordering::Relaxed),
        shared.display_idx.load(Ordering::Relaxed),
    );
    let _ = std::fs::write(path, text);
}

// ---------------------------------------------------------------------------
// 이미지 처리 (캡처 스레드에서 실행)
// ---------------------------------------------------------------------------

/// 프레임에서 (가로, 세로, 데이터, 픽셀 크기, [R,G,B] 채널 위치)를 꺼낸다.
/// 플랫폼별로 도착하는 픽셀 포맷이 달라도 전부 처리할 수 있게 해 둠.
fn frame_parts(frame: &Frame) -> Option<(usize, usize, &[u8], usize, [usize; 3])> {
    let (w, h, data, px, rgb) = match frame {
        Frame::BGRA(f) => (f.width, f.height, &f.data, 4, [2, 1, 0]),
        Frame::BGRx(f) => (f.width, f.height, &f.data, 4, [2, 1, 0]),
        Frame::RGBx(f) => (f.width, f.height, &f.data, 4, [0, 1, 2]),
        Frame::XBGR(f) => (f.width, f.height, &f.data, 4, [3, 2, 1]),
        Frame::BGR0(f) => (f.width, f.height, &f.data, 3, [2, 1, 0]),
        Frame::RGB(f) => (f.width, f.height, &f.data, 3, [0, 1, 2]),
        Frame::YUVFrame(_) => return None,
    };
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((w as usize, h as usize, data.as_slice(), px, rgb))
}

/// 프레임 처리 결과. 내용이 직전과 같으면 tiny가 None (게시·그리기 생략).
struct Processed {
    hash: u64,
    tiny: Option<Tiny>,
}

/// 축소(박스 평균, 비가역) 후 블러를 적용한 작은 이미지를 만든다.
/// 축소 결과의 해시가 직전 프레임과 같으면 블러·게시를 통째로 건너뛴다.
fn process_frame(frame: &Frame, target_w: usize, sigma: f32, prev_hash: u64) -> Option<Processed> {
    let (w, h, data, px, [ri, gi, bi]) = frame_parts(frame)?;
    if data.len() < w * h * px {
        return None;
    }
    let tw = target_w.clamp(2, w.max(2));
    let th = ((h * tw + w / 2) / w).clamp(2, h.max(2));

    // 입력이 아주 클 때(Windows에서 원본 해상도가 그대로 올 때 등)는
    // 픽셀을 건너뛰며 샘플링해 연산량을 일정하게 유지한다.
    // bin 하나의 폭이 step의 3배 이상이 되도록 잡아 빈 bin이 생기지 않게 함.
    let step = (w / (tw * 3)).max(1);

    let mut acc = vec![0u32; tw * th * 3];
    let mut cnt = vec![0u32; tw * th];
    let mut y = 0;
    while y < h {
        let ty = (y * th / h).min(th - 1);
        let row = &data[y * w * px..(y + 1) * w * px];
        let mut x = 0;
        while x < w {
            let tx = (x * tw / w).min(tw - 1);
            let p = &row[x * px..x * px + px];
            let a = (ty * tw + tx) * 3;
            acc[a] += p[ri] as u32;
            acc[a + 1] += p[gi] as u32;
            acc[a + 2] += p[bi] as u32;
            cnt[ty * tw + tx] += 1;
            x += step;
        }
        y += step;
    }

    let mut rgb = vec![0u8; tw * th * 3];
    // FNV-1a 해시를 함께 계산해 "내용이 안 바뀐 프레임"을 감지한다
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..tw * th {
        let c = cnt[i].max(1);
        let r = (acc[i * 3] / c) as u8;
        let g = (acc[i * 3 + 1] / c) as u8;
        let b = (acc[i * 3 + 2] / c) as u8;
        rgb[i * 3] = r;
        rgb[i * 3 + 1] = g;
        rgb[i * 3 + 2] = b;
        let px = ((r as u64) << 16) | ((g as u64) << 8) | (b as u64);
        hash = (hash ^ px).wrapping_mul(0x0000_0100_0000_01B3);
    }

    if hash == prev_hash {
        // 블러 결과도 동일할 것이므로 여기서 끝 — 게시·그리기 생략
        return Some(Processed { hash, tiny: None });
    }

    box_blur(&mut rgb, tw, th, sigma);
    Some(Processed {
        hash,
        tiny: Some(Tiny { w: tw, h: th, rgb }),
    })
}

/// 러닝섬 방식 박스 블러 2회 반복(가우시안 근사). 커널 크기와 무관하게 픽셀당 비용이
/// 일정해서 기존 가우시안 컨볼루션보다 수 배 저렴하다.
fn box_blur(rgb: &mut [u8], w: usize, h: usize, sigma: f32) {
    let r = (sigma.round() as usize).max(1);
    if w < 2 || h < 2 {
        return;
    }
    let mut tmp = vec![0u8; rgb.len()];
    for _ in 0..2 {
        box_pass_h(rgb, &mut tmp, w, h, r);
        box_pass_v(&tmp, rgb, w, h, r);
    }
}

fn box_pass_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let norm = (2 * r + 1) as u32;
    let recip = (65536 + norm / 2) / norm; // 나눗셈 대신 고정소수 곱
    for y in 0..h {
        let row = &src[y * w * 3..(y + 1) * w * 3];
        let orow = &mut dst[y * w * 3..(y + 1) * w * 3];
        let idx = |x: isize| -> usize { x.clamp(0, w as isize - 1) as usize * 3 };
        let mut s = [0u32; 3];
        for o in -(r as isize)..=(r as isize) {
            let p = idx(o);
            s[0] += row[p] as u32;
            s[1] += row[p + 1] as u32;
            s[2] += row[p + 2] as u32;
        }
        for x in 0..w {
            orow[x * 3] = ((s[0] * recip) >> 16) as u8;
            orow[x * 3 + 1] = ((s[1] * recip) >> 16) as u8;
            orow[x * 3 + 2] = ((s[2] * recip) >> 16) as u8;
            let ap = idx(x as isize + 1 + r as isize);
            let rp = idx(x as isize - r as isize);
            s[0] = s[0] + row[ap] as u32 - row[rp] as u32;
            s[1] = s[1] + row[ap + 1] as u32 - row[rp + 1] as u32;
            s[2] = s[2] + row[ap + 2] as u32 - row[rp + 2] as u32;
        }
    }
}

fn box_pass_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let norm = (2 * r + 1) as u32;
    let recip = (65536 + norm / 2) / norm;
    let idx = |y: isize| -> usize { y.clamp(0, h as isize - 1) as usize };
    let stride = w * 3;
    // 열 방향 러닝섬을 행 단위로 유지해 캐시 친화적으로 처리
    let mut s = vec![0u32; stride];
    for o in -(r as isize)..=(r as isize) {
        let row = &src[idx(o) * stride..(idx(o) + 1) * stride];
        for (si, &v) in s.iter_mut().zip(row.iter()) {
            *si += v as u32;
        }
    }
    for y in 0..h {
        let orow = &mut dst[y * stride..(y + 1) * stride];
        for (o, &si) in orow.iter_mut().zip(s.iter()) {
            *o = ((si * recip) >> 16) as u8;
        }
        let add = &src[idx(y as isize + 1 + r as isize) * stride..][..stride];
        let rem = &src[idx(y as isize - r as isize) * stride..][..stride];
        for i in 0..stride {
            s[i] = s[i] + add[i] as u32 - rem[i] as u32;
        }
    }
}

// ---------------------------------------------------------------------------
// 캡처 스레드
// ---------------------------------------------------------------------------

fn capture_thread(shared: Arc<Shared>, proxy: EventLoopProxy<()>) {
    let set_status = |msg: &str| {
        *shared.status.lock().unwrap() = msg.to_string();
        let _ = proxy.send_event(());
    };

    if !scap::is_supported() {
        set_status("이 환경에서는 화면 캡처가 지원되지 않습니다 (macOS 12.3+ / Windows 10 1903+ 필요)");
        return;
    }

    if !scap::has_permission() {
        scap::request_permission();
        set_status("화면 기록 권한이 필요합니다 — 시스템 설정 › 개인정보 보호 › 화면 기록에서 허용한 뒤 앱을 다시 실행해 주세요");
        // 권한이 즉시 반영되는 환경을 위해 잠시 재확인
        for _ in 0..150 {
            if shared.quit.load(Ordering::Relaxed) {
                return;
            }
            if scap::has_permission() {
                break;
            }
            thread::sleep(Duration::from_secs(2));
        }
        if !scap::has_permission() {
            return;
        }
    }

    // 스트림이 어떤 방식으로 죽어도(오류 통보 없이 조용히 멈추는 경우 포함)
    // 이 시간 안에는 반드시 새 스트림으로 교체된다.
    const STALL_LIMIT: Duration = Duration::from_secs(20);

    // 펌프 스레드 → 관제 루프로 프레임을 전달하는 채널 (세대번호로 구분)
    let (ftx, frx) = mpsc::channel::<(u64, Frame)>();

    loop {
        if shared.quit.load(Ordering::Relaxed) {
            return;
        }

        // 대상 디스플레이와 제외 창 목록 구성
        let targets = catch_unwind(scap::get_all_targets).unwrap_or_default();
        let displays: Vec<Target> = targets
            .iter()
            .filter(|t| matches!(t, Target::Display(_)))
            .cloned()
            .collect();
        if displays.is_empty() {
            set_status("캡처할 디스플레이를 찾지 못했습니다 — 재시도 중");
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        shared.display_count.store(displays.len(), Ordering::Relaxed);
        let di = shared.display_idx.load(Ordering::Relaxed) % displays.len();

        // 자기 자신(블러 창)은 캡처에서 제외 → 거울 속 거울 방지 (macOS에서 동작)
        let excluded: Vec<Target> = targets
            .iter()
            .filter(|t| matches!(t, Target::Window(w) if w.title.starts_with(WINDOW_TITLE)))
            .cloned()
            .collect();

        let fps = FPS_OPTIONS[shared.fps_idx.load(Ordering::Relaxed) % FPS_OPTIONS.len()];
        let options = Options {
            fps,
            show_cursor: true,
            show_highlight: false,
            target: Some(displays[di].clone()),
            crop_area: None,
            output_type: FrameType::BGRAFrame,
            // 캡처 단계에서 이미 640px 급으로 GPU 축소 → 이후 연산이 전부 가벼워짐
            output_resolution: Resolution::_480p,
            excluded_targets: if excluded.is_empty() { None } else { Some(excluded) },
        };

        // ---- 펌프 스레드 시작 ----
        // get_next_frame()은 스트림이 조용히 죽으면 영원히 잠들 수 있다.
        // 그래서 프레임 대기는 '희생 가능한' 펌프 스레드에 맡기고,
        // 이 관제 루프는 타임아웃 있는 수신으로 워치독 역할을 겸한다.
        let gen = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        {
            let ftx = ftx.clone();
            let shared = shared.clone();
            thread::spawn(move || pump_frames(gen, options, ftx, ready_tx, shared));
        }
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => set_status(""),
            Ok(Err(msg)) => {
                set_status(&format!("{msg} — 재시도 중"));
                thread::sleep(Duration::from_secs(2));
                continue;
            }
            Err(_) => {
                set_status("캡처 시작이 지연되고 있습니다 — 재시도 중");
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        }

        // 캡처 엔진이 fps 제한을 지원하지 않는 플랫폼(Windows) 대비, 앱 레벨 스로틀 간격.
        let min_gap = Duration::from_millis((800 / fps.max(1)) as u64);
        let mut last_hash: u64 = 0;
        let mut last_proc: Option<Instant> = None;
        let mut last_frame = Instant::now();

        // ---- 관제 루프: 프레임 처리 + 워치독 ----
        loop {
            if shared.quit.load(Ordering::Relaxed) {
                return;
            }
            if shared.rebuild.swap(false, Ordering::Relaxed) {
                break; // 설정 변경 → 스트림 재구성
            }
            match frx.recv_timeout(Duration::from_secs(1)) {
                Ok((g, frame)) => {
                    if g != gen {
                        continue; // 이전 세대의 잔여 프레임은 무시
                    }
                    last_frame = Instant::now();
                    if shared.paused.load(Ordering::Relaxed) {
                        continue; // 화면 동결
                    }
                    if let Some(t) = last_proc {
                        if t.elapsed() < min_gap {
                            continue; // 설정 fps 초과분은 버림
                        }
                    }
                    let (tw, sigma, _) =
                        STRENGTHS[shared.strength.load(Ordering::Relaxed) % STRENGTHS.len()];
                    let processed = catch_unwind(AssertUnwindSafe(|| {
                        process_frame(&frame, tw, sigma, last_hash)
                    }))
                    .unwrap_or(None);
                    if let Some(out) = processed {
                        last_hash = out.hash;
                        if let Some(tiny) = out.tiny {
                            *shared.frame.lock().unwrap() = Some(tiny);
                            let _ = proxy.send_event(());
                        }
                    }
                    last_proc = Some(Instant::now());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // 화면이 조용하면 프레임이 안 오는 게 정상이지만, 스트림이 조용히
                    // 죽은 것과 겉으로는 구분할 수 없다. 그래서 일정 시간 프레임이
                    // 없으면 스트림을 새로 만든다 — 어느 쪽이었든 화면 멈춤이
                    // 이 시간 안에 스스로 풀린다.
                    if last_frame.elapsed() > STALL_LIMIT {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // 루프 이탈 → 다음 반복에서 세대가 올라가고 새 스트림이 만들어진다.
        // 이전 펌프 스레드는 자기 세대가 지난 것을 확인하면 스스로 정리하고 종료한다.
    }
}

/// 펌프 스레드: 캡처 스트림을 만들고 프레임을 관제 루프로 퍼 올린다.
/// get_next_frame()이 영원히 잠들 수 있으므로 이 스레드는 희생 가능하게 설계했다 —
/// 자신의 세대가 지나면 다음 프레임 수신 시 스트림을 정리하고 스스로 종료한다.
fn pump_frames(
    gen: u64,
    options: Options,
    ftx: mpsc::Sender<(u64, Frame)>,
    ready_tx: mpsc::Sender<Result<(), String>>,
    shared: Arc<Shared>,
) {
    let mut capturer = match catch_unwind(AssertUnwindSafe(|| Capturer::build(options))) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let _ = ready_tx.send(Err(format!("캡처 시작 실패({e})")));
            return;
        }
        Err(_) => {
            let _ = ready_tx.send(Err("캡처 초기화 오류".to_string()));
            return;
        }
    };
    if catch_unwind(AssertUnwindSafe(|| capturer.start_capture())).is_err() {
        let _ = ready_tx.send(Err("캡처 스트림을 시작하지 못했습니다".to_string()));
        return;
    }
    let _ = ready_tx.send(Ok(()));

    let _ = catch_unwind(AssertUnwindSafe(|| loop {
        match capturer.get_next_frame() {
            Ok(frame) => {
                let obsolete = shared.generation.load(Ordering::Relaxed) != gen
                    || shared.quit.load(Ordering::Relaxed);
                if obsolete || ftx.send((gen, frame)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }));
    let _ = catch_unwind(AssertUnwindSafe(|| capturer.stop_capture()));
}

// ---------------------------------------------------------------------------
// 표시 (UI 스레드)
// ---------------------------------------------------------------------------

/// 작은 블러 이미지를 창 크기로 확대해 그린다. 여백은 어두운 회색.
///
/// 비용이 창 크기에 비례해 커지지 않도록, 비싼 보간(bilinear)은 폭 1024px까지만
/// 수행하고 그 이상은 값 복제(nearest)로 채운다. 블러 화면 특성상 시각적 차이가 없고,
/// 레티나 풀스크린 창에서도 보간 연산량이 일정하게 유지된다.
fn draw_tiny(tiny: &Tiny, out: &mut [u32], bw: usize, bh: usize) {
    const BG: u32 = 0x0014_1418;
    if tiny.w < 1 || tiny.h < 1 {
        out.fill(BG);
        return;
    }

    // 화면 비율 유지(레터박스)
    let scale = f64::min(bw as f64 / tiny.w as f64, bh as f64 / tiny.h as f64);
    let dw = ((tiny.w as f64 * scale).round() as usize).clamp(1, bw);
    let dh = ((tiny.h as f64 * scale).round() as usize).clamp(1, bh);
    let ox = (bw - dw) / 2;
    let oy = (bh - dh) / 2;

    // 여백(레터박스)만 배경색으로 칠한다 — 전체 채우기 낭비 제거
    for y in 0..oy {
        out[y * bw..(y + 1) * bw].fill(BG);
    }
    for y in oy + dh..bh {
        out[y * bw..(y + 1) * bw].fill(BG);
    }
    if ox > 0 || ox + dw < bw {
        for y in oy..oy + dh {
            out[y * bw..y * bw + ox].fill(BG);
            out[y * bw + ox + dw..(y + 1) * bw].fill(BG);
        }
    }

    // 중간 해상도 결정: 보간은 여기까지만
    const MID_CAP: usize = 1024;
    let (mw, mh) = if dw > MID_CAP {
        (MID_CAP, ((dh * MID_CAP + dw / 2) / dw).max(1))
    } else {
        (dw, dh)
    };

    let mid = bilinear_u32(tiny, mw, mh);

    if mw == dw && mh == dh {
        // 창이 작으면 보간 결과를 그대로 복사
        for y in 0..dh {
            let o = (oy + y) * bw + ox;
            out[o..o + dw].copy_from_slice(&mid[y * mw..(y + 1) * mw]);
        }
    } else {
        // 값 복제 확대: 행 하나를 만들어 두고 같은 소스 행이 반복되는 동안 memcpy만 수행
        let xmap: Vec<usize> = (0..dw).map(|x| x * mw / dw).collect();
        let mut row = vec![0u32; dw];
        let mut last_sy = usize::MAX;
        for y in 0..dh {
            let sy = (y * mh / dh).min(mh - 1);
            if sy != last_sy {
                let srow = &mid[sy * mw..(sy + 1) * mw];
                for (dst, &sx) in row.iter_mut().zip(xmap.iter()) {
                    *dst = srow[sx];
                }
                last_sy = sy;
            }
            let o = (oy + y) * bw + ox;
            out[o..o + dw].copy_from_slice(&row);
        }
    }
}

/// tiny를 dw×dh로 분리형 bilinear 확대해 packed 0RGB(u32) 버퍼로 반환.
fn bilinear_u32(tiny: &Tiny, dw: usize, dh: usize) -> Vec<u32> {
    struct Map {
        i0: usize,
        i1: usize,
        f: u32, // 0..256 고정소수
    }
    let make_map = |dst: usize, src: usize| -> Vec<Map> {
        (0..dst)
            .map(|d| {
                let pos = ((d as f64 + 0.5) / dst as f64 * src as f64 - 0.5).max(0.0);
                let i0 = (pos.floor() as usize).min(src - 1);
                let i1 = (i0 + 1).min(src - 1);
                let f = ((pos - i0 as f64) * 256.0).round().clamp(0.0, 256.0) as u32;
                Map { i0, i1, f }
            })
            .collect()
    };
    let xmap = make_map(dw, tiny.w);
    let ymap = make_map(dh, tiny.h);

    // 가로 패스: tiny(w×h) → tmp(dw×h)
    let mut tmp = vec![0u8; dw * tiny.h * 3];
    for y in 0..tiny.h {
        let row = &tiny.rgb[y * tiny.w * 3..(y + 1) * tiny.w * 3];
        let orow = &mut tmp[y * dw * 3..(y + 1) * dw * 3];
        for (x, m) in xmap.iter().enumerate() {
            let p0 = &row[m.i0 * 3..m.i0 * 3 + 3];
            let p1 = &row[m.i1 * 3..m.i1 * 3 + 3];
            let g = 256 - m.f;
            orow[x * 3] = ((p0[0] as u32 * g + p1[0] as u32 * m.f) >> 8) as u8;
            orow[x * 3 + 1] = ((p0[1] as u32 * g + p1[1] as u32 * m.f) >> 8) as u8;
            orow[x * 3 + 2] = ((p0[2] as u32 * g + p1[2] as u32 * m.f) >> 8) as u8;
        }
    }
    // 세로 패스: tmp → mid
    let mut mid = vec![0u32; dw * dh];
    for (y, m) in ymap.iter().enumerate() {
        let r0 = &tmp[m.i0 * dw * 3..(m.i0 + 1) * dw * 3];
        let r1 = &tmp[m.i1 * dw * 3..(m.i1 + 1) * dw * 3];
        let g = 256 - m.f;
        let orow = &mut mid[y * dw..(y + 1) * dw];
        for x in 0..dw {
            let rr = (r0[x * 3] as u32 * g + r1[x * 3] as u32 * m.f) >> 8;
            let gg = (r0[x * 3 + 1] as u32 * g + r1[x * 3 + 1] as u32 * m.f) >> 8;
            let bb = (r0[x * 3 + 2] as u32 * g + r1[x * 3 + 2] as u32 * m.f) >> 8;
            orow[x] = (rr << 16) | (gg << 8) | bb;
        }
    }
    mid
}

// ---------------------------------------------------------------------------
// winit 앱
// ---------------------------------------------------------------------------

struct App {
    shared: Arc<Shared>,
    proxy: EventLoopProxy<()>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    capture_started: bool,
    last_title: String,
}

impl App {
    fn update_title(&mut self) {
        let Some(window) = &self.window else { return };
        let s = &self.shared;
        let status = s.status.lock().unwrap().clone();
        let title = if status.is_empty() {
            let (_, _, name) = STRENGTHS[s.strength.load(Ordering::Relaxed) % STRENGTHS.len()];
            let fps = FPS_OPTIONS[s.fps_idx.load(Ordering::Relaxed) % FPS_OPTIONS.len()];
            let paused = if s.paused.load(Ordering::Relaxed) {
                " · 일시정지"
            } else {
                ""
            };
            let disp = {
                let n = s.display_count.load(Ordering::Relaxed);
                if n > 1 {
                    format!(" · 모니터 {}/{}", s.display_idx.load(Ordering::Relaxed) % n + 1, n)
                } else {
                    String::new()
                }
            };
            format!("{WINDOW_TITLE} — 블러 {name} · {fps}fps{disp}{paused}")
        } else {
            format!("{WINDOW_TITLE} — {status}")
        };
        if title != self.last_title {
            window.set_title(&title);
            self.last_title = title;
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (Some(bw), Some(bh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if surface.resize(bw, bh).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        {
            let guard = self.shared.frame.lock().unwrap();
            match guard.as_ref() {
                Some(tiny) => draw_tiny(tiny, &mut buffer, bw.get() as usize, bh.get() as usize),
                None => buffer.fill(0x0014_1418),
            }
        }
        let _ = buffer.present();
    }

    fn on_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed || event.repeat {
            return;
        }
        let s = &self.shared;
        let mut needs_rebuild = false;
        match &event.logical_key {
            Key::Character(c) => match c.as_str() {
                "1" => s.strength.store(0, Ordering::Relaxed),
                "2" => s.strength.store(1, Ordering::Relaxed),
                "3" => s.strength.store(2, Ordering::Relaxed),
                // 한글 입력 상태에서도 동작하도록 자판 위치의 한글 자모도 함께 매핑
                "f" | "F" | "ㄹ" => {
                    let i = (s.fps_idx.load(Ordering::Relaxed) + 1) % FPS_OPTIONS.len();
                    s.fps_idx.store(i, Ordering::Relaxed);
                    needs_rebuild = true;
                }
                "m" | "M" | "ㅡ" => {
                    let n = s.display_count.load(Ordering::Relaxed).max(1);
                    let i = (s.display_idx.load(Ordering::Relaxed) + 1) % n;
                    s.display_idx.store(i, Ordering::Relaxed);
                    needs_rebuild = true;
                }
                _ => return,
            },
            Key::Named(NamedKey::Space) => {
                let p = s.paused.load(Ordering::Relaxed);
                s.paused.store(!p, Ordering::Relaxed);
            }
            _ => return,
        }
        if needs_rebuild {
            s.rebuild.store(true, Ordering::Relaxed);
        }
        save_config(s);
        self.update_title();
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title(WINDOW_TITLE)
                .with_inner_size(LogicalSize::new(960.0, 560.0));
            let window = Rc::new(
                event_loop
                    .create_window(attrs)
                    .expect("창을 만들지 못했습니다"),
            );
            let context =
                softbuffer::Context::new(window.clone()).expect("그래픽 컨텍스트 생성 실패");
            let surface =
                softbuffer::Surface::new(&context, window.clone()).expect("표시 서피스 생성 실패");
            self.window = Some(window);
            self.surface = Some(surface);
            self.update_title();
        }
        // 창이 생긴 뒤에 캡처를 시작해야 자기 창 제외 목록에 이 창이 들어감
        if !self.capture_started {
            self.capture_started = true;
            let shared = self.shared.clone();
            let proxy = self.proxy.clone();
            thread::spawn(move || capture_thread(shared, proxy));
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.update_title();
        // request_redraw()에 맡기지 않고 즉시 그린다 — 창이 다른 창에 완전히
        // 가려져 있을 때 macOS가 재그리기 요청을 미루거나 생략할 수 있는데,
        // 그러면 디스코드가 캡처해 가는 이 창의 내용이 멈춘 것처럼 보인다.
        self.draw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.shared.quit.store(true, Ordering::Relaxed);
                save_config(&self.shared);
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event),
            _ => {}
        }
    }
}

/// macOS App Nap 방지.
/// 블러 창은 보통 다른 창 뒤에 가려진 채 오래 동작하는데, macOS는 그런 앱을
/// "한가하다"고 판단해 App Nap(절전 격하) 대상으로 삼을 수 있다. 절전 격하가
/// 일어나면 창 갱신·캡처 전달이 지연되어 디스코드의 창 캡처 세션이 장시간 공유
/// 중 끊어지는 원인이 될 수 있으므로, 시스템에 "사용자 주도 작업 중"임을 선언해
/// 앱이 실행되는 동안 App Nap을 차단한다.
#[cfg(target_os = "macos")]
fn prevent_app_nap() {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    let info = NSProcessInfo::processInfo();
    let reason = NSString::from_str("Live screen blur for screen sharing");
    let activity = unsafe {
        info.beginActivityWithOptions_reason(NSActivityOptions::NSActivityUserInitiated, &reason)
    };
    // 활동 토큰을 앱 종료 시까지 유지 (해제하면 App Nap 차단이 풀림)
    std::mem::forget(activity);
}

#[cfg(not(target_os = "macos"))]
fn prevent_app_nap() {}

fn main() {
    prevent_app_nap();
    let shared = Arc::new(Shared::new());
    load_config(&shared);

    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("이벤트 루프 생성 실패");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    let mut app = App {
        shared,
        proxy,
        window: None,
        surface: None,
        capture_started: false,
        last_title: String::new(),
    };
    let _ = event_loop.run_app(&mut app);
}
