use anyhow::{Context, Result};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::CreateTranscriptionRequestArgs;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::env;
use std::process::Stdio;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};

const AUDIO_PATH: &str = "/tmp/r_stt.wav";

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = env::var("GROQ_API_KEY").context("GROQ_API_KEY not set")?;
    let mut child = Command::new("pw-record")
        .arg(AUDIO_PATH)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn pw-record — is pipewire installed?")?;
    let mut sigusr1 = signal(SignalKind::user_defined1())?;
    sigusr1.recv().await;
    kill(
        Pid::from_raw(child.id().context("child already exited")? as i32),
        Signal::SIGTERM,
    )?;
    child.wait().await?;
    if !std::path::Path::new(AUDIO_PATH).exists() {
        anyhow::bail!("recording failed — {AUDIO_PATH} not found");
    }
    let config = OpenAIConfig::default()
        .with_api_base("https://api.groq.com/openai/v1")
        .with_api_key(api_key);
    let client = Client::with_config(config);
    let request = CreateTranscriptionRequestArgs::default()
        .model("whisper-large-v3")
        .file(std::path::PathBuf::from(AUDIO_PATH))
        .temperature(0.0_f32)
        .build()?;
    let response = client.audio().transcribe(request).await?;
    print!("{}", response.text.trim());
    std::fs::remove_file(AUDIO_PATH).ok();
    Ok(())
}
