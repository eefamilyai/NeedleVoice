//! Shared version resources for every NeedleVoice executable.
//!
//! Each binary used to set only `ProductName` and `FileDescription`, leaving
//! `CompanyName`, `LegalCopyright` and `OriginalFilename` blank. A Windows
//! executable with no publisher identity is one of the things a reputation
//! check has nothing to go on for, so all four are filled in here, from one
//! place, for all of them.

use std::path::Path;

/// `OriginalFilename` is the name the linker will have given the binary, which
/// has to match what the installer actually writes to disk.
pub fn describe(res: &mut winresource::WindowsResource, original_filename: &str, description: &str) {
    res.set("CompanyName", "NeedleVoice");
    res.set("ProductName", "NeedleVoice");
    res.set("FileDescription", description);
    res.set("OriginalFilename", original_filename);
    res.set("LegalCopyright", "Copyright (C) 2026 NeedleVoice");
    // "A name is required for the value" — these keep Add/Remove Programs and
    // the file properties sheet from showing blanks.
    res.set("InternalName", original_filename.trim_end_matches(".exe"));
    res.set("FileVersion", env!("CARGO_PKG_VERSION"));
    res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
}

/// The application manifest every binary embeds.
///
/// * `asInvoker`: nothing here needs elevation, and asking for it would make
///   the installer prompt for a password it has no use for.
/// * `PerMonitorV2`: correct rendering on a mixed-DPI desktop.
/// * A `<compatibility>` list naming the supported Windows versions, because an
///   executable that declares none is treated as pre-Windows-8 and is run in a
///   compatibility shim.
pub fn manifest() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="NeedleVoice.Application" version="1.0.0.0" processorArchitecture="*"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <!-- Windows 8.1 -->
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#
}

/// Write the manifest where `winresource` will pick it up, and return the path.
pub fn write_manifest(out_dir: &Path) -> std::path::PathBuf {
    let path = out_dir.join("needlevoice.manifest");
    std::fs::write(&path, manifest()).expect("writing the application manifest");
    path
}

/// Apply the icon, the version resources and the manifest to this binary.
///
/// The paths handed to `winresource` are relative, and this build script is run
/// from the crate's `OUT_DIR` to make them so. `rc.exe` takes its paths inside a
/// double-quoted string in a generated `.rc` file, and the absolute path to a
/// checkout can contain a quote or a backslash — `D:\Jaryl's Stuff\…` produced
/// `file not found: D:\Jaryl\'s Stuff\…` and failed every build. Relative names
/// with no separators sidestep the quoting entirely.
pub fn apply(out_dir: &Path, icon: &Path, original_filename: &str, description: &str) {
    let manifest = write_manifest(out_dir);
    let icon = icon.canonicalize().unwrap_or_else(|_| icon.to_path_buf());
    let started_in = std::env::current_dir().expect("current directory");
    std::env::set_current_dir(out_dir).expect("entering OUT_DIR");

    let mut res = winresource::WindowsResource::new();
    if icon.exists() {
        let icon = copy_beside(&icon, out_dir);
        res.set_icon_with_id(&icon, "1");
    }
    let manifest_name = manifest.file_name().expect("manifest name").to_string_lossy().to_string();
    res.set_manifest_file(&manifest_name);
    describe(&mut res, original_filename, description);
    let result = res.compile();

    let _ = std::env::set_current_dir(started_in);
    result.expect("embedding Windows resources");
}

/// Put a copy of `file` next to the manifest so a bare file name can be used.
fn copy_beside(file: &Path, out_dir: &Path) -> String {
    let Some(name) = file.file_name() else { return file.to_string_lossy().to_string() };
    let target = out_dir.join(name);
    if std::fs::copy(file, &target).is_ok() {
        name.to_string_lossy().to_string()
    } else {
        file.to_string_lossy().to_string()
    }
}
