# Blur Screen — 화면공유용 블러 창

현재 화면을 강하게 블러 처리한 모습을 창 하나로 보여주는 프로그램입니다.
디스코드 화면공유에서 **이 창만** 공유하면, 상대에게는 블러된 화면만 보이고
실제 화면(원고)은 절대 전송되지 않습니다.

- 캡처는 OS 공식 API(macOS: ScreenCaptureKit / Windows: Graphics Capture)가 수행
- 기본 10fps + 강한 축소 + 변경 감지라 CPU를 거의 쓰지 않음 → 디스코드 연결에 부담 없음
- 블러는 "해상도 축소(비가역)" 기반이라 어떤 방법으로도 글자를 복원할 수 없음

## 배포판 만들기 (GitHub Actions — 자동)

이 저장소에 `.github/workflows/build.yml` 이 있으면, 커밋할 때마다 클라우드에서
macOS/Windows 배포판이 자동으로 빌드됩니다.

1. 저장소 상단 **Actions** 탭 → 왼쪽 **build** → 초록색으로 끝난 실행을 클릭
2. 페이지 하단 **Artifacts**에서 `BlurScreen-mac`, `BlurScreen-windows` 다운로드
3. 받은 파일의 압축을 한 번 풀면 안에 지인에게 보낼 zip이 들어 있습니다
   - `BlurScreen-mac.zip` — BlurScreen.app + 안내문 (Apple Silicon/Intel 겸용)
   - `BlurScreen-windows.zip` — BlurScreen.exe + 안내문
4. 그 zip을 그대로 지인에게 전달하면 됩니다 (안내문에 첫 실행 방법이 적혀 있음)

수동으로 다시 빌드하려면 Actions 탭 → build → **Run workflow** 버튼.
Artifacts는 90일 뒤 지워지므로 받은 zip은 따로 보관해 두세요.

## 내 맥에서 직접 빌드 (선택)

```bash
cargo build --release          # 실행 파일: target/release/blurscreen
./target/release/blurscreen    # 바로 실행

bash packaging/make_app.sh     # 배포용 BlurScreen.app 생성 (dist/ 폴더)
```

직접 빌드한 .app은 현재 맥과 같은 종류(Apple Silicon)에서만 돕니다.
인텔 맥 지인까지 커버하려면 Actions 빌드를 쓰세요.

## 사용법

1. 프로그램 실행 (첫 실행 시 화면 기록 권한 허용 → 앱 재시작)
2. 디스코드 → 화면 공유 → 창 탭 → **Blur Screen** 선택
3. 평소처럼 작업 — 상대에게는 블러 화면만 보입니다

공유 중에는 창을 **최소화하지 말고 다른 창 뒤에 가려두세요**
(최소화하면 공유 화면이 멈출 수 있습니다).

| 키 | 기능 |
|---|---|
| `1` / `2` / `3` | 블러 강도 약 / 중 / 강 (기본: 중) |
| `F` | 프레임레이트 5 → 10 → 15fps (기본: 10) |
| `M` | 모니터 전환 |
| `Space` | 일시정지(화면 동결) |

설정은 `~/.blurscreen.conf`에 저장됩니다. 강도·fps 변경은 화면에 다음
변화가 생기는 순간(마우스만 움직여도) 적용됩니다.

## 문제 해결

| 증상 | 해결 |
|---|---|
| 창 제목에 "화면 기록 권한이 필요합니다" | 시스템 설정 → 개인정보 보호 및 보안 → 화면 기록 허용 후 앱 재시작 |
| 창이 검게만 나옴 | 마우스를 움직여 보세요 (화면 변화가 없으면 첫 프레임이 늦게 옵니다) |
| 캡처 오류 표시 | 자동 재연결됩니다. 해상도 변경·잠자기 복귀 직후 잠깐 나타날 수 있음 |

## 요구 환경

- macOS 12.3 이상 / Windows 10 버전 1903 이상
- 직접 빌드 시: Rust 툴체인(`cargo`)
