use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{auth, pipe, record};

// ── CLI definition ────────────────────────────────────────────────────

const QUALITY_DEFAULT: &str = "150";

#[derive(Parser)]
#[command(
    name = "blrec",
    about = "Record audio from Bilibili live streams",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Show debug messages
    #[arg(long, short = 'v')]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage Bilibili authentication
    #[command(arg_required_else_help = true)]
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Pipe raw FLV stream to stdout, or serve via TCP with -l/-p
    Pipe {
        /// Bilibili room ID (short or real)
        room_id: i64,
        /// Stream quality number (default: 150)
        #[arg(long, short = 'q', default_value = QUALITY_DEFAULT)]
        quality: u32,
        /// Max duration in seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Listen on address[:port] (e.g. 127.0.0.1:3000). Use :0 for random port.
        #[arg(long, short = 'l')]
        listen: Option<String>,
        /// TCP port, implies -l 127.0.0.1 if -l not given. Overrides port in -l.
        #[arg(long, short = 'p')]
        port: Option<u16>,
    },
    /// Record live stream to file
    Record {
        /// Bilibili room ID (short or real)
        room_id: i64,
        /// Output format: wav, mp3, flac, or flv
        #[arg(long, short = 'f', value_parser = ["wav", "mp3", "flac", "flv"])]
        format: Option<String>,
        /// Output file path (default: auto-generated)
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Stream quality number (default: 150)
        #[arg(long, short = 'q', default_value = QUALITY_DEFAULT)]
        quality: u32,
        /// Max recording duration in seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Skip video track (audio only)
        #[arg(long)]
        no_video: bool,
        /// Skip audio track (video only)
        #[arg(long)]
        no_audio: bool,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Login by scanning QR code
    Login,
    /// Login with browser cookies
    #[command(name = "use-cookies")]
    UseCookies {
        /// Cookie header string (at minimum SESSDATA)
        #[arg(action = clap::ArgAction::Append)]
        cookies: Vec<String>,
    },
    /// Logout and clear stored cookies
    Logout,
    /// Check if stored cookies are still valid
    Check,
}

// ── Dispatch ──────────────────────────────────────────────────────────

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Auth { action } => dispatch_auth(action).await,
        Commands::Pipe {
            room_id,
            quality,
            timeout,
            listen,
            port,
        } => {
            let timeout = timeout.map(std::time::Duration::from_secs);
            let bind_addr = resolve_listen_addr(listen.as_ref(), port)?;
            pipe::pipe(room_id, quality, timeout, bind_addr).await
        }
        Commands::Record {
            room_id,
            format,
            output,
            quality,
            timeout,
            no_video,
            no_audio,
        } => {
            let format = resolve_format(format.as_deref(), output.as_deref())?;
            record::record(
                room_id,
                format,
                output,
                quality,
                timeout.map(std::time::Duration::from_secs),
                no_video,
                no_audio,
            )
            .await
        }
    }
}

async fn dispatch_auth(action: AuthAction) -> Result<()> {
    match action {
        AuthAction::Login => auth::login().await,
        AuthAction::UseCookies { cookies } => {
            for c in cookies {
                auth::use_cookies(c).await?;
            }
            Ok(())
        }
        AuthAction::Logout => auth::logout().await,
        AuthAction::Check => auth::check().await,
    }
}

fn resolve_format(format_arg: Option<&str>, output: Option<&str>) -> Result<record::Format> {
    match format_arg {
        Some("wav") => check_output_consistency(record::Format::Wav, output),
        Some("mp3") => check_output_consistency(record::Format::Mp3, output),
        Some("flac") => check_output_consistency(record::Format::Flac, output),
        Some("flv") => check_output_consistency(record::Format::Flv, output),
        Some(s) => anyhow::bail!("unsupported format: {s}"),
        None => {
            // Try to infer from output path extension
            match output
                .and_then(|o| std::path::Path::new(o).extension())
                .and_then(|e| e.to_str())
                .and_then(record::Format::from_ext)
            {
                Some(fmt) => Ok(fmt),
                None => anyhow::bail!(
                    "Must specify either -f/--format or -o with a recognised extension \
                     (wav, mp3, flac, flv)"
                ),
            }
        }
    }
}

/// If -o has a recognisable extension, it must agree with -f.
fn check_output_consistency(
    format: record::Format,
    output: Option<&str>,
) -> Result<record::Format> {
    if let Some(ext) = output
        .and_then(|o| std::path::Path::new(o).extension())
        .and_then(|e| e.to_str())
        && let Some(inferred) = record::Format::from_ext(ext)
        && inferred != format
    {
        anyhow::bail!(
            "Format mismatch: -f {} but output extension is .{ext}",
            format.extension()
        );
    }
    Ok(format)
}

// ── Listen-address resolution ─────────────────────────────────────────

/// Resolve the bind address from `-l` and `-p` flags.
///
/// | `-l`        | `-p`  | result                |
/// |-------------|-------|-----------------------|
/// | (none)      | (none)| `None` (stdout mode)  |
/// | (none)      | 3000  | `127.0.0.1:3000`      |
/// | `0.0.0.0:0` | (none)| `0.0.0.0:0` (random)  |
/// | `127.0.0.1` | 3000  | `127.0.0.1:3000`      |
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
                    None => {
                        anyhow::bail!("'{addr}' has no port. Use '{addr}:<port>' or --port/-p.")
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn test_resolve() {
        // No listen, no port → stdout
        assert_eq!(resolve_listen_addr(None, None).unwrap(), None);
        // Port only → default loopback
        assert_eq!(
            resolve_listen_addr(None, Some(3000)).unwrap(),
            s("127.0.0.1:3000")
        );
        // Full addr:port
        assert_eq!(
            resolve_listen_addr(s("127.0.0.1:3000").as_ref(), None).unwrap(),
            s("127.0.0.1:3000")
        );
        // Random port
        assert_eq!(
            resolve_listen_addr(s("0.0.0.0:0").as_ref(), None).unwrap(),
            s("0.0.0.0:0")
        );
        // Host + separate port
        assert_eq!(
            resolve_listen_addr(s("127.0.0.1").as_ref(), Some(3000)).unwrap(),
            s("127.0.0.1:3000")
        );
        // Override port
        assert_eq!(
            resolve_listen_addr(s("127.0.0.1:3000").as_ref(), Some(4000)).unwrap(),
            s("127.0.0.1:4000")
        );
        // IPv6 bracketed
        assert_eq!(
            resolve_listen_addr(s("[::1]:3000").as_ref(), None).unwrap(),
            s("[::1]:3000")
        );
        // IPv6 bracketed + port override
        assert_eq!(
            resolve_listen_addr(s("[::1]:3000").as_ref(), Some(4000)).unwrap(),
            s("[::1]:4000")
        );
        // IPv6 bracketed, no port, -p provides it
        assert_eq!(
            resolve_listen_addr(s("[::1]").as_ref(), Some(3000)).unwrap(),
            s("[::1]:3000")
        );
        // Bare IPv6 + -p
        assert_eq!(
            resolve_listen_addr(s("::1").as_ref(), Some(3000)).unwrap(),
            s("[::1]:3000")
        );
        // Hostname:port
        assert_eq!(
            resolve_listen_addr(s("localhost:3000").as_ref(), None).unwrap(),
            s("localhost:3000")
        );
    }

    #[test]
    fn test_resolve_errors() {
        // No port anywhere
        assert!(resolve_listen_addr(s("127.0.0.1").as_ref(), None).is_err());
        assert!(resolve_listen_addr(s("::1").as_ref(), None).is_err());
        assert!(resolve_listen_addr(s("[::1]").as_ref(), None).is_err());
    }
}
