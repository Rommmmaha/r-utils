use clap::Parser;
mod cmd_cycle;
mod cmd_mute;
mod cmd_ptt;
mod cmd_stt;
mod cmd_volume;
mod overlay;
mod utils;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(clap::Subcommand)]
enum Commands {
    Cycle,
    Vol {
        amount: String,
    },
    Ptt,
    Mute,
    Stt,
}
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Cycle => cmd_cycle::run()?,
        Commands::Vol { amount } => cmd_volume::run(&amount)?,
        Commands::Ptt => cmd_ptt::run()?,
        Commands::Mute => cmd_mute::run()?,
        Commands::Stt => cmd_stt::run()?,
    }
    Ok(())
}
