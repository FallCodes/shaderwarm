use std::fs;
use std::path::Path;

use shaderwarm_core::steam::Steam;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn manifest(appid: u32, name: &str, installdir: &str) -> String {
    format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{installdir}\"\n}}\n"
    )
}

#[test]
fn discovers_games_across_libraries_and_skips_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Steam");
    let second = tmp.path().join("SteamLibrary");
    let unmounted = tmp.path().join("gone");

    write(
        &root.join("steamapps/libraryfolders.vdf"),
        &format!(
            "\"libraryfolders\"\n{{\n\"0\" {{ \"path\" \"{}\" }}\n\"1\" {{ \"path\" \"{}\" }}\n\"2\" {{ \"path\" \"{}\" }}\n}}\n",
            root.display(),
            second.display(),
            unmounted.display()
        ),
    );
    write(&root.join("steamapps/appmanifest_1384160.acf"), &manifest(1384160, "GUILTY GEAR -STRIVE-", "GGST"));
    write(&root.join("steamapps/appmanifest_1493710.acf"), &manifest(1493710, "Proton Experimental", "Proton - Experimental"));
    write(&second.join("steamapps/appmanifest_2050650.acf"), &manifest(2050650, "Resident Evil 4", "RESIDENT EVIL 4  BIOHAZARD RE4"));
    write(&second.join("steamapps/appmanifest_1628350.acf"), &manifest(1628350, "Steam Linux Runtime 3.0 (sniper)", "SteamLinuxRuntime_sniper"));
    write(&second.join("steamapps/appmanifest_broken.acf"), "\"AppState\" {");

    // A Proton install referenced by config_info, so the root resolves.
    let proton = tmp.path().join("compat/GE-Proton11-11");
    write(&proton.join("proton"), "#!/usr/bin/env python3\n");
    write(&proton.join("version"), "1792432585 GE-Proton11-11\n");
    write(
        &second.join("steamapps/compatdata/2050650/config_info"),
        &format!("CachyOS-11.0-100\n{}/files/share/fonts/\n", proton.display()),
    );

    let steam = Steam::from_root(&root).unwrap();
    assert_eq!(steam.libraries.len(), 2, "unmounted library must be dropped");

    let games = steam.games();
    let names: Vec<_> = games.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["GUILTY GEAR -STRIVE-", "Resident Evil 4"]);

    let re4 = steam.game(2050650).unwrap();
    assert_eq!(re4.install_dir, second.canonicalize().unwrap().join("steamapps/common/RESIDENT EVIL 4  BIOHAZARD RE4"));
    assert!(re4.shadercache().ends_with("steamapps/shadercache/2050650"));
    let info = re4.proton().unwrap();
    assert_eq!(info.name, "GE-Proton11-11");
    assert_eq!(info.root.as_deref(), Some(proton.as_path()));

    assert!(steam.game(1384160).unwrap().proton().is_none());
}
