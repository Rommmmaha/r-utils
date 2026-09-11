use crate::overlay::{Overlay, PTT_METER_PATH, Phase};
use crate::utils;
use anyhow::{Context, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::signal::unix::SignalKind;

/// Push-to-talk, mirroring stt: unmute, badge the live mic continuously,
/// exit once told to stop (release bind signals, volume watch is backup).
pub fn run() -> anyhow::Result<()> {
    // One recorder at a time (ptt and stt share the mic trigger).
    let Some(_session) = utils::claim_session() else {
        return Ok(());
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Handlers first so an early release signal can't hit default-terminate.
    let mut sigusr1 = tokio::signal::unix::signal(SignalKind::user_defined1())?;
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
    let mut overlay = Overlay::spawn(Phase::MicOn, Some(PTT_METER_PATH));
    let mut child = tokio::process::Command::new("timeout")
        .arg("65")
        .arg("pw-record")
        .arg(PTT_METER_PATH)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn pw-record — is pipewire installed?")?;
    utils::set_mic_volume(utils::MIC_OPEN_VOL);

    tokio::select! {
        _ = utils::wait_recording_end() => {}
        _ = sigusr1.recv() => {}
        _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    };
    utils::stop_child(&mut child).await;
    std::fs::remove_file(PTT_METER_PATH).ok();
    utils::set_mic_volume("0.0");
    overlay.finish();
    Ok(())
}
