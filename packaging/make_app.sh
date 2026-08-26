#!/bin/bash
# BlurScreen.app 번들 생성 스크립트
# 사용법: bash packaging/make_app.sh [실행파일경로]
#   인자를 생략하면 target/release/blurscreen 을 사용합니다.
set -euo pipefail

BIN="${1:-target/release/blurscreen}"
if [ ! -f "$BIN" ]; then
  echo "실행 파일이 없습니다: $BIN — 먼저 cargo build --release 를 실행해 주세요." >&2
  exit 1
fi

APP="dist/BlurScreen.app"
rm -rf dist
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/blurscreen"
chmod +x "$APP/Contents/MacOS/blurscreen"
cp "$(dirname "$0")/Info.plist" "$APP/Contents/Info.plist"

# ad-hoc 서명 (Apple Silicon 실행 요건 충족용 — 개발자 계정 불필요)
codesign --force --deep -s - "$APP"

echo "완료: $APP"
