#!/usr/bin/env python3
"""BlurScreen 앱 아이콘 생성 스크립트 (개발용).

밤하늘 바탕에 초승달과 원고 글줄을 두고, 오른쪽 절반을 젖빛 유리가 덮어
글줄이 흐려지는 모습 — "선명한 원고는 내 쪽에, 상대에게는 흐린 화면만".

생성물:
  assets/icon/AppIcon-1024.png   원본 미리보기 (macOS 그리드: 여백 + 그림자)
  packaging/AppIcon.icns         macOS 앱 번들 아이콘
  packaging/windows/app.ico      Windows 실행 파일 아이콘 (여백 적게, 그림자 없음)
  assets/icon/window64.rgba      Windows 창·작업 표시줄용 64×64 원시 RGBA

필요: Python 3 + Pillow + NumPy
사용: python3 tools/gen_app_icon.py
"""
import math
import os

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
S = 2048  # 2배로 그린 뒤 줄여서 가장자리를 매끄럽게


def squircle_mask(half, n=5.0, dy=0.0):
    y, x = np.mgrid[0:S, 0:S].astype(np.float32)
    u = np.abs((x + 0.5 - S / 2) / half)
    v = np.abs((y + 0.5 - S / 2 - dy) / half)
    d = u ** n + v ** n
    edge = 1.0 - np.clip((d - 1.0) / 0.012, 0, 1)
    return Image.fromarray((edge * 255).astype(np.uint8), "L")


def lin_gradient(c0, c1, angle_deg):
    y, x = np.mgrid[0:S, 0:S].astype(np.float32) / S
    a = math.radians(angle_deg)
    t = x * math.cos(a) + y * math.sin(a)
    t = (t - t.min()) / (t.max() - t.min())
    c0 = np.array(c0, np.float32)
    c1 = np.array(c1, np.float32)
    img = c0 * (1 - t[..., None]) + c1 * t[..., None]
    return Image.fromarray(img.astype(np.uint8), "RGB")


def glow(cx, cy, r, color, strength):
    y, x = np.mgrid[0:S, 0:S].astype(np.float32)
    d = np.sqrt((x - cx) ** 2 + (y - cy) ** 2) / r
    a = np.clip(1 - d, 0, 1) ** 2 * strength
    return Image.new("RGB", (S, S), color), Image.fromarray((a * 255).astype(np.uint8), "L")


def artwork():
    """정사각형 전체를 채운 그림 (잘라내기 전)"""
    bg = lin_gradient((17, 22, 50), (60, 42, 98), 70)
    mx, my = S * 0.33, S * 0.23
    layer, alpha = glow(mx, my, S * 0.33, (255, 210, 140), 0.32)
    bg.paste(layer, (0, 0), alpha)
    base = bg.convert("RGBA")

    # 초승달
    r = S * 0.082
    moon = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(moon)
    d.ellipse([mx - r, my - r, mx + r, my + r], fill=255)
    d.ellipse([mx - r + r * 0.55, my - r - r * 0.28, mx + r + r * 0.55, my + r - r * 0.28], fill=0)
    base.paste(Image.new("RGBA", (S, S), (255, 222, 160, 255)), (0, 0), moon)

    # 원고 글줄 (첫 줄은 제목, 달빛 색)
    lines = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(lines)
    x0, y, lh = int(S * 0.215), int(S * 0.40), int(S * 0.036)
    d.rounded_rectangle([x0, y, x0 + int(S * 0.30), y + int(lh * 1.15)], lh // 2, fill=(255, 226, 168, 255))
    y += int(S * 0.095)
    for length in (0.57, 0.52, 0.56, 0.40):
        d.rounded_rectangle([x0, y, x0 + int(S * length), y + lh], lh // 2, fill=(244, 238, 226, 255))
        y += int(S * 0.072)
    base.alpha_composite(lines)

    # 오른쪽 젖빛 유리: 아래 그림을 흐리고 살짝 밝힘 + 위쪽 광택
    gx = int(S * 0.52)
    region = base.crop((gx, 0, S, S)).filter(ImageFilter.GaussianBlur(S * 0.03))
    region = Image.alpha_composite(region, Image.new("RGBA", region.size, (200, 210, 255, 34)))
    yy = np.linspace(0, 1, S, dtype=np.float32)
    sheen = (np.clip(1 - yy * 1.4, 0, 1) ** 2 * 40).astype(np.uint8)
    sl = Image.new("RGBA", region.size, (255, 255, 255, 0))
    sl.putalpha(Image.fromarray(np.repeat(sheen[:, None], region.size[0], 1), "L"))
    region = Image.alpha_composite(region, sl)

    # 유리 가장자리 그림자 → 유리 → 가장자리 하이라이트
    sh = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(sh).rectangle([gx - int(S * 0.02), 0, gx, S], fill=(0, 0, 0, 70))
    base.alpha_composite(sh.filter(ImageFilter.GaussianBlur(S * 0.01)))
    base.paste(region, (gx, 0))
    hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(hl).rectangle([gx, 0, gx + int(S * 0.005), S], fill=(255, 255, 255, 120))
    base.alpha_composite(hl)
    return base


def framed(art, half_frac, shadow):
    """squircle로 잘라 1024×1024 RGBA로"""
    half = S * half_frac
    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    if shadow:
        sh = squircle_mask(half, dy=S * 0.012).filter(ImageFilter.GaussianBlur(S * 0.012))
        canvas.paste((0, 0, 0, 255), (0, 0), sh.point(lambda v: int(v * 0.38)))
    canvas.paste(art, (0, 0), squircle_mask(half))
    return canvas.resize((1024, 1024), Image.LANCZOS)


def main():
    art = artwork()
    os.makedirs(os.path.join(ROOT, "assets", "icon"), exist_ok=True)
    os.makedirs(os.path.join(ROOT, "packaging", "windows"), exist_ok=True)

    # macOS: Apple 아이콘 그리드(1024 중 824가 본체) + 그림자
    mac = framed(art, 0.4023, shadow=True)
    mac.save(os.path.join(ROOT, "assets", "icon", "AppIcon-1024.png"))
    mac.save(os.path.join(ROOT, "packaging", "AppIcon.icns"))

    # Windows: 작은 크기에서 잘 보이게 여백을 줄이고 그림자 없이
    win = framed(art, 0.485, shadow=False)
    sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    win.save(os.path.join(ROOT, "packaging", "windows", "app.ico"), sizes=sizes)
    w64 = win.resize((64, 64), Image.LANCZOS).convert("RGBA")
    with open(os.path.join(ROOT, "assets", "icon", "window64.rgba"), "wb") as f:
        f.write(w64.tobytes())
    print("완료: AppIcon-1024.png, AppIcon.icns, app.ico, window64.rgba")


if __name__ == "__main__":
    main()
