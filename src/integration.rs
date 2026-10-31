//! "Open in Bootleg Toolbox" in the file manager's right-click menu, for folders and audio
//! files, for the current user only (no administrator rights): the registry under HKCU on
//! Windows; Dolphin, Nautilus and Nemo on Linux. `install` also refreshes the entries when
//! the program has moved; `remove` takes every one of them away.

use std::path::Path;

const LABEL: &str = "Open in Bootleg Toolbox";
/// Audio files the entry is offered for, besides folders.
const AUDIO_EXT: [&str; 14] = ["flac", "wav", "ape", "shn", "wv", "mp3", "mp2", "m4a", "mka", "aif", "aiff", "aifc", "ogg", "oga"];

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;

    const KEY: &str = "BootlegToolbox";
    /// No console window flashing up for reg.exe.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// The program's former name (Bootleg Helper, up to version 0.1.3) named its keys so.
    const LEGACY_KEY: &str = "BootlegHelper";

    fn keys_named(key: &str) -> Vec<(String, &'static str)> {
        let mut keys = vec![
            (format!(r"HKEY_CURRENT_USER\Software\Classes\Directory\shell\{key}"), "%1"),
            (format!(r"HKEY_CURRENT_USER\Software\Classes\Directory\Background\shell\{key}"), "%V"),
        ];
        for ext in AUDIO_EXT {
            keys.push((format!(r"HKEY_CURRENT_USER\Software\Classes\SystemFileAssociations\.{ext}\shell\{key}"), "%1"));
        }
        keys
    }

    fn keys() -> Vec<(String, &'static str)> {
        keys_named(KEY)
    }

    /// All the keys in one .reg file, imported by a single reg.exe call.
    fn import(lines: &str) -> Result<(), String> {
        let text = format!("Windows Registry Editor Version 5.00\r\n\r\n{lines}");
        let mut data = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            data.extend_from_slice(&u.to_le_bytes());
        }
        let file = std::env::temp_dir().join(format!("bootleg-toolbox-{}.reg", std::process::id()));
        std::fs::write(&file, data).map_err(|e| e.to_string())?;
        let status = std::process::Command::new("reg")
            .arg("import")
            .arg(&file)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(&file);
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => Err("reg.exe could not update the registry".into()),
            Err(e) => Err(format!("cannot run reg.exe: {e}")),
        }
    }

    /// A string value as .reg files write it.
    fn reg_string(text: &str) -> String {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }

    pub fn install(exe: &Path) -> Result<String, String> {
        let exe = exe.display().to_string();
        let mut lines = String::new();
        for (key, arg) in keys() {
            lines.push_str(&format!("[{key}]\r\n@={}\r\n\"Icon\"={}\r\n\r\n", reg_string(LABEL), reg_string(&exe)));
            lines.push_str(&format!("[{key}\\command]\r\n@={}\r\n\r\n", reg_string(&format!("\"{exe}\" \"{arg}\""))));
        }
        import(&lines)?;
        Ok(format!("\"{LABEL}\" added to the right-click menu of folders and audio files (on Windows 11: under Show more options)"))
    }

    pub fn remove() -> Result<String, String> {
        let lines: String = keys().into_iter().map(|(key, _)| format!("[-{key}]\r\n\r\n")).collect();
        import(&lines)?;
        Ok(format!("\"{LABEL}\" removed from the right-click menu"))
    }

    pub fn remove_legacy() -> bool {
        let keys = keys_named(LEGACY_KEY);
        let present = std::process::Command::new("reg")
            .arg("query")
            .arg(&keys[0].0)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if present {
            let lines: String = keys.into_iter().map(|(key, _)| format!("[-{key}]\r\n\r\n")).collect();
            let _ = import(&lines);
        }
        present
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::path::PathBuf;

    fn data_home() -> Result<PathBuf, String> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| "no home folder".to_string())
    }

    fn on_path(program: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
    }

    /// (file manager, file written, whether that file manager is installed)
    fn files() -> Result<Vec<(&'static str, PathBuf, bool)>, String> {
        let data = data_home()?;
        Ok(vec![
            ("Dolphin", data.join("kio/servicemenus/bootleg-toolbox.desktop"), on_path("dolphin")),
            ("Dolphin (KDE 5)", data.join("kservices5/ServiceMenus/bootleg-toolbox.desktop"), on_path("dolphin") && on_path("kbuildsycoca5")),
            ("Nautilus", data.join(format!("nautilus/scripts/{LABEL}")), on_path("nautilus")),
            ("Nemo", data.join("nemo/actions/bootleg-toolbox.nemo_action"), on_path("nemo")),
        ])
    }

    /// A path as a Desktop Entry Exec argument: quoted, with the characters the
    /// specification asks to escape, then the backslashes of the string value doubled.
    fn exec_arg(exe: &Path) -> String {
        let mut quoted = String::from("\"");
        for c in exe.to_string_lossy().chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            if c == '%' {
                quoted.push('%');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted.replace('\\', "\\\\")
    }

    fn content(manager: &str, exe: &Path) -> String {
        let mimes = "inode/directory;audio/flac;audio/x-flac;audio/x-wav;audio/wav;audio/x-ape;audio/x-wavpack;audio/x-shorten;audio/mpeg;";
        match manager {
            "Nautilus" => format!(
                "#!/bin/sh\n# Bootleg Toolbox: opens the selected folders and files.\nIFS='\n'\nexec '{}' $NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\n",
                exe.display().to_string().replace('\'', "'\\''")
            ),
            "Nemo" => format!(
                "[Nemo Action]\nName={LABEL}\nComment=Check, convert and fix lossless concert recordings\nExec={} %F\nIcon-Name=bootleg-toolbox\nSelection=any\nExtensions=dir;{};\n",
                exec_arg(exe),
                AUDIO_EXT.join(";")
            ),
            _ => format!(
                "[Desktop Entry]\nType=Service\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nMimeType={mimes}\nActions=open\nX-KDE-Priority=TopLevel\n\n\
                 [Desktop Action open]\nName={LABEL}\nIcon=bootleg-toolbox\nExec={} %F\n",
                exec_arg(exe)
            ),
        }
    }

    pub fn install(exe: &Path) -> Result<String, String> {
        use std::os::unix::fs::PermissionsExt;
        let mut done = Vec::new();
        for (manager, path, installed) in files()? {
            if !installed {
                continue;
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, content(manager, exe)).map_err(|e| format!("{}: {e}", path.display()))?;
            // Dolphin (KDE 6) and Nautilus only run executable entries.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            if !done.contains(&manager.trim_end_matches(" (KDE 5)")) {
                done.push(manager.trim_end_matches(" (KDE 5)"));
            }
        }
        // The icon the entries name, as "Add to the applications menu" installs it.
        let icon_dir = data_home()?.join("icons/hicolor/256x256/apps");
        std::fs::create_dir_all(&icon_dir).map_err(|e| e.to_string())?;
        std::fs::write(icon_dir.join("bootleg-toolbox.png"), include_bytes!("../assets/icon.png")).map_err(|e| e.to_string())?;
        if done.is_empty() {
            return Err("no supported file manager found (Dolphin, Nautilus or Nemo)".into());
        }
        Ok(format!("\"{LABEL}\" added to the right-click menu of {}", done.join(", ")))
    }

    pub fn remove() -> Result<String, String> {
        for (_, path, _) in files()? {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        Ok(format!("\"{LABEL}\" removed from the right-click menu"))
    }

    /// The files of the program's former name (Bootleg Helper, up to version 0.1.3).
    pub fn remove_legacy() -> bool {
        let Ok(data) = data_home() else { return false };
        let mut found = false;
        for file in [
            "kio/servicemenus/bootleg-helper.desktop",
            "kservices5/ServiceMenus/bootleg-helper.desktop",
            "nautilus/scripts/Open in Bootleg Helper",
            "nemo/actions/bootleg-helper.nemo_action",
            "applications/bootleg-helper.desktop",
            "icons/hicolor/256x256/apps/bootleg-helper.png",
        ] {
            let path = data.join(file);
            if path.is_file() && std::fs::remove_file(&path).is_ok() {
                found = true;
            }
        }
        found
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::*;
    pub fn install(_: &Path) -> Result<String, String> {
        Err("not available on this system".into())
    }
    pub fn remove() -> Result<String, String> {
        Ok(String::new())
    }
    pub fn remove_legacy() -> bool {
        false
    }
}

/// Adds (or refreshes) the right-click entry for this program file where it is now.
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    platform::install(&exe)
}

pub fn remove() -> Result<String, String> {
    platform::remove()
}

/// Removes the right-click entries, menu entry and icon left by the program under its former
/// name (Bootleg Helper); true if there were any.
pub fn remove_legacy() -> bool {
    platform::remove_legacy()
}
