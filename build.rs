fn main() {
    windows_reactor_setup::as_framework_dependent();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("src/assets/launcher.ico");
        res.compile().expect("failed to compile windows icon resource");
    }
}
