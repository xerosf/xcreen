fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/icons/icon.ico");
    println!("cargo:rerun-if-changed=src/xcreen.exe.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    windows_reactor_setup::as_self_contained();
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:src/xcreen.exe.manifest");

    let mut resources = winres::WindowsResource::new();
    resources.set_icon("src/icons/icon.ico");
    resources.set("FileDescription", "XCreen - Adaptive Brightness Control");
    resources.set("ProductName", "XCreen");
    resources.set("CompanyName", "Sansith Fernando");
    resources.set("LegalCopyright", "Copyright © 2026 Sansith Fernando");
    resources.compile()?;
    Ok(())
}
