use crate::overlay::{AUDIO_PATH, Overlay, Phase};
use crate::utils;
use anyhow::{Context, Result};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::CreateTranscriptionRequestArgs;
use std::env;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::signal::unix::SignalKind;

pub fn run() -> Result<()> {
    let api_key = env::var("GROQ_API_KEY").context("GROQ_API_KEY not set")?;
    // One recorder at a time (ptt and stt share the mic trigger).
    let Some(_session) = utils::claim_session() else {
        return Ok(());
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(api_key))
}

enum Stop {
    Transcribe,
    Abort,
}

// Anything under ~5ms of audio is an accidental tap, not dictation.
const MIN_WAV: u64 = 1024;

async fn async_main(api_key: String) -> Result<()> {
    // Handlers first so an early release signal can't hit default-terminate.
    let mut sigusr1 = tokio::signal::unix::signal(SignalKind::user_defined1())?;
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
    let mut overlay = Overlay::spawn(Phase::Record, Some(AUDIO_PATH));
    let mut child = Command::new("timeout")
        .arg("65")
        .arg("pw-record")
        .arg(AUDIO_PATH)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn pw-record — is pipewire installed?")?;
    utils::set_mic_volume(utils::MIC_OPEN_VOL);

    // Release bind signals us (volume watch is the backup); every stop path
    // mutes, so end state never depends on mute/unmute landing order.
    let stop = tokio::select! {
        _ = utils::wait_recording_end() => Stop::Transcribe,
        _ = sigusr1.recv() => Stop::Transcribe,
        _ = tokio::time::sleep(Duration::from_secs(60)) => Stop::Transcribe,
        _ = sigint.recv() => Stop::Abort,
        _ = sigterm.recv() => Stop::Abort,
    };
    utils::stop_child(&mut child).await;
    utils::set_mic_volume("0.0");
    if matches!(stop, Stop::Abort) {
        std::fs::remove_file(AUDIO_PATH).ok();
        overlay.finish();
        std::process::exit(130);
    }

    let len = std::fs::metadata(AUDIO_PATH).map(|m| m.len()).unwrap_or(0);
    if len == 0 {
        overlay.finish();
        anyhow::bail!("recording failed — {AUDIO_PATH} not found");
    }
    if len < MIN_WAV {
        std::fs::remove_file(AUDIO_PATH).ok();
        overlay.finish();
        return Ok(());
    }
    overlay.set_phase(Phase::Transcribing);
    let config = OpenAIConfig::default()
        .with_api_base("https://api.groq.com/openai/v1")
        .with_api_key(api_key);
    let client = Client::with_config(config);
    let request = CreateTranscriptionRequestArgs::default()
        .model("whisper-large-v3")
        .file(std::path::PathBuf::from(AUDIO_PATH))
        .temperature(0.0_f32)
        .build()?;
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        client.audio().transcribe(request),
    )
    .await;
    std::fs::remove_file(AUDIO_PATH).ok();
    let response = response.context("transcribe timed out")??;
    print!("{}", response.text.trim());
    overlay.finish();
    Ok(())
}
