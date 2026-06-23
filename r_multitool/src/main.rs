use clap::Parser;
mod cmd_cycle;
mod cmd_ptt;
mod cmd_volume;
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
    Ptt {
        state: String,
        #[arg(long)]
        color: Option<String>,
    },
}
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Cycle => cmd_cycle::run()?,
        Commands::Vol { amount } => cmd_volume::run(&amount)?,
        Commands::Ptt { state, color } => cmd_ptt::run(&state, color)?,
    }
    Ok(())
}
