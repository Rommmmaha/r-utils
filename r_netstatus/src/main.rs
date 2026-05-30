use chrono::Local;
use colored::Colorize;
use std::env;
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::thread::sleep;
use std::time::{Duration, Instant};
fn main() {
    let input = env::args()
        .nth(1)
        .unwrap_or_else(|| "1.1.1.1:53".to_string());
    let target_addr = input
        .to_socket_addrs()
        .expect(
            "Failed to parse or resolve address (ensure you include the port, e.g., domain.com:80)",
        )
        .next()
        .expect("No socket addresses resolved");
    let timeout = Duration::from_secs(2);
    let mut last_status: Option<bool> = None;
    let mut last_change_instant = Instant::now();
    loop {
        let loop_start = Instant::now();
        let is_success = TcpStream::connect_timeout(&target_addr, timeout).is_ok();
        if last_status != Some(is_success) {
            let now_instant = Instant::now();
            if last_status.is_some() {
                let duration = now_instant.duration_since(last_change_instant);
                let output =
                    humantime::format_duration(Duration::from_secs(duration.as_secs())).to_string();
                if last_status.unwrap() {
                    println!("{}", output.green());
                } else {
                    println!("{}", output.red());
                }
            }
            let timestamp = Local::now().format("%H:%M:%S").to_string();
            let output = format!("{}", timestamp);
            if is_success {
                print!("{}│Online │", output.green());
            } else {
                print!("{}│Offline│", output.red());
            }
            io::stdout().flush().unwrap();
            last_status = Some(is_success);
            last_change_instant = now_instant;
        }
        let elapsed = loop_start.elapsed();
        if elapsed < timeout {
            sleep(timeout - elapsed);
        }
    }
}
