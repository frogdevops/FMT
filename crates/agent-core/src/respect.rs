#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineReason {
    AntiCheat(String),
}

/// Module-name markers for active anti-cheat / anti-tamper. If any loaded
/// module matches, we decline out of respect — we never try to bypass it.
const ANTICHEAT_MARKERS: &[&str] = &[
    "easyanticheat",
    "battleye",
    "beclient",
    "vanguard",
    "denuvo",
    "xigncode",
];

/// Returns `Some(reason)` if the process is protected and we must not engage.
pub fn should_decline(loaded_modules: &[String]) -> Option<DeclineReason> {
    for module in loaded_modules {
        let lower = module.to_lowercase();
        if ANTICHEAT_MARKERS.iter().any(|m| lower.contains(m)) {
            return Some(DeclineReason::AntiCheat(module.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_when_no_protection_present() {
        let modules = vec![
            "GameAssembly.dll".to_string(),
            "UnityPlayer.dll".to_string(),
        ];
        assert_eq!(should_decline(&modules), None);
    }

    #[test]
    fn declines_on_easyanticheat_case_insensitive() {
        let modules = vec!["EasyAntiCheat.dll".to_string()];
        assert_eq!(
            should_decline(&modules),
            Some(DeclineReason::AntiCheat("EasyAntiCheat.dll".to_string()))
        );
    }

    #[test]
    fn declines_on_battleye() {
        let modules = vec!["BEClient_x64.dll".to_string()];
        assert_eq!(
            should_decline(&modules),
            Some(DeclineReason::AntiCheat("BEClient_x64.dll".to_string()))
        );
    }
}
