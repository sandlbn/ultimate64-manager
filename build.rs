fn main() {
    // Only compile Windows resources on Windows
    #[cfg(target_os = "windows")]
    {
        // Check if icon.ico exists before trying to use it
        if std::path::Path::new("app_icon.ico").exists() {
            let mut res = winres::WindowsResource::new();

            res.set_icon("app_icon.ico");

            res.set("ProductName", "Ultimate64 Manager");
            res.set(
                "FileDescription",
                "Ultimate64 Manager - Control your Ultimate64",
            );
            res.set("LegalCopyright", "Copyright 2026");

            // Compile the resource
            if let Err(e) = res.compile() {
                eprintln!("Warning: Failed to compile Windows resources: {}", e);
            }
        } else {
            println!("cargo:warning=app_icon.ico not found, skipping Windows icon embedding");
        }
    }
}
