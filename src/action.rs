// 키보드·메뉴·전역 단축키가 공통으로 쓰는 조작 목록

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleCover,
    ToggleFreeze,
    Strength(usize),
    Fps(usize),
    CycleFps,
    Monitor(usize),
    CycleMonitor,
    ToggleBadge,
    OpenPermission,
    OpenLogs,
    Quit,
}

impl Action {
    /// 메뉴 항목 ID → 조작
    #[allow(dead_code)] // 메뉴가 없는 플랫폼에서는 쓰이지 않음
    pub fn from_id(id: &str) -> Option<Action> {
        let num = |p: &str| id.strip_prefix(p).and_then(|n| n.parse::<usize>().ok());
        Some(match id {
            "cover" => Action::ToggleCover,
            "freeze" => Action::ToggleFreeze,
            "badge" => Action::ToggleBadge,
            "perm" => Action::OpenPermission,
            "logs" => Action::OpenLogs,
            "quit" => Action::Quit,
            _ => {
                if let Some(n) = num("strength-") {
                    Action::Strength(n)
                } else if let Some(n) = num("fps-") {
                    Action::Fps(n)
                } else if let Some(n) = num("monitor-") {
                    Action::Monitor(n)
                } else {
                    return None;
                }
            }
        })
    }
}
