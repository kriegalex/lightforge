//! `lightforge doctor` and the Diagnostics block in settings: what this
//! machine has, held against what Battle.net and WoW under Proton need.
//!
//! Read-only, and nothing fails: a fact that cannot be read is reported as
//! such.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::paths::Layout;
use crate::proton::{self, ProtonSource};
use crate::runtime;

/// The hard `nofile` limit esync needs; systemd 240 and newer set it.
const NOFILE_MIN: u64 = 524_288;

/// Free space a library should keep: a retail patch day can stage 15 GiB.
const FREE_MIN: u64 = 20 << 30;

/// Where drivers are looked for besides the XDG data dirs, which a session
/// can set without `/usr/share`; the last is NixOS's.
const ICD_DIRS: &[&str] = &[
    "/usr/share/vulkan/icd.d",
    "/etc/vulkan/icd.d",
    "/usr/local/share/vulkan/icd.d",
    "/run/opengl-driver/share/vulkan/icd.d",
];

/// Variables that replace the loader's search with a list of driver files.
const ICD_OVERRIDES: &[&str] = &["VK_DRIVER_FILES", "VK_ICD_FILENAMES"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// What to do about it, when there is something to do.
    pub hint: Option<String>,
}

fn check(name: impl Into<String>, status: Status, detail: impl Into<String>, hint: Option<String>) -> Check {
    Check { name: name.into(), status, detail: detail.into(), hint }
}

pub fn run(config: &Config, layout: &Layout, proton_dir: &Path) -> Vec<Check> {
    let nvidia_version = crate::debug::nvidia_version();
    let mut checks = vec![open_files(), ntsync(), vulkan(), loader(), gnutls()];
    checks.push(vulkan_32bit(nvidia_version.as_deref()));
    checks.extend(libraries(layout));
    checks.push(proton(config, proton_dir));
    checks.push(python());
    checks.push(prefix(layout));
    match nvidia_version {
        Some(version) => checks.extend(nvidia(&version)),
        None => checks.extend(open_nvidia(&crate::debug::cards())),
    }
    checks
}

/// The plain-text report, for a terminal or a pasted bug report.
pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let _ = writeln!(out, "{:<6}{:<20}{}", c.status.label(), c.name, c.detail);
        if let Some(hint) = &c.hint {
            let _ = writeln!(out, "{:<26}{hint}", "");
        }
    }
    crate::debug::scrub(&out)
}

fn open_files() -> Check {
    let Some((soft, hard)) = std::fs::read_to_string("/proc/self/limits").ok().as_deref().and_then(nofile) else {
        return check("open files", Status::Warn, "could not read /proc/self/limits", None);
    };
    let show = |n: u64| if n == u64::MAX { "unlimited".to_owned() } else { n.to_string() };
    let detail = format!("soft {}, hard {}", show(soft), show(hard));
    match hard < NOFILE_MIN {
        // Proton raises the soft limit to the hard one itself.
        true => check(
            "open files",
            Status::Warn,
            detail,
            Some(format!(
                "esync needs a hard limit of {NOFILE_MIN}: set DefaultLimitNOFILE in systemd's \
                 system.conf and user.conf"
            )),
        ),
        false => check("open files", Status::Ok, detail, None),
    }
}

/// Soft and hard `Max open files` from `/proc/<pid>/limits`.
fn nofile(limits: &str) -> Option<(u64, u64)> {
    let line = limits.lines().find(|l| l.starts_with("Max open files"))?;
    let mut values = line["Max open files".len()..].split_whitespace().map(|v| match v {
        "unlimited" => Some(u64::MAX),
        n => n.parse().ok(),
    });
    Some((values.next()??, values.next()??))
}

fn ntsync() -> Check {
    match Path::new("/dev/ntsync").exists() {
        true => check("ntsync", Status::Ok, "available", None),
        false => check(
            "ntsync",
            Status::Ok,
            "not available, Proton uses fsync",
            Some("ntsync needs kernel 6.14 or newer with the ntsync module loaded".to_owned()),
        ),
    }
}

fn vulkan() -> Check {
    let (overridden, files) = icd_files();
    let mut drivers: Vec<String> = files
        .iter()
        .filter_map(|file| Some(file.file_name()?.to_string_lossy().into_owned()))
        .collect();
    drivers.sort();
    drivers.dedup();
    if let Some(name) = overridden {
        return check("vulkan", Status::Ok, format!("set by {name}: {}", drivers.join(", ")), None);
    }
    match drivers.is_empty() {
        // Warn: a driver somewhere this does not look still counts.
        true => check(
            "vulkan",
            Status::Warn,
            "no Vulkan driver found",
            Some("install your GPU's Vulkan driver; DXVK and VKD3D cannot run without one".to_owned()),
        ),
        false => check("vulkan", Status::Ok, drivers.join(", "), None),
    }
}

/// The driver files the loader reads: the ones an override variable names,
/// returned with that variable, or every `.json` in the driver directories.
fn icd_files() -> (Option<&'static str>, Vec<PathBuf>) {
    let set = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if let Some((name, files)) = ICD_OVERRIDES.iter().find_map(|name| Some((*name, set(name)?))) {
        return (Some(name), override_files(&files));
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dirs = icd_dirs(set("XDG_DATA_HOME").as_deref(), home.as_deref(), set("XDG_DATA_DIRS").as_deref());
    (None, dirs.iter().flat_map(|dir| json_in(dir)).collect())
}

/// The files an override lists. The loader also takes a folder there and
/// reads the manifests in it.
fn override_files(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .flat_map(|path| match path.is_dir() {
            true => json_in(&path),
            false => vec![path],
        })
        .collect()
}

/// The `.json` files in `dir`, none when it cannot be read.
fn json_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .map(|entry| entry.path())
        .collect()
}

/// The driver directories the loader searches, from the XDG variables as
/// set (unset or empty is `None`) and their defaults otherwise.
fn icd_dirs(data_home: Option<&str>, home: Option<&Path>, data_dirs: Option<&str>) -> Vec<PathBuf> {
    let data_home = match data_home {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home.map(|home| home.join(".local/share")),
    };
    let data_dirs = data_dirs.unwrap_or("/usr/local/share:/usr/share");
    let mut dirs: Vec<PathBuf> = data_home
        .into_iter()
        .chain(std::env::split_paths(data_dirs))
        .map(|dir| dir.join("vulkan/icd.d"))
        .chain(ICD_DIRS.iter().map(PathBuf::from))
        .collect();
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

fn loader() -> Check {
    if !runtime::needs_32bit_libraries() {
        return check("32-bit runtime", Status::Ok, "not needed on this processor", None);
    }
    if runtime::has_32bit_loader() {
        return check("32-bit runtime", Status::Ok, "present", None);
    }
    let hint = format!("Battle.net is 32-bit, the games are not; {}", fix(runtime::remedy()));
    check("32-bit runtime", Status::Warn, "no 32-bit loader", Some(hint))
}

/// A distribution's remedy as one line of a hint.
fn fix(remedy: runtime::Remedy) -> String {
    match (remedy.command, remedy.note) {
        (Some(command), Some(note)) => format!("on {}: {command} ({note})", remedy.distro),
        (Some(command), None) => format!("on {}: {command}", remedy.distro),
        (None, Some(note)) => note,
        (None, None) => "install your distribution's 32-bit libraries".to_owned(),
    }
}

/// `proton` is a Python script. Without an interpreter, starting it fails
/// as if Proton itself were missing.
fn python() -> Check {
    match crate::tweaks::on_path("python3") {
        Some(_) => check("python3", Status::Ok, "present", None),
        None => check(
            "python3",
            Status::Fail,
            "missing",
            Some("Proton's launcher is a Python script; install your distribution's python3".to_owned()),
        ),
    }
}

fn gnutls() -> Check {
    if !runtime::needs_32bit_libraries() {
        return check("32-bit gnutls", Status::Ok, "not needed on this processor", None);
    }
    match runtime::has_32bit_tls() {
        Some(true) => check("32-bit gnutls", Status::Ok, "present", None),
        Some(false) => check(
            "32-bit gnutls",
            Status::Warn,
            "missing",
            Some("Battle.net then runs inside the Steam Linux Runtime, which Steam has to have installed".to_owned()),
        ),
        None => check("32-bit gnutls", Status::Ok, "unknown, ldconfig could not be read", None),
    }
}

/// Battle.net's client is 32-bit and draws through Vulkan. With the 32-bit
/// loader and no 32-bit driver, every other check passes, and the client
/// closes before its login: its GPU process finds no device.
fn vulkan_32bit(nvidia: Option<&str>) -> Check {
    if !runtime::needs_32bit_libraries() {
        return check("32-bit vulkan", Status::Ok, "not needed on this processor", None);
    }
    // ldconfig would see no 32-bit library then, driver or not; the runtime
    // check warns and names the command.
    if !runtime::has_32bit_loader() {
        return check("32-bit vulkan", Status::Ok, "not checked, no 32-bit runtime", None);
    }
    let (_, files) = icd_files();
    let manifests: Vec<(PathBuf, String)> =
        files.into_iter().filter_map(|file| std::fs::read_to_string(&file).ok().map(|text| (file, text))).collect();
    // No driver at all is the vulkan check's warning, and an override naming
    // files that cannot be read says nothing about what is installed.
    if manifests.is_empty() {
        return check("32-bit vulkan", Status::Ok, "unknown, no driver file could be read", None);
    }
    match has_32bit_driver(&manifests, runtime::has_32bit_lib) {
        Some(true) => check("32-bit vulkan", Status::Ok, "present", None),
        Some(false) => {
            // On NVIDIA's driver Mesa's would not help: it ships its own.
            let remedy = match nvidia {
                Some(version) => runtime::nvidia_remedy(version),
                None => runtime::remedy(),
            };
            let hint = format!("Battle.net closes before its login without one; {}", fix(remedy));
            check("32-bit vulkan", Status::Warn, "no 32-bit driver", Some(hint))
        }
        None => check("32-bit vulkan", Status::Ok, "unknown, ldconfig could not be read", None),
    }
}

/// Whether a driver manifest names a 32-bit library. A path is judged by the
/// file: Fedora's `radeon_icd.i686.json` names `/usr/lib/libvulkan_radeon.so`.
/// A bare name goes to `lookup`, which answers from ldconfig's cache, where
/// `dlopen` finds it unless `LD_LIBRARY_PATH` points elsewhere: Arch and
/// Debian ship one manifest for both architectures. `None` when only
/// `lookup` could tell and it could not.
fn has_32bit_driver(manifests: &[(PathBuf, String)], lookup: impl Fn(&str) -> Option<bool>) -> Option<bool> {
    let mut unknown = false;
    for (file, text) in manifests {
        let Some(library) = library_path(text) else { continue };
        let found = match library.contains('/') {
            // The loader reads a relative path from the manifest's folder.
            true => is_elf32(&file.parent().unwrap_or(Path::new("/")).join(&library)),
            false => lookup(&library).unwrap_or_else(|| {
                unknown = true;
                false
            }),
        };
        if found {
            return Some(true);
        }
    }
    (!unknown).then_some(false)
}

/// `ICD.library_path` from a driver manifest.
fn library_path(manifest: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(manifest).ok()?;
    Some(json.get("ICD")?.get("library_path")?.as_str()?.to_owned())
}

/// Whether `path` is a 32-bit ELF file: the magic, then class 1.
fn is_elf32(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 5];
    std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut head)).is_ok() && head == *b"\x7fELF\x01"
}

fn libraries(layout: &Layout) -> Vec<Check> {
    let gib = |bytes: u64| bytes as f64 / f64::from(1u32 << 30);
    layout
        .libraries()
        .all()
        .iter()
        .map(|library| {
            let name = format!("space on {}", library.label);
            let Some(disk) = library.disk() else {
                let detail = library
                    .state()
                    .reason(&library.label)
                    .unwrap_or_else(|| format!("{} could not be read", library.games.display()));
                return check(name, Status::Warn, detail, None);
            };
            let detail = format!(
                "{:.1} GiB free of {:.0} GiB at {}",
                gib(disk.free),
                gib(disk.total),
                library.games.display()
            );
            match disk.free < FREE_MIN {
                true => check(name, Status::Warn, detail, Some("a patch can need 15 GiB of room".to_owned())),
                false => check(name, Status::Ok, detail, None),
            }
        })
        .collect()
}

fn proton(config: &Config, proton_dir: &Path) -> Check {
    let steam = crate::steam::SteamRoot::discover();
    let builds = proton::discover_all(steam.as_ref(), proton_dir);
    let managed = builds.iter().filter(|b| b.source == ProtonSource::Managed).count();
    let pin = config.default_proton.version();

    let Some(build) = proton::select(&builds, pin) else {
        return match pin {
            Some(pin) => check(
                "proton",
                Status::Fail,
                format!("pinned {pin} is not installed"),
                Some("install it under Settings, Proton, or remove the pin".to_owned()),
            ),
            None => check(
                "proton",
                Status::Fail,
                "no supported Proton build",
                Some(format!("setup installs {}, or run `lightforge proton-install`", proton::TESTED)),
            ),
        };
    };
    let detail = |why: &str| format!("{} ({why}), {managed} managed by Lightforge", build.version);
    match (pin, build.version == proton::TESTED) {
        (Some(_), _) => check("proton", Status::Ok, detail("pinned"), None),
        (None, true) => check("proton", Status::Ok, detail("tested default"), None),
        (None, false) => check(
            "proton",
            Status::Warn,
            detail("newest installed"),
            Some(format!("Lightforge is tested with {}", proton::TESTED)),
        ),
    }
}

fn prefix(layout: &Layout) -> Check {
    let status = crate::prefix::PrefixStatus::inspect(layout);
    if !status.initialized {
        return check(
            "prefix",
            Status::Warn,
            "not set up yet",
            Some("run setup from the app, or `lightforge setup`".to_owned()),
        );
    }
    let detail = format!(
        "built by {}, game link {}",
        status.built_by.as_deref().unwrap_or("an unknown Proton"),
        status.link.describe()
    );
    match status.link.is_usable() {
        true => check("prefix", Status::Ok, detail, None),
        false => check("prefix", Status::Warn, detail, Some("`lightforge prefix-link` repairs the link".to_owned())),
    }
}

fn nvidia(version: &str) -> Vec<Check> {
    let host = crate::tweaks::HostTools::detect();
    let driver = match host.wayland && host.nvidia_lacks_explicit_sync() {
        true => check(
            "nvidia driver",
            Status::Warn,
            version,
            Some("Wayland needs 555 or newer for explicit sync; see TROUBLESHOOTING.md".to_owned()),
        ),
        false => check("nvidia driver", Status::Ok, version, None),
    };

    let modeset = match std::fs::read_to_string("/sys/module/nvidia_drm/parameters/modeset") {
        Ok(value) if value.trim() == "Y" => check("nvidia modeset", Status::Ok, "on", None),
        Ok(_) => check(
            "nvidia modeset",
            Status::Warn,
            "off",
            Some("Wayland and gamescope need nvidia_drm.modeset=1 on the kernel command line".to_owned()),
        ),
        Err(_) => check("nvidia modeset", Status::Warn, "nvidia_drm is not loaded", None),
    };

    let gl32 = match runtime::has_32bit_lib("libGLX_nvidia.so.0") {
        Some(true) => check("nvidia 32-bit gl", Status::Ok, "present", None),
        Some(false) => {
            let remedy = runtime::nvidia_remedy(version);
            let fix = match (remedy.command, remedy.note) {
                (Some(command), Some(note)) => format!("; on {}: {command} ({note})", remedy.distro),
                (Some(command), None) => format!("; on {}: {command}", remedy.distro),
                (None, Some(note)) => format!("; {note}"),
                (None, None) => String::new(),
            };
            check(
                "nvidia 32-bit gl",
                Status::Warn,
                "missing",
                Some(format!("Battle.net is 32-bit and needs the driver's 32-bit libraries{fix}")),
            )
        }
        None => check("nvidia 32-bit gl", Status::Ok, "unknown, ldconfig could not be read", None),
    };

    vec![driver, modeset, gl32]
}

/// An NVIDIA card on nouveau, which games reach through NVK. A GPU hang there
/// froze a whole machine (GlitchTip #38, RTX 3060); the proprietary driver
/// recovers and only the game dies (Xid 109).
fn open_nvidia(cards: &[String]) -> Option<Check> {
    let card = cards.iter().find(|card| card.starts_with("nouveau 10de:"))?;
    Some(check(
        "nvidia driver",
        Status::Warn,
        card.as_str(),
        Some("On nouveau/NVK a game's GPU hang can freeze the whole system; the proprietary driver recovers from it".to_owned()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/proc/self/limits` from a CachyOS desktop, 2026-10-28, trailing
    /// spaces and all.
    const LIMITS: &str = "\
Limit                     Soft Limit           Hard Limit           Units     
Max cpu time              unlimited            unlimited            seconds   
Max open files            1048576              2097152              files     
Max locked memory         8388608              8388608              bytes     
";

    #[test]
    fn an_nvidia_card_on_nouveau_is_warned_about() {
        // As GlitchTip #38 reported it.
        let warned = open_nvidia(&["nouveau 10de:2503".to_owned()]).expect("a warning");
        assert_eq!(warned.status, Status::Warn);
        assert_eq!(warned.detail, "nouveau 10de:2503");
    }

    #[test]
    fn other_cards_on_open_drivers_are_left_alone() {
        assert_eq!(open_nvidia(&["amdgpu 1002:7550".to_owned(), "i915 8086:a780".to_owned()]), None);
    }

    #[test]
    fn the_open_file_limit_is_read_from_proc() {
        assert_eq!(nofile(LIMITS), Some((1_048_576, 2_097_152)));
    }

    #[test]
    fn an_unlimited_open_file_limit_counts_as_enough() {
        let limits = LIMITS.replace("1048576              2097152", "unlimited            unlimited");
        assert_eq!(nofile(&limits), Some((u64::MAX, u64::MAX)));
    }

    #[test]
    fn vulkan_drivers_are_looked_for_in_every_xdg_data_dir() {
        let dirs = icd_dirs(None, Some(Path::new("/home/gabriel")), Some("/var/lib/flatpak/exports/share:/usr/share"));
        assert_eq!(dirs[0], Path::new("/home/gabriel/.local/share/vulkan/icd.d"));
        assert_eq!(dirs[1], Path::new("/var/lib/flatpak/exports/share/vulkan/icd.d"));
        assert!(dirs.contains(&PathBuf::from("/run/opengl-driver/share/vulkan/icd.d")));
        let usr = dirs.iter().filter(|d| *d == Path::new("/usr/share/vulkan/icd.d")).count();
        assert_eq!(usr, 1, "listed once though named twice");
    }

    #[test]
    fn unset_xdg_dirs_fall_back_to_their_defaults() {
        let dirs = icd_dirs(Some("/data"), None, None);
        assert_eq!(dirs[0], Path::new("/data/vulkan/icd.d"));
        assert_eq!(dirs[1], Path::new("/usr/local/share/vulkan/icd.d"));
        assert_eq!(dirs[2], Path::new("/usr/share/vulkan/icd.d"));
    }

    /// Mesa's radeon manifest on a CachyOS desktop, 2026-11-09: one file for
    /// both architectures, naming the library without a folder.
    const ARCH_RADEON: &str =
        r#"{"ICD": {"api_version": "1.4.354", "library_path": "libvulkan_radeon.so"}, "file_format_version": "1.0.1"}"#;

    /// As Fedora 45's mesa-vulkan-drivers writes them, one per architecture.
    fn fedora_manifest(library: &str, arch: &str) -> String {
        format!(
            r#"{{"ICD": {{"api_version": "1.4.354", "library_arch": "{arch}", "library_path": "{library}"}}, "file_format_version": "1.0.1"}}"#
        )
    }

    #[test]
    fn a_32_bit_loader_with_a_64_bit_driver_has_no_32_bit_driver() {
        // What the Arch command left: lib32-vulkan-icd-loader beside vulkan-radeon.
        let manifests = [
            (PathBuf::from("/usr/share/vulkan/icd.d/radeon_icd.json"), ARCH_RADEON.to_owned()),
            (PathBuf::from("/usr/share/vulkan/icd.d/broken.json"), "not json".to_owned()),
            (PathBuf::from("/usr/share/vulkan/icd.d/layer.json"), r#"{"layer": {}}"#.to_owned()),
        ];
        assert_eq!(has_32bit_driver(&manifests, |_| Some(false)), Some(false));
        assert_eq!(has_32bit_driver(&manifests, |lib| Some(lib == "libvulkan_radeon.so")), Some(true));
    }

    #[test]
    fn a_driver_named_by_path_is_judged_by_its_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("radeon64.so"), b"\x7fELF\x02\x01\x01\0").unwrap();
        std::fs::write(dir.path().join("radeon32.so"), b"\x7fELF\x01\x01\x01\0").unwrap();
        let at = |name: &str| dir.path().join(name);
        let x86_64 = (at("radeon_icd.x86_64.json"), fedora_manifest(&at("radeon64.so").to_string_lossy(), "64"));
        let i686 = (at("radeon_icd.i686.json"), fedora_manifest(&at("radeon32.so").to_string_lossy(), "32"));
        let relative = (at("radeon_icd.json"), fedora_manifest("./radeon32.so", "32"));
        // A path settles it either way, with no ldconfig to ask.
        let no_cache = |_: &str| None;
        assert_eq!(has_32bit_driver(std::slice::from_ref(&x86_64), no_cache), Some(false));
        assert_eq!(has_32bit_driver(&[x86_64.clone(), i686], no_cache), Some(true));
        assert_eq!(has_32bit_driver(&[x86_64, relative], no_cache), Some(true));

        std::fs::write(at("short.so"), b"\x7fEL").unwrap();
        std::fs::write(at("script.so"), b"#!/bin/sh\n").unwrap();
        for not_32 in ["short.so", "script.so", "missing.so"] {
            assert!(!is_elf32(&at(not_32)), "{not_32}");
        }
    }

    #[test]
    fn a_bare_name_without_ldconfig_is_unknown_rather_than_missing() {
        let bare = (PathBuf::from("/usr/share/vulkan/icd.d/radeon_icd.json"), ARCH_RADEON.to_owned());
        assert_eq!(has_32bit_driver(std::slice::from_ref(&bare), |_| None), None);

        // Unknown neither hides a driver found by its path nor turns a 64-bit
        // one into a missing 32-bit one.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("radeon32.so"), b"\x7fELF\x01\x01\x01\0").unwrap();
        std::fs::write(dir.path().join("radeon64.so"), b"\x7fELF\x02\x01\x01\0").unwrap();
        let by_path = |name: &str, arch: &str| (dir.path().join(format!("{arch}.json")), fedora_manifest(name, arch));
        let i686 = by_path("./radeon32.so", "32");
        let x86_64 = by_path("./radeon64.so", "64");
        assert_eq!(has_32bit_driver(&[bare.clone(), i686], |_| None), Some(true));
        assert_eq!(has_32bit_driver(&[bare, x86_64], |_| None), None);
    }

    /// The loader takes folders in `VK_DRIVER_FILES` too. Read as a file, a
    /// folder would leave no manifest and a false "no 32-bit driver".
    #[test]
    fn a_folder_in_an_override_stands_for_its_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("icd.d");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("radeon_icd.json"), ARCH_RADEON).unwrap();
        std::fs::write(folder.join("README"), "not a manifest").unwrap();
        let single = dir.path().join("nvidia_icd.json");
        let value = std::env::join_paths([&folder, &single]).unwrap();

        let mut files = override_files(&value.to_string_lossy());
        files.sort();
        assert_eq!(files, [folder.join("radeon_icd.json"), single]);
    }

    #[test]
    fn a_report_puts_hints_under_their_check() {
        let report = render(&[
            check("ntsync", Status::Ok, "available", None),
            check("open files", Status::Warn, "soft 1024, hard 4096", Some("raise it".to_owned())),
        ]);
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("warn  open files"));
        assert_eq!(lines[2].trim(), "raise it");
    }
}
