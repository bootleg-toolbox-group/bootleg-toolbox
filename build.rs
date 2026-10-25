//! Windows: puts the icon (and the version details) into the .exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Bootleg Toolbox");
        res.set("FileDescription", "Bootleg Toolbox");
        res.set("LegalCopyright", "Rattlehead80. Free software, GNU GPL v3 or later.");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no icon in the .exe: {e}");
        }
    }
}
