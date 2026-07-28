use regex::Regex;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
struct ImgJob {
    idx: usize,
    id: String,
    out_path: PathBuf,
}
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        return decode_and_copy(&args[1]);
    }
    list_and_render()
}
fn decode_and_copy(input_item: &str) -> Result<(), Box<dyn Error>> {
    let mut decode_proc = Command::new("cliphist")
        .arg("decode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = decode_proc.stdin.take() {
        stdin.write_all(input_item.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let decode_stdout = decode_proc
        .stdout
        .take()
        .ok_or("Failed to capture cliphist stdout")?;
    let mut wl_copy_proc = Command::new("wl-copy").stdin(decode_stdout).spawn()?;
    wl_copy_proc.wait()?;
    decode_proc.wait()?;
    Ok(())
}
fn cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Ok(xdg_cache) = env::var("XDG_CACHE_HOME") {
        if !xdg_cache.is_empty() {
            return Ok(PathBuf::from(xdg_cache).join("cliphist/img"));
        }
    }
    let home = env::var("HOME").map_err(|_| "HOME environment variable is not set")?;
    Ok(PathBuf::from(home).join(".cache/cliphist/img"))
}
fn list_and_render() -> Result<(), Box<dyn Error>> {
    let tmp_dir = cache_dir()?;
    fs::create_dir_all(&tmp_dir)?;
    let skip_re = Regex::new(r"^[0-9]+\s<meta http-equiv=")?;
    let match_re = Regex::new(r"^([0-9]+)\s(?:\[\[\s)?binary.*(jpg|jpeg|png|bmp)")?;
    let list_proc = Command::new("cliphist")
        .arg("list")
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout_reader = BufReader::new(
        list_proc
            .stdout
            .ok_or("Failed to capture cliphist list stdout")?,
    );
    let mut lines: Vec<String> = Vec::new();
    let mut jobs: Vec<ImgJob> = Vec::new();
    let mut results: Vec<Option<PathBuf>> = Vec::new();
    let mut valid_names: HashSet<String> = HashSet::new();
    for line_result in stdout_reader.lines() {
        let line = line_result?;
        if skip_re.is_match(&line) {
            continue;
        }
        if let Some(caps) = match_re.captures(&line) {
            let id = caps[1].to_string();
            let ext = caps[2].to_string();
            let filename = format!("{}.{}", id, ext);
            let out_path = tmp_dir.join(&filename);
            valid_names.insert(filename);
            let idx = lines.len();
            lines.push(line);
            if is_cached(&out_path) {
                results.push(Some(out_path));
            } else {
                results.push(None);
                jobs.push(ImgJob { idx, id, out_path });
            }
        } else {
            lines.push(line);
            results.push(None);
        }
    }
    if !jobs.is_empty() {
        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(jobs.len());
        let mut chunks: Vec<Vec<ImgJob>> = (0..num_workers).map(|_| Vec::new()).collect();
        for (i, job) in jobs.into_iter().enumerate() {
            chunks[i % num_workers].push(job);
        }
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                thread::spawn(move || {
                    let mut done = Vec::with_capacity(chunk.len());
                    for job in chunk {
                        let ok = decode_to_file(&job.id, &job.out_path).is_ok();
                        done.push((job.idx, if ok { Some(job.out_path) } else { None }));
                    }
                    done
                })
            })
            .collect();
        for handle in handles {
            let done = handle
                .join()
                .map_err(|_| "a decode worker thread panicked")?;
            for (idx, path) in done {
                results[idx] = path;
            }
        }
    }
    prune_stale_cache(&tmp_dir, &valid_names)?;
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    for (i, line) in lines.iter().enumerate() {
        match &results[i] {
            Some(path) => writeln!(out, "{}\x00icon\x1f{}", line, path.display())?,
            None => writeln!(out, "{}", line)?,
        }
    }
    out.flush()?;
    Ok(())
}
fn is_cached(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}
fn decode_to_file(id: &str, out_path: &Path) -> Result<(), Box<dyn Error>> {
    let out_file = File::create(out_path)?;
    let mut decode_proc = Command::new("cliphist")
        .arg("decode")
        .stdin(Stdio::piped())
        .stdout(out_file)
        .spawn()?;
    if let Some(mut stdin) = decode_proc.stdin.take() {
        stdin.write_all(format!("{}\t\n", id).as_bytes())?;
    }
    let status = decode_proc.wait()?;
    if status.success() {
        Ok(())
    } else {
        let _ = fs::remove_file(out_path);
        Err("cliphist decode failed".into())
    }
}
fn prune_stale_cache(tmp_dir: &Path, valid_names: &HashSet<String>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(tmp_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !valid_names.contains(name.as_ref()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}
