# Security policy

ptyro is pre-1.0 and public for early testing. Only the latest `master` is supported; there are
no maintained release branches yet.

## Reporting a vulnerability

Please report privately through GitHub: **Security → Report a vulnerability** on this
repository. That opens a private advisory only the maintainer can see. Don't open a public issue
for anything that could be exploited.

Include what you ran, what you expected, what happened, and the ptyro version (`ptyro --version`)
and OS. You should get an answer within a week. A fix comes with a credit in the changelog unless
you'd rather not be named.

## What counts

The model is described in [SPEC §17](SPEC.md#17-security-model). In short, these are in scope:

- Anything that lets someone other than you reach the daemon: the Unix socket, `ptyro serve`,
  device tokens and pairing codes, Web Push, notification callbacks.
- A device doing more than its scope allows (`read`, `approve`, `control`, `full`).
- Repository content (`.ptyro.toml`, other tools' config, a cloned repo) running code before
  you trusted it.
- An agent answering its own permission prompts, or escaping a run policy, through ptyro.
- Crashes or memory blow-ups from terminal output or protocol input.

These are known limits rather than bugs: code you or your agents run as your own user can do
anything you can, including talking to the socket; and trusting a script trusts whatever files
it calls.

## Before attaching logs

Session recordings (`ptyro replay`, `data/sessions/*/pty.log`) and crash reports contain
whatever was on screen, which can include secrets. Please look through them before attaching
them anywhere public.
