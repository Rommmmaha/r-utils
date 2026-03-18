use anyhow::{Context, Result};
use clap::Parser;
use content_inspector::{ContentType, inspect};
use ignore::WalkBuilder;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(default_value = ".")]
    input: PathBuf,
    #[arg(short, long, default_value = "codebase_dump.md")]
    output: PathBuf,
    #[arg(long)]
    hidden: bool,
    #[arg(short, long)]
    verbose: bool,
}

// List of specific filenames to always ignore
const IGNORED_FILES: &[&str] = &["Cargo.lock", "gradlew", "gradlew.bat"];

fn main() -> Result<()> {
    let args = Args::parse();
    let current_dir = env::current_dir().context("Failed to get current directory")?;
    let input_path = fs::canonicalize(&args.input)
        .context(format!("Failed to resolve input path: {:?}", args.input))?;

    let mut output_file = File::create(&args.output)
        .context(format!("Failed to create output file: {:?}", args.output))?;

    println!("Scanning: {}", input_path.display());
    println!("Output:   {}", args.output.display());

    writeln!(output_file, "# Codebase Dump\n")?;
    writeln!(
        output_file,
        "> **NOTICE FOR AI:** The following content is a representation of a codebase state for context. Do not copy the Markdown formatting or structure used in this dump for your own output responses.\n"
    )?;
    writeln!(
        output_file,
        "- **Root Directory:** `{}`",
        make_relative(&input_path, &current_dir).display()
    )?;
    writeln!(
        output_file,
        "- **Generated on:** {}\n",
        chrono::Local::now().to_rfc3339()
    )?;
    writeln!(output_file, "---\n")?;

    let mut builder = WalkBuilder::new(&input_path);
    builder
        .hidden(!args.hidden)
        .git_ignore(true)
        .require_git(false)
        .git_global(true)
        .git_exclude(true);

    let output_abs = fs::canonicalize(&args.output).unwrap_or_else(|_| args.output.clone());
    let walker = builder.build();

    let mut file_count = 0;
    let mut total_bytes: u64 = 0;
    let mut processed_paths: Vec<PathBuf> = Vec::new();
    let mut llms_txt_path: Option<PathBuf> = None;
    let target_llms_txt = input_path.join("llms.txt");

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();

                if path.is_dir() {
                    continue;
                }

                // --- IGNORE LOGIC START ---
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if IGNORED_FILES.contains(&file_name) {
                        if args.verbose {
                            println!("Auto-ignoring: {}", file_name);
                        }
                        continue;
                    }
                }
                // --- IGNORE LOGIC END ---

                if let Ok(abs_path) = fs::canonicalize(path) {
                    if abs_path == output_abs {
                        continue;
                    }
                    if abs_path == target_llms_txt {
                        llms_txt_path = Some(abs_path);
                        continue;
                    }
                }

                if process_file(
                    path,
                    &mut output_file,
                    &args,
                    &mut file_count,
                    &mut total_bytes,
                    &current_dir,
                )
                .is_ok()
                {
                    processed_paths.push(path.to_path_buf());
                }
            }
            Err(err) => eprintln!("Error walking directory: {}", err),
        }
    }

    if let Some(path) = llms_txt_path {
        if process_file(
            &path,
            &mut output_file,
            &args,
            &mut file_count,
            &mut total_bytes,
            &current_dir,
        )
        .is_ok()
        {
            processed_paths.push(path);
        }
    }

    println!("Done! Processed {} files.", file_count);
    println!(
        "Total dump size: {:.2} MB",
        total_bytes as f64 / 1024.0 / 1024.0
    );

    Ok(())
}

fn process_file(
    path: &Path,
    output: &mut File,
    args: &Args,
    count: &mut usize,
    total_bytes: &mut u64,
    base_dir: &Path,
) -> Result<()> {
    if is_binary(path)? {
        if args.verbose {
            println!("Skipping binary: {:?}", path);
        }
        return Ok(());
    }

    let metadata = fs::metadata(path)?;
    let size = metadata.len();
    let modified: SystemTime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mod_fmt: chrono::DateTime<chrono::Local> = modified.into();
    let display_path = make_relative(path, base_dir);
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    if args.verbose {
        println!("Adding: {:?}", display_path);
    }

    writeln!(output, "## File: `./{}`", display_path.display())?;
    writeln!(
        output,
        "- **Size:** {} bytes | **Modified:** {}",
        size,
        mod_fmt.format("%Y-%m-%d %H:%M:%S")
    )?;
    writeln!(output, "\n```{}", extension)?;

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        match line {
            Ok(l) => {
                let escaped_line = l.replace("```", "` ` `");
                writeln!(output, "{}", escaped_line)?;
            }
            Err(_) => {
                writeln!(output, "[NON-UTF8-SEQUENCE-REMOVED]")?;
            }
        }
    }

    writeln!(output, "```\n\n---\n")?;

    *count += 1;
    *total_bytes += size;
    Ok(())
}

fn is_binary(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buffer = [0; 1024];
    let n = io::Read::read(&mut file, &mut buffer)?;
    if n == 0 {
        return Ok(false);
    }
    Ok(inspect(&buffer[..n]) == ContentType::BINARY)
}

fn make_relative(path: &Path, base: &Path) -> PathBuf {
    match path.strip_prefix(base) {
        Ok(p) => p.to_path_buf(),
        Err(_) => path.to_path_buf(),
    }
}
