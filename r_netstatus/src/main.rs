use chrono::Local;
use colored::Colorize;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn main() {
    let target_addr: SocketAddr = "1.1.1.1:53".parse().unwrap();
    let timeout = Duration::from_secs(1);
    let mut last_status: Option<bool> = None;
    let mut last_change_instant = Instant::now();
    loop {
        let loop_start = Instant::now();
        let is_success = TcpStream::connect_timeout(&target_addr, timeout).is_ok();
        if last_status != Some(is_success) {
            let now_instant = Instant::now();
            if last_status.is_some() {
                let duration = now_instant.duration_since(last_change_instant);
                let secs = duration.as_secs();
                let hh = secs / 3600;
                let mm = (secs % 3600) / 60;
                let ss = secs % 60;
                let output = format!("({:02}:{:02}:{:02})", hh, mm, ss);
                if last_status.unwrap() {
                    println!("{}", output.green());
                } else {
                    println!("{}", output.red());
                }
            }
            let timestamp = Local::now().format("%H:%M:%S").to_string();
            let output = format!("[{}]", timestamp);
            if is_success {
                println!("{} +", output.green());
            } else {
                println!("{} -", output.red());
            }
            io::stdout().flush().unwrap();
            last_status = Some(is_success);
            last_change_instant = now_instant;
        }
        let elapsed = loop_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            sleep(Duration::from_secs(1) - elapsed);
        }
    }
}
