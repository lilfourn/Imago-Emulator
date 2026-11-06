//! Gives the Windows executable its icon and version information.
//!
//! GPUI shows the exe's icon resource 1 on its windows and the taskbar,
//! and Explorer shows the icon, name and version on the file and in its
//! properties. The resources are compiled from `assets/app-icon.ico` (made
//! by `packaging/icons.sh`) and the crate's version. GPUI embeds its own
//! manifest as manifest resource 1; icons and version information are
//! other resource types, so they don't clash.
//!
//! Only a Windows build machine has the resource compiler, as for GPUI's
//! manifest and shaders, so a cross-build from a Mac or Linux gets no icon.
//! Releases are built on Windows. Nothing happens for other systems.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows::embed_resources();
    }
}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    /// The name shown in Explorer, the taskbar and the installer.
    const NAME: &str = "Imago Emulator";
    /// The same notice as the Mac's `NSHumanReadableCopyright`.
    const COPYRIGHT: &str = "© 2026 Luke Fournier · imagoemulator.com";

    /// Writes the resource script and has it compiled into the app's
    /// executable alone (not its tests).
    pub fn embed_resources() {
        let var = |name: &str| env::var(name).unwrap_or_else(|_| panic!("cargo sets {name}"));
        let version = var("CARGO_PKG_VERSION");
        // Four numbers, as Windows keeps them; a pre-release counts as its
        // release.
        let numbers = format!(
            "{},{},{},0",
            var("CARGO_PKG_VERSION_MAJOR"),
            var("CARGO_PKG_VERSION_MINOR"),
            var("CARGO_PKG_VERSION_PATCH"),
        );
        // The resource compiler takes forward slashes, and needs no
        // escaping for them.
        let icon = PathBuf::from(var("CARGO_MANIFEST_DIR"))
            .join("../../assets/app-icon.ico")
            .display()
            .to_string()
            .replace('\\', "/");
        let script = format!(
            r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "FileDescription", "{NAME}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "ds-emulator"
            VALUE "LegalCopyright", "{COPYRIGHT}"
            VALUE "OriginalFilename", "ds-emulator.exe"
            VALUE "ProductName", "{NAME}"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
        );
        let rc = PathBuf::from(var("OUT_DIR")).join("ds-emulator.rc");
        fs::write(&rc, script).expect("write the resource script");
        embed_resource::compile_for(&rc, ["ds-emulator"], embed_resource::NONE)
            .manifest_required()
            .expect("compile the Windows resources (rc.exe, from the Windows SDK)");
    }
}
