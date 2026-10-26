fn use_llvm_rc(res: &mut winres::WindowsResource) {
    // winres only runs a file named rc.exe. Point it at whatever `RC` names.
    let Some(rc) = std::env::var_os("RC").map(std::path::PathBuf::from) else {
        return;
    };
    if !rc.is_file() {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out_dir = std::path::PathBuf::from(out_dir);
    let shim = out_dir.join("rc.exe");
    if std::fs::copy(&rc, &shim).is_err() {
        return;
    }
    if let Some(path) = out_dir.to_str() {
        res.set_toolkit_path(path);
    }
}

fn main() {
    // Only run on Windows
    if cfg!(target_os = "windows") {
        windows_reactor_setup::as_self_contained();

        let mut res = winres::WindowsResource::new();

        // Set icon if present (expects .ico file)
        let ico_path = std::path::Path::new("src/icons/icon.ico");
        if ico_path.exists() {
            res.set_icon(ico_path.to_str().unwrap());
        } else {
            println!(
                "cargo:warning=Windows .ico not found at {} - skipping icon embedding",
                ico_path.display()
            );
        }

        // Pass application manifest to the linker so it merges with the WinUI 3 manifest from windows-reactor-setup
        let manifest_path = std::path::Path::new("src/XCreen.exe.manifest");
        if manifest_path.exists() {
            println!(
                "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
                manifest_path.display()
            );
        } else {
            println!(
                "cargo:warning=Manifest not found at {} - dark mode may not work",
                manifest_path.display()
            );
        }

        // Set some basic metadata
        res.set("FileDescription", "XCreen - Adaptive Brightness Control");
        res.set("ProductName", "XCreen");
        res.set("CompanyName", "Sansith Fernando");
        // llvm-rc rejects the copyright sign unless /C 65001 is passed, which winres does not.
        res.set("LegalCopyright", "Copyright (c) 2025 Sansith Fernando");

        use_llvm_rc(&mut res);

        if let Err(e) = res.compile() {
            // Don't fail the build if resources can't be compiled; just warn
            println!("cargo:warning=Failed to set Windows resources: {}", e);
        }
    }
}
