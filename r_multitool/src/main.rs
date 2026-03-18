use std::env;
mod cmd_cycle;
mod cmd_hyprland;
mod cmd_ptt;
mod cmd_volume;
mod utils;
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return;
    }
    match args[1].as_str() {
        "cycle" => cmd_cycle::run(),
        "vol" => {
            if args.len() > 2 {
                let _ = cmd_volume::run(&args[2]);
            }
        }
        "ptt" => {
            if args.len() > 2 {
                cmd_ptt::run(&args[2]);
            }
        }
        "ws" => {
            if args.len() > 2 {
                cmd_hyprland::run(&args[2]);
            }
        }
        _ => {}
    }
}
