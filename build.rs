fn main() {
    // Only run on Windows
    if cfg!(target_os = "windows") {
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

        // Embed application manifest for Windows 10/11 dark mode support
        let manifest_path = std::path::Path::new("src/xcreen.exe.manifest");
        if manifest_path.exists() {
            res.set_manifest_file(manifest_path.to_str().unwrap());
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
        res.set("LegalCopyright", "Copyright © 2025 Sansith Fernando");

        if let Err(e) = res.compile() {
            // Don't fail the build if resources can't be compiled; just warn
            println!("cargo:warning=Failed to set Windows resources: {}", e);
        }
    }
}
