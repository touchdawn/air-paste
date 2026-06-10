//! Cross-platform "start at login" toggle for the tray, with no extra dependencies:
//! - macOS: a per-user LaunchAgent plist in `~/Library/LaunchAgents` (launchd loads it at login).
//! - Windows: an `HKCU\…\Run` value written via `reg.exe` (with `CREATE_NO_WINDOW` so no console
//!   flashes).
//! - Other platforms: unsupported (no-op / error).

use std::io;

/// Whether the tray is registered to start at login.
pub fn is_autostart_enabled() -> bool {
    imp::is_enabled()
}

/// Enable or disable starting the tray at login. Points at the current executable, so it works
/// for both the bare binary and a bundled `.app`.
pub fn set_autostart(enabled: bool) -> io::Result<()> {
    imp::set(enabled)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{fs, io, path::PathBuf};

    const LABEL: &str = "com.airpaste.tray";

    fn plist_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").filter(|value| !value.is_empty())?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }

    pub fn is_enabled() -> bool {
        plist_path().map(|path| path.exists()).unwrap_or(false)
    }

    pub fn set(enabled: bool) -> io::Result<()> {
        let path =
            plist_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?;
        if enabled {
            let exe = std::env::current_exe()?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            // launchd loads ~/Library/LaunchAgents plists at login; RunAtLoad then starts it.
            // We only write the file (no `launchctl load`, which would launch a duplicate now).
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                 \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\">\n<dict>\n\
                 \t<key>Label</key><string>{LABEL}</string>\n\
                 \t<key>ProgramArguments</key>\n\t<array><string>{exe}</string></array>\n\
                 \t<key>RunAtLoad</key><true/>\n\
                 \t<key>ProcessType</key><string>Interactive</string>\n\
                 </dict>\n</plist>\n",
                exe = xml_escape(&exe.to_string_lossy()),
            );
            fs::write(&path, plist)
        } else if path.exists() {
            fs::remove_file(&path)
        } else {
            Ok(())
        }
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

#[cfg(windows)]
mod imp {
    use std::{io, os::windows::process::CommandExt, process::Command};

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "AirPaste";
    // Don't pop a console window for the helper process (we're a GUI/windows-subsystem app).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn is_enabled() -> bool {
        Command::new("reg")
            .args(["query", RUN_KEY, "/v", VALUE])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    pub fn set(enabled: bool) -> io::Result<()> {
        if enabled {
            let exe = std::env::current_exe()?;
            // Quote the path so a path with spaces is parsed as one argument at login.
            let quoted = format!("\"{}\"", exe.display());
            let status = Command::new("reg")
                .args([
                    "add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &quoted, "/f",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !status.success() {
                return Err(io::Error::other("reg add failed"));
            }
        } else {
            let _ = Command::new("reg")
                .args(["delete", RUN_KEY, "/v", VALUE, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use std::io;

    pub fn is_enabled() -> bool {
        false
    }

    pub fn set(_enabled: bool) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "autostart not supported on this platform",
        ))
    }
}
