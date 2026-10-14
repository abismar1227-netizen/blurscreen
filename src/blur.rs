// 이미지 처리: 축소(비가역) → 블러 → 창 크기로 확대 표시.
// 플랫폼과 무관한 순수 연산만 담아 두어, 리눅스에서도 단위 테스트가 가능하다.

/// 블러 처리가 끝난 작은 RGB 이미지
pub struct Tiny {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>, // w*h*3
}

/// (블러 후 이미지의 목표 가로 픽셀, 블러 반경 기준값, 라벨)
/// 폭이 작을수록 정보가 더 많이 소멸함 (원본 4K 기준 192px = 1/20 수준).
/// "강"은 v6에서 BlurScreen Plus의 "강"(640px 기준 반경 32)과 비슷한 흐림이 되도록 강화.
pub const STRENGTHS: [(usize, f32, &str); 3] =
    [(480, 1.3, "약"), (320, 1.7, "중"), (192, 10.0, "강")];

/// 원시 프레임 한 장의 정보 (플랫폼별 픽셀 포맷 차이를 흡수)
pub struct RawFrame<'a> {
    pub w: usize,
    pub h: usize,
    pub data: &'a [u8],
    /// 픽셀당 바이트 수 (3 또는 4)
    pub px: usize,
    /// [R, G, B] 채널의 바이트 위치
    pub rgb: [usize; 3],
}

/// 프레임 처리 결과. 내용이 직전과 같으면 tiny가 None (게시·그리기 생략).
pub struct Processed {
    pub hash: u64,
    pub tiny: Option<Tiny>,
}

/// 축소(박스 평균, 비가역) 후 블러를 적용한 작은 이미지를 만든다.
/// 축소 결과의 해시가 직전 프레임과 같으면 블러·게시를 통째로 건너뛴다.
pub fn process_frame(frame: &RawFrame, target_w: usize, sigma: f32, prev_hash: u64) -> Option<Processed> {
    let (w, h, data, px) = (frame.w, frame.h, frame.data, frame.px);
    let [ri, gi, bi] = frame.rgb;
    if w == 0 || h == 0 || data.len() < w * h * px {
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
    // 강도(폭·반경)도 해시에 섞어, 강도가 바뀌면 같은 화면이라도 다시 처리되게 한다
    hash = (hash ^ ((tw as u64) << 32 | sigma.to_bits() as u64)).wrapping_mul(0x0000_0100_0000_01B3);

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
pub fn box_blur(rgb: &mut [u8], w: usize, h: usize, sigma: f32) {
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

/// 창 배경(레터박스·보호 화면)의 기본 색
pub const BG: u32 = 0x0014_1418;

/// 작은 블러 이미지를 창 크기로 확대해 그린다. 여백은 어두운 회색.
///
/// 비용이 창 크기에 비례해 커지지 않도록, 비싼 보간(bilinear)은 폭 1024px까지만
/// 수행하고 그 이상은 값 복제(nearest)로 채운다. 블러 화면 특성상 시각적 차이가 없고,
/// 레티나 풀스크린 창에서도 보간 연산량이 일정하게 유지된다.
pub fn draw_tiny(tiny: &Tiny, out: &mut [u32], bw: usize, bh: usize) {
    if tiny.w < 1 || tiny.h < 1 || bw == 0 || bh == 0 {
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
