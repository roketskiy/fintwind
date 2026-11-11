use std::path::PathBuf;

use anyhow::{anyhow, Context as _};

use crate::Arguments;

const PRODUCTION_DIRECTORY_NAME: &str = "Fintwind";

pub(super) struct StartupProfile {
    pub data_dir: PathBuf,
    pub task_path: PathBuf,
    pub settings_path: PathBuf,
    pub legacy_settings_path: PathBuf,
    pub isolated_settings: Option<PathBuf>,
}

pub(super) fn resolve(arguments: &Arguments) -> anyhow::Result<StartupProfile> {
    if let Some(directory) = arguments.data_dir.as_ref() {
        std::fs::create_dir_all(directory).context("could not create daemon data directory")?;
        return Ok(profile(directory.clone(), directory.join("daemon.json")));
    }

    if arguments.production_data {
        let data_dir = fintwind_protocol::profile_paths::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(PRODUCTION_DIRECTORY_NAME);
        std::fs::create_dir_all(&data_dir).context("could not create daemon data directory")?;
        if let Some(expected) = std::env::var_os("FINTWIND_MYGO_EXPECTED_DATA_DIR") {
            if std::fs::canonicalize(&data_dir)? != std::fs::canonicalize(expected)? {
                return Err(anyhow!("launcher and daemon production data paths differ"));
            }
        }
        let settings_path = fintwind_core::DaemonSettings::default_path();
        let legacy_settings_path = data_dir.join("settings.json");
        return Ok(StartupProfile {
            task_path: data_dir.join("app.db"),
            data_dir,
            settings_path,
            legacy_settings_path,
            isolated_settings: None,
        });
    }

    let task_path = fintwind_core::persistence::StateStore::default_path();
    let data_dir = task_path
        .parent()
        .ok_or_else(|| anyhow!("task database has no parent directory"))?
        .to_owned();
    std::fs::create_dir_all(&data_dir).context("could not create daemon data directory")?;
    let settings_path = fintwind_core::DaemonSettings::default_path();
    let legacy_settings_path = task_path.with_file_name("settings.json");
    Ok(StartupProfile {
        task_path,
        data_dir,
        settings_path,
        legacy_settings_path,
        isolated_settings: None,
    })
}

fn profile(data_dir: PathBuf, isolated_settings: PathBuf) -> StartupProfile {
    StartupProfile {
        task_path: data_dir.join("app.db"),
        settings_path: isolated_settings.clone(),
        legacy_settings_path: isolated_settings.clone(),
        isolated_settings: Some(isolated_settings),
        data_dir,
    }
}
