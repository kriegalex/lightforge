//! Who this build is, and whose servers it uses.
//!
//! The names the desktop and the data folders know it by, where its releases
//! are published, the project's own services (art, the addon proxy, bug
//! reports) and the links its menu offers: what a fork, or a package pointed
//! at other servers, changes in code.
//!
//! The GUI's values are here as well, though only the GUI reads them: the
//! front ends depend on this crate and not the other way round, so this is
//! the one place all of them can live. The file names under `packaging/`
//! follow [`APP_ID`].

/// `directories`' qualifier, organisation and application. On Linux only the
/// application names the folders, lowercased: `lightforge` in the XDG config,
/// data and cache folders.
pub const PROJECT_DIRS: (&str, &str, &str) = ("io", "Tuxcore", "Lightforge");

/// The desktop entry's basename, Wayland `app_id` and AppStream id, which
/// must match (`packaging/README.md`).
pub const APP_ID: &str = "pro.tuxcore.Lightforge";

/// The menu entry's file name: [`APP_ID`] and `.desktop`. A second string
/// because `concat!` cannot join constants; the test below keeps the two in
/// step.
pub const DESKTOP_ENTRY: &str = "pro.tuxcore.Lightforge.desktop";

/// The source repository, as `cargo install --git` takes it.
pub const REPOSITORY: &str = "https://github.com/LightforgeProject/lightforge";

/// GitHub Releases endpoint for Lightforge itself.
pub const RELEASES_URL: &str = "https://api.github.com/repos/LightforgeProject/lightforge/releases";

/// Where to send someone who wants the new build.
pub const RELEASES_PAGE: &str = "https://github.com/LightforgeProject/lightforge/releases";

/// Where every release's files are published.
pub const RELEASE_DOWNLOADS: &str = "https://github.com/LightforgeProject/lightforge/releases/download";

/// The Arch packages, as the release left them on `main`: the tag still has
/// `sha256sums=('SKIP')`, the checksums are committed after it.
pub const PKGBUILDS: &str =
    "https://raw.githubusercontent.com/LightforgeProject/lightforge/main/packaging/aur";

/// The one-line install for everyone the upgrade routes cannot help.
pub const INSTALLER_COMMAND: &str = "curl -fsSL https://raw.githubusercontent.com/LightforgeProject/lightforge/main/packaging/installer/install.sh | bash";

/// The COPR repository's file, as `dnf copr enable` writes it.
pub const COPR_REPO: &str = "etc/yum.repos.d/_copr:copr.fedorainfracloud.org:simplykenny:lightforge.repo";

/// The command that writes [`COPR_REPO`].
pub const COPR_ENABLE: &str = "sudo dnf5 copr enable simplykenny/lightforge";

/// Where themed branding comes from unless `assets_source` says otherwise.
pub const DEFAULT_ASSETS_SOURCE: &str = "https://lightforge.tuxcore.pro/assets/";

/// Where the addon catalog is read from unless `addon_catalog` says otherwise.
///
/// CurseForge needs a key, so it goes through a proxy; ours by default. The
/// keyless providers never come through here.
pub const DEFAULT_ADDON_CATALOG: &str = "https://lightforge.tuxcore.pro/";

/// Our GlitchTip project; a DSN is a write-only key meant to ship.
pub const REPORT_DSN: &str = "https://788f3511837f42abade303476cdeef11@bugs.k3nny.tech/1";

/// Where a screenshot is uploaded, so the issue carries a link (GlitchTip
/// drops attachments). `/upload`: the proxy routes `/shots/v1/*` only.
pub const SCREENSHOT_UPLOAD: &str = "https://lightforge.tuxcore.pro/shots/v1/upload";

/// Where the app menu's links point. `None` is a destination that does not
/// exist yet: the entry draws dimmed and unclickable until it is filled in.
pub const DISCORD_URL: Option<&str> = Some("https://discord.gg/vtTgmUV9Bq");
pub const X_URL: Option<&str> = Some("https://x.com/simplyKennybtw");
/// Where someone can say thanks. Offered, never asked for: these sit beside
/// the places to get help, not in front of anything.
pub const KOFI_URL: Option<&str> = Some("https://ko-fi.com/simplykenny");
pub const PAYPAL_URL: Option<&str> = Some("https://paypal.me/simplykenny");

#[cfg(test)]
mod tests {
    use super::*;

    /// A compositor matches a window to its menu entry by the app id alone,
    /// and the installer route copies the entry by name: an id changed here
    /// and not in `packaging/` leaves the taskbar a placeholder icon
    /// (`packaging/README.md`).
    #[test]
    fn the_packaged_menu_entry_and_appstream_file_carry_the_app_id() {
        assert_eq!(DESKTOP_ENTRY, format!("{APP_ID}.desktop"));
        let packaging = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging");
        let read = |name: &str| {
            std::fs::read_to_string(packaging.join(name)).unwrap_or_else(|e| panic!("packaging/{name}: {e}"))
        };
        let entry = read(DESKTOP_ENTRY);
        for key in ["Icon", "StartupWMClass"] {
            let line = format!("{key}={APP_ID}");
            assert!(entry.lines().any(|l| l == line), "packaging/{DESKTOP_ENTRY} has no {line}");
        }
        let metainfo = read(&format!("{APP_ID}.metainfo.xml"));
        let id = format!("<id>{APP_ID}</id>");
        assert!(metainfo.contains(&id), "packaging/{APP_ID}.metainfo.xml has no {id}");
    }
}
