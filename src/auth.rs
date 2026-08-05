use std::time::Duration;

use anyhow::{Context as _, Result};
use api_req::{error::ApiErr, ApiCaller as _};
use qrcode::{render::unicode, QrCode};
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::api::{AuthApi, BiliApi};
use crate::cookies::{add_cookie_jar, delete_auth, load_auth, parse_cookies, save_auth};
use crate::payload::{LogoutPayload, NavPayload, QrPayload, QrPollPayload};
use crate::response::{LogoutResp, NavData, NavResp, QrData, QrPollData, QrPollResp, QrResp};

pub async fn login() -> Result<()> {
    let QrResp {
        data: QrData { url, qrcode_key },
    } = AuthApi::request(QrPayload).await?;

    let code = QrCode::new(url.as_ref())?;
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    println!("{image}");

    loop {
        sleep(Duration::from_secs(3)).await;
        let QrPollResp {
            data: QrPollData { code, message },
        } = AuthApi::request(QrPollPayload {
            qrcode_key: qrcode_key.clone(),
        })
        .await?;
        match code {
            0 => {
                info!("Login successfully.");
                break;
            }
            86101 | 86090 => {}
            _ => {
                error!("{}", message);
                return Ok(());
            }
        }
    }

    let NavResp {
        code,
        data: NavData { mid, uname, .. },
    } = BiliApi::request(NavPayload).await?;

    if code != 0 {
        anyhow::bail!("Failed to verify login: code={code}");
    }
    let mid = mid.context("Not logged in — QR scan may have failed")?;
    let uname = uname.context("No username in response")?;
    save_auth(mid, &uname)?;
    println!("Hello😊, {uname}.");
    Ok(())
}

pub async fn use_cookies(cookies: String) -> Result<()> {
    add_cookie_jar(parse_cookies(&cookies));

    let NavResp {
        code,
        data: NavData { mid, uname, .. },
    } = BiliApi::request(NavPayload).await?;

    if code != 0 {
        anyhow::bail!("Failed to verify cookies: code={code}");
    }
    let mid = mid.context("Not logged in — cookies may be expired or invalid")?;
    let uname = uname.context("No username in response")?;
    save_auth(mid, &uname)?;
    println!("Hello😊, {uname}.");
    Ok(())
}

pub async fn logout() -> Result<()> {
    let auth = load_auth()?;
    match auth {
        Some((_mid, name, cookies)) => {
            let parsed = parse_cookies(&cookies).collect::<Vec<_>>();
            let bili_jct = parsed
                .iter()
                .find(|c| c.name() == "bili_jct")
                .map(|c| c.value().to_owned());

            if let Some(jct) = bili_jct {
                add_cookie_jar(parsed.into_iter());
                let LogoutResp { code, message } =
                    AuthApi::request(LogoutPayload { biliCSRF: jct }).await?;
                if code != 0 {
                    warn!(
                        "Server logout may have failed (code={code}): {:?}",
                        message
                    );
                }
            } else {
                warn!("No bili_jct cookie found — skipping server-side logout");
            }

            delete_auth()?;
            println!("Goodbye👋, {name}.");
        }
        None => {
            info!("Not logged in — nothing to do.");
        }
    }
    Ok(())
}

pub async fn check() -> Result<()> {
    let auth = load_auth()?;
    match auth {
        Some((mid, name, cookies)) => {
            add_cookie_jar(parse_cookies(&cookies));
            match BiliApi::request(NavPayload).await {
                Ok(NavResp {
                    data:
                        NavData {
                            mid: Some(resp_mid), ..
                        },
                    ..
                }) if resp_mid == mid => {
                    println!("✅ Cookies valid. Logged in as: {name} (mid={mid})");
                }
                Ok(_) => {
                    println!("⚠️  Cookies may be expired — user mismatch.");
                }
                Err(ApiErr::UnDeserializeable(_)) => {
                    println!("❌ Cookies expired — please re-login.");
                }
                Err(e) => return Err(e.into()),
            }
        }
        None => {
            println!(
                "Not logged in. Use `blarec auth login` or `blarec auth use-cookies <COOKIES>`."
            );
        }
    }
    Ok(())
}
