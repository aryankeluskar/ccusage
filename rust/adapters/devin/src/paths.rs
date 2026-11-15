use std::{collections::HashSet, env, path::PathBuf};

use crate::Result;

pub(super) const DEVIN_DATA_DIR_ENV: &str = "DEVIN_DATA_DIR";
pub(super) const DEVIN_SESSIONS_DIR_ENV: &str = "DEVIN_SESSIONS_DIR";

fn dirs_from_env(name: &str) -> Vec<PathBuf> {
    let Some(value) = env::var(name).ok() else {
        return Vec::new();
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn default_data_dir() -> Result<PathBuf> {
    if let Some(appdata) = env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(appdata).join("devin").join("cli"));
    }
    let home =
        crate::home::home_dir().ok_or_else(|| crate::cli_error("home directory is not set"))?;
    Ok(home.join(".local").join("share").join("devin").join("cli"))
}

pub(super) fn sessions_db_paths() -> Result<Vec<PathBuf>> {
    // `DEVIN_SESSIONS_DIR` is the Devin CLI's own relocation variable and holds
    // `sessions.db` directly; `DEVIN_DATA_DIR` names the data root that contains
    // `cli/sessions.db`. Both accept comma-separated lists.
    let mut dirs = dirs_from_env(DEVIN_SESSIONS_DIR_ENV);
    dirs.extend(
        dirs_from_env(DEVIN_DATA_DIR_ENV)
            .into_iter()
            .map(|root| root.join("cli")),
    );
    if dirs.is_empty() {
        dirs.push(default_data_dir()?);
    }
    let mut seen = HashSet::new();
    Ok(dirs
        .into_iter()
        .map(|dir| dir.join("sessions.db"))
        .filter(|path| path.is_file())
        .filter(|path| seen.insert(path.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use ccusage_test_support::{EnvVarsGuard, fs_fixture};

    use super::*;

    /// Sets the given variables, clears the other Devin discovery variables,
    /// and restores everything on drop.
    fn isolate(vars: &[(&'static str, Option<OsString>)]) -> EnvVarsGuard {
        let mut all: Vec<(&'static str, Option<OsString>)> =
            vec![(DEVIN_DATA_DIR_ENV, None), (DEVIN_SESSIONS_DIR_ENV, None)];
        all.extend_from_slice(vars);
        EnvVarsGuard::set_many(all)
    }

    #[test]
    fn discovers_sessions_db_under_devin_data_dir() {
        let fixture = fs_fixture!({
            "devin-data/cli/sessions.db": "",
        });
        let _env = isolate(&[(
            DEVIN_DATA_DIR_ENV,
            Some(fixture.path("devin-data").into_os_string()),
        )]);

        let paths = sessions_db_paths().unwrap();

        assert_eq!(paths, vec![fixture.path("devin-data/cli/sessions.db")]);
    }

    #[test]
    fn discovers_sessions_db_directly_under_devin_sessions_dir() {
        let fixture = fs_fixture!({
            "sessions-dir/sessions.db": "",
        });
        let _env = isolate(&[(
            DEVIN_SESSIONS_DIR_ENV,
            Some(fixture.path("sessions-dir").into_os_string()),
        )]);

        let paths = sessions_db_paths().unwrap();

        assert_eq!(paths, vec![fixture.path("sessions-dir/sessions.db")]);
    }

    #[test]
    fn keeps_sessions_dir_ahead_of_data_dir_entries() {
        let fixture = fs_fixture!({
            "sessions-dir/sessions.db": "",
            "devin-data/cli/sessions.db": "",
        });
        let data_dir = fixture.path("devin-data");
        let duplicated = format!("{},{}", data_dir.display(), data_dir.display());
        let _env = isolate(&[
            (
                DEVIN_SESSIONS_DIR_ENV,
                Some(fixture.path("sessions-dir").into_os_string()),
            ),
            (DEVIN_DATA_DIR_ENV, Some(duplicated.into())),
        ]);

        let paths = sessions_db_paths().unwrap();

        // Sessions-dir entries come first, and the data root passed twice in
        // the comma-separated list is read once.
        assert_eq!(
            paths,
            vec![
                fixture.path("sessions-dir/sessions.db"),
                fixture.path("devin-data/cli/sessions.db"),
            ]
        );
    }

    #[test]
    fn prefers_appdata_on_windows_style_environments() {
        let fixture = fs_fixture!({});
        let _ = fixture.create_dir_all("appdata/devin/cli");
        std::fs::write(fixture.path("appdata/devin/cli/sessions.db"), []).unwrap();
        let _env = isolate(&[("APPDATA", Some(fixture.path("appdata").into_os_string()))]);

        let paths = sessions_db_paths().unwrap();

        assert_eq!(paths, vec![fixture.path("appdata/devin/cli/sessions.db")]);
    }

    #[test]
    fn skips_missing_databases() {
        let fixture = fs_fixture!({});
        let _env = isolate(&[(
            DEVIN_DATA_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);

        let paths = sessions_db_paths().unwrap();

        assert!(paths.is_empty());
    }
}
