//! Makes and checks supporter keys.
//!
//! ```text
//! supporter-key keygen                    make the signing key, once
//! supporter-key public                    the signing key's public half
//! supporter-key issue [--tier TIER] [--name NAME] [--count N]
//!                                         print new keys, one a line;
//!                                         TIER is supporter (the default),
//!                                         patron or founder
//! supporter-key check KEY                 what a key says, as the app reads it
//! ```
//!
//! The signing key is a file only its owner can read,
//! `~/.config/imago-emulator/supporter-signing.key`, or the file
//! `--signing-key FILE` names. Whoever has it can make keys, and a lost
//! one cannot be made again: keep a copy somewhere safe, and never in the
//! repository. This tool never prints it.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use emu_supporter::Tier;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("supporter-key: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut command = None;
    let mut name = String::new();
    let mut tier = Tier::Supporter;
    let mut count = 1_u32;
    let mut signing_key = None;
    let mut key = None;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = |what: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{argument} needs {what}"))
        };
        match argument.as_str() {
            "--name" => name = value("a name")?,
            "--tier" => {
                let given = value("a tier")?;
                tier = Tier::from_name(&given).ok_or_else(|| {
                    format!("unknown tier {given:?}; use supporter, patron or founder")
                })?;
            }
            "--count" => {
                count = value("a number")?
                    .parse()
                    .map_err(|_| "--count needs a number".to_owned())?;
            }
            "--signing-key" => signing_key = Some(PathBuf::from(value("a file")?)),
            "-h" | "--help" => command = Some("help"),
            "keygen" | "public" | "issue" | "check" if command.is_none() => {
                command = Some(argument.as_str());
            }
            other if command == Some("check") && key.is_none() => key = Some(other.to_owned()),
            other => return Err(format!("unknown argument {other:?}; try --help")),
        }
    }
    let signing_key = || {
        signing_key
            .clone()
            .or_else(default_signing_key)
            .ok_or_else(|| "no home folder; name the file with --signing-key".to_owned())
    };
    match command {
        Some("keygen") => {
            let path = signing_key()?;
            let seed = emu_supporter::new_seed();
            write_secret(&path, &seed)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            println!("Signing key written to {}", path.display());
            println!("Keep a copy somewhere safe. Its public half, for PUBLIC_KEYS:");
            println!("{}", rust_array(&emu_supporter::public_key(&seed)));
        }
        Some("public") => {
            let seed = read_secret(&signing_key()?)?;
            println!("{}", rust_array(&emu_supporter::public_key(&seed)));
        }
        Some("issue") => {
            let seed = read_secret(&signing_key()?)?;
            let public = emu_supporter::public_key(&seed);
            if !emu_supporter::PUBLIC_KEYS.contains(&public) {
                return Err("the app does not trust this signing key: \
                    its public half is not in PUBLIC_KEYS"
                    .to_owned());
            }
            let mut out = io::stdout().lock();
            for _ in 0..count {
                let key =
                    emu_supporter::issue(&seed, tier, emu_supporter::new_id(), today(), &name)
                        .map_err(|error| error.to_string())?;
                writeln!(out, "{key}").map_err(|error| error.to_string())?;
            }
        }
        Some("check") => {
            let key = key.ok_or("check needs a key")?;
            let supporter = emu_supporter::read(&key).map_err(|error| error.to_string())?;
            let (year, month, day) = emu_supporter::date(supporter.issued);
            let name = if supporter.name.is_empty() {
                "no name"
            } else {
                &supporter.name
            };
            println!(
                "Good: {} key {:016x}, issued {year}-{month:02}-{day:02}, {name}",
                supporter.tier.label(),
                supporter.id
            );
        }
        _ => {
            println!(
                "supporter-key keygen | public | issue [--tier TIER] [--name NAME] [--count N] | check KEY\n\
                 \x20 --tier TIER          supporter (the default), patron or founder\n\
                 \x20 --signing-key FILE   instead of ~/.config/imago-emulator/supporter-signing.key"
            );
        }
    }
    Ok(())
}

/// Today, in days since 1970-01-01, by UTC.
fn today() -> u32 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    u32::try_from(seconds / 86_400).unwrap_or(u32::MAX)
}

/// Where the signing key is kept unless `--signing-key` says.
fn default_signing_key() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("imago-emulator")
            .join("supporter-signing.key"),
    )
}

/// `key` as a line of Rust, to paste into `PUBLIC_KEYS`.
fn rust_array(key: &[u8; 32]) -> String {
    let bytes: Vec<String> = key.iter().map(|byte| format!("0x{byte:02X}")).collect();
    format!("[{}]", bytes.join(", "))
}

/// Writes `seed` to a new file at `path` that only its owner can read.
/// Never over a file that is there: that would end every key it signed.
fn write_secret(path: &Path, seed: &[u8; 32]) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "{}", STANDARD.encode(seed))
}

/// The signing key's seed in the file at `path`.
fn read_secret(path: &Path) -> Result<[u8; 32], String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "cannot read the signing key {}: {error} (make one with `supporter-key keygen`)",
            path.display()
        )
    })?;
    STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| format!("{} is not a signing key", path.display()))
}
