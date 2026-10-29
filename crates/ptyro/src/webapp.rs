//! The web app's server as a setting (ROADMAP WEB1.9): `ptyro web [on|off|status]`. The daemon
//! supervises `ptyro serve --managed` (SPEC §16.3); this turns `[remote] serve` on or off and says
//! how it is, in words that tell a new user what changes and what to do next. The doctor, `daemon
//! status`, `ptyro init` and the web app's settings share these lines.

use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request, WebServe};

use crate::cli::{Ctx, age};

/// What turning it on changes, in a sentence a person decides by.
pub const WHAT_IT_CHANGES: &str = "Turning it on makes the daemon run a web server: it is HTTPS only, and `[remote] access` says who can reach it \
     (this computer, your Tailscale network, or every network) on `[remote] port`. It comes back whenever the daemon does. Nothing opens to anyone until you pair a phone \
     (`ptyro pair`), and each device can be revoked.";

/// How the server is, as lines to print. `short` is the one-line form for a status listing.
pub fn lines(w: &WebServe) -> Vec<String> {
    let mut out = Vec::new();
    match (w.wanted, w.up, &w.url) {
        (_, true, Some(url)) => {
            let since = w.since_ms.map(age).map(|a| format!(", up {a}")).unwrap_or_default();
            let pid = w.pid.map(|p| format!(" (pid {p}{since})")).unwrap_or_default();
            out.push(format!("up at {url}{pid}"));
        }
        (true, _, _) => out.push("on, but not up yet".into()),
        (false, _, _) => out.push("off".into()),
    }
    if let Ok(a) = w.access.parse::<ptyro_core::config::Access>() {
        out.push(format!(
            "who can reach it: {a} ({}), on port {}; change it with `ptyro config set remote.access localhost|tailscale|open` or in Settings → remote",
            a.blurb(),
            w.port
        ));
    }
    match (w.cert.as_str(), &w.cert_note) {
        ("tailscale", None) => out.push(
            "certificate: Tailscale's, for this computer's tailnet name: nothing to install on a phone that opens it (ptyro's own for any other address)"
                .into(),
        ),
        ("tailscale", Some(note)) => out.push(format!(
            "certificate: Tailscale's, but renewing it failed: {note}"
        )),
        (_, Some(note)) => out.push(format!(
            "certificate: ptyro's own (trust it once on each phone); Tailscale's isn't in use: {note}"
        )),
        ("ptyro", None) => out.push("certificate: ptyro's own (trust it once on each phone, from /ca.crt)".into()),
        ("own", _) => out.push("certificate: yours (`tls_cert`)".into()),
        _ => {}
    }
    if let Some(pid) = w.by_hand {
        let at = w.url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default();
        out.push(if w.wanted {
            format!("a `ptyro serve` you started by hand (pid {pid}) holds the port{at}: stop it (Ctrl-C) and the daemon's takes over")
        } else {
            format!("a `ptyro serve` you started by hand is running (pid {pid}){at}; it ends with that terminal. `ptyro web on` has the daemon run it instead")
        });
    }
    if w.wanted && !w.up {
        if let Some(why) = &w.last_exit {
            out.push(format!("the last run ended: {why}"));
        }
        out.push(format!("its log: {}", w.log.display()));
    }
    if w.wanted && w.restarts > 0 {
        out.push(format!("restarted {} time(s) since the daemon started", w.restarts));
    }
    out
}

/// The next step for this state: what to type, not what's wrong.
pub fn next(w: &WebServe) -> String {
    match (w.wanted, w.up) {
        (true, true) => "pair a phone: `ptyro pair` shows a QR code".into(),
        (true, false) if w.last_exit.as_deref().is_some_and(|e| e.contains("in use")) => format!(
            "another program holds port {}: stop it, or pick another port (`ptyro config set remote.port 7092`, or Settings → remote)",
            w.port
        ),
        (true, false) => format!("see why: `tail {}`; or `ptyro doctor`", w.log.display()),
        (false, _) => "turn it on: `ptyro web on`".into(),
    }
}

pub async fn ask(
    ctx: &Ctx,
    on: Option<bool>,
    via: Option<String>,
) -> Result<(WebServe, Vec<String>), Error> {
    let c = ctx.connect(on == Some(true)).await?;
    match c.request(Request::Web { on, via, tailnet_cert: None }).await? {
        Reply::Web { web, notes } => Ok((web, notes)),
        _ => Err(Error::internal("unexpected reply")),
    }
}

/// Whether to use Tailscale's certificate, asked once when the web app first comes up over
/// Tailscale and the tailnet can give one (the answer is `[remote] tailscale_cert`).
async fn ask_tailnet_cert(ctx: &Ctx) -> Result<(), Error> {
    use std::io::{BufRead, Write};
    eprint!(
        "Use Tailscale's certificate for this computer's name? Phones on your tailnet then need \
         nothing installed. The name goes into public certificate logs. [Y/n] "
    );
    let _ = std::io::stderr().flush();
    let mut a = String::new();
    let _ = std::io::stdin().lock().read_line(&mut a);
    let yes = !matches!(a.trim().to_lowercase().as_str(), "n" | "no");
    let c = ctx.connect(true).await?;
    c.request(Request::Web { on: None, via: None, tailnet_cert: Some(yes) }).await?;
    println!(
        "  {}",
        if yes {
            "using Tailscale's certificate (`ptyro config set remote.tailscale_cert false` undoes it)"
        } else {
            "not using it: phones trust ptyro's own certificate once, from /ca.crt"
        }
    );
    Ok(())
}

pub async fn run(
    ctx: &Ctx,
    action: &str,
    access: Option<ptyro_core::config::Access>,
    json: bool,
    pair: bool,
) -> Result<i32, Error> {
    let via = access.map(|a| a.to_string());
    let on = match action {
        "on" => Some(true),
        "off" => Some(false),
        "status" => None,
        other => return Err(Error::invalid(format!("{other:?}: on, off or status"))),
    };
    if on.is_none() && via.is_some() {
        return Err(Error::invalid(
            "--access goes with `on`; `ptyro config set remote.access …` changes it any time",
        ));
    }
    let (web, notes) = match ask(ctx, on, via).await {
        Err(e) if on.is_none() && e.code == ErrorCode::DaemonUnavailable => {
            return Err(Error::new(
                e.code,
                "the daemon isn't running; `ptyro web on` starts it with the web app",
            ));
        }
        r => r?,
    };
    if json {
        println!("{}", serde_json::json!({ "web": web, "notes": notes }));
        return Ok(i32::from(on == Some(true) && !web.up));
    }
    println!("web app: {}", lines(&web).join("\n  "));
    for n in &notes {
        println!("  {n}");
    }
    match on {
        Some(true) if web.up => {
            if !crate::service::installed() {
                println!(
                    "  it stops when the daemon does: `ptyro daemon install-service` starts both at login"
                );
            }
            if web.cert_ask && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                ask_tailnet_cert(ctx).await?;
            }
            if pair {
                return crate::remote::pair(ctx, "phone".into(), None, None, false).await;
            }
            println!("  next: {}", next(&web));
        }
        Some(true) => {
            println!("  next: {}", next(&web));
            return Ok(1);
        }
        Some(false) => println!(
            "  off: the daemon stopped it, and won't start it again. Devices stay paired for when it is back."
        ),
        None => {
            if !web.wanted {
                println!("  {WHAT_IT_CHANGES}");
            }
            println!("  next: {}", next(&web));
        }
    }
    Ok(0)
}
