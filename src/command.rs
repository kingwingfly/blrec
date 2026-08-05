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
                .about("Pipe raw FLV stream to stdout (for ffmpeg / ffplay)")
                .arg_required_else_help(true)
                .args([
                    Arg::new("streamer_id")
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
                ]),
            Command::new("record")
                .about("Record audio from live stream to file")
                .arg_required_else_help(true)
                .args([
                    Arg::new("streamer_id")
                        .help("Bilibili room ID (short or real)")
                        .required(true)
                        .value_parser(value_parser!(i64)),
                    Arg::new("format")
                        .help("Output audio format: wav, mp3, or flac")
                        .long("format")
                        .short('f')
                        .required(true)
                        .value_parser(["wav", "mp3", "flac"]),
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
            let id = *sub_matches.get_one::<i64>("streamer_id").unwrap();
            let quality = *sub_matches.get_one::<u32>("quality").unwrap();
            let timeout = sub_matches
                .get_one::<u64>("timeout")
                .map(|t| std::time::Duration::from_secs(*t));
            pipe::pipe(id, quality, timeout).await?;
        }
        Some(("record", sub_matches)) => {
            let id = *sub_matches.get_one::<i64>("streamer_id").unwrap();
            let format_str = sub_matches.get_one::<String>("format").unwrap().as_str();
            let output = sub_matches
                .get_one::<String>("output")
                .map(|s| s.to_owned());
            let quality = *sub_matches.get_one::<u32>("quality").unwrap();
            let timeout = sub_matches
                .get_one::<u64>("timeout")
                .map(|t| std::time::Duration::from_secs(*t));

            let fmt = match format_str {
                "wav" => record::Format::Wav,
                "mp3" => record::Format::Mp3,
                "flac" => record::Format::Flac,
                s => anyhow::bail!("unsupported format: {s}. Use wav, mp3, or flac"),
            };
            record::record(id, fmt, output, quality, timeout).await?;
        }
        _ => unreachable!(),
    }
    Ok(())
}
