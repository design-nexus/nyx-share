//! nyx-share: share from the command line through the Nyx Share service.

mod files_provider;

use nyx_share_client::{Client, Content, Error, Job, JobState, ShareOptions};
use nyx_share_core::units;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: nyx-share [OPTIONS] [FILE...]
       nyx-share destinations [--json]
       nyx-share jobs [--json]
       nyx-share cancel JOB
       nyx-share version

Opens the Share Sheet for the files, text and links given.

Options:
  --text TEXT          Share TEXT (may be repeated)
  --url URL            Share a link (may be repeated)
  --to DESTINATION     Send straight to a destination, without the Share Sheet
                       (see `nyx-share destinations`)
  --folder DIR         The folder for --to save
  --no-wait            Don't wait for --to to finish
  -h, --help           Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nyx-share: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, Error> {
    let json = args.iter().any(|a| a == "--json");
    match args.first().map(String::as_str) {
        None => {
            eprint!("{USAGE}");
            Ok(ExitCode::from(2))
        }
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some("version" | "--version") => {
            println!("nyx-share {}", env!("CARGO_PKG_VERSION"));
            match Client::connect().and_then(|c| c.version()) {
                Ok(v) => println!("service API {v}"),
                Err(e) => println!("service: {e}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("destinations") => {
            let dests = Client::connect()?.destinations(&[], 0)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&dests).unwrap_or_default());
            } else {
                for d in dests {
                    let kinds: Vec<&str> = d.kinds.iter().map(|k| k.as_str()).collect();
                    let mut notes = vec![kinds.join(", ")];
                    if let Some(max) = d.max_items {
                        notes.push(format!("{max} at a time"));
                    }
                    if !d.available {
                        notes.push(d.status.clone().unwrap_or_else(|| "unavailable".into()));
                    }
                    if d.pinned {
                        notes.push("pinned".into());
                    }
                    if d.hidden {
                        notes.push("hidden".into());
                    }
                    println!("{:<12} {:<16} {}", d.id, d.name, notes.join(" · "));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("jobs") => {
            let jobs = Client::connect()?.jobs()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs).unwrap_or_default());
            } else if jobs.is_empty() {
                println!("No transfers since the service started.");
            } else {
                for j in jobs {
                    println!("{}  {:<9} {}  {}", j.id, format!("{:?}", j.state).to_lowercase(), j.destination_name, describe(&j));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        // Started by Nyx Files from its provider registration; not for people.
        Some("files-provider") => Ok(files_provider::run()),
        Some("cancel") => {
            let id = args.get(1).ok_or_else(|| Error::Refused("Which job? See `nyx-share jobs`.".into()))?;
            Client::connect()?.cancel(id)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(_) => share(args),
    }
}

fn share(args: &[String]) -> Result<ExitCode, Error> {
    let mut content = Content::new();
    let (mut to, mut folder, mut wait) = (None, None, true);
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<'_, String>, flag: &str| {
        it.next().cloned().ok_or_else(|| Error::Refused(format!("{flag} needs a value")))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--text" => {
                content.text(value(&mut it, "--text")?);
            }
            "--url" => {
                content.url(value(&mut it, "--url")?);
            }
            "--to" | "--destination" => to = Some(value(&mut it, "--to")?),
            "--folder" => folder = Some(value(&mut it, "--folder")?),
            "--no-wait" => wait = false,
            "--" => {
                for path in it.by_ref() {
                    content.file(path)?;
                }
            }
            flag if flag.starts_with('-') => return Err(Error::Refused(format!("Unknown option {flag}. See --help."))),
            path => {
                content.file(path)?;
            }
        }
    }
    if content.is_empty() {
        return Err(Error::Refused("Nothing to share. See --help.".into()));
    }
    let folder = folder.map(|f| std::path::absolute(&f).map(|p| p.display().to_string())).transpose()?;
    let client = Client::connect()?;
    let Some(to) = to else {
        if folder.is_some() {
            return Err(Error::Refused("--folder only applies with --to".into()));
        }
        client.share(&content, &ShareOptions { app_name: Some("Terminal".into()), ..Default::default() })?;
        return Ok(ExitCode::SUCCESS);
    };
    let options = ShareOptions { app_name: Some("Terminal".into()), folder, notify: Some(!wait) };
    let id = client.share_to(&to, &content, &options)?;
    if !wait {
        println!("{id}");
        return Ok(ExitCode::SUCCESS);
    }
    let tty = std::io::stderr().is_terminal();
    let job = client.wait(&id, |job| {
        if tty && job.state == JobState::Running && job.bytes_total > 0 {
            let pct = job.fraction().unwrap_or(0.0) * 100.0;
            eprint!("\r\x1b[2K{} … {pct:.0}%", job.destination_name);
            let _ = std::io::stderr().flush();
        }
    })?;
    if tty {
        eprint!("\r\x1b[2K");
    }
    match job.state {
        JobState::Completed => {
            println!("{}", job.message.unwrap_or_default());
            for out in &job.outputs {
                println!("{out}");
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("nyx-share: {}", job.message.unwrap_or_else(|| "The transfer stopped".into()));
            Ok(ExitCode::FAILURE)
        }
    }
}

fn describe(job: &Job) -> String {
    match (&job.state, &job.message) {
        (JobState::Running, _) => format!("{} of {}", units::size(job.bytes_done), units::size(job.bytes_total)),
        (_, Some(m)) => m.clone(),
        _ => String::new(),
    }
}
