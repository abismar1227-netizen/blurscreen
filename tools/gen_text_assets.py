#!/usr/bin/env python3
"""보호 화면·배지에 쓰는 한글 문구를 미리 그려 두는 개발용 스크립트.

앱이 폰트 엔진을 싣지 않도록, 고정 문구를 4비트 알파 마스크로 렌더링해
assets/text/*.a4 와 src/text_assets.rs 를 생성한다. 문구를 바꿀 때만 실행하면 된다.

필요: Python 3 + Pillow, Noto Sans CJK (KR) 폰트 (SIL Open Font License)
사용: python3 tools/gen_text_assets.py
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT_DIR = os.path.join(ROOT, "assets", "text")
RS_PATH = os.path.join(ROOT, "src", "text_assets.rs")
FONT_DIR = os.environ.get("CJK_FONT_DIR", "/usr/share/fonts/opentype/noto")
KR_INDEX = 1  # Noto Sans CJK .ttc 안의 KR 서체 번호

# (상수 이름, 굵기, 픽셀 크기, 문구) — 크기는 가로 1920px(레티나 960pt) 창 기준
TEXTS = [
    ("TITLE", "Bold", 64, "보호 화면"),
    ("COVERED", "Medium", 32, "즉시 가림 중입니다"),
    ("COVERED_HINT", "Regular", 24, "단축키를 한 번 더 누르거나 메뉴에서 ‘가림 해제’를 선택하세요"),
    ("CONNECTING", "Medium", 32, "화면을 연결하고 있습니다"),
    ("RECONNECTING", "Medium", 32, "화면을 다시 연결하고 있습니다"),
    ("PERMISSION", "Medium", 32, "화면 기록 권한이 필요합니다"),
    ("PERMISSION_HINT", "Regular", 24, "메뉴의 ‘화면 기록 권한 설정 열기’에서 허용한 뒤 앱을 다시 실행해 주세요"),
    ("BADGE_LIVE", "Medium", 22, "블러 중"),
    ("BADGE_FROZEN", "Medium", 22, "동결"),
]


def render(weight: str, size: int, text: str) -> Image.Image:
    path = os.path.join(FONT_DIR, f"NotoSansCJK-{weight}.ttc")
    font = ImageFont.truetype(path, size, index=KR_INDEX)
    name = font.getname()[0]
    if "KR" not in name:
        sys.exit(f"KR 서체가 아닙니다: {name}")
    left, top, right, bottom = font.getbbox(text)
    pad = 2
    w, h = right - left + pad * 2, bottom - top + pad * 2
    img = Image.new("L", (w, h), 0)
    ImageDraw.Draw(img).text((pad - left, pad - top), text, font=font, fill=255)
    return img


def pack4(img: Image.Image) -> bytes:
    """8비트 알파 → 4비트 두 픽셀씩 한 바이트 (상위 니블이 왼쪽 픽셀)."""
    px = list(img.getdata())
    if len(px) % 2:
        px.append(0)
    out = bytearray()
    for i in range(0, len(px), 2):
        a = (px[i] + 8) // 17
        b = (px[i + 1] + 8) // 17
        out.append((a << 4) | b)
    return bytes(out)


def main() -> None:
    os.makedirs(OUT_DIR, exist_ok=True)
    lines = [
        "// 자동 생성 파일 — tools/gen_text_assets.py 로 다시 만드세요. 직접 수정 금지.",
        "// 문구 렌더링: Noto Sans CJK KR (SIL Open Font License 1.1)",
        "",
        "use crate::cover::PackedMask;",
        "",
    ]
    total = 0
    for const, weight, size, text in TEXTS:
        img = render(weight, size, text)
        data = pack4(img)
        total += len(data)
        fname = const.lower() + ".a4"
        with open(os.path.join(OUT_DIR, fname), "wb") as f:
            f.write(data)
        lines.append(f"/// {text}")
        lines.append(
            f"pub const {const}: PackedMask = PackedMask {{ w: {img.width}, h: {img.height}, "
            f"data: include_bytes!(\"../assets/text/{fname}\") }};"
        )
    with open(RS_PATH, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"{len(TEXTS)}개 문구, 총 {total:,} 바이트 → {OUT_DIR}")


if __name__ == "__main__":
    main()
