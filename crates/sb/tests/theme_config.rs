//! The theme file end to end (spec success criterion 8; Task 9). Each test
//! points `SB_CONFIG` at its own temporary file, never `/opt/sb`.

mod common;

/// sb's error block (a write to a full disk) with `config` as the theme
/// file, coloured though piped.
fn run_with_config(config: &str, profile: Option<&str>) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, config).expect("write config");
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let mut cmd = common::sb();
    cmd.args(["--color=always", "version"])
        .stdout(full)
        .env("SB_CONFIG", &path)
        .env_remove("SB_COLOR_PROFILE");
    if let Some(profile) = profile {
        cmd.env("SB_COLOR_PROFILE", profile);
    }
    String::from_utf8(cmd.output().expect("run sb").stderr).expect("utf-8")
}

#[test]
fn a_theme_override_changes_the_colours() {
    let stderr = run_with_config("[theme.colors]\nproblem = \"#102030\"\n", None);
    assert!(stderr.contains("\x1b[38;2;16;32;48m✗"), "{stderr:?}");
}

#[test]
fn the_theme_file_can_choose_the_profile() {
    let stderr = run_with_config("[theme]\ncolor_profile = \"ascii\"\n", None);
    assert!(!stderr.contains('\x1b'), "{stderr:?}");
}

/// The environment wins over the file's profile (ADR-002).
#[test]
fn the_environment_beats_the_files_profile() {
    let stderr = run_with_config("[theme]\ncolor_profile = \"ascii\"\n", Some("truecolor"));
    assert!(stderr.contains("\x1b[38;2;255;110;122m✗"), "{stderr:?}");
}

/// An invalid file warns and the built-in theme is used.
#[test]
fn an_invalid_file_warns_and_falls_back() {
    let stderr = run_with_config("[theme.colors]\nproblem = \"red\"\n", None);
    assert!(stderr.contains("warning:"), "{stderr:?}");
    assert!(stderr.contains("isn't a #RRGGBB colour"), "{stderr:?}");
    assert!(stderr.contains("using the built-in theme"), "{stderr:?}");
    assert!(stderr.contains("\x1b[38;2;255;110;122m✗"), "{stderr:?}");
}

/// A bad profile in the file is blamed on the file, not the environment.
#[test]
fn a_bad_profile_in_the_file_names_the_file() {
    let stderr = run_with_config("[theme]\ncolor_profile = \"bogus\"\n", None);
    assert!(
        stderr.contains("theme config") && stderr.contains("unknown colour profile"),
        "{stderr:?}"
    );
    assert!(!stderr.contains("SB_COLOR_PROFILE"), "{stderr:?}");
}

/// No file is the normal case: no warning, the built-in theme.
#[test]
fn no_file_is_quiet() {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = common::sb()
        .args(["--color=always", "version"])
        .stdout(full)
        .env("SB_CONFIG", "/nonexistent/sb/config.toml")
        .output()
        .expect("run sb");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(!stderr.contains("warning:"), "{stderr:?}");
}

/// The config path is shown escaped too (it may hold anything).
#[test]
fn the_path_in_the_warning_is_escaped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let odd = dir.path().join("odd\x1b[31mdir");
    std::fs::create_dir(&odd).expect("mkdir");
    let config = odd.join("config.toml");
    std::fs::write(&config, "[theme]\nname = \"nope\"\n").expect("write config");
    let out = common::sb()
        .args(["--color=never", "version"])
        .env("SB_CONFIG", &config)
        .output()
        .expect("run sb");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown theme"), "{stderr:?}");
    assert!(!out.stderr.contains(&0x1b), "{stderr:?}");
}
