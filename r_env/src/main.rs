use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process;
fn secrets_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config/r/secrets.json")
}
fn init_secrets() -> Result<(), String> {
    let path = secrets_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    if !path.exists() {
        fs::write(&path, "{}")
            .map_err(|e| format!("Failed to create {}: {}", path.display(), e))?;
    }
    Ok(())
}
fn load_secrets() -> Result<HashMap<String, String>, String> {
    let path = secrets_path();
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| format!("{} must be a JSON object", path.display()))?;
    let mut secrets = HashMap::new();
    for (key, val) in obj {
        let s = val
            .as_str()
            .ok_or_else(|| format!("Value for '{}' must be a string", key))?;
        secrets.insert(key.clone(), s.to_string());
    }
    Ok(secrets)
}
fn escape_val(s: &str) -> String {
    s.replace('\'', "'\\''")
}
fn main() {
    let args: Vec<String> = env::args().collect();
    let args = &args[1..];
    let dash = args.iter().position(|a| a == "--");
    match dash {
        Some(pos) => {
            let keys = &args[..pos];
            let cmd = &args[pos + 1..];
            if keys.is_empty() {
                eprintln!("r_env: no keys specified before '--'");
                process::exit(1);
            }
            if cmd.is_empty() {
                eprintln!("r_env: no command specified after '--'");
                process::exit(1);
            }
            init_secrets().unwrap_or_else(|e| {
                eprintln!("r_env: {}", e);
                process::exit(1);
            });
            let secrets = load_secrets().unwrap_or_else(|e| {
                eprintln!("r_env: {}", e);
                process::exit(1);
            });
            for key in keys {
                match secrets.get(key) {
                    Some(val) => unsafe { env::set_var(key, val) },
                    None => {
                        eprintln!("r_env: key '{}' not found in secrets", key);
                        process::exit(1);
                    }
                }
            }
            let err = std::process::Command::new(&cmd[0]).args(&cmd[1..]).exec();
            eprintln!("r_env: exec failed: {}", err);
            process::exit(1);
        }
        None => {
            let keys = args;
            if keys.is_empty() {
                eprintln!("r_env: no keys specified");
                process::exit(1);
            }
            init_secrets().unwrap_or_else(|e| {
                eprintln!("r_env: {}", e);
                process::exit(1);
            });
            let secrets = load_secrets().unwrap_or_else(|e| {
                eprintln!("r_env: {}", e);
                process::exit(1);
            });
            for key in keys {
                match secrets.get(key) {
                    Some(val) => println!("export {}='{}';", key, escape_val(val)),
                    None => {
                        eprintln!("r_env: key '{}' not found in secrets", key);
                        process::exit(1);
                    }
                }
            }
        }
    }
}
