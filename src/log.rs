// 동작 기록(로그). 디스코드 공유가 끊긴 시각과 앱 내부 사건(스트림 재구성,
// 오류, 잠자기 복귀 등)을 대조하기 위한 작은 텍스트 파일.
//
// - 화면 내용은 절대 기록하지 않는다. 사건 이름과 숫자만 남긴다.
// - 파일이 1MB를 넘으면 blurscreen.old.log 로 한 번 돌리고 새로 쓴다.
// - 같은 메시지가 연달아 반복되면 한 줄로 묶어 "(N회 반복)"으로 남긴다.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 1024 * 1024;
const FILE_NAME: &str = "blurscreen.log";
const OLD_NAME: &str = "blurscreen.old.log";

struct Inner {
    dir: PathBuf,
    file: Option<File>,
    written: u64,
    last_msg: String,
    repeats: u32,
}

static LOGGER: OnceLock<Mutex<Inner>> = OnceLock::new();

/// 운영체제별 로그 폴더
pub fn default_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Logs/BlurScreen"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(PathBuf::from(base).join("BlurScreen").join("logs"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".blurscreen-logs"))
    }
}

/// 로그를 시작한다. 실패해도 앱은 정상 동작한다(기록만 생략).
pub fn init(dir: Option<PathBuf>) {
    let Some(dir) = dir else { return };
    let _ = std::fs::create_dir_all(&dir);
    let (file, written) = open(&dir);
    let _ = LOGGER.set(Mutex::new(Inner {
        dir,
        file,
        written,
        last_msg: String::new(),
        repeats: 0,
    }));
}

/// 로그 폴더 경로 (메뉴의 "로그 폴더 열기"용)
pub fn dir() -> Option<PathBuf> {
    LOGGER.get().map(|m| m.lock().unwrap().dir.clone())
}

fn open(dir: &Path) -> (Option<File>, u64) {
    let path = dir.join(FILE_NAME);
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join(OLD_NAME));
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            (Some(f), len)
        }
        Err(_) => (None, 0),
    }
}

/// 현재 시각을 "2026-10-14 09:10:11.123 +09:00" 형식으로
fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f %:z").to_string()
}

fn write_line(inner: &mut Inner, line: &str) {
    if inner.written > MAX_BYTES {
        let (file, written) = {
            inner.file = None; // 닫은 뒤 돌린다 (Windows는 열린 파일 이름을 못 바꿈)
            open(&inner.dir)
        };
        inner.file = file;
        inner.written = written;
    }
    if let Some(f) = inner.file.as_mut() {
        if f.write_all(line.as_bytes()).is_ok() {
            inner.written += line.len() as u64;
        }
    }
}

/// 사건 하나를 기록한다. 어느 스레드에서 불러도 된다.
pub fn event(msg: &str) {
    let Some(m) = LOGGER.get() else { return };
    let Ok(mut inner) = m.lock() else { return };
    if msg == inner.last_msg {
        inner.repeats += 1;
        return;
    }
    if inner.repeats > 0 {
        let line = format!("{}  (직전 메시지 {}회 더 반복)\n", stamp(), inner.repeats);
        write_line(&mut inner, &line);
        inner.repeats = 0;
    }
    let line = format!("{}  {}\n", stamp(), msg);
    write_line(&mut inner, &line);
    inner.last_msg = msg.to_string();
}

/// 반복 묶음을 닫고 마무리한다 (종료 시)
pub fn flush() {
    event("— 기록 끝 —");
}
