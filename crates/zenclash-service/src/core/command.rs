use serde::{Deserialize, Serialize};
use strum_macros::{AsRefStr, EnumString};

#[derive(Debug, Clone, Serialize, Deserialize, EnumString, AsRefStr)]
pub enum IpcCommand {
    #[strum(serialize = "/version")]
    GetVersion,
    #[strum(serialize = "/installation")]
    InspectInstallation,
    #[strum(serialize = "/status")]
    Status,
    #[strum(serialize = "/clash/logs")]
    GetClashLogs,

    #[strum(serialize = "/clash/log-snapshot")]
    GetClashLogSnapshot,

    #[strum(serialize = "/clash/start")]
    StartClash,
    #[strum(serialize = "/clash/stop")]
    StopClash,
    #[strum(serialize = "/clash/stage-runtime")]
    StageRuntime,
    #[strum(serialize = "/clash/runtime-file")]
    ReadRuntimeFile,
    #[strum(serialize = "/system-proxy")]
    SetSystemProxy,
    #[strum(serialize = "/writer")]
    UpdateWriter,
    #[strum(serialize = "/magic")]
    Magic,
}
