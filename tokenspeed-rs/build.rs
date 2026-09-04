// 嵌入应用图标与版本信息（Windows 资源编译器，来自 VS2022 + Windows SDK）
fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        winresource::WindowsResource::new()
            .set_icon("assets/app.ico")
            .set("FileDescription", "tokenspeed - AI coding speed monitor")
            .set("ProductName", "tokenspeed")
            .set("FileVersion", "0.5.7")
            .set("ProductVersion", "0.5.7")
            .compile()
            .expect("embed windows resources");
    }
}
