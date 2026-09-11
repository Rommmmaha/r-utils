use crate::utils;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};

/// Release action: end a live ptt/stt session, then mute. The pid is verified
/// via cmdline so a recycled pid can never catch a stray SIGUSR1.
pub fn run() -> anyhow::Result<()> {
    if let Ok(pid) = std::fs::read_to_string(utils::SESSION_PID_FILE)
        .unwrap_or_default()
        .trim()
        .parse::<i32>()
    {
        let cmdline =
            std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline.contains("r_multitool") {
            let _ = kill(Pid::from_raw(pid), Signal::SIGUSR1);
        }
    }
    utils::exec_silent("wpctl", &["set-volume", "@DEFAULT_AUDIO_SOURCE@", "0.0"]);
    Ok(())
}
