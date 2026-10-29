<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Remote access

Check on your agents and steer them from another machine or your phone (SPEC §16, ROADMAP M9).
There are two ways in:

| From | How | Auth | Encryption |
|---|---|---|---|
| Another computer | `ptyro --remote host …` (the CLI and the TUI) | your ssh login | ssh |
| A phone or any browser | `ptyro serve` + the web app | a paired device token | HTTPS: ptyro's own certificate, or Tailscale's, or your proxy's |

Everything is off until you turn it on. The daemon's clients reach it through a local socket;
it opens a TCP port of its own only for approve/deny links in notifications, when you set
`[notifications.callback] listen` (loopback or Tailscale unless `insecure = true`).

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

## Several machines: `ptyro machine`

Another machine's workspaces can sit beside yours, everywhere: the TUI's sidebar, every CLI
command and the web app (so a phone paired with this machine sees them too).

```bash
ptyro machine add ssh://me@laptop2   # over ssh, as --remote (keys or an agent)
ptyro machine pair                   # on laptop2: a code, for when there's no ssh …
ptyro machine add 'https://laptop2…/#machine=ptp_…&ca=…&name=laptop2'   # … added here
ptyro machine ls                     # how each link stands, its workspaces
ptyro ls                             # yours, and laptop2:fix-flaky-test
ptyro send laptop2:fix-flaky-test "run the tests"
ptyro machine hide laptop2 old       # not shown here (nothing changes there); unhide shows it
ptyro machine rm laptop2
```

- **Names.** Everything of laptop2's carries its name: workspaces `laptop2:work` (shown
  `laptop2 / work`), sessions `laptop2:fix-flaky-test`, worktrees `laptop2:myrepo/feat`, profiles
  `laptop2:work`. A name only laptop2 has works bare too; one both have needs the machine.
- **It lives there.** A mirrored workspace's projects, worktrees, profiles and logins are
  laptop2's: things started in it run there. `+ new workspace` asks which machine.
- **Both ways, no hub.** Add each machine on the other and pair your phone with each: whichever
  is awake shows everything it can reach. A machine only ever offers its own workspaces, so
  two machines adding each other don't echo.
- **Offline.** An unreachable machine's workspaces stay listed as they were, marked "offline since
  14:02"; anything that would change them says so. The link retries by itself (up to every
  minute); `ptyro machine retry` (or the row menu) tries now.
- **Same version.** Linked machines must run the same ptyro. A link to another version stays down
  and says which machine to update and what to run there.
- **Alerts** of laptop2's agents reach you through the machine you're connected to (its
  desktop notifications, sinks and your paired devices), under its notification settings. A phone
  paired with both shows one notification for one alert.
- **Trust.** A machine added with a code holds a token with `full` scope on the other: it can do
  everything there. It is listed under `ptyro devices` as "machine"; revoke it there. A code
  works once, for 10 minutes, and pins the other machine's certificate (its own HTTPS); ssh needs
  nothing new.
- **What stays per machine:** runs, recipes, change sets, messages and leases, the scratchpad,
  and the agents and editors offered in the new-task wizard.

## From your phone: `ptyro serve`

### 1. Serve, over HTTPS

Phones need HTTPS to install the app and to show notifications, so `ptyro serve` is **always
HTTPS**, with a certificate it makes (or one of your own, below). There is no plain-HTTP mode.

You choose two things, and each is one setting (Settings → remote in the TUI and the web app,
`ptyro config set` on the command line):

| `remote.access` | who can reach the web app |
|---|---|
| `localhost` (the default) | this computer only (`127.0.0.1`) |
| `tailscale` | this computer and your Tailscale network: it listens on loopback and this computer's Tailscale addresses, nothing else, so the Wi-Fi never sees it. If Tailscale comes up after the server, it starts listening there by itself |
| `open` | every network interface (`0.0.0.0`): the Wi-Fi or LAN too, and anything else that reaches this computer. Devices still need a pairing token |

`remote.port` is the port, `7091` unless you change it. The address phones open is
`https://<this computer>:<port>`: its Tailscale name for `tailscale`, its LAN address for `open`.

```bash
ptyro web on --access tailscale    # or localhost, open; nothing given: Tailscale if it is up, else open
ptyro config set remote.access open
ptyro config set remote.port 8443
ptyro serve --access open --port 8443   # by hand, for one run (the daemon's own is `ptyro web on`)
```

**Tailscale's certificate (nothing to install).** With `access = tailscale`, and "HTTPS
Certificates" turned on for your tailnet (admin console → DNS → HTTPS Certificates), ptyro asks
Tailscale for a real certificate for this computer's name (`box.<tailnet>.ts.net`) with
`tailscale cert`, and uses it for that name. It is a Let's Encrypt certificate, so every phone and
browser already trusts it: **Android, which won't install the app or allow notifications on a
certificate it doesn't trust, needs no CA step at all.** `ptyro pair` and `ptyro web` say when
it is in use.

- **Nothing to turn on.** It is automatic whenever the web app is shared over Tailscale. The
  computer's name goes into public certificate logs (every Let's Encrypt certificate does); to
  avoid that, share another way (`remote.access`) or use your own certificate.
- **It starts at once.** The server comes up with its own certificate and switches the tailnet
  name over when Tailscale's has arrived (a few seconds the first time); it asks again every 12
  hours, and Tailscale renews it when it is close to its end.
- **Other addresses are unchanged.** A Let's Encrypt certificate can't name an IP address, so
  `https://192.168.1.5:7091`, `<name>.local` and `localhost` still show ptyro's own certificate
  (below). The server picks by the name the client asked for.
- **If it can't.** ptyro keeps its own certificate and `ptyro web` / `ptyro doctor` say why and
  how to fix it: HTTPS Certificates aren't on for the tailnet, Tailscale isn't connected, or on
  Linux `sudo tailscale set --operator=$USER` lets your user ask for one.
- **Machines.** `ptyro machine add` accepts a `*.ts.net` machine's Tailscale certificate (checked
  against the public roots) as well as the CA pinned in its code.

**ptyro's certificate.** The first time, `ptyro serve` makes a small certificate authority (CA)
and a certificate it signs for this machine: `localhost`, its name (and `<name>.local`), its LAN
and Tailscale addresses. They're kept in `~/.local/share/ptyro/serve/tls/` (the keys `0600`); the
certificate is renewed before it ends, and when the machine's addresses change, and the CA stays
the same. A phone trusts the CA once. `ptyro pair` says where it is (`https://<address>/ca.crt`)
and its SHA-256, so you can check it's yours:

- **iPhone, iPad:** open `https://<address>/ca.crt` in Safari and allow the download. Then
  Settings → General → VPN & Device Management → *ptyro local CA* → Install, and Settings →
  General → About → Certificate Trust Settings → turn on *ptyro local CA*.
- **Android:** download it from the same address, then Settings → Security (or Security &
  privacy) → More → Encryption & credentials → Install a certificate → CA certificate.
- **A desktop browser:** add `ca.crt` to the system's trusted roots (macOS: Keychain Access →
  System → drag it in → Always Trust), or accept the warning once.

Then open the address `ptyro pair` shows. Anyone who could reach `serve` still needs a paired
device's token; the certificate only keeps it private on the way.

**What trusting the CA means.** A device that trusts `ca.crt` trusts that CA for *every* site, not
only yours: whoever can read its key (`~/.local/share/ptyro/serve/tls/ca-key.pem`, mode 0600,
valid ten years) could make a certificate for any site that such a device would accept. So keep
that directory out of backups and synced folders. To start over, stop `ptyro serve`, delete
`~/.local/share/ptyro/serve/tls/`, start it again, and trust the new `ca.crt` on each device
(then remove the old one). Prefer your own certificate when you can: it needs no CA on your
phone.

**Your own certificate.** (It takes the place of Tailscale's too.) `ptyro serve --cert fullchain.pem --key privkey.pem` (or
`tls_cert` and `tls_key` under `[remote]`) uses a certificate you have, from Let's Encrypt or
your company's CA, say: phones that trust its issuer need nothing installed, and there's no
`/ca.crt`. The files are read again every 12 hours, so a renewed certificate is picked up.

**Have the daemon run it.** `ptyro web on` (or the question in `ptyro init`, or `ptyro daemon start
--serve`, or More → Web app in the web app, or `remote.web_on` in the TUI) sets `[remote] serve = true`
in `~/.config/ptyro/config.toml`. The daemon then starts `ptyro serve` itself, restarts it if it
dies, and keeps it through live upgrades and restarts. Its log is `~/.local/state/ptyro/serve.log`.

```bash
ptyro web on                    # on: your Tailscale network if it is running here, else every network
ptyro web on --access localhost # or choose: tailscale, open (the Wi-Fi too), localhost (this computer only)
ptyro web                       # how it is: up at <address>, since when, who runs it, what's wrong
ptyro pair                      # then a QR code for the phone (or `ptyro web on --pair`)
ptyro web off                   # stop it; paired devices stay paired
```

What turning it on changes: the daemon runs a web server that listens where `[remote] access` says, on
`[remote] port`, and it comes back whenever the daemon does. Install
the login service (`ptyro daemon install-service`) and it is up after a reboot too; the phone is
paired once and stays paired. Nothing opens to anyone until a device is paired, and each can be
revoked. Turning it off from the web app ends the page you are looking at, so it asks first.

If it won't start, `ptyro web` and `ptyro doctor` say why: most often the port is taken (a
`ptyro serve` you started by hand, which `ptyro web` names with its pid; or another program).
Stop it, or set another `remote.port`; the daemon tries again on its own.

```toml
[remote]
serve = true
access = "tailscale"        # localhost (this computer), tailscale (and your tailnet) or open (every network)
port = 7091
# tls_cert = "~/certs/fullchain.pem"   # your own certificate instead of ptyro's (with tls_key)
# tls_key = "~/certs/privkey.pem"
# public_url = "https://workbox.example.com:7091"   # what QR codes point at, if it isn't found automatically
approval_wait = "10m"       # how long a held Claude permission prompt waits for your phone
push = true                 # Web Push for the kinds in notifications.on
# push_hosts = ["ntfy.example.com"]   # a self-hosted push service, besides the browsers' own
```

**Older config files.** `listen`, `tailscale`, `local`, `insecure` and `trusted_proxy` under
`[remote]` are gone: five settings that had to agree became `access` and `port`. A file that
still has them is read as what they meant (`tailscale` or a tailnet `listen` is `tailscale`,
`local` or a `0.0.0.0` listen is `open`, the port of `listen` is `port`), `ptyro web on` rewrites
the file, and `ptyro config check` says what changed. Two things went with them: ptyro no longer
runs `tailscale serve` for you (the phone opens `https://<name>.<tailnet>.ts.net:7091`, which
shows Tailscale's own certificate, or else ptyro's, trusted once; `tailscale serve reset` removes an old mapping), and plain HTTP
and reverse-proxy modes are not offered. To put your own TLS proxy in front, point it at the
HTTPS port, set `public_url` to the proxy's address, and use your own certificate for it.

**Host names.** `ptyro serve` answers only requests addressed to an IP address, `localhost`, the
`public_url` host, or this machine's Tailscale name (`box.tailnet.ts.net`, `box`); anything else
gets `421`. That stops DNS rebinding: a web page whose own name is made to resolve to
127.0.0.1 can't talk to ptyro under that name. A proxy that rewrites `Host` to its upstream
(nginx's default, `127.0.0.1:7091`) works too.

### 2. Pair the phone

```bash
ptyro pair phone                 # a one-time QR code; scan it with the phone's camera
                                 # (full control: it warns you; --scope approve for less)
ptyro pair tablet --scope control
ptyro devices                    # what's paired, last seen, from where, push on/off
ptyro devices revoke 3f9a1c2b7e04  # the token stops working at once; open connections close
```

`ptyro serve` must be running when the phone opens the link. The QR code carries a **pairing
code**, not the device's token, in the URL's fragment (`#pair=ptp_…`). Browsers never send
fragments to a server, so it doesn't reach logs or proxies. The app trades the code for the
device's token once (`POST /api/pair`), stores the token, and removes the code from the address
bar.

A pairing code works **once, and for 10 minutes**. That matters because what `ptyro pair`
prints stays wherever it was printed: in a ptyro pane, that is the scrollback and the session
recording, which agents and `read` devices can see. Once claimed or expired, the printed code is
worthless. `ptyro pair` waits and tells you when the device signed in, and from where; if that
wasn't your phone, revoke it. Until then, `ptyro devices` lists the device as `unclaimed`. Run
`ptyro pair` in a plain terminal when you can.

Tokens are 256 random bits; the daemon keeps only their SHA-256 (and the code's, until it is
used).

**The link always has its port** (`https://192.168.1.9:7091/#pair=…`, `http://[::1]:7091/…`,
`https://box.tail1234.ts.net:443/…`), never `0.0.0.0`. If `--url` names a port ptyro isn't
listening on, `ptyro pair` says so and prints the `--url` that works.

**Pairing a name again replaces it.** `ptyro pair phone --scope full` when `phone` exists makes a
new link; when it is opened, the old token stops working, its connections close, and the device
has the scope you asked for now (higher or lower). `ptyro pair` says "Replacing the existing
device 'phone' (was approve, now full)". Until the new link is opened, the old one keeps working.

**Scopes.** Each includes the ones before it:

| Scope | Can |
|---|---|
| `read` | see sessions, screens, transcripts, status, the inbox, runs |
| `approve` | also answer permission prompts and gates, clear attention, mute |
| `control` | also type into sessions, stop/resume/convert them, start *configured* agents |
| `full` (default, with a warning) | everything a local client can (any command line, worktrees and shipping, the config, upgrades, pairing devices): what the whole web app needs |

Pairing and revoking work from the machine itself, and from a device paired with `--scope full` (the web app's Devices page).

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

### A terminal in the browser

With a device of `control` scope or more, the session page's screen takes keys: tap it (or click
it) and type. It is the daemon's own terminal, so `vim`, `htop`, `claude` and your shell behave as
they do in the TUI: arrows, Ctrl and Alt combinations, F-keys, bracketed paste, the mouse when the
program wants it (hold Shift to select instead).

- **On a phone**, the row under the screen has Esc, Tab, Ctrl, Alt and ⇧ (tap one for the next key,
  tap again to hold it), arrows (hold to repeat), Home/End/PgUp/PgDn, Ctrl-C/D/Z/L/R, punctuation,
  F1–F12 and Paste. **Aa** has text size, fit or 1:1, the on-screen keyboard on or off (off keeps a
  hardware keyboard's keys without covering the screen), find, and *Resize the session to this
  screen* (everyone attached sees the new size; whoever types last decides).
- **History.** Scroll with the wheel or a drag; **↓ Live** returns (any key does too).
- **Selecting.** Drag with a mouse, double-click a word, triple-click a line; on a touch screen
  hold, then drag. The bar copies, selects everything, opens a link, or steps through find
  results. A program that copies to the clipboard (OSC 52) puts it on yours, or offers a Copy
  button when the browser wants a tap.
- **Below `control`** the screen is still live and you can scroll, select and copy, but nothing
  you type reaches the session.

## The whole app

Everything the CLI does, the web app does, in tabs along the bottom (the list of commands and
where each lives is [`parity.md`](parity.md)). `ptyro pair --scope full` gives a device all of it;
a device with less sees the pages and is told what it lacks.

- **Sessions**: ＋ starts a task (the task, the agent, and where: a new worktree, from a PR, an
  issue, a branch or your uncommitted changes; an existing one; a shell). A session's **⋯**: stop,
  restart, rename, summarize, hibernate, scrollback, events, replay, save its recording. ☑ types one
  thing into several.
- **Projects**: the sidebar's tree. A worktree's page has its sessions and status; **Changes** is
  the diff, where tapping a line comments for the agent and one button sends them all; **Commits**,
  **Checks** (the PR and its CI), **Finish** (a PR, or merge into main), archive, remove.
- **Runs** and **Sets**: fan-outs and recipes, gates waiting for you, the compare-and-pick view,
  change sets with their contract, checks, checkpoints and merge train.
- **More**: health (each finding with its fix), usage, devices (pair another with a link and a QR
  code), profiles and logins, the config editor (checked as you save, applied at once),
  integrations, the daemon (logs, upgrade, restart, login service), first-run setup, and a tour.

Changing the machine (the config file, profiles, the login service, the daemon's restart, the
tour) goes through `POST /api/host`, which `ptyro serve` accepts from a `full` device only, and
only when it runs on the same machine as the daemon (not with `--remote`).

**The tour** (More → Take the tour) starts a ptyro of its own, with a demo repository and a pretend
agent, and a second `ptyro serve` for it on a free port of this computer (loopback, plain HTTP),
and opens it in a new tab with a coach: start a task, answer the agent, review, finish, clean up.
It's reachable from this computer only, so it opens from a browser on it. "End the tour" stops
its daemon and deletes everything it made; one left behind is swept the next time a tour starts.

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
| Token theft from logs or proxies | Tokens travel only in the WebSocket's first message or a request header, never a URL. The link `ptyro pair` prints holds a pairing code instead, in the fragment (never sent). Stored hashed. |
| The pairing link read off a screen, scrollback or recording | It is a code, not the token: claimed once, within 10 minutes, then worthless. `ptyro pair` reports who claimed it and from where; failed claims count toward the lockout. During those minutes it *is* a credential, so prefer a plain terminal. |
| Token guessing | 256-bit tokens and codes; 10 failed logins or pairing claims from one address lock it out for 10 minutes, at most 4 logins from one address are in flight at once, and `ptyro serve` takes at most 256 connections. |
| A stale notification or link approving a newer prompt | Every answer names the prompt it was shown (its `since`); if the agent asks something else by then, the answer is refused ("that prompt changed") instead of approving it. Approve/deny links carry it too. |
| A paired phone changing the machine | The config file, profiles, the login service, the daemon's restart and the tour need a `full` device (`POST /api/host` checks that with the daemon, which is the authority on scopes). A `control` device can type, answer and stop what it started; it can't start any command, write the config or pair a device. |
| Strangers steering an agent through a PR | PR review comments reach the agent only from the repository's owner, members and collaborators; anyone else's come to you. |
| A lost phone | `ptyro devices revoke ID`: immediate, and live connections close. |
| A browser posing as a local client | `ptyro serve` rewrites every Hello to `kind: web` with its real origin; the daemon holds web clients to their device's scope. |
| A device doing more than intended | Scopes, enforced in the daemon for every request and keystroke. Unknown requests need `full`. Remote actions on a session go to its event log (`ptyro events -s S`) with the device's name. |
| Cross-site WebSocket hijacking | No cookies: authentication is in-band, so a hostile page has nothing to ride on. |
| DNS rebinding (a page whose name resolves to your machine) | Requests must be addressed to an IP, `localhost`, `public_url`'s host or the tailnet name; others get 421. |
| Forged `X-Forwarded-For` to dodge the lockout | Only believed from loopback with `trusted_proxy` (or under `tailscale serve`), and only its last entry. |
| Clickjacking, injected scripts | A strict CSP (`default-src 'none'`, scripts only from the app itself), `X-Frame-Options: DENY`, no third-party requests. |
| Push payload snooping | Web Push is end-to-end encrypted (RFC 8291) for the device; push services see ciphertext. VAPID (RFC 8292) identifies your daemon. Subscriptions are accepted only for the browsers' push services (and the hosts in `[remote] push_hosts`), so a device can't make the daemon post elsewhere. |
| An agent approving prompts, or pairing a device for itself | An agent can't answer permission prompts or gates, type into a session that is blocked on one, pair or revoke devices, or trust a repository's scripts, from any client. See below for what this does and doesn't stop. Devices can't pair devices, except one paired with `--scope full`, which can run any command as you anyway. |

Remote input is equivalent to running commands as you. Pair only devices you control. `full` is the default because the whole web app needs it, and `ptyro pair` warns you in plain
words; pair with `--scope approve` for notifications and approvals only, or `--scope control` to
type from the phone.

**What the agent rules are for.** They stop an agent's mistake, or a prompt injection talking
it into casual misuse ("run `ptyro send other --raw 1`", "pair a device and print the link").
They are not a boundary against code determined to get around them: an agent runs as you, so
it can do anything you can, by means ptyro never sees. ptyro tells an agent's request apart by
the requesting process (its ancestry, its POSIX session, the environment it was started with);
a process that leaves all three looks like you at a plain shell, and is treated like you
(SPEC §17).

## Notes (the scratchpad)

The web app has a **Notes** page (`✎` in the header): the scratchpad file (`$PTYRO_SCRATCHPAD`) in a
box that saves as you type and merges with anything written meanwhile (a CLI `add`, an editor);
a Preview in which tapping a checkbox ticks the item; the open items; a find box; and the saved
copies, from which one can be put back. The session list has a quick-add box, and each session a
`✎` button that fills it with the session's task and a link back. Writing needs a device paired
with `--scope control`; a read-only device reads. The app is also a **share target**: share text
or a link to ptyro from the phone's share sheet and it is added to the pad. While ptyro is
unreachable, adds and edits wait in the browser (IndexedDB) and are sent in order on reconnect.
