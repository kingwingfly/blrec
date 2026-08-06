use anyhow::{Context as _, Result};
use api_req::{COOKIE_JAR, CookieStore as _};
use cookie::Cookie;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct AuthFile {
    mid: i64,
    name: String,
    cookies: String,
}

pub fn parse_cookies(cookies: &str) -> impl Iterator<Item = Cookie<'_>> {
    Cookie::split_parse_encoded(cookies).filter_map(|res| res.ok())
}

/// Inject cookies into the global `api_req` cookie jar.
pub fn add_cookie_jar<'a>(cookies: impl Iterator<Item = Cookie<'a>>) {
    cookies.into_iter().for_each(|mut c| {
        c.set_domain("bilibili.com");
        COOKIE_JAR.add_cookie_str(
            &c.encoded().to_string(),
            &"https://bilibili.com".parse().unwrap(),
        );
    });
}

/// Serialize the current cookie jar to a header string.
pub fn current_cookies() -> Result<String> {
    Ok(COOKIE_JAR
        .cookies(&"https://bilibili.com".parse().unwrap())
        .context("Auth cookies should be set")?
        .to_str()?
        .to_owned())
}

pub fn auth_file_path() -> std::path::PathBuf {
    std::path::PathBuf::from(".blrec").join("auth.json")
}

pub fn save_auth(mid: i64, name: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let cookies = current_cookies()?;
    let path = auth_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let data = AuthFile {
        mid,
        name: name.to_string(),
        cookies,
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(serde_json::to_string_pretty(&data)?.as_bytes())?;
    tracing::info!("Auth saved to {}", path.display());
    Ok(())
}

pub fn load_auth() -> Result<Option<(i64, String, String)>> {
    let path = auth_file_path();
    if !path.exists() {
        return Ok(None);
    }
    let data: AuthFile = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    Ok(Some((data.mid, data.name, data.cookies)))
}

pub fn delete_auth() -> Result<()> {
    let path = auth_file_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}
