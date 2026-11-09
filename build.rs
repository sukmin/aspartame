//! Embeds the application icon and version information into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let icon = "assets/icon.ico";
    if !std::path::Path::new(icon).exists() {
        println!("cargo:warning=assets/icon.ico not found; building without an executable icon");
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon);
    res.set("ProductName", "Aspartame");
    res.set("FileDescription", "Aspartame Image Viewer");
    if let Err(err) = res.compile() {
        // Missing rc.exe (Windows SDK) should not break the build; only the icon is lost.
        println!("cargo:warning=failed to embed Windows resources: {err}");
    }
}
