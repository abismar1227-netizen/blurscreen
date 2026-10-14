// 보호 화면과 상태 배지 그리기.
// 한글 문구는 미리 그려 둔 4비트 알파 마스크(text_assets.rs)를 확대·축소해 찍는다.
// 폰트 엔진을 싣지 않으므로 가볍고, 두 운영체제에서 똑같이 보인다.

use crate::blur::BG;
use crate::text_assets as t;

/// 4비트 알파 마스크 (두 픽셀이 한 바이트, 상위 니블이 왼쪽 픽셀)
pub struct PackedMask {
    pub w: usize,
    pub h: usize,
    pub data: &'static [u8],
}

impl PackedMask {
    #[inline]
    pub fn alpha(&self, x: usize, y: usize) -> u32 {
        let i = y * self.w + x;
        let b = self.data.get(i / 2).copied().unwrap_or(0);
        let nib = if i % 2 == 0 { b >> 4 } else { b & 0x0f };
        nib as u32 * 17
    }
}

/// 보호 화면의 종류
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoverKind {
    /// 사용자가 즉시 가림을 켬
    Covered,
    /// 첫 화면을 기다리는 중
    Connecting,
    /// 오류 후 재연결 중 (아직 보여줄 블러 화면이 없음)
    Reconnecting,
    /// macOS 화면 기록 권한 없음
    Permission,
}

const TITLE_COLOR: u32 = 0x00F0_F0F2;
const LINE_COLOR: u32 = 0x00C8_C8CE;
const HINT_COLOR: u32 = 0x008E_8E98;

/// 창 크기에 맞춘 문구 배율. 마스크는 가로 1920px 창 기준으로 그려져 있다.
fn text_scale(bw: usize, bh: usize, widest: usize) -> f32 {
    let base = f32::min(bw as f32 / 1920.0, bh as f32 / 1080.0) * 1.25;
    let fit = (bw as f32 * 0.9) / widest.max(1) as f32;
    base.min(fit).clamp(0.3, 1.6)
}

/// 보호 화면을 창 전체에 그린다
pub fn draw_cover(out: &mut [u32], bw: usize, bh: usize, kind: CoverKind) {
    out.fill(BG);
    if bw == 0 || bh == 0 {
        return;
    }
    let (line, hint): (&PackedMask, Option<&PackedMask>) = match kind {
        CoverKind::Covered => (&t::COVERED, Some(&t::COVERED_HINT)),
        CoverKind::Connecting => (&t::CONNECTING, None),
        CoverKind::Reconnecting => (&t::RECONNECTING, None),
        CoverKind::Permission => (&t::PERMISSION, Some(&t::PERMISSION_HINT)),
    };
    let widest = [t::TITLE.w, line.w, hint.map_or(0, |h| h.w)].into_iter().max().unwrap_or(1);
    let s = text_scale(bw, bh, widest);

    let gap1 = (22.0 * s) as usize;
    let gap2 = (14.0 * s) as usize;
    let sh = |m: &PackedMask| ((m.h as f32 * s).round() as usize).max(1);
    let total = sh(&t::TITLE) + gap1 + sh(line) + hint.map_or(0, |h| gap2 + sh(h));
    let mut top = bh.saturating_sub(total) / 2;

    blit(out, bw, bh, &t::TITLE, bw / 2, top, s, TITLE_COLOR, 255);
    top += sh(&t::TITLE) + gap1;
    blit(out, bw, bh, line, bw / 2, top, s, LINE_COLOR, 255);
    if let Some(h) = hint {
        top += sh(line) + gap2;
        blit(out, bw, bh, h, bw / 2, top, s, HINT_COLOR, 255);
    }
}

/// 왼쪽 아래 구석에 작은 상태 배지를 그린다 (블러 화면 위에 덧그림)
pub fn draw_badge(out: &mut [u32], bw: usize, bh: usize, frozen: bool) {
    let m = if frozen { &t::BADGE_FROZEN } else { &t::BADGE_LIVE };
    let s = (f32::min(bw as f32 / 1920.0, bh as f32 / 1080.0) * 1.25).clamp(0.5, 1.6);
    let tw = ((m.w as f32 * s).round() as usize).max(1);
    let th = ((m.h as f32 * s).round() as usize).max(1);
    let pad_x = (14.0 * s) as usize;
    let pad_y = (7.0 * s) as usize;
    let margin = (18.0 * s) as usize;
    let box_w = tw + pad_x * 2;
    let box_h = th + pad_y * 2;
    if box_w + margin > bw || box_h + margin > bh {
        return;
    }
    let x0 = margin;
    let y0 = bh - margin - box_h;
    // 반투명 검정 상자
    for y in y0..y0 + box_h {
        for p in &mut out[y * bw + x0..y * bw + x0 + box_w] {
            *p = blend(*p, 0, 200);
        }
    }
    blit(out, bw, bh, m, x0 + box_w / 2, y0 + pad_y, s, TITLE_COLOR, 255);
}

#[inline]
fn blend(dst: u32, src: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let mix = |sh: u32| -> u32 {
        let d = (dst >> sh) & 0xff;
        let c = (src >> sh) & 0xff;
        ((c * a + d * inv + 127) / 255) << sh
    };
    mix(16) | mix(8) | mix(0)
}

/// 마스크를 배율 s로 찍는다. (cx: 가운데 x, top: 위쪽 y)
/// 축소는 영역 평균, 확대는 bilinear — 어느 배율에서도 글자가 깨지지 않게.
#[allow(clippy::too_many_arguments)]
pub fn blit(out: &mut [u32], bw: usize, bh: usize, m: &PackedMask, cx: usize, top: usize, s: f32, color: u32, opacity: u32) {
    if m.w == 0 || m.h == 0 || s <= 0.0 {
        return;
    }
    let dw = ((m.w as f32 * s).round() as usize).max(1);
    let dh = ((m.h as f32 * s).round() as usize).max(1);
    let x0 = cx as isize - (dw / 2) as isize;
    let y0 = top as isize;
    let inv = 1.0 / s;
    for dy in 0..dh {
        let y = y0 + dy as isize;
        if y < 0 || y >= bh as isize {
            continue;
        }
        for dx in 0..dw {
            let x = x0 + dx as isize;
            if x < 0 || x >= bw as isize {
                continue;
            }
            let a = if s < 1.0 {
                // 축소: 원본의 [sx0, sx1) × [sy0, sy1) 영역 평균
                let sx0 = (dx as f32 * inv) as usize;
                let sy0 = (dy as f32 * inv) as usize;
                let sx1 = (((dx + 1) as f32 * inv).ceil() as usize).clamp(sx0 + 1, m.w);
                let sy1 = (((dy + 1) as f32 * inv).ceil() as usize).clamp(sy0 + 1, m.h);
                let mut sum = 0u32;
                let mut n = 0u32;
                for sy in sy0.min(m.h - 1)..sy1 {
                    for sx in sx0.min(m.w - 1)..sx1 {
                        sum += m.alpha(sx, sy);
                        n += 1;
                    }
                }
                sum / n.max(1)
            } else {
                // 확대: bilinear
                let fx = ((dx as f32 + 0.5) * inv - 0.5).max(0.0);
                let fy = ((dy as f32 + 0.5) * inv - 0.5).max(0.0);
                let ix = (fx as usize).min(m.w - 1);
                let iy = (fy as usize).min(m.h - 1);
                let jx = (ix + 1).min(m.w - 1);
                let jy = (iy + 1).min(m.h - 1);
                let tx = fx - ix as f32;
                let ty = fy - iy as f32;
                let top_ = m.alpha(ix, iy) as f32 * (1.0 - tx) + m.alpha(jx, iy) as f32 * tx;
                let bot = m.alpha(ix, jy) as f32 * (1.0 - tx) + m.alpha(jx, jy) as f32 * tx;
                (top_ * (1.0 - ty) + bot * ty) as u32
            };
            if a == 0 {
                continue;
            }
            let i = y as usize * bw + x as usize;
            out[i] = blend(out[i], color, a * opacity / 255);
        }
    }
}
