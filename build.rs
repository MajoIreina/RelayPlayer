fn main() {
    println!("cargo:rerun-if-changed=assets/relay.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Relay · PS5 播放器")
            .set("FileDescription", "Relay · PS5 播放器")
            .set("OriginalFilename", "RelayPlayer.exe")
            .set("InternalName", "RelayPlayer");
        if std::path::Path::new("assets/relay.ico").exists() {
            res.set_icon("assets/relay.ico");
        }
        res.compile().expect("compile Windows resources");
    }
}
