//! Every game in a WoW install Lightforge knows, as Agent would record it.

use crate::buildinfo::BuildInfo;
use crate::library::State;
use crate::paths::Layout;
use crate::tact::productdb::{self, ProductInstall};

/// A record for every product with a `.build.info` row and a flavor folder
/// in one of the layout's WoW installs, whether Lightforge manages it or not.
///
/// Agent reclaims the store space no product in its registry uses: its update
/// of `wowf` logged `Recoverable: 83588MiB` and took 77 GiB of a `wow` it had
/// never been told about.
///
/// One record per product, as Agent keeps one per uid: from the library
/// Lightforge plays it from (its own, else the default) when that one holds
/// it, else from the first that does. None while its own library is
/// unmounted: what another holds may be a copy left behind by a move.
///
/// Recorded under the path Agent already uses for that folder, when it
/// records a game there: one folder has several Windows paths (`C:/Program
/// Files (x86)/World of Warcraft` and the games drive's), and Agent's own
/// registry lists a shared install's products under one.
pub(super) fn installed(layout: &Layout) -> Vec<ProductInstall> {
    let agent_dir = layout.agent_dir();
    let (account, geo) = productdb::countries(&agent_dir);
    let known = productdb::read(&agent_dir.join("product.db")).unwrap_or_default();
    let folder = |windows: &str| {
        let path = layout.resolve_windows(windows)?;
        std::fs::canonicalize(&path).ok()
    };
    let mut records: Vec<ProductInstall> = Vec::new();
    for library in layout.libraries().all() {
        // Unmounted, or a second config entry for a volume another one holds.
        if matches!(library.state(), State::Missing | State::Foreign) {
            continue;
        }
        let install = layout.wow_install_in(library);
        let info = match BuildInfo::load(install.join(".build.info")) {
            Ok(info) => info,
            // A file that is there but unreadable leaves its games unprotected.
            Err(e) if install.join(".build.info").exists() => {
                tracing::warn!("{}: {e}", install.display());
                continue;
            }
            Err(_) => continue,
        };
        let here = std::fs::canonicalize(&install).ok();
        let path = known
            .iter()
            .filter(|k| !k.is_infrastructure())
            .find(|k| here.is_some() && folder(&k.install_path) == here)
            .map(|k| k.install_path.clone())
            .unwrap_or_else(|| library.windows_wow_path());
        for code in info.products() {
            // A row without its folder is a product that is not there: Agent
            // would only report it broken.
            let flavor = crate::paths::flavor_dir_name(&install, &code);
            if !install.join(&flavor).is_dir() {
                continue;
            }
            let home = layout.library(&code);
            if home.is_some_and(|home| matches!(home.state(), State::Missing | State::Foreign)) {
                continue;
            }
            let record = ProductInstall::from_build_info(&info, &code, &flavor, path.as_str());
            let record = match record {
                Ok(record) => record.with_countries(&account, &geo),
                Err(e) => {
                    tracing::warn!("{} in {}: {e}", code.as_str(), install.display());
                    continue;
                }
            };
            let assigned = home.is_some_and(|home| home.id == library.id);
            match records.iter().position(|r| r.product == record.product) {
                Some(at) if assigned => records[at] = record,
                Some(_) => {}
                None => records.push(record),
            }
        }
    }
    // Agent's record stands, but one in another folder plays a copy that
    // Lightforge does not update.
    for record in &records {
        let name = record.product.as_str();
        let theirs = known.iter().find(|k| k.product == name || k.product_code == name);
        let ours = folder(&record.install_path);
        if let Some(theirs) = theirs.filter(|t| folder(&t.install_path) != ours) {
            tracing::warn!(
                "Battle.net records {name} at {}, not in {}; left as it is",
                theirs.install_path,
                record.install_path
            );
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Libraries, Library};
    use crate::product::ProductCode;

    const BUILD_INFO: &str = concat!(
        "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|Tags!STRING:0|Version!STRING:0|",
        "Product!STRING:0\n",
        "eu|1|bc493ecb2898766c0f8947a99b4b3d65|Windows x86_64 EU? frFR|12.1.5.70783|wow\n",
        "eu|1|5ada954223f7c409ea172361f5c9e565|Windows x86_64 EU? frFR|1.60.1.70862|wowf\n",
        "eu|1|3645f0fe9dc5215ea90eaf7cdb7379ce|Windows x86_64 EU? frFR|1.15.9.69722|",
        "wow_classic_era\n",
    );

    /// A WoW install holding `flavors`, each named by its flavor file as Agent
    /// leaves them: Forever's folder is not the one its code would give.
    fn wow_install(layout: &Layout, library: &Library, flavors: &[(&str, &str)]) {
        let install = layout.wow_install_in(library);
        for (flavor, code) in flavors {
            let dir = install.join(flavor);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".flavor.info"), format!("Product Flavor!STRING:0\n{code}\n"))
                .unwrap();
        }
        std::fs::write(install.join(".build.info"), BUILD_INFO).unwrap();
    }

    fn record(product: &str, version: &str) -> ProductInstall {
        ProductInstall {
            product: product.into(),
            install_path: crate::paths::WINDOWS_WOW_PATH.into(),
            region: "eu".into(),
            locale: "frFR".into(),
            account_country: "CHE".into(),
            geo_country: "CH".into(),
            flavor_dir: "_forever_".into(),
            version: version.into(),
            build_key: "5ada954223f7c409ea172361f5c9e565".into(),
            tags: String::new(),
            key_ring: None,
            installed_at_ms: 0,
            family: "wow".into(),
        }
    }

    /// Retail and Forever share an install, and Agent only knows Forever,
    /// which Lightforge installed after a first-run adoption.
    #[test]
    fn a_game_sharing_an_install_with_a_registered_one_is_registered_too() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(dir.path());
        let flavors = [("_retail_", "wow"), ("_forever_", "wowf")];
        wow_install(&layout, &layout.libraries().all()[0], &flavors);
        let agent_dir = layout.agent_dir();
        std::fs::create_dir_all(&agent_dir).unwrap();
        // Behind `.build.info`, as Agent leaves a record it means to update.
        let agent = productdb::encode(&[record("wowf", "1.60.0.1")]);
        std::fs::write(agent_dir.join("product.db"), agent).unwrap();

        // Classic Era has a row but no folder: not there to protect.
        let installed = installed(&layout);
        let codes: Vec<_> = installed.iter().map(|r| r.product.as_str()).collect();
        assert_eq!(codes, ["wow", "wowf"]);
        assert_eq!(productdb::register_missing(&agent_dir, &installed).unwrap(), ["wow"]);

        let registry = productdb::read(&agent_dir.join("product.db")).unwrap();
        let found: Vec<_> = registry
            .iter()
            .map(|r| (r.product.as_str(), r.install_path.as_str(), r.version.as_deref()))
            .collect();
        assert_eq!(
            found,
            [
                ("wowf", crate::paths::WINDOWS_WOW_PATH, Some("1.60.0.1")),
                ("wow", crate::paths::WINDOWS_WOW_PATH, Some("12.1.5.70783")),
            ]
        );
        // Agent's record now stands: the next pass has nothing to add.
        assert!(productdb::register_missing(&agent_dir, &installed).unwrap().is_empty());
    }

    /// Battle.net records the games it installed into the main library under
    /// the games drive, not the `C:` link Lightforge uses: one folder, two
    /// paths, and a sibling under the other would be another install to Agent.
    #[test]
    fn a_game_is_recorded_under_the_path_agent_already_uses_for_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(dir.path());
        let main = &layout.libraries().all()[0];
        wow_install(&layout, main, &[("_retail_", "wow"), ("_classic_era_", "wow_classic_era")]);
        std::fs::create_dir_all(layout.dosdevices()).unwrap();
        std::os::unix::fs::symlink(&main.games, layout.dosdevices().join("e:")).unwrap();
        let agent_dir = layout.agent_dir();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let mut classic = record("wow_classic_era", "1.15.9.69722");
        classic.install_path = "E:/World of Warcraft".into();
        std::fs::write(agent_dir.join("product.db"), productdb::encode(&[classic])).unwrap();

        let paths: Vec<_> =
            installed(&layout).into_iter().map(|r| (r.product, r.install_path)).collect();
        assert_eq!(
            paths,
            [
                ("wow".to_owned(), "E:/World of Warcraft".to_owned()),
                ("wow_classic_era".to_owned(), "E:/World of Warcraft".to_owned()),
            ]
        );
    }

    /// Retail's own drive is unmounted, and the other library still holds the
    /// copy a move left behind: registering that one would point Agent at it.
    #[test]
    fn a_game_whose_own_library_is_unmounted_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let main = Library::main_at(dir.path().join("games"), "home");
        let mut gone = Library::adopted(dir.path().join("usb").join("Games"), "USB");
        gone.claimed = true;
        let mut libraries = Libraries::new(vec![main.clone(), gone.clone()]);
        libraries.assign(ProductCode::Retail, gone.id.clone());
        let layout = Layout::with_root(dir.path()).with_libraries(libraries);
        wow_install(&layout, &main, &[("_retail_", "wow"), ("_forever_", "wowf")]);

        let codes: Vec<_> = installed(&layout).into_iter().map(|r| r.product).collect();
        assert_eq!(codes, ["wowf"]);
    }

    /// Two adopted installs can both hold Retail. Agent keeps one record per
    /// uid, so it gets the copy the config plays, under that library's drive.
    #[test]
    fn a_game_in_two_libraries_is_recorded_once_where_the_config_puts_it() {
        let dir = tempfile::tempdir().unwrap();
        let main = Library::main_at(dir.path().join("games"), "home");
        let mut other = Library::adopted(dir.path().join("ssd").join("Games"), "SSD");
        other.games_letter = "f:".into();
        let paths = |libraries: Libraries| {
            let layout = Layout::with_root(dir.path()).with_libraries(libraries);
            for library in layout.libraries().all() {
                wow_install(&layout, library, &[("_retail_", "wow")]);
            }
            installed(&layout).into_iter().map(|r| (r.product, r.install_path)).collect::<Vec<_>>()
        };

        let mut assigned = Libraries::new(vec![main.clone(), other.clone()]);
        assigned.assign(ProductCode::Retail, other.id.clone());
        assert_eq!(paths(assigned), [("wow".to_owned(), "F:/World of Warcraft".to_owned())]);

        // Assigned nowhere: the default library, as Lightforge would play it.
        let unassigned = Libraries::new(vec![other, main]);
        assert_eq!(
            paths(unassigned),
            [("wow".to_owned(), crate::paths::WINDOWS_WOW_PATH.to_owned())]
        );
    }
}
