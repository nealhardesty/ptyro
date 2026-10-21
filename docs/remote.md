# Remote access

Check on your agents and steer them from another machine or your phone (SPEC §16, ROADMAP M9).
There are two ways in:

| From | How | Auth | Encryption |
|---|---|---|---|
| Another computer | `ptyro --remote host …` (the CLI and the TUI) | your ssh login | ssh |
| A phone or any browser | `ptyro serve` + the web app | a paired device token | Tailscale or TLS in front |

Everything is off until you turn it on. The daemon itself only ever listens on a local socket.

## From another computer: `--remote`

```bash
ptyro --remote me@workbox            # the TUI, on workbox's daemon
ptyro --remote me@workbox ls         # any command
export PTYRO_REMOTE=ssh://me@workbox:2222   # or for every command in this shell
```

ptyro runs `ssh -T -o BatchMode=yes host ptyro proxy`, which bridges ssh's stdio to the remote
daemon's socket (starting it if needed). Frames are compressed (zstd) on this path.

- **Login without a prompt.** It needs key-based login (or an ssh agent): the TUI owns your
  terminal, so ssh can't ask for a password. `ssh workbox ptyro --version` should just work.
- **Where ptyro is.** ptyro must be installed there. It's found in `~/.cargo/bin`,
  `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` or the remote `PATH`. Set
  `PTYRO_REMOTE_BIN` to name another path.
- **Another transport.** `PTYRO_SSH` replaces the ssh command, e.g. `PTYRO_SSH="tailscale ssh"`.
- **Files stay local.** Commands that read files on *this* machine still read them here:
  `replay`/`export` (recordings), `config`, `init`, and paths you pass to `project add`. Use them
  on the remote machine (`ssh workbox ptyro replay …`).

## From your phone: `ptyro serve`

### 1. Serve, with HTTPS in front

Phones need HTTPS to install the app and to show notifications. With Tailscale this is one
flag:

```bash
ptyro serve --tailscale          # listens on 127.0.0.1:7091 and runs `tailscale serve` for HTTPS
```

To keep it running, set `[remote] serve = true` in `~/.config/ptyro/config.toml`. The daemon
then starts `ptyro serve` itself, restarts it if it dies, and keeps it through live upgrades.
Its log is `~/.local/state/ptyro/serve.log`.

```toml
[remote]
serve = true
listen = "127.0.0.1:7091"   # loopback or a Tailscale address (100.64.0.0/10); others need insecure = true
# public_url = "https://workbox.tailnet.ts.net"   # what QR codes point at (found automatically with Tailscale)
approval_wait = "10m"       # how long a held Claude permission prompt waits for your phone
push = true                 # Web Push for blocked / errored / done / idle (notifications.on)
# push_hosts = ["ntfy.example.com"]   # a self-hosted push service, besides the browsers' own
# trusted_proxy = true     # a reverse proxy on this machine fronts serve (automatic for tailscale serve)
```

**Without Tailscale.** Put a TLS reverse proxy in front of `127.0.0.1:7091`. Caddy:

```caddyfile
ptyro.example.com {
    reverse_proxy 127.0.0.1:7091
}
```

Set `public_url = "https://ptyro.example.com"` and `trusted_proxy = true`. The proxy must pass
WebSocket upgrades on `/ws` (Caddy and nginx with `proxy_http_version 1.1` + `Upgrade` headers
do). It refuses to listen on a non-loopback, non-Tailscale address unless you pass `--insecure`:
on plain HTTP, tokens would cross the network in clear.

**Client addresses behind a proxy.** The login lockout and the audit trail go by the client's
address. With `trusted_proxy = true`, a request from loopback takes it from the *last*
`X-Forwarded-For` entry (the one your proxy added; earlier ones are whatever the client sent),
and `Tailscale-User-Login` names the person. Without it those headers are ignored, because any
local process could send them. `tailscale serve` fronting the port (`--tailscale`, or set up by
hand before `ptyro serve` starts) turns this on by itself.

**Host names.** `ptyro serve` answers only requests addressed to an IP address, `localhost`, the
`public_url` host, or this machine's Tailscale name (`box.tailnet.ts.net`, `box`); anything else
gets `421`. That stops DNS rebinding: a web page whose own name is made to resolve to
127.0.0.1 can't talk to ptyro under that name. A proxy that rewrites `Host` to its upstream
(nginx's default, `127.0.0.1:7091`) works too.

### 2. Pair the phone

```bash
ptyro pair phone                 # a QR code; scan it with the phone's camera
ptyro pair tablet --scope approve
ptyro devices                    # what's paired, last seen, from where, push on/off
ptyro devices revoke 3f9a1c2b7e04  # the token stops working at once; open connections close
```

The QR code opens the web app with the token in the URL's fragment (`#pair=…`). Browsers never
send fragments to a server, so the token doesn't reach logs or proxies. The app stores it and
removes it from the address bar. Tokens are 256 random bits; the daemon keeps only their
SHA-256.

**Scopes.** Each includes the ones before it:

| Scope | Can |
|---|---|
| `read` | see sessions, screens, transcripts, status, the inbox, runs |
| `approve` | also answer permission prompts and gates, clear attention, mute |
| `control` (default) | also type into sessions, stop/resume/convert them, start *configured* agents |
| `full` | everything a local client can (any command line, config, upgrades) |

Pairing and revoking only work from the machine itself, never from a device.

### 3. Use it

On iPhone: Share → Add to Home Screen, then open it from the home screen (iOS only allows push
notifications there). Tap 🔔 to turn notifications on; a test notification arrives.

- **The list.** Blocked agents are at the top, with Allow / Deny right on the card.
- **A session.** Its live screen is drawn from the daemon's own frames, at the session's size:
  your phone never resizes it. There's a reply box (Enter sends), keys (Esc, Tab, arrows, ⏎,
  ^C), and the permission card with its context.
- **Structured sessions** (`ptyro new --structured`) show their transcript as a chat: messages,
  tool calls with output and diffs, permission cards. "Terminal" switches to the screen view.
- **Notifications** for a blocked agent have Allow / Deny buttons on platforms that show actions
  (Android, desktop Chrome). Tapping one answers without opening the app.

### Remote approvals

While a device is paired, Claude's permission prompts are *held*: Claude's `PermissionRequest`
hook waits (up to `approval_wait`) for an answer from any client. A phone's Allow or Deny then
reaches Claude as an exact decision, not keystrokes.

Holding costs nothing at the terminal. Claude keeps showing its own permission dialog while
the hook waits, and whoever answers first wins: answer in the dialog and Claude ignores the
hook (verified on Claude Code 2.1.305). Other agents are answered with their dialog's keys, or
through their protocol for structured sessions.

## Threat model

| Threat | Mitigation |
|---|---|
| Someone on the network reaches `ptyro serve` | Nothing gets past the first message without a valid device token; static files only otherwise. Listening is loopback/Tailscale-only unless `--insecure`. |
| Token theft from logs or proxies | Tokens travel in the URL fragment (never sent) and the WebSocket's first message, not the URL. Stored hashed. |
| Token guessing | 256-bit tokens; 10 failed logins from one address lock it out for 10 minutes, at most 4 logins from one address are in flight at once, and `ptyro serve` takes at most 256 connections. |
| A stale notification or link approving a newer prompt | Every answer names the prompt it was shown (its `since`); if the agent asks something else by then, the answer is refused ("that prompt changed") instead of approving it. Approve/deny links carry it too. |
| Strangers steering an agent through a PR | PR review comments reach the agent only from the repository's owner, members and collaborators; anyone else's come to you. |
| A lost phone | `ptyro devices revoke ID`: immediate, and live connections close. |
| A browser posing as a local client | `ptyro serve` rewrites every Hello to `kind: web` with its real origin; the daemon holds web clients to their device's scope. |
| A device doing more than intended | Scopes, enforced in the daemon for every request and keystroke. Unknown requests need `full`. Remote actions on a session go to its event log (`ptyro events -s S`) with the device's name. |
| Cross-site WebSocket hijacking | No cookies: authentication is in-band, so a hostile page has nothing to ride on. |
| DNS rebinding (a page whose name resolves to your machine) | Requests must be addressed to an IP, `localhost`, `public_url`'s host or the tailnet name; others get 421. |
| Forged `X-Forwarded-For` to dodge the lockout | Only believed from loopback with `trusted_proxy` (or under `tailscale serve`), and only its last entry. |
| Clickjacking, injected scripts | A strict CSP (`default-src 'none'`, scripts only from the app itself), `X-Frame-Options: DENY`, no third-party requests. |
| Push payload snooping | Web Push is end-to-end encrypted (RFC 8291) for the device; push services see ciphertext. VAPID (RFC 8292) identifies your daemon. Subscriptions are accepted only for the browsers' push services, so a device can't make the daemon post elsewhere. |
| An agent approving its own prompts | Agents can't answer permission prompts or gates, from any client (M7 policy). Devices can't pair devices. |

Remote input is equivalent to running commands as you. Pair only devices you control, and
prefer `approve` for a phone you just want notifications and approvals on.
