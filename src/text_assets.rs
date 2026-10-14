// 자동 생성 파일 — tools/gen_text_assets.py 로 다시 만드세요. 직접 수정 금지.
// 문구 렌더링: Noto Sans CJK KR (SIL Open Font License 1.1)

use crate::cover::PackedMask;

/// 보호 화면
pub const TITLE: PackedMask = PackedMask { w: 254, h: 65, data: include_bytes!("../assets/text/title.a4") };
/// 즉시 가림 중입니다
pub const COVERED: PackedMask = PackedMask { w: 254, h: 35, data: include_bytes!("../assets/text/covered.a4") };
/// 단축키를 한 번 더 누르거나 메뉴에서 ‘가림 해제’를 선택하세요
pub const COVERED_HINT: PackedMask = PackedMask { w: 612, h: 28, data: include_bytes!("../assets/text/covered_hint.a4") };
/// 화면을 연결하고 있습니다
pub const CONNECTING: PackedMask = PackedMask { w: 342, h: 35, data: include_bytes!("../assets/text/connecting.a4") };
/// 화면을 다시 연결하고 있습니다
pub const RECONNECTING: PackedMask = PackedMask { w: 408, h: 35, data: include_bytes!("../assets/text/reconnecting.a4") };
/// 화면 기록 권한이 필요합니다
pub const PERMISSION: PackedMask = PackedMask { w: 379, h: 36, data: include_bytes!("../assets/text/permission.a4") };
/// 메뉴의 ‘화면 기록 권한 설정 열기’에서 허용한 뒤 앱을 다시 실행해 주세요
pub const PERMISSION_HINT: PackedMask = PackedMask { w: 717, h: 28, data: include_bytes!("../assets/text/permission_hint.a4") };
/// 블러 중
pub const BADGE_LIVE: PackedMask = PackedMask { w: 70, h: 26, data: include_bytes!("../assets/text/badge_live.a4") };
/// 동결
pub const BADGE_FROZEN: PackedMask = PackedMask { w: 44, h: 26, data: include_bytes!("../assets/text/badge_frozen.a4") };
