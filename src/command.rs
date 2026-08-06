use anyhow::Result;
use clap::{command, value_parser, Arg, ArgAction, Command};

use crate::{auth, pipe, record};

pub async fn run() -> anyhow::Result<()> {
    let mut cmd = command!()
        .arg_required_else_help(true)
        .args([Arg::new("verbose")
            .help("Show debug messages")
            .long("verbose")
            .short('v')
            .action(ArgAction::SetTrue)])
        .subcommands([
            Command::new("auth")
                .about("Manage Bilibili authentication")
                .arg_required_else_help(true)
                .subcommands([
                    Command::new("login").about("Login by scanning QR code"),
                    Command::new("use-cookies")
                        .about("Login with browser cookies")
                        .arg_required_else_help(true)
                        .args([Arg::new("cookies")
                            .help("Cookie header string (at minimum SESSDATA)")
                            .action(ArgAction::Append)]),
                    Command::new("logout").about("Logout and clear stored cookies"),
                    Command::new("check").about("Check if stored cookies are still valid"),
                ]),
            Command::new("pipe")
                .about("Pipe raw FLV stream to stdout, or serve via TCP with -l/-p")
                .arg_required_else_help(true)
                .args([
                    Arg::new("room_id")
                        .help("Bilibili room ID (short or real)")
                        .required(true)
                        .value_parser(value_parser!(i64)),
                    Arg::new("quality")
                        .help("Stream quality number (default: 150)")
                        .long("quality")
                        .short('q')
                        .value_parser(value_parser!(u32))
                        .default_value("150"),
                    Arg::new("timeout")
                        .help("Max duration in seconds")
                        .long("timeout")
                        .value_parser(value_parser!(u64)),
                    Arg::new("listen")
                        .help("Listen on address[:port] (e.g. 127.0.0.1:3000). Use :0 for random port.")
                        .long("listen")
                        .short('l')
                        .value_parser(value_parser!(String)),
                    Arg::new("port")
                        .help("TCP port, implies -l 127.0.0.1 if -l not given. Overrides port in -l.")
                        .long("port")
                        .short('p')
                        .value_parser(value_parser!(u16)),
                ]),
            Command::new("record")
                .about("Record live stream to file")
                .arg_required_else_help(true)
                .args([
                    Arg::new("room_id")
                        .help("Bilibili room ID (short or real)")
                        .required(true)
                        .value_parser(value_parser!(i64)),
                    Arg::new("format")
                        .help("Output format: wav, mp3, flac, or flv")
                        .long("format")
                        .short('f')
                        .value_parser(["wav", "mp3", "flac", "flv"]),
                    Arg::new("output")
                        .help("Output file path (default: auto-generated)")
                        .long("output")
                        .short('o')
                        .value_parser(value_parser!(String)),
                    Arg::new("quality")
                        .help("Stream quality number (default: 150)")
                        .long("quality")
                        .short('q')
                        .value_parser(value_parser!(u32))
                        .default_value("150"),
                    Arg::new("timeout")
                        .help("Max recording duration in seconds")
                        .long("timeout")
                        .value_parser(value_parser!(u64)),
                    Arg::new("no-video")
                        .help("Skip video track (audio only)")
                        .long("no-video")
                        .action(ArgAction::SetTrue),
                    Arg::new("no-audio")
                        .help("Skip audio track (video only)")
                        .long("no-audio")
                        .action(ArgAction::SetTrue),
                ]),
        ]);

    let matches = cmd.get_matches_mut();

    match matches.subcommand() {
        Some(("auth", sub_matches)) => match sub_matches.subcommand() {
            Some(("login", _)) => auth::login().await?,
            Some(("use-cookies", sub)) => {
                for cookies in sub.get_many::<String>("cookies").unwrap() {
                    auth::use_cookies(cookies.to_owned()).await?;
                }
            }
            Some(("logout", _)) => auth::logout().await?,
            Some(("check", _)) => auth::check().await?,
            _ => unreachable!(),
        },
        Some(("pipe", sub_matches)) => {
            let id = *sub_matches.get_one::<i64>("room_id").unwrap();
            let quality = *sub_matches.get_one::<u32>("quality").unwrap();
            let timeout = sub_matches
                .get_one::<u64>("timeout")
                .map(|t| std::time::Duration::from_secs(*t));
            let listen = sub_matches.get_one::<String>("listen");
            let port = sub_matches.get_one::<u16>("port").copied();

            let bind_addr = resolve_listen_addr(listen, port)?;
            pipe::pipe(id, quality, timeout, bind_addr).await?;
        }
        Some(("record", sub_matches)) => {
            let id = *sub_matches.get_one::<i64>("room_id").unwrap();
            let output = sub_matches
                .get_one::<String>("output")
                .map(|s| s.to_owned());
            let quality = *sub_matches.get_one::<u32>("quality").unwrap();
            let timeout = sub_matches
                .get_one::<u64>("timeout")
                .map(|t| std::time::Duration::from_secs(*t));
            let no_video = sub_matches.get_flag("no-video");
            let no_audio = sub_matches.get_flag("no-audio");

            // Resolve format: -f flag > -o extension > error
            let format = match sub_matches.get_one::<String>("format") {
                Some(f) => match f.as_str() {
                    "wav" => record::Format::Wav,
                    "mp3" => record::Format::Mp3,
                    "flac" => record::Format::Flac,
                    "flv" => record::Format::Flv,
                    s => anyhow::bail!("unsupported format: {s}"),
                },
                None => {
                    // Try to infer from output path extension
                    match output.as_deref().and_then(|o| {
                        std::path::Path::new(o)
                            .extension()
                            .and_then(|e| e.to_str())
                            .and_then(record::Format::from_ext)
                    }) {
                        Some(fmt) => fmt,
                        None => anyhow::bail!(
                            "Must specify either -f/--format or -o with a recognised extension \
                             (wav, mp3, flac, flv)"
                        ),
                    }
                }
            };

            // If both -f and -o given with recognisable extension, they must agree
            if let Some(ref out) = output
                && let Some(ext) = std::path::Path::new(out)
                    .extension()
                    .and_then(|e| e.to_str())
                    && let Some(inferred) = record::Format::from_ext(ext)
                        && inferred != format {
                            anyhow::bail!(
                                "Format mismatch: -f {} but output extension is .{ext}",
                                format.extension()
                            );
                        }

            record::record(
                id, format, output, quality, timeout, no_video, no_audio,
            ).await?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Resolve the bind address from `-l` and `-p` flags.
///
/// | `-l`       | `-p`  | result                |
/// |------------|-------|-----------------------|
/// | (none)     | (none)| `None` (stdout mode)  |
/// | (none)     | 3000  | `127.0.0.1:3000`      |
/// | `0.0.0.0:0`| (none)| `0.0.0.0:0` (random)  |
/// | `127.0.0.1`| 3000  | `127.0.0.1:3000`      |
/// | `127.0.0.1:3000` | 4000 | `127.0.0.1:4000` |
fn resolve_listen_addr(listen: Option<&String>, port: Option<u16>) -> Result<Option<String>> {
    match (listen, port) {
        (None, None) => Ok(None),
        (None, Some(p)) => Ok(Some(format!("127.0.0.1:{p}"))),
        (Some(addr), port) => {
            let colon_count = addr.chars().filter(|&c| c == ':').count();
            // addr:port or [IPv6]:port
            let has_port = addr.contains("]:") || colon_count == 1;
            // IPv6 without brackets (e.g. ::1) — has colons but no port
            let is_bare_ipv6 = colon_count >= 2 && !addr.starts_with('[');

            if is_bare_ipv6 {
                // Bare IPv6 — must provide port via -p
                match port {
                    Some(p) => Ok(Some(format!("[{addr}]:{p}"))),
                    None => anyhow::bail!(
                        "IPv6 address '{addr}' has no port. Use '[{addr}]:<port>' or --port/-p."
                    ),
                }
            } else if has_port {
                // addr already has a port
                if let Some(p) = port {
                    // -p overrides the port in -l
                    let host = if let Some(bracket_end) = addr.find(']') {
                        &addr[..=bracket_end]
                    } else if let Some(colon) = addr.rfind(':') {
                        &addr[..colon]
                    } else {
                        addr.as_str()
                    };
                    Ok(Some(format!("{host}:{p}")))
                } else {
                    Ok(Some(addr.clone()))
                }
            } else {
                // addr has no port
                match port {
                    Some(p) => Ok(Some(format!("{addr}:{p}"))),
                    None => anyhow::bail!(
                        "'{addr}' has no port. Use '{addr}:<port>' or --port/-p."
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(s: &str) -> Option<String> { Some(s.to_string()) }

    #[test]
    fn test_resolve() {
        // No listen, no port → stdout
        assert_eq!(resolve_listen_addr(None, None).unwrap(), None);
        // Port only → default loopback
        assert_eq!(resolve_listen_addr(None, Some(3000)).unwrap(), s("127.0.0.1:3000"));
        // Full addr:port
        assert_eq!(resolve_listen_addr(s("127.0.0.1:3000").as_ref(), None).unwrap(), s("127.0.0.1:3000"));
        // Random port
        assert_eq!(resolve_listen_addr(s("0.0.0.0:0").as_ref(), None).unwrap(), s("0.0.0.0:0"));
        // Host + separate port
        assert_eq!(resolve_listen_addr(s("127.0.0.1").as_ref(), Some(3000)).unwrap(), s("127.0.0.1:3000"));
        // Override port
        assert_eq!(resolve_listen_addr(s("127.0.0.1:3000").as_ref(), Some(4000)).unwrap(), s("127.0.0.1:4000"));
        // IPv6 bracketed
        assert_eq!(resolve_listen_addr(s("[::1]:3000").as_ref(), None).unwrap(), s("[::1]:3000"));
        // IPv6 bracketed + port override
        assert_eq!(resolve_listen_addr(s("[::1]:3000").as_ref(), Some(4000)).unwrap(), s("[::1]:4000"));
        // IPv6 bracketed, no port, -p provides it
        assert_eq!(resolve_listen_addr(s("[::1]").as_ref(), Some(3000)).unwrap(), s("[::1]:3000"));
        // Bare IPv6 + -p
        assert_eq!(resolve_listen_addr(s("::1").as_ref(), Some(3000)).unwrap(), s("[::1]:3000"));
        // Hostname:port
        assert_eq!(resolve_listen_addr(s("localhost:3000").as_ref(), None).unwrap(), s("localhost:3000"));
    }

    #[test]
    fn test_resolve_errors() {
        // No port anywhere
        assert!(resolve_listen_addr(s("127.0.0.1").as_ref(), None).is_err());
        assert!(resolve_listen_addr(s("::1").as_ref(), None).is_err());
        assert!(resolve_listen_addr(s("[::1]").as_ref(), None).is_err());
    }
}
