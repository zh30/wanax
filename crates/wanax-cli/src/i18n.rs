use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" | "english" => Some(Self::En),
            "zh" | "zh-cn" | "zh-hans" | "cn" => Some(Self::Zh),
            _ => None,
        }
    }

}

thread_local! {
    static LANG: Cell<Lang> = const { Cell::new(Lang::En) };
}

pub fn set_lang(lang: Lang) {
    LANG.with(|c| c.set(lang));
}

pub fn current_lang() -> Lang {
    LANG.with(Cell::get)
}

pub fn resolve_lang(flag: Option<&str>) -> Lang {
    if let Some(s) = flag {
        if let Some(l) = Lang::parse(s) {
            return l;
        }
    }
    std::env::var("WANAX_LANG")
        .ok()
        .and_then(|s| Lang::parse(&s))
        .unwrap_or(Lang::En)
}

pub fn t(key: &str) -> &'static str {
    match (current_lang(), key) {
        (Lang::Zh, "no_runs") => "没有运行记录。",
        (Lang::Zh, "no_verdict") => "没有裁决。",
        (Lang::Zh, "status_header") => "运行                                 状态                 单元     美元       回合   最近事件",
        (Lang::Zh, "initialized") => "已初始化 .wanax/ 与 specs/example.contract.md",
        (Lang::Zh, "init_next") => "下一步：git add .wanax/config.toml specs/",
        (Lang::Zh, "git_ok") => "git: 正常",
        (Lang::Zh, "git_missing") => "git: 缺失",
        (Lang::Zh, "key_present") => "已设置",
        (Lang::Zh, "key_missing") => "未设置",
        (Lang::Zh, "lock_none") => "锁: 无",
        (Lang::Zh, "disk_writable") => "磁盘: 可写",
        (Lang::Zh, "disk_not_writable") => "磁盘: 不可写",
        (Lang::Zh, "contracts_ok") => "契约: 绑定测试不在 allowed_globs 内",
        (Lang::Zh, "adapter_ok") => "正常",
        (Lang::Zh, "adapter_missing") => "缺失",
        (Lang::Zh, "plugin_ok") => "插件 agent-spec: 正常",
        (Lang::Zh, "plugin_missing") => "插件 agent-spec: 缺失",
        (Lang::Zh, "plugin_off") => "插件 agent-spec: 未启用",
        (Lang::En, "no_runs") => "No runs.",
        (Lang::En, "no_verdict") => "No verdict.",
        (Lang::En, "status_header") => {
            "Run                              State              Unit     USD        Turns  LastEvent"
        }
        (Lang::En, "initialized") => "initialized .wanax/ and specs/example.contract.md",
        (Lang::En, "init_next") => "Next: git add .wanax/config.toml specs/",
        (Lang::En, "git_ok") => "git: ok",
        (Lang::En, "git_missing") => "git: missing",
        (Lang::En, "key_present") => "present",
        (Lang::En, "key_missing") => "missing",
        (Lang::En, "lock_none") => "lock: none",
        (Lang::En, "disk_writable") => "disk: writable",
        (Lang::En, "disk_not_writable") => "disk: not writable",
        (Lang::En, "contracts_ok") => "contracts: binding tests outside allowed_globs",
        (Lang::En, "adapter_ok") => "ok",
        (Lang::En, "adapter_missing") => "missing",
        (Lang::En, "plugin_ok") => "plugin agent-spec: ok",
        (Lang::En, "plugin_missing") => "plugin agent-spec: missing",
        (Lang::En, "plugin_off") => "plugin agent-spec: off",
        (_, _) => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zh_no_runs() {
        set_lang(Lang::Zh);
        assert_eq!(t("no_runs"), "没有运行记录。");
        set_lang(Lang::En);
        assert_eq!(t("no_runs"), "No runs.");
    }
}
