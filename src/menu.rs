// 메뉴 막대(macOS)·알림 영역(Windows) 아이콘과 그 메뉴, 전역 가림 단축키.
// 같은 메뉴를 공유 창 우클릭에도 띄운다. 운영체제 기본 메뉴라 열 때만 동작한다.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::action::Action;
use crate::blur::STRENGTHS;
use crate::icons::{self, IconKind};
use crate::FPS_OPTIONS;

/// 메뉴에 반영할 현재 상태
#[derive(Clone, PartialEq, Debug)]
pub struct MenuState {
    pub status_line: String,
    pub covered: bool,
    pub frozen: bool,
    pub strength: usize,
    pub fps_idx: usize,
    pub display_idx: usize,
    pub display_names: Vec<String>,
    pub badge: bool,
}

#[cfg(target_os = "macos")]
pub const HOTKEY_LABEL: &str = "⌃⌥⌘B";
#[cfg(not(target_os = "macos"))]
pub const HOTKEY_LABEL: &str = "Ctrl+Alt+Shift+B";

pub struct AppMenu {
    pub menu: Menu,
    status: MenuItem,
    cover: MenuItem,
    freeze: CheckMenuItem,
    strengths: Vec<CheckMenuItem>,
    fps: Vec<CheckMenuItem>,
    monitor_sub: Submenu,
    monitors: Vec<CheckMenuItem>,
    monitor_names: Vec<String>,
    badge: CheckMenuItem,
    tray: Option<TrayIcon>,
    tray_kind: Option<IconKind>,
    _hotkeys: Option<(GlobalHotKeyManager, HotKey)>,
    pub hotkey_ok: bool,
    last: Option<MenuState>,
}

fn icon(kind: IconKind) -> Option<Icon> {
    // macOS 메뉴 막대는 템플릿 아이콘(시스템이 밝기·다크 모드에 맞춰 색을 입힘)
    let template = cfg!(target_os = "macos");
    let size = 44;
    Icon::from_rgba(icons::render(kind, size, template), size, size).ok()
}

impl AppMenu {
    /// 이벤트 루프가 돌기 시작한 뒤(메인 스레드)에 만들어야 한다.
    /// `send`는 메뉴·단축키 조작을 이벤트 루프로 보내는 함수.
    pub fn new(send: impl Fn(Action) + Send + Sync + Clone + 'static) -> AppMenu {
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "시작하는 중…", false, None);
        let cover = MenuItem::with_id("cover", format!("즉시 가림  ({HOTKEY_LABEL})"), true, None);
        let freeze = CheckMenuItem::with_id("freeze", "화면 동결  (Space)", true, false, None);

        let strength_sub = Submenu::new("블러 강도", true);
        let strengths: Vec<CheckMenuItem> = STRENGTHS
            .iter()
            .enumerate()
            .map(|(i, (_, _, name))| {
                CheckMenuItem::with_id(format!("strength-{i}"), format!("{name}  ({})", i + 1), true, false, None)
            })
            .collect();
        for item in &strengths {
            let _ = strength_sub.append(item);
        }

        let fps_sub = Submenu::new("프레임 속도  (F)", true);
        let fps: Vec<CheckMenuItem> = FPS_OPTIONS
            .iter()
            .enumerate()
            .map(|(i, f)| CheckMenuItem::with_id(format!("fps-{i}"), format!("{f} fps"), true, false, None))
            .collect();
        for item in &fps {
            let _ = fps_sub.append(item);
        }

        let monitor_sub = Submenu::new("모니터  (M)", true);
        let badge = CheckMenuItem::with_id("badge", "공유 화면에 상태 배지 표시", true, false, None);
        let logs = MenuItem::with_id("logs", "동작 기록 폴더 열기…", true, None);
        let quit = MenuItem::with_id("quit", "종료", true, None);

        let sep = || PredefinedMenuItem::separator();
        let _ = menu.append_items(&[&status, &sep(), &cover, &freeze, &sep(), &strength_sub, &fps_sub, &monitor_sub]);
        let _ = menu.append_items(&[&sep(), &badge]);
        #[cfg(target_os = "macos")]
        {
            let perm = MenuItem::with_id("perm", "화면 기록 권한 설정 열기…", true, None);
            let _ = menu.append(&perm);
        }
        let _ = menu.append_items(&[&logs, &sep(), &quit]);

        // 메뉴 클릭 → 이벤트 루프
        {
            let send = send.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if let Some(a) = Action::from_id(&e.id.0) {
                    send(a);
                }
            }));
        }

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip("Blur Screen")
            .with_icon_templated_opt(icon(IconKind::Live))
            .build();
        let tray = match tray {
            Ok(t) => Some(t),
            Err(e) => {
                crate::log::event(&format!("메뉴 막대 아이콘 생성 실패: {e}"));
                None
            }
        };

        // 전역 가림 단축키
        let hotkeys = (|| -> Result<(GlobalHotKeyManager, HotKey), String> {
            let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
            #[cfg(target_os = "macos")]
            let mods = Modifiers::CONTROL | Modifiers::ALT | Modifiers::META;
            #[cfg(not(target_os = "macos"))]
            let mods = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
            let key = HotKey::new(Some(mods), Code::KeyB);
            manager.register(key).map_err(|e| e.to_string())?;
            Ok((manager, key))
        })();
        let hotkey_ok = hotkeys.is_ok();
        match &hotkeys {
            Ok((_, key)) => {
                let id = key.id();
                let send = send.clone();
                GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                    if e.id() == id && e.state() == HotKeyState::Pressed {
                        send(Action::ToggleCover);
                    }
                }));
            }
            Err(e) => crate::log::event(&format!("전역 단축키 등록 실패: {e}")),
        }

        AppMenu {
            menu,
            status,
            cover,
            freeze,
            strengths,
            fps,
            monitor_sub,
            monitors: Vec::new(),
            monitor_names: Vec::new(),
            badge,
            tray,
            tray_kind: Some(IconKind::Live),
            _hotkeys: hotkeys.ok(),
            hotkey_ok,
            last: None,
        }
    }

    /// 상태가 바뀐 부분만 메뉴·아이콘에 반영한다
    pub fn update(&mut self, st: &MenuState) {
        if self.last.as_ref() == Some(st) {
            return;
        }
        self.status.set_text(&st.status_line);
        self.cover.set_text(if st.covered {
            format!("가림 해제 · 다시 시작  ({HOTKEY_LABEL})")
        } else {
            format!("즉시 가림  ({HOTKEY_LABEL})")
        });
        self.freeze.set_checked(st.frozen);
        self.freeze.set_enabled(!st.covered);
        for (i, item) in self.strengths.iter().enumerate() {
            item.set_checked(i == st.strength);
        }
        for (i, item) in self.fps.iter().enumerate() {
            item.set_checked(i == st.fps_idx);
        }
        self.badge.set_checked(st.badge);

        // 모니터 목록이 바뀌었을 때만 하위 메뉴를 다시 만든다
        if st.display_names != self.monitor_names {
            for item in self.monitors.drain(..) {
                let _ = self.monitor_sub.remove(&item);
            }
            let names: Vec<String> = if st.display_names.is_empty() {
                vec!["모니터 1".to_string()]
            } else {
                st.display_names.clone()
            };
            for (i, name) in names.iter().enumerate() {
                let label = if name.trim().is_empty() { format!("모니터 {}", i + 1) } else { format!("{}. {name}", i + 1) };
                let item = CheckMenuItem::with_id(format!("monitor-{i}"), label, true, false, None);
                let _ = self.monitor_sub.append(&item);
                self.monitors.push(item);
            }
            self.monitor_names = st.display_names.clone();
        }
        let n = self.monitors.len().max(1);
        for (i, item) in self.monitors.iter().enumerate() {
            item.set_checked(i == st.display_idx % n);
        }

        let kind = if st.covered {
            IconKind::Covered
        } else if st.frozen {
            IconKind::Frozen
        } else {
            IconKind::Live
        };
        if let Some(tray) = &self.tray {
            if self.tray_kind != Some(kind) {
                let _ = set_tray_icon(tray, icon(kind));
                self.tray_kind = Some(kind);
            }
            let _ = tray.set_tooltip(Some(format!("Blur Screen — {}", st.status_line)));
        }
        self.last = Some(st.clone());
    }
}

/// macOS는 템플릿 아이콘, Windows는 색 아이콘으로 설정
trait TemplatedOpt {
    fn with_icon_templated_opt(self, icon: Option<Icon>) -> Self;
}

impl TemplatedOpt for TrayIconBuilder {
    fn with_icon_templated_opt(self, icon: Option<Icon>) -> Self {
        match icon {
            None => self,
            #[cfg(target_os = "macos")]
            Some(i) => self.with_icon_templated(i),
            #[cfg(not(target_os = "macos"))]
            Some(i) => self.with_icon(i),
        }
    }
}

fn set_tray_icon(tray: &TrayIcon, icon: Option<Icon>) -> tray_icon::Result<()> {
    #[cfg(target_os = "macos")]
    {
        tray.set_icon_templated(icon)
    }
    #[cfg(not(target_os = "macos"))]
    {
        tray.set_icon(icon)
    }
}
