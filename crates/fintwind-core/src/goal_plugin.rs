use anyhow::Context as _;
use fintwind_protocol::goal::{GoalAction, GoalSnapshot};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

static ENABLED: AtomicBool = AtomicBool::new(false);
static REVISION: AtomicU64 = AtomicU64::new(0);

fn settings_files() -> &'static Mutex<Vec<Weak<PathBuf>>> {
    static FILES: OnceLock<Mutex<Vec<Weak<PathBuf>>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(Vec::new()))
}

fn write_settings(path: &Path, enabled: bool, revision: u64) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&json!({ "enabled": enabled, "revision": revision }))?;
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let result = std::fs::write(path, bytes);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result.context("could not update private goal plugin settings")
}

pub(crate) fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    let mut files = settings_files()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let revision = if ENABLED.swap(enabled, Ordering::SeqCst) != enabled {
        REVISION.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        REVISION.load(Ordering::SeqCst)
    };
    let mut failure = None;
    files.retain(|file| {
        if let Some(path) = file.upgrade() {
            if let Err(error) = write_settings(&path, enabled, revision) {
                failure = Some(error);
            }
            true
        } else {
            false
        }
    });
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) struct GoalPlugin {
    directory: PathBuf,
    settings: Arc<PathBuf>,
}

impl GoalPlugin {
    pub(crate) fn create(parent: &Path) -> anyhow::Result<Self> {
        let directory = parent.join(format!("goal-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let plugin = Self {
            settings: Arc::new(directory.join("settings.json")),
            directory,
        };
        std::fs::write(
            plugin.directory.join("index.ts"),
            include_str!("../../../resources/opencode-goal-plugin.ts"),
        )?;
        let mut files = settings_files()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        write_settings(
            &plugin.settings,
            ENABLED.load(Ordering::SeqCst),
            REVISION.load(Ordering::SeqCst),
        )?;
        files.push(Arc::downgrade(&plugin.settings));
        Ok(plugin)
    }

    pub(crate) fn config(&self, inherited: Option<&str>) -> anyhow::Result<String> {
        let inherited = inherited
            .map(str::to_owned)
            .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
        let mut config: Value = match inherited.filter(|text| !text.trim().is_empty()) {
            Some(text) => serde_json::from_str(&text).context("invalid inline OpenCode config")?,
            None => json!({}),
        };
        let object = config
            .as_object_mut()
            .context("inline OpenCode config must be an object")?;
        let plugins = object
            .entry("plugins")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("inline OpenCode plugins must be an array")?;
        plugins.push(json!(self.directory));
        // The settlement tool only records the model's verdict in plugin
        // memory, so it must never sit behind a per-call approval prompt: in
        // ask modes that would stall every Goal turn.
        match object.entry("permission") {
            serde_json::map::Entry::Vacant(entry) => {
                entry.insert(json!({ "fintwind_goal_settle": "allow" }));
            }
            serde_json::map::Entry::Occupied(mut entry) => {
                if let Some(permissions) = entry.get_mut().as_object_mut() {
                    permissions.insert("fintwind_goal_settle".into(), json!("allow"));
                }
            }
        }
        Ok(serde_json::to_string(&config)?)
    }
}

impl Drop for GoalPlugin {
    fn drop(&mut self) {
        settings_files()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|file| !file.ptr_eq(&Arc::downgrade(&self.settings)));
        let _ = std::fs::remove_file(&*self.settings);
        let _ = std::fs::remove_file(self.directory.join("index.ts"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

pub(crate) fn control(
    server: &crate::opencode_session::OpenCodeServer,
    directory: &str,
    session_id: &str,
    action: GoalAction,
) -> anyhow::Result<Option<GoalSnapshot>> {
    control_with_input(server, directory, session_id, action, None)
}

pub(crate) fn control_with_input(
    server: &crate::opencode_session::OpenCodeServer,
    directory: &str,
    session_id: &str,
    action: GoalAction,
    input_id: Option<&str>,
) -> anyhow::Result<Option<GoalSnapshot>> {
    let mut body = json!({ "sessionID": session_id, "action": action });
    if let Some(input_id) = input_id {
        body["inputID"] = json!(input_id);
    }
    let response = server.request_for_directory_with_timeout(
        directory,
        "POST",
        "/api/rpc/fintwind.goal/control",
        Some(&json!({ "input": body })),
        std::time::Duration::from_secs(10),
    )?;
    serde_json::from_value(response.get("output").cloned().unwrap_or(response))
        .context("invalid goal plugin response")
}
