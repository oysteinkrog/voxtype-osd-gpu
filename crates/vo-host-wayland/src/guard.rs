//! Keeps dev tools off the real desktop.
//!
//! A cargo example (any binary under `target/<profile>/examples/`) or the preview tool that
//! connects to Wayland refuses the real session: `WAYLAND_DISPLAY` unset (libwayland then
//! falls back to `wayland-0`), a default socket name `wayland-N`, or the socket the user's
//! systemd environment names. Run them in a nested compositor with a socket name of its own
//! (`kwin_wayland --virtual --socket my-test`). `VO_REAL_DISPLAY=1`, or `--real-display` on
//! the command line, opens on the real desktop on purpose.
//!
//! Installed binaries, such as the voxtype popup, are not dev tools and are never refused.

use std::path::Path;

/// What the decision depends on, gathered once so the rule can be tested on its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayFacts {
    /// `WAYLAND_DISPLAY` of this process.
    pub wayland_display: Option<String>,
    /// `WAYLAND_DISPLAY` in the user's systemd environment: the real session's socket.
    pub session_display: Option<String>,
    /// This is a cargo example or the preview tool.
    pub dev_tool: bool,
    /// `VO_REAL_DISPLAY=1` or `--real-display`.
    pub real_display_asked: bool,
}

impl DisplayFacts {
    /// The facts for this process. Reads the systemd environment only for a dev tool.
    pub fn current() -> DisplayFacts {
        let dev_tool = std::env::current_exe().is_ok_and(|p| is_dev_tool(&p))
            || std::env::args().nth(1).is_some_and(|a| a == "--preview");
        DisplayFacts {
            wayland_display: std::env::var("WAYLAND_DISPLAY").ok().filter(|v| !v.is_empty()),
            session_display: if dev_tool { session_display() } else { None },
            dev_tool,
            real_display_asked: std::env::var("VO_REAL_DISPLAY").is_ok_and(|v| v == "1")
                || std::env::args().any(|a| a == "--real-display"),
        }
    }
}

fn is_dev_tool(exe: &Path) -> bool {
    exe.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "examples")
        || exe.file_name().is_some_and(|n| n == "vo-preview")
}

fn session_display() -> Option<String> {
    let out = std::process::Command::new("systemctl").args(["--user", "show-environment"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("WAYLAND_DISPLAY="))
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

/// `wayland-0`, `wayland-1` and so on: the names a desktop session's compositor takes.
fn default_name(name: &str) -> bool {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or(name);
    base.strip_prefix("wayland-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `Err` with the reason and the fix when a dev tool would open on the real desktop.
pub fn check(f: &DisplayFacts) -> Result<(), String> {
    if !f.dev_tool || f.real_display_asked {
        return Ok(());
    }
    let why = match &f.wayland_display {
        None => "WAYLAND_DISPLAY is not set, so libwayland would use wayland-0".to_string(),
        Some(d) if f.session_display.as_deref() == Some(d.as_str()) => {
            format!("WAYLAND_DISPLAY={d} is the desktop session's own socket")
        }
        Some(d) if default_name(d) => format!("WAYLAND_DISPLAY={d} is a desktop session socket name"),
        Some(_) => return Ok(()),
    };
    Err(format!(
        "refusing to open a dev window on the real desktop: {why}. Run it in a nested compositor with \
         its own socket (kwin_wayland --virtual --socket NAME, then WAYLAND_DISPLAY=NAME), or pass \
         --real-display (VO_REAL_DISPLAY=1) to open it here on purpose"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(display: Option<&str>, session: Option<&str>) -> DisplayFacts {
        DisplayFacts {
            wayland_display: display.map(String::from),
            session_display: session.map(String::from),
            dev_tool: true,
            real_display_asked: false,
        }
    }

    #[test]
    fn dev_tools_refuse_the_real_session() {
        assert!(check(&facts(None, None)).unwrap_err().contains("not set"));
        assert!(check(&facts(Some("wayland-0"), None)).is_err());
        assert!(check(&facts(Some("wayland-1"), Some("wayland-1"))).unwrap_err().contains("session's own socket"));
        assert!(check(&facts(Some("/run/user/1000/wayland-2"), None)).is_err());
        // A session whose socket has its own name is still caught through systemd.
        assert!(check(&facts(Some("desk"), Some("desk"))).is_err());
    }

    #[test]
    fn nested_sockets_overrides_and_installed_binaries_pass() {
        assert!(check(&facts(Some("m4-scrub-wl"), Some("wayland-1"))).is_ok());
        assert!(check(&facts(Some("wayland-test"), None)).is_ok());
        assert!(check(&DisplayFacts { real_display_asked: true, ..facts(None, None) }).is_ok());
        assert!(check(&DisplayFacts { dev_tool: false, ..facts(Some("wayland-0"), Some("wayland-0")) }).is_ok());
    }

    #[test]
    fn examples_and_the_preview_are_dev_tools() {
        assert!(is_dev_tool(Path::new("/w/target/debug/examples/toast")));
        assert!(is_dev_tool(Path::new("/w/target/release/examples/depth_demo-1a2b")));
        assert!(is_dev_tool(Path::new("/w/target/debug/vo-preview")));
        assert!(!is_dev_tool(Path::new("/usr/bin/voxtype-osd-vo")));
    }
}
