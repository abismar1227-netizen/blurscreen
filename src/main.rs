// BlurScreen — 화면공유용 블러 창 (v0.6)
//
// 현재 화면을 강하게 블러 처리한 모습을 창 하나로 보여줍니다.
// 디스코드 등에서 "이 창"만 공유하면 실제 화면(원고)은 절대 나가지 않습니다.
//
// 구조:
//   캡처 스레드: OS 공식 캡처 API(macOS: ScreenCaptureKit / Windows: Graphics.Capture, scap 크레이트)
//     → 640px 급으로 축소 수신 → 추가 축소(비가역, 텍스트 정보 소멸) → 박스 블러
//     → 공유 슬롯(길이 1, 최신 프레임만 유지)
//   UI 스레드(winit + softbuffer): 슬롯의 작은 이미지를 창 크기로 확대 표시.
//     화면 변화가 없어도 2초마다 다시 그려(생존 신호) 공유 쪽에 새 프레임이 계속 가게 한다.
//   메뉴 막대/알림 영역 메뉴 + 공유 창 우클릭 메뉴 + 전역 즉시 가림 단축키.
//   동작 기록: 스트림 재구성·오류·잠자기 복귀 등을 로그 파일로 남김(화면 내용은 기록 안 함).
//
// 단축키(창 선택 시):  1/2/3 블러 약/중/강 · F fps 전환 · M 모니터 전환 · Space 동결
// 전역 단축키:         macOS ⌃⌥⌘B / Windows Ctrl+Alt+Shift+B — 즉시 가림 켜기/끄기

// Windows에서 실행 시 검은 콘솔 창이 같이 뜨지 않게 함
#![cfg_attr(windows, windows_subsystem = "windows")]

mod action;
mod blur;
mod cover;
mod icons;
mod log;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menu;
mod text_assets;

use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use scap::capturer::{Capturer, Options, Resolution};
use scap::frame::{Frame, FrameType};
use scap::Target;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use action::Action;
use blur::{RawFrame, Tiny, STRENGTHS};
use cover::CoverKind;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const WINDOW_TITLE: &str = "Blur Screen";
pub const FPS_OPTIONS: [u32; 3] = [5, 10, 15];

/// 화면 변화가 없어도 이 간격마다 마지막 화면을 다시 그린다 (공유 쪽 생존 신호)
const KEEPALIVE: Duration = Duration::from_secs(2);
/// 모니터 구성 확인·통계 기록 주기
const HOUSEKEEP: Duration = Duration::from_secs(3);
/// 통계 한 줄을 남기는 주기
const STATS_EVERY: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// 공유 상태
// ---------------------------------------------------------------------------

struct Shared {
    /// 길이 1의 프레임 슬롯 — 항상 최신 프레임만 유지(큐 누적 없음)
    frame: Mutex<Option<Tiny>>,
    /// 오류/안내 메시지(빈 문자열이면 정상)
    status: Mutex<String>,
    strength: AtomicUsize,
    fps_idx: AtomicUsize,
    display_idx: AtomicUsize,
    display_count: AtomicUsize,
    display_names: Mutex<Vec<String>>,
    paused: AtomicBool,
    /// 즉시 가림 — 켜져 있는 동안 캡처 스트림을 내리고 보호 화면만 보여준다
    covered: AtomicBool,
    /// macOS 화면 기록 권한이 없어 기다리는 중
    need_permission: AtomicBool,
    /// 공유 창에 상태 배지 표시 (기본 꺼짐)
    badge: AtomicBool,
    /// fps/모니터 변경 시 캡처 스트림 재구성 요청
    rebuild: AtomicBool,
    quit: AtomicBool,
    /// 현재 유효한 캡처 세대 번호 — 구세대 펌프 스레드가 스스로 종료하는 기준
    generation: AtomicU64,
    /// 이 앱 공유 창의 시스템 창 번호 (macOS 캡처 제외용, 0 = 모름)
    own_window_id: AtomicU32,
    // 통계 (동작 기록용)
    frames_in: AtomicU64,
    frames_published: AtomicU64,
    presents: AtomicU64,
    rebuilds: AtomicU64,
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
            display_names: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            covered: AtomicBool::new(false),
            need_permission: AtomicBool::new(false),
            badge: AtomicBool::new(false),
            rebuild: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            own_window_id: AtomicU32::new(0),
            frames_in: AtomicU64::new(0),
            frames_published: AtomicU64::new(0),
            presents: AtomicU64::new(0),
            rebuilds: AtomicU64::new(0),
        }
    }
}

/// 이벤트 루프로 보내는 신호
#[derive(Debug)]
enum UserEvent {
    /// 새 프레임 또는 상태 변화 — 다시 그리기
    Wake,
    /// 메뉴·전역 단축키 조작
    #[allow(dead_code)] // 메뉴가 없는 플랫폼에서는 쓰이지 않음
    Action(Action),
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
            "badge" => shared.badge.store(n != 0, Ordering::Relaxed),
            _ => {}
        }
    }
}

fn save_config(shared: &Shared) {
    let Some(path) = config_path() else { return };
    let text = format!(
        "strength={}\nfps={}\ndisplay={}\nbadge={}\n",
        shared.strength.load(Ordering::Relaxed),
        shared.fps_idx.load(Ordering::Relaxed),
        shared.display_idx.load(Ordering::Relaxed),
        shared.badge.load(Ordering::Relaxed) as u8,
    );
    let _ = std::fs::write(path, text);
}

// ---------------------------------------------------------------------------
// 캡처 스레드
// ---------------------------------------------------------------------------

/// 프레임에서 원시 픽셀 정보를 꺼낸다. 플랫폼별로 도착하는 픽셀 포맷이 달라도 처리.
/// macOS의 "변화 없음(idle)" 신호는 크기 0인 프레임으로 오며, 여기서 None이 된다.
fn frame_parts(frame: &Frame) -> Option<RawFrame<'_>> {
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
    Some(RawFrame { w: w as usize, h: h as usize, data: data.as_slice(), px, rgb })
}

fn capture_thread(shared: Arc<Shared>, proxy: EventLoopProxy<UserEvent>) {
    let set_status = |msg: &str| {
        let mut s = shared.status.lock().unwrap();
        if *s != msg {
            if !msg.is_empty() {
                log::event(&format!("상태: {msg}"));
            }
            *s = msg.to_string();
        }
        drop(s);
        let _ = proxy.send_event(UserEvent::Wake);
    };

    if !scap::is_supported() {
        set_status("이 환경에서는 화면 캡처가 지원되지 않습니다 (macOS 12.3+ / Windows 10 1903+ 필요)");
        return;
    }

    if !scap::has_permission() {
        scap::request_permission();
        shared.need_permission.store(true, Ordering::Relaxed);
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
        shared.need_permission.store(false, Ordering::Relaxed);
        log::event("화면 기록 권한 확인됨");
    }

    // 스트림이 어떤 방식으로 죽어도(오류 통보 없이 조용히 멈추는 경우 포함)
    // 이 시간 안에는 반드시 새 스트림으로 교체된다.
    const STALL_LIMIT: Duration = Duration::from_secs(20);
    // 한 번의 대기(최대 1초)가 이보다 길게 걸렸다면 시스템이 잠들었다 깨어난 것으로 본다
    const SLEEP_GAP: Duration = Duration::from_secs(8);
    // 이 이상 프레임이 끊겼다가 다시 오면 기록해 둔다
    const GAP_NOTE: Duration = Duration::from_secs(15);

    // 펌프 스레드 → 관제 루프로 프레임을 전달하는 채널 (세대번호로 구분)
    let (ftx, frx) = mpsc::channel::<(u64, Frame)>();
    let mut was_covered = false;

    loop {
        if shared.quit.load(Ordering::Relaxed) {
            return;
        }

        // ---- 즉시 가림 중: 스트림 없이 대기 ----
        if shared.covered.load(Ordering::Relaxed) {
            if !was_covered {
                was_covered = true;
                // 세대를 올려 기존 펌프 스레드가 다음 프레임에서 스트림을 정리하게 한다
                shared.generation.fetch_add(1, Ordering::Relaxed);
                log::event("즉시 가림: 캡처 스트림 중지");
            }
            thread::sleep(Duration::from_millis(150));
            continue;
        }
        if was_covered {
            was_covered = false;
            log::event("가림 해제: 캡처 재개");
        }

        // ---- 펌프 스레드 시작 ----
        // get_next_frame()은 스트림이 조용히 죽으면 영원히 잠들 수 있다.
        // 그래서 프레임 대기는 '희생 가능한' 펌프 스레드에 맡기고,
        // 이 관제 루프는 타임아웃 있는 수신으로 워치독 역할을 겸한다.
        //
        // 캡처 대상(Target)은 Windows에서 스레드 간 이동이 불가능한 원시 핸들을
        // 담고 있어서, 옵션 구성은 여기서 하지 않고 펌프 스레드 안에서 한다.
        let gen = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
        shared.rebuilds.fetch_add(1, Ordering::Relaxed);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
        {
            let ftx = ftx.clone();
            let shared = shared.clone();
            let proxy = proxy.clone();
            thread::spawn(move || pump_frames(gen, ftx, ready_tx, shared, proxy));
        }
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(desc)) => {
                log::event(&format!("캡처 시작 (세대 {gen}, {desc})"));
                set_status("");
            }
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
        let fps = FPS_OPTIONS[shared.fps_idx.load(Ordering::Relaxed) % FPS_OPTIONS.len()];
        let min_gap = Duration::from_millis((800 / fps.max(1)) as u64);
        let mut last_hash: u64 = 0;
        let mut last_proc: Option<Instant> = None;
        let mut last_frame = Instant::now();
        let mut last_tick = (Instant::now(), SystemTime::now());

        // ---- 관제 루프: 프레임 처리 + 워치독 ----
        let reason: String = loop {
            if shared.quit.load(Ordering::Relaxed) {
                return;
            }
            if shared.covered.load(Ordering::Relaxed) {
                break "즉시 가림".into();
            }
            if shared.rebuild.swap(false, Ordering::Relaxed) {
                break "설정 변경".into(); // fps·모니터 변경 → 스트림 재구성
            }

            // 잠자기 복귀 감지: 1초짜리 대기가 비정상적으로 길었는지
            // (단조 시계가 잠자는 동안 멈추는 OS도 있으므로 벽시계와 둘 다 본다)
            let now = (Instant::now(), SystemTime::now());
            let mono_gap = now.0.duration_since(last_tick.0);
            let wall_gap = now.1.duration_since(last_tick.1).unwrap_or_default();
            last_tick = now;
            if mono_gap > SLEEP_GAP || wall_gap > SLEEP_GAP {
                break format!(
                    "잠자기 복귀 추정 (대기 공백 {:.0}초)",
                    mono_gap.max(wall_gap).as_secs_f32()
                );
            }

            match frx.recv_timeout(Duration::from_secs(1)) {
                Ok((g, frame)) => {
                    if g != gen {
                        continue; // 이전 세대의 잔여 프레임은 무시
                    }
                    let gap = last_frame.elapsed();
                    if gap > GAP_NOTE {
                        log::event(&format!("프레임 공백 {:.1}초 후 수신 재개", gap.as_secs_f32()));
                    }
                    last_frame = Instant::now();
                    let Some(raw) = frame_parts(&frame) else {
                        continue; // 변화 없음(idle) 신호 — 스트림이 살아 있다는 뜻
                    };
                    shared.frames_in.fetch_add(1, Ordering::Relaxed);
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
                        blur::process_frame(&raw, tw, sigma, last_hash)
                    }))
                    .unwrap_or(None);
                    if let Some(out) = processed {
                        last_hash = out.hash;
                        if let Some(tiny) = out.tiny {
                            // 가림이 켜진 직후 도착한 프레임은 게시하지 않는다
                            if !shared.covered.load(Ordering::Relaxed) {
                                *shared.frame.lock().unwrap() = Some(tiny);
                                shared.frames_published.fetch_add(1, Ordering::Relaxed);
                                let _ = proxy.send_event(UserEvent::Wake);
                            }
                        }
                    }
                    last_proc = Some(Instant::now());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // macOS는 화면이 조용해도 '변화 없음' 신호가 오므로 여기까지 오면
                    // 스트림이 조용히 죽은 것이다. Windows는 변화가 있을 때만 프레임이
                    // 오므로 조용한 화면에서도 재구성될 수 있다 — 어느 쪽이든 무해하다.
                    if last_frame.elapsed() > STALL_LIMIT {
                        break format!("워치독: {}초간 프레임 없음", STALL_LIMIT.as_secs());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break "펌프 채널 끊김".into(),
            }
        };
        log::event(&format!("스트림 재구성 (세대 {gen}, 사유: {reason})"));
        // 루프 이탈 → 다음 반복에서 세대가 올라가고 새 스트림이 만들어진다.
        // 이전 펌프 스레드는 자기 세대가 지난 것을 확인하면 스스로 정리하고 종료한다.
    }
}

/// 펌프 스레드: 캡처 대상을 고르고 스트림을 만들어 프레임을 관제 루프로 퍼 올린다.
/// get_next_frame()이 영원히 잠들 수 있으므로 이 스레드는 희생 가능하게 설계했다 —
/// 자신의 세대가 지나면 다음 프레임 수신 시 스트림을 정리하고 스스로 종료한다.
fn pump_frames(
    gen: u64,
    ftx: mpsc::Sender<(u64, Frame)>,
    ready_tx: mpsc::Sender<Result<String, String>>,
    shared: Arc<Shared>,
    proxy: EventLoopProxy<UserEvent>,
) {
    // 대상 디스플레이와 제외 창 목록 구성 (Target은 이 스레드 밖으로 내보내지 않는다)
    let targets = catch_unwind(scap::get_all_targets).unwrap_or_default();
    let displays: Vec<Target> = targets
        .iter()
        .filter(|t| matches!(t, Target::Display(_)))
        .cloned()
        .collect();
    if displays.is_empty() {
        let _ = ready_tx.send(Err("캡처할 디스플레이를 찾지 못했습니다".to_string()));
        return;
    }
    let names: Vec<String> = displays
        .iter()
        .map(|t| match t {
            Target::Display(d) => d.title.clone(),
            _ => String::new(),
        })
        .collect();
    shared.display_count.store(displays.len(), Ordering::Relaxed);
    {
        let mut cur = shared.display_names.lock().unwrap();
        if *cur != names {
            *cur = names;
            let _ = proxy.send_event(UserEvent::Wake); // 메뉴의 모니터 목록 갱신
        }
    }
    let di = shared.display_idx.load(Ordering::Relaxed) % displays.len();

    // 자기 자신(블러 창)은 캡처에서 제외 → 거울 속 거울 방지 (macOS에서 동작)
    // 창 번호로 찾고, 번호를 모를 때는 제목으로 찾는다.
    let own_id = shared.own_window_id.load(Ordering::Relaxed);
    let excluded: Vec<Target> = targets
        .iter()
        .filter(|t| {
            matches!(t, Target::Window(w)
                if (own_id != 0 && w.id == own_id) || w.title.starts_with(WINDOW_TITLE))
        })
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
        excluded_targets: if excluded.is_empty() { None } else { Some(excluded.clone()) },
    };

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
    let _ = ready_tx.send(Ok(format!(
        "모니터 {}/{}, {fps}fps, 자기 창 제외 {}개",
        di + 1,
        displays.len(),
        excluded.len()
    )));

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
// winit 앱
// ---------------------------------------------------------------------------

struct App {
    shared: Arc<Shared>,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    capture_started: bool,
    last_title: String,
    last_present: Instant,
    last_housekeep: Instant,
    last_stats: Instant,
    stats_base: [u64; 4],
    monitor_sig: Option<Vec<(String, u32, u32, i32, i32)>>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    menu: Option<menu::AppMenu>,
}

impl App {
    fn status_text(&self) -> String {
        let s = &self.shared;
        let status = s.status.lock().unwrap().clone();
        if s.covered.load(Ordering::Relaxed) {
            return "즉시 가림 중".to_string();
        }
        if !status.is_empty() {
            return status;
        }
        let (_, _, name) = STRENGTHS[s.strength.load(Ordering::Relaxed) % STRENGTHS.len()];
        let fps = FPS_OPTIONS[s.fps_idx.load(Ordering::Relaxed) % FPS_OPTIONS.len()];
        let mode = if s.paused.load(Ordering::Relaxed) { "동결" } else { "실시간" };
        let n = s.display_count.load(Ordering::Relaxed);
        let disp = if n > 1 {
            format!(" · 모니터 {}/{}", s.display_idx.load(Ordering::Relaxed) % n + 1, n)
        } else {
            String::new()
        };
        format!("{mode} · 블러 {name} · {fps}fps{disp}")
    }

    fn update_title(&mut self) {
        let status = self.status_text();
        let title = format!("{WINDOW_TITLE} — {status}");
        if let Some(window) = &self.window {
            if title != self.last_title {
                window.set_title(&title);
                self.last_title = title;
            }
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(m) = &mut self.menu {
            let s = &self.shared;
            m.update(&menu::MenuState {
                status_line: status,
                covered: s.covered.load(Ordering::Relaxed),
                frozen: s.paused.load(Ordering::Relaxed),
                strength: s.strength.load(Ordering::Relaxed),
                fps_idx: s.fps_idx.load(Ordering::Relaxed),
                display_idx: s.display_idx.load(Ordering::Relaxed),
                display_names: s.display_names.lock().unwrap().clone(),
                badge: s.badge.load(Ordering::Relaxed),
            });
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
        let (w, h) = (bw.get() as usize, bh.get() as usize);
        let s = &self.shared;
        if s.covered.load(Ordering::Relaxed) {
            cover::draw_cover(&mut buffer, w, h, CoverKind::Covered);
        } else {
            let guard = s.frame.lock().unwrap();
            match guard.as_ref() {
                Some(tiny) => {
                    blur::draw_tiny(tiny, &mut buffer, w, h);
                    if s.badge.load(Ordering::Relaxed) {
                        cover::draw_badge(&mut buffer, w, h, s.paused.load(Ordering::Relaxed));
                    }
                }
                None => {
                    let kind = if s.need_permission.load(Ordering::Relaxed) {
                        CoverKind::Permission
                    } else if !s.status.lock().unwrap().is_empty() {
                        CoverKind::Reconnecting
                    } else {
                        CoverKind::Connecting
                    };
                    cover::draw_cover(&mut buffer, w, h, kind);
                }
            }
        }
        let _ = buffer.present();
        self.last_present = Instant::now();
        s.presents.fetch_add(1, Ordering::Relaxed);
    }

    /// 키보드·메뉴·전역 단축키 조작을 처리한다
    fn apply(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let s = self.shared.clone();
        match action {
            Action::ToggleCover => {
                let now = !s.covered.load(Ordering::Relaxed);
                s.covered.store(now, Ordering::Relaxed);
                if now {
                    // 캐시된 블러 화면도 지운다 — 가림 해제 시 새 화면부터 보여준다
                    *s.frame.lock().unwrap() = None;
                    s.paused.store(false, Ordering::Relaxed);
                }
                log::event(if now { "즉시 가림 켬" } else { "즉시 가림 끔" });
            }
            Action::ToggleFreeze => {
                if s.covered.load(Ordering::Relaxed) {
                    return;
                }
                let p = !s.paused.load(Ordering::Relaxed);
                s.paused.store(p, Ordering::Relaxed);
                log::event(if p { "화면 동결" } else { "동결 해제" });
            }
            Action::Strength(i) => {
                s.strength.store(i.min(STRENGTHS.len() - 1), Ordering::Relaxed);
            }
            Action::Fps(i) => {
                s.fps_idx.store(i.min(FPS_OPTIONS.len() - 1), Ordering::Relaxed);
                s.rebuild.store(true, Ordering::Relaxed);
            }
            Action::CycleFps => {
                let i = (s.fps_idx.load(Ordering::Relaxed) + 1) % FPS_OPTIONS.len();
                s.fps_idx.store(i, Ordering::Relaxed);
                s.rebuild.store(true, Ordering::Relaxed);
            }
            Action::Monitor(i) => {
                s.display_idx.store(i, Ordering::Relaxed);
                s.rebuild.store(true, Ordering::Relaxed);
            }
            Action::CycleMonitor => {
                let n = s.display_count.load(Ordering::Relaxed).max(1);
                let i = (s.display_idx.load(Ordering::Relaxed) + 1) % n;
                s.display_idx.store(i, Ordering::Relaxed);
                s.rebuild.store(true, Ordering::Relaxed);
            }
            Action::ToggleBadge => {
                let b = !s.badge.load(Ordering::Relaxed);
                s.badge.store(b, Ordering::Relaxed);
            }
            Action::OpenPermission => {
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                        .spawn();
                }
                return;
            }
            Action::OpenLogs => {
                if let Some(dir) = log::dir() {
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(&dir).spawn();
                    #[cfg(target_os = "windows")]
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                    let _ = dir;
                }
                return;
            }
            Action::Quit => {
                self.shutdown();
                event_loop.exit();
                return;
            }
        }
        save_config(&s);
        self.update_title();
        self.draw();
    }

    fn shutdown(&mut self) {
        if self.shared.quit.swap(true, Ordering::Relaxed) {
            return;
        }
        save_config(&self.shared);
        log::flush();
    }

    fn on_key(&mut self, event: KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed || event.repeat {
            return;
        }
        let action = match &event.logical_key {
            Key::Character(c) => match c.as_str() {
                "1" => Action::Strength(0),
                "2" => Action::Strength(1),
                "3" => Action::Strength(2),
                // 한글 입력 상태에서도 동작하도록 자판 위치의 한글 자모도 함께 매핑
                "f" | "F" | "ㄹ" => Action::CycleFps,
                "m" | "M" | "ㅡ" => Action::CycleMonitor,
                _ => return,
            },
            Key::Named(NamedKey::Space) => Action::ToggleFreeze,
            _ => return,
        };
        self.apply(action, event_loop);
    }

    /// 주기 점검: 모니터 구성 변경 감지, 통계 기록
    fn housekeep(&mut self, event_loop: &ActiveEventLoop) {
        self.last_housekeep = Instant::now();

        let sig: Vec<(String, u32, u32, i32, i32)> = event_loop
            .available_monitors()
            .map(|m| {
                let size = m.size();
                let pos = m.position();
                (m.name().unwrap_or_default(), size.width, size.height, pos.x, pos.y)
            })
            .collect();
        match &self.monitor_sig {
            Some(old) if *old != sig => {
                log::event(&format!("모니터 구성 변경 감지 ({}대 → {}대) — 재연결", old.len(), sig.len()));
                self.shared.rebuild.store(true, Ordering::Relaxed);
                self.monitor_sig = Some(sig);
            }
            None => self.monitor_sig = Some(sig),
            _ => {}
        }

        if self.last_stats.elapsed() >= STATS_EVERY {
            let s = &self.shared;
            let now = [
                s.frames_in.load(Ordering::Relaxed),
                s.frames_published.load(Ordering::Relaxed),
                s.presents.load(Ordering::Relaxed),
                s.rebuilds.load(Ordering::Relaxed),
            ];
            let d: Vec<u64> = now.iter().zip(self.stats_base.iter()).map(|(a, b)| a - b).collect();
            log::event(&format!(
                "동작 중 · 최근 {}분: 수신 {} · 처리 {} · 표시 {} · 스트림 생성 {}",
                STATS_EVERY.as_secs() / 60,
                d[0],
                d[1],
                d[2],
                d[3]
            ));
            self.stats_base = now;
            self.last_stats = Instant::now();
        }
    }

    /// 공유 창 우클릭 → 메뉴 막대와 같은 메뉴를 띄운다
    fn show_context_menu(&self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let (Some(window), Some(m)) = (&self.window, &self.menu) else { return };
            let Ok(handle) = window.window_handle() else { return };
            match handle.as_raw() {
                #[cfg(target_os = "windows")]
                RawWindowHandle::Win32(h) => {
                    use tray_icon::menu::ContextMenu;
                    // Windows는 메뉴가 닫힐 때까지 여기서 기다린다(winit이 그동안의 이벤트를 보관)
                    unsafe {
                        m.menu.show_context_menu_for_hwnd(h.hwnd.get(), None);
                    }
                }
                #[cfg(target_os = "macos")]
                RawWindowHandle::AppKit(h) => {
                    // macOS는 메뉴가 열려 있는 동안 별도 루프가 돈다. winit 이벤트 처리 도중에
                    // 띄우면 재진입 문제가 생기므로, 처리가 끝난 뒤 메인 큐에서 띄운다.
                    let view = h.ns_view.as_ptr() as usize;
                    CONTEXT_MENU.with(|c| *c.borrow_mut() = Some(m.menu.clone()));
                    dispatch2::DispatchQueue::main().exec_async(move || {
                        use tray_icon::menu::ContextMenu;
                        CONTEXT_MENU.with(|c| {
                            if let Some(menu) = c.borrow().as_ref() {
                                unsafe {
                                    menu.show_context_menu_for_nsview(view as *const std::ffi::c_void, None);
                                }
                            }
                        });
                    });
                }
                _ => {
                    let _ = m;
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    static CONTEXT_MENU: std::cell::RefCell<Option<tray_icon::menu::Menu>> =
        const { std::cell::RefCell::new(None) };
}

/// macOS: 공유 창의 시스템 창 번호(CGWindowID)를 얻는다 — 캡처 제외용
#[cfg(target_os = "macos")]
fn own_window_number(window: &Window) -> Option<u32> {
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    unsafe {
        let view: &AnyObject = h.ns_view.cast::<AnyObject>().as_ref();
        let win: *mut AnyObject = objc2::msg_send![view, window];
        if win.is_null() {
            return None;
        }
        let n: isize = objc2::msg_send![&*win, windowNumber];
        (n > 0).then_some(n as u32)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title(WINDOW_TITLE)
                .with_inner_size(LogicalSize::new(960.0, 560.0));
            // Windows: 창 제목줄·작업 표시줄 아이콘 (macOS는 앱 번들 아이콘을 씀)
            #[cfg(target_os = "windows")]
            let attrs = {
                use winit::platform::windows::WindowAttributesExtWindows;
                let icon = || {
                    winit::window::Icon::from_rgba(
                        include_bytes!("../assets/icon/window64.rgba").to_vec(),
                        64,
                        64,
                    )
                    .ok()
                };
                attrs.with_window_icon(icon()).with_taskbar_icon(icon())
            };
            let window = Rc::new(
                event_loop
                    .create_window(attrs)
                    .expect("창을 만들지 못했습니다"),
            );
            let context =
                softbuffer::Context::new(window.clone()).expect("그래픽 컨텍스트 생성 실패");
            let surface =
                softbuffer::Surface::new(&context, window.clone()).expect("표시 서피스 생성 실패");
            #[cfg(target_os = "macos")]
            if let Some(id) = own_window_number(&window) {
                self.shared.own_window_id.store(id, Ordering::Relaxed);
            }
            self.window = Some(window);
            self.surface = Some(surface);
        }

        // 메뉴 막대 아이콘·전역 단축키 — 이벤트 루프가 돌기 시작한 뒤에 만들어야 한다
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self.menu.is_none() {
            let proxy = Arc::new(Mutex::new(self.proxy.clone()));
            let send = move |a: Action| {
                if let Ok(p) = proxy.lock() {
                    let _ = p.send_event(UserEvent::Action(a));
                }
            };
            let m = menu::AppMenu::new(send);
            if m.hotkey_ok {
                log::event(&format!("전역 가림 단축키 등록됨 ({})", menu::HOTKEY_LABEL));
            }
            self.menu = Some(m);
        }
        self.update_title();
        self.draw();

        // 창이 생긴 뒤에 캡처를 시작해야 자기 창 제외 목록에 이 창이 들어감
        if !self.capture_started {
            self.capture_started = true;
            let shared = self.shared.clone();
            let proxy = self.proxy.clone();
            thread::spawn(move || capture_thread(shared, proxy));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                self.update_title();
                // request_redraw()에 맡기지 않고 즉시 그린다 — 창이 다른 창에 완전히
                // 가려져 있을 때 macOS가 재그리기 요청을 미루거나 생략할 수 있는데,
                // 그러면 디스코드가 캡처해 가는 이 창의 내용이 멈춘 것처럼 보인다.
                self.draw();
            }
            UserEvent::Action(a) => self.apply(a, event_loop),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // 생존 신호: 새 프레임이 없어도 마지막 화면을 주기적으로 다시 내보낸다
        if self.last_present.elapsed() >= KEEPALIVE {
            self.draw();
        }
        if self.last_housekeep.elapsed() >= HOUSEKEEP {
            self.housekeep(event_loop);
        }
        let next = std::cmp::min(self.last_present + KEEPALIVE, self.last_housekeep + HOUSEKEEP);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.shutdown();
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, event_loop),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                self.show_context_menu();
            }
            WindowEvent::Occluded(hidden) => {
                log::event(if hidden { "공유 창이 완전히 가려짐" } else { "공유 창이 다시 보임" });
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
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
    log::init(log::default_dir());
    prevent_app_nap();
    let shared = Arc::new(Shared::new());
    load_config(&shared);
    log::event(&format!(
        "BlurScreen {APP_VERSION} 시작 ({}, 블러 {}, {}fps)",
        std::env::consts::OS,
        STRENGTHS[shared.strength.load(Ordering::Relaxed)].2,
        FPS_OPTIONS[shared.fps_idx.load(Ordering::Relaxed)]
    ));

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("이벤트 루프 생성 실패");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    let now = Instant::now();
    let mut app = App {
        shared,
        proxy,
        window: None,
        surface: None,
        capture_started: false,
        last_title: String::new(),
        last_present: now,
        last_housekeep: now,
        last_stats: now,
        stats_base: [0; 4],
        monitor_sig: None,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        menu: None,
    };
    let _ = event_loop.run_app(&mut app);
}
