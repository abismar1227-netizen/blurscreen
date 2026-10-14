// 메뉴 막대·트레이 아이콘을 코드로 그린다 (이미지 파일 없음).
// 모니터 모양 안에 상태를 표시: 실시간 = 흐린 글줄, 동결 = 일시정지, 가림 = 꽉 찬 화면.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconKind {
    Live,
    Frozen,
    Covered,
}

/// 단위 좌표(0..1)의 점이 도형 안에 있는지
fn inside(kind: IconKind, x: f32, y: f32) -> bool {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| x >= x0 && x < x1 && y >= y0 && y < y1;
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        if !rect(x0, y0, x1, y1) {
            return false;
        }
        let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
        // clamp 대신 max/min — 반경이 변의 절반과 같을 때 부동소수 오차로 패닉하지 않게
        let cx = x.max(x0 + r).min(x1 - r);
        let cy = y.max(y0 + r).min(y1 - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };

    // 모니터 받침
    if rect(0.42, 0.72, 0.58, 0.84) || rrect(0.26, 0.84, 0.74, 0.93, 0.03) {
        return true;
    }
    let outer = rrect(0.06, 0.10, 0.94, 0.74, 0.09);
    if !outer {
        return false;
    }
    let inner = rrect(0.155, 0.195, 0.845, 0.645, 0.03);
    if !inner {
        return true; // 테두리
    }
    match kind {
        IconKind::Covered => true,
        IconKind::Frozen => rect(0.36, 0.29, 0.45, 0.56) || rect(0.55, 0.29, 0.64, 0.56),
        IconKind::Live => {
            rrect(0.24, 0.27, 0.76, 0.34, 0.035)
                || rrect(0.24, 0.39, 0.66, 0.46, 0.035)
                || rrect(0.24, 0.51, 0.71, 0.58, 0.035)
        }
    }
}

/// size×size RGBA 아이콘. template=true면 macOS 메뉴 막대용(검정+알파, 시스템이 색을 입힘).
pub fn render(kind: IconKind, size: u32, template: bool) -> Vec<u8> {
    let (r, g, b) = if template { (0u8, 0u8, 0u8) } else { (0x3b, 0x82, 0xf6) };
    let n = size as usize;
    let ss = 4; // 4×4 초표본 → 가장자리 부드럽게
    let mut out = vec![0u8; n * n * 4];
    for py in 0..n {
        for px in 0..n {
            let mut hit = 0;
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / n as f32;
                    if inside(kind, x, y) {
                        hit += 1;
                    }
                }
            }
            let a = (hit * 255 / (ss * ss)) as u8;
            let i = (py * n + px) * 4;
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = a;
        }
    }
    out
}
