//! A Mac app's icon as a PNG, for the library to show an RPG Maker game by
//! its player app's icon. Blocking; run it off the UI thread.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

/// Saves the icon of the app at `app` as a PNG at `to`.
///
/// # Errors
///
/// Fails if the app names no icon file, or `sips` cannot convert it.
pub fn save(app: &Path, to: &Path) -> io::Result<()> {
    // `plutil` reads both the XML and the binary form of Info.plist.
    let name = run(Command::new("plutil")
        .args(["-extract", "CFBundleIconFile", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist")))?;
    let mut icon = app.join("Contents/Resources").join(name.trim());
    // Info.plist may leave out the extension.
    if icon.extension().is_none() {
        icon.set_extension("icns");
    }
    if let Some(folder) = to.parent() {
        fs::create_dir_all(folder)?;
    }
    // `sips` exits as if it worked when it cannot read the icon, so what
    // it leaves at `to` says whether it did.
    fs::remove_file(to).ok();
    run(Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(&icon)
        .arg("--out")
        .arg(to))?;
    if !to.is_file() {
        return Err(io::Error::other(format!(
            "cannot convert {} to a PNG",
            icon.display()
        )));
    }
    Ok(())
}

/// Runs `command` to the end: what it printed, or if it failed, what it
/// said went wrong.
fn run(command: &mut Command) -> io::Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(said.trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ds-emulator-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_icon_named_in_info_plist_is_saved_as_a_png() {
        let dir = temp_dir("app-icon");
        let app = dir.join("Game.app");
        fs::create_dir_all(app.join("Contents/Resources")).expect("app folders");
        let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
            <plist version=\"1.0\"><dict>\
            <key>CFBundleIconFile</key><string>icon.png</string>\
            </dict></plist>\n";
        fs::write(app.join("Contents/Info.plist"), plist).expect("Info.plist");
        let mut icon = Vec::new();
        let mut encoder = png::Encoder::new(&mut icon, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&[0xFF; 16]))
            .expect("encoded");
        fs::write(app.join("Contents/Resources/icon.png"), &icon).expect("icon");
        let to = dir.join("icons/game.png");
        save(&app, &to).expect("saved");
        assert!(crate::boxart::is_png(&to));
        fs::remove_file(app.join("Contents/Resources/icon.png")).expect("removed");
        assert!(save(&app, &dir.join("missing.png")).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
