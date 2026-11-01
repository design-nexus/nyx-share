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
       nyx-share computers [add ADDRESS|DEVICE | remove NAME | trust NAME | untrust NAME]
       nyx-share nas [add NAME URI [--user U] [--domain D] [--tailscale URI] [--skip-existing]
                      [--password-stdin] | remove NAME | test NAME]
       nyx-share version

Opens the Share Sheet for the files, text and links given.

Options:
  --text TEXT          Share TEXT (may be repeated)
  --url URL            Share a link (may be repeated)
  --to DESTINATION     Send straight to a destination, without the Share Sheet
                       (see `nyx-share destinations`)
  --device NAME        Send straight to a computer or nearby device, by name
  --nas NAME           Send straight to a NAS folder, by name
  --subfolder PATH     With --nas: into this folder under the NAS folder
  --folder DIR         The folder for --to save
  --no-wait            Don't wait for --to to finish
  -h, --help           Show this help
";

fn main() -> ExitCode {
    // Exit quietly when the reader goes away, as in `nyx-share jobs | head`.
    // SAFETY: before any other thread starts.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
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
                println!("No transfers yet.");
            } else {
                for j in jobs {
                    println!("{}  {:<9} {}  {}", j.id, format!("{:?}", j.state).to_lowercase(), j.destination_name, describe(&j));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        // Started by Nyx Files from its provider registration; not for people.
        Some("files-provider") => Ok(files_provider::run()),
        Some("computers") => computers(&args[1..]),
        Some("nas") => nas(&args[1..]),
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
    let (mut to, mut folder, mut wait, mut subfolder) = (None, None, true, None);
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
            "--device" => to = Some(device(&value(&mut it, "--device")?)?),
            "--nas" => to = Some(nas_destination(&value(&mut it, "--nas")?)?),
            "--subfolder" => subfolder = Some(value(&mut it, "--subfolder")?),
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
    let options = ShareOptions { app_name: Some("Terminal".into()), folder, subfolder, notify: Some(!wait), notify_after: None };
    let id = client.share_to(&to, &content, &options)?;
    if !wait {
        println!("{id}");
        return Ok(ExitCode::SUCCESS);
    }
    let tty = std::io::stderr().is_terminal();
    let job = client.wait(&id, |job| {
        if tty && job.state == JobState::Running {
            match &job.step {
                Some(step) => eprint!("\r\x1b[2K{step}"),
                None if job.bytes_total > 0 => {
                    let pct = job.fraction().unwrap_or(0.0) * 100.0;
                    eprint!("\r\x1b[2K{} … {pct:.0}%", job.destination_name);
                }
                None => {}
            }
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

/// The destination id of the device or computer called `name`.
fn device(name: &str) -> Result<String, Error> {
    let client = Client::connect()?;
    let _ = client.scan();
    let dests = client.destinations(&[], 0)?;
    let devices: Vec<_> = dests
        .iter()
        .filter(|d| matches!(d.section, nyx_share_core::Section::Devices | nyx_share_core::Section::Nearby))
        .collect();
    devices
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(name))
        .or_else(|| devices.iter().find(|d| d.name.to_lowercase().starts_with(&name.to_lowercase())))
        .map(|d| d.id.clone())
        .ok_or_else(|| {
            let known: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            Error::Refused(if known.is_empty() {
                format!("No device called {name}, and none are in reach")
            } else {
                format!("No device called {name}. In reach: {}", known.join(", "))
            })
        })
}

fn nas_destination(name: &str) -> Result<String, Error> {
    let list = Client::connect()?.nas()?;
    list.iter().find(|n| n.name.eq_ignore_ascii_case(name)).map(|n| format!("nas:{}", n.id)).ok_or_else(|| {
        let known: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
        Error::NotFound(if known.is_empty() {
            "No NAS folders are saved. Add one with: nyx-share nas add NAME URI".into()
        } else {
            format!("No NAS folder called {name}. Saved: {}", known.join(", "))
        })
    })
}

fn nas(args: &[String]) -> Result<ExitCode, Error> {
    let client = Client::connect()?;
    let find = |name: &str| -> Result<nyx_share_client::NasInfo, Error> {
        client
            .nas()?
            .into_iter()
            .find(|n| n.name.eq_ignore_ascii_case(name) || n.id == name)
            .ok_or_else(|| Error::NotFound(format!("No NAS folder called {name}")))
    };
    let arg = |i: usize, what: &str| args.get(i).cloned().ok_or_else(|| Error::Refused(format!("Which {what}?")));
    match args.first().map(String::as_str) {
        None => {
            let list = client.nas()?;
            if list.is_empty() {
                println!("No NAS folders. Add one with: nyx-share nas add NAME URI");
            }
            for n in list {
                let mut notes = vec![n.uri.clone()];
                if let Some(t) = &n.tailscale_uri {
                    notes.push(format!("Tailscale {t}"));
                }
                if n.has_password {
                    notes.push("password saved".into());
                }
                if n.conflict == "skip" {
                    notes.push("skips existing names".into());
                }
                println!("{:<20} {}", n.name, notes.join(" · "));
            }
        }
        Some("add") => {
            let mut n = nyx_share_client::NasInfo {
                name: arg(1, "name")?,
                uri: arg(2, "address (smb://, sftp://, nfs:// or file://)")?,
                conflict: "keep-both".into(),
                ..Default::default()
            };
            let mut password = false;
            let mut it = args[3..].iter();
            while let Some(flag) = it.next() {
                let mut val = || it.next().cloned().ok_or_else(|| Error::Refused(format!("{flag} needs a value")));
                match flag.as_str() {
                    "--user" => n.username = Some(val()?),
                    "--domain" => n.domain = Some(val()?),
                    "--tailscale" => n.tailscale_uri = Some(val()?),
                    "--skip-existing" => n.conflict = "skip".into(),
                    "--password-stdin" => password = true,
                    other => return Err(Error::Refused(format!("Unknown option {other}"))),
                }
            }
            if let Some(existing) = client.nas()?.into_iter().find(|x| x.name.eq_ignore_ascii_case(&n.name)) {
                n.id = existing.id;
            }
            if password && n.id.is_empty() {
                n.id = uuid::Uuid::new_v4().to_string();
            }
            if password {
                let mut secret = String::new();
                std::io::stdin().read_line(&mut secret)?;
                let secret = secret.trim_end_matches(['\r', '\n']);
                nyx_share_nas::secrets::store(&n.id, &n.name, secret).map_err(Error::Failed)?;
            }
            let id = client.save_nas(&n)?;
            println!("Saved {} ({id})", n.name);
        }
        Some("remove") => {
            let n = find(&arg(1, "NAS folder")?)?;
            client.remove_nas(&n.id)?;
            println!("Removed {}", n.name);
        }
        Some("test") => {
            let n = find(&arg(1, "NAS folder")?)?;
            println!("{}", client.test_nas(&n.id)?);
        }
        Some(other) => return Err(Error::Refused(format!("Unknown nas command {other}. See --help."))),
    }
    Ok(ExitCode::SUCCESS)
}

fn computers(args: &[String]) -> Result<ExitCode, Error> {
    let client = Client::connect()?;
    let find = |name: &str| -> Result<nyx_share_client::ComputerInfo, Error> {
        client
            .computers()?
            .into_iter()
            .find(|c| c.name.eq_ignore_ascii_case(name) || c.id == name)
            .ok_or_else(|| Error::NotFound(format!("No saved computer called {name}")))
    };
    let arg = |i: usize, what: &str| args.get(i).cloned().ok_or_else(|| Error::Refused(format!("Which {what}?")));
    match args.first().map(String::as_str) {
        None => {
            let list = client.computers()?;
            if list.is_empty() {
                println!("No saved computers. Add one with: nyx-share computers add ADDRESS");
            }
            for c in list {
                let addrs: Vec<String> = [c.lan.map(|a| format!("LAN {a}")), c.tailscale.map(|a| format!("Tailscale {a}"))]
                    .into_iter()
                    .flatten()
                    .collect();
                let trust = if c.trusted { " · trusted" } else { "" };
                println!("{:<20} {:<9} {}{trust}", c.name, c.presence, addrs.join(" · "));
            }
        }
        Some("add") => {
            let from = arg(1, "address or device")?;
            // A nearby device by name, else an address.
            let from = match client
                .destinations(&[], 0)?
                .into_iter()
                .find(|d| d.provider == "localsend" && d.name.eq_ignore_ascii_case(&from))
            {
                Some(d) => d.id,
                None => from,
            };
            let c = client.add_computer(&from)?;
            println!(
                "Saved {} (key {}…{})",
                c.name,
                &c.fingerprint[..8.min(c.fingerprint.len())],
                &c.fingerprint[c.fingerprint.len().saturating_sub(8)..]
            );
        }
        Some("remove") => {
            let c = find(&arg(1, "computer")?)?;
            client.remove_computer(&c.id)?;
            println!("Removed {}", c.name);
        }
        Some(t @ ("trust" | "untrust")) => {
            let mut c = find(&arg(1, "computer")?)?;
            c.trusted = t == "trust";
            client.save_computer(&c)?;
            println!("{}: {}", c.name, if c.trusted { "files are received without asking" } else { "every offer asks first" });
        }
        Some(other) => return Err(Error::Refused(format!("Unknown computers command {other}. See --help."))),
    }
    Ok(ExitCode::SUCCESS)
}

fn describe(job: &Job) -> String {
    match (&job.state, &job.message) {
        (JobState::Running, _) => match &job.step {
            Some(step) => step.clone(),
            None => format!("{} of {}", units::size(job.bytes_done), units::size(job.bytes_total)),
        },
        (_, Some(m)) => m.clone(),
        _ => String::new(),
    }
}
