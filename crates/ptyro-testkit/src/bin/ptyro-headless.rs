//! `ptyro-headless`: run a program in ptyro's PTY + emulator with scripted input, then print the
//! final screen and a report of every OSC 7501 status, notification and bell. Used to record
//! `.ptylog` fixtures and to check terminal compatibility without a human (docs/fixtures.md).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use ptyro_core::TermSize;
use ptyro_testkit::headless::{Script, run, unescape};

const USAGE: &str = "usage: ptyro-headless [--size COLSxROWS] [--timeout SECS] [--send SECS:TEXT]... \
[--resize SECS:COLSxROWS]... [--snapshot SECS]... [--expect TEXT]... [--record FILE] [--ansi] -- CMD [ARGS...]";

fn size(s: &str) -> Result<TermSize> {
    let (c, r) = s.split_once('x').context("expected COLSxROWS")?;
    Ok(TermSize::new(c.parse()?, r.parse()?))
}

fn at(s: &str) -> Result<(Duration, &str)> {
    let (t, rest) = s.split_once(':').context("expected SECONDS:VALUE")?;
    Ok((Duration::from_secs_f64(t.parse()?), rest))
}

fn parse() -> Result<(Script, Vec<String>)> {
    let mut s = Script::new(vec![]);
    let mut expect = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value\n{USAGE}"));
        match a.as_str() {
            "--size" => s.size = size(&val()?)?,
            "--timeout" => s.timeout = Duration::from_secs_f64(val()?.parse()?),
            "--send" => {
                let v = val()?;
                let (t, text) = at(&v)?;
                s.sends.push((t, unescape(text)?));
            }
            "--resize" => {
                let v = val()?;
                let (t, sz) = at(&v)?;
                s.resizes.push((t, size(sz)?));
            }
            "--snapshot" => s.snapshots.push(Duration::from_secs_f64(val()?.parse()?)),
            "--expect" => expect.push(val()?),
            "--record" => s.record = Some(val()?.into()),
            "--ansi" => s.ansi = true,
            "--" => {
                s.argv = args.by_ref().collect();
                break;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    if s.argv.is_empty() {
        bail!("no command given\n{USAGE}");
    }
    Ok((s, expect))
}

async fn main_async() -> Result<i32> {
    let (script, expect) = parse()?;
    let (argv, size) = (script.argv.clone(), script.size);
    let r = run(script).await?;
    for (t, screen) in &r.snapshots {
        println!("──── snapshot @{:.1}s {size} ────", t.as_secs_f32());
        print!("{screen}");
    }
    println!("──── screen {size} ────");
    print!("{}", r.screen);
    println!("──── report ────");
    println!("command: {argv:?}");
    println!("ran {:.1}s, exit {:?}, title {:?}", r.elapsed.as_secs_f32(), r.exit, r.title);
    println!("modes at end: {}", r.modes);
    for l in &r.log {
        println!("{l}");
    }
    let missing: Vec<_> = expect.iter().filter(|e| !r.screen.contains(e.as_str())).collect();
    if !missing.is_empty() {
        bail!("expected text not on screen: {missing:?}");
    }
    Ok(r.exit.unwrap_or(0))
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let code = match rt.block_on(main_async()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ptyro-headless: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
