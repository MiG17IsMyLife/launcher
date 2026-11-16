use winresource::VersionInfo;

fn main() {
    windows_reactor_setup::as_framework_dependent();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 资源文件版本取 src/main.rs 的 APP_VERSION（界面显示同一常量），避免两处不同步
        println!("cargo:rerun-if-changed=src/main.rs");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("src/assets/launcher.ico");
        let version = app_version();
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        // 资源管理器「详细信息」的「文件版本 / 产品版本」读 VERSIONINFO 的**数值**字段，
        // 而 winresource 默认用 CARGO_PKG_VERSION（0.1.0）填它，set() 只写字符串表
        // （.NET / PowerShell 读的那一份）。两边都要对齐，否则会出现
        // 「属性里 0.1.0.0、代码里 1.0.0.1」的不一致。
        match packed_version(&version) {
            Some(packed) => {
                res.set_version_info(VersionInfo::FILEVERSION, packed);
                res.set_version_info(VersionInfo::PRODUCTVERSION, packed);
            }
            None => println!(
                "cargo::warning=APP_VERSION `{version}` 不是四段数字，VERSIONINFO 数值字段未设置"
            ),
        }
        res.compile()
            .expect("failed to compile windows icon resource");
    }
}

/// `MAJOR.MINOR.PATCH.RELEASE` → VERSIONINFO 要求的 u64（每段 16 位）。
fn packed_version(ver: &str) -> Option<u64> {
    let parts: Vec<u64> = ver
        .split('.')
        .map(|s| s.trim().parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    match parts[..] {
        [major, minor, patch, release] => {
            Some((major << 48) | (minor << 32) | (patch << 16) | release)
        }
        _ => None,
    }
}

/// 从 `src/main.rs` 取出 `pub const APP_VERSION: &str = "…"` 的字面值。
fn app_version() -> String {
    std::fs::read_to_string("src/main.rs")
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            let rest = line.split_once("const APP_VERSION")?.1;
            let lit = rest.split_once('"')?.1;
            Some(lit.split('"').next()?.trim().to_string())
        })
        .unwrap_or_else(|| "0.0.0.0".to_string())
}
