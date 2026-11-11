# ptyro security review: what is missing (2026-11-11)

Scope: read-only review of `the review checkout` (branch `docs/review-2026-11`). Nothing was built, run or probed. Paths are relative to that checkout. Four readers split the code: daemon IPC / web / devices / machines; agents / recipes / git; terminal / storage / notifier; install / CI / supply chain / Windows.

Labels: **VERIFIED** = the code path was read end to end. **SUSPECTED** = reasoned from partial reading, needs confirmation. "Accepted" = SPEC §17 or SECURITY.md already names it as a known limit.

---

## 1. Threat model

### 1.1 Assets

| Asset | Where |
|---|---|
| Code execution as the user | daemon (owns every PTY), `Spawn`, scripts, verify checks, `/api/host` |
| Session contents (screens, scrollback, recordings, chat text) | daemon memory, `data/sessions/*/pty.log`, `chat.db` |
| Credentials ptyro holds | device token hashes (`ptyro.db` kv), other machines' tokens in clear (`data/machines/*.token`), local CA key (`data/serve/tls/ca-key.pem`), VAPID key, callback HMAC key (`data/callback.key`), profile secrets, config-file tokens/webhooks |
| Human consent | permission prompts, gates, script/recipe trust, pairing |
| Integrity of what gets shipped | change sets, push/PR/merge, verify evidence |
| The release channel | GitHub release assets, install scripts, Homebrew tap, self-update |

### 1.2 Trust boundaries and what the code does about each

| Boundary | Enforced in code | Assumed / not enforced |
|---|---|---|
| Other local user → daemon socket | **Enforced.** Dir 0700 with owner check, socket 0600 under umask 077, peer-uid on accept (`ptyro-daemon/src/listen.rs:74-141`); client checks the server's uid (`ptyro-client/src/lib.rs:479-490`). Windows: SID-only DACL, reject-remote, first-instance, client checks the pipe's server. | Client fails open if peer creds are unavailable (S-27). Pairing code visible in `ps` with `pair --open` (S-07). |
| Network → `ptyro serve` | **Enforced.** Always TLS, 256-bit tokens hashed and compared in constant time, Hello rewritten to `web`, scope checked per request and keystroke, lockout, connection caps, Host and Origin checks, strict CSP, no `innerHTML` anywhere in `web/src`. | First contact is trust-on-first-use of a self-signed CA. Tokens never expire. `open` (0.0.0.0) is the automatic choice without Tailscale. |
| Paired device → scope | **Enforced** (`devices::needed` is an exhaustive match; `guard_device`). | `/api/host` operations leave no audit record (S-10). Default scope is `full`. |
| Machine ↔ machine | TLS pinned to the CA fingerprint in the code, or public WebPKI for `*.ts.net`. | Link is always `full`; token in clear on disk; plain `http://` codes accepted (S-08, S-09). |
| Agent in a session → daemon | **Tripwire only.** Caller placed by pid ancestry / session leader / env; a short refusal list. | Fails open by design (accepted). Beyond that: without a run policy nearly the whole API is open, and a spawned shell session launders every refusal (S-01, S-02). |
| Agent → agent | Mail sender forced to the caller, framed as not-from-user, capped. | `MsgList` returns everyone's mail (S-04). No per-agent capability separation. |
| Child output → daemon and clients | **Mostly enforced.** Emulator is the consumer; clients paint cells; OSC caps; queries never echo. | Title stack memory blow-up (S-05); `replay` writes raw bytes (S-06); some CLI columns print agent text raw (S-15). |
| Repository → code execution | **Enforced** for scripts/recipes (fingerprinted trust; load, fingerprint and run in one pass). Background git hardened. | Raw-command verify/fanout paths skip trust (S-03). Several repo values sit outside the fingerprint (S-19). User-asked git ops run repo hooks (accepted). |
| Web page in the browser → serve | **Enforced.** Host allow-list (DNS rebinding), Origin check on `/ws` and POST, bearer token (no ambient cookie), no CORS headers. | Origin fallback ignores scheme and port (S-22, low because nothing is ambient). |
| Release channel → installed binary | Checksum mandatory; provenance optional. | Checksum is same-origin; provenance fails open (S-11). |
| Same-uid process → ptyro's files | **Not a boundary** (accepted, SPEC §17). | Everything in 1.1 is readable/writable by any process the user or an agent runs. |

### 1.3 Actors

1. Another local user. 2. A malicious or prompt-injected agent in a session. 3. Hostile terminal output (a `cat`, an ssh host). 4. A hostile repository. 5. A network attacker (LAN with `open`, tailnet peer, path to a machine link). 6. A malicious web page. 7. A stolen device or leaked token. 8. Supply chain (release assets, CI, crates).

The model SPEC §17 states ("the boundary is the OS user") is honest and the outer boundaries are well built. The gaps cluster in two places: the **agent tripwire is much thinner than the SPEC's table implies**, and a handful of **untrusted-string-to-sink** paths (curl config, raw replay, CLI prints, emulator title stack).

---

## 2. Findings

Ordered by severity. No critical findings.

### High

**S-01 · high · An agent sheds every refusal by spawning a shell session** — VERIFIED
- Why high: no placement evasion and no file access needed; uses only ptyro's own API; defeats every "refused to any agent" row in SPEC §17 (answering prompts, pairing, trust, daemon upgrade).
- Evidence: `ptyro-daemon/src/mail.rs:177-180` — `if s.kind() != SessionKind::Agent { return Ok(()); }` ("nothing is refused to other kinds of session"). `Spawn` is not in the universal refusal list (`mail.rs:194-228`), and typing into a non-blocked session is allowed.
- Sketch: the agent asks the daemon to spawn a plain shell session, then sends text into it. Commands run there are placed in a non-agent session, so `answer`, `pair`, `project trust` and the rest pass as if a person typed them.
- Mitigation: carry provenance — a session spawned by an agent-placed caller inherits "agent" for guard purposes (and so do its descendants); refuse raw-`argv` `Spawn` and send-into-shell from agent callers unless a policy allows it.
- Accepted? No. SPEC §17 accepts `setsid`-style escapes "from a process ptyro can't place"; this caller is placed correctly and still passes. SECURITY.md lists "an agent answering its own permission prompts… through ptyro" as in scope.

**S-02 · high · Without a run policy, an agent has nearly the whole daemon API** — VERIFIED
- Why high: the default state (no lead, no policy) is the common one; one `ptyro` call from inside the agent.
- Evidence: `mail.rs:230` — `let Some((run, Some(policy), _)) = self.run_of_session(..) else { return Ok(()) };`. Before it only gates/answers, device pair/revoke, push subscribe, trust-carrying requests and `Upgrade{binary:Some}` are refused.
- Open to any agent, and material: `Spawn` with arbitrary argv (the `dangerous = true` gate and badge apply only when `agent` is named, `launch.rs:459-482`, so an argv carrying an agent's skip-permissions flag is neither checked nor badged); `SetVerify` including removing its own verified-done gate (`mail.rs:1852`); `Web{on:true}` (turn the web server on, possibly on 0.0.0.0); `MachineAdd/Remove`; `DeviceList`, `DeviceClaim`; `SetupRun`; `ReloadConfig`, `Shutdown`, `Upgrade{binary:None}`; `ProjectAdd/Untrust`, `WorktreeRun`, `RecipeRun` without `trust`; reading any session's screen, scrollback, replay and export; typing into any non-blocked session.
- Mitigation: make the allow-list the default for every agent session and let policies widen it. At minimum add `SetVerify`, `Web`, `Machine*`, `SetupRun`, `Shutdown`/`Upgrade`, raw-argv `Spawn` and cross-session reads to the universal refusals.
- Accepted? Partly: SPEC §17 says typing into idle agents, mail, reading and spawning *configured* agents stay allowed. Web, machines, verify, setup and raw argv are not mentioned.

### Medium

**S-03 · med · A policy-limited lead runs an arbitrary command through `fanout`'s `test`** — VERIFIED
- Why: the one actor ptyro promises to hold to an allow-list gets daemon-side execution outside its sandbox.
- Evidence: `mail.rs:1623-1670` passes `test` unvalidated; `runs.rs:2477` writes it as a verify check; `verify.rs:67` — `let Some(name) = name else { return argv("check", cmd) };` runs any string that is not a script name as a command, with no trust check. `crates/ptyro/src/mcp.rs:348` advertises "scripts.run.test or a command".
- Mitigation: from an agent caller accept only a trusted `scripts.run.*` name for `test`/`verify`/`judge`.
- Accepted? No (SECURITY.md: "escaping a run policy, through ptyro" is in scope).

**S-04 · med · `MsgList` returns every session's mail to any caller** — VERIFIED
- Evidence: `mail.rs:1786-1806` takes no caller and filters only by optional `run`; `MsgCheck`/`MsgAck` are pinned to the caller. `MsgList` is on the policy allow-list (`mail.rs:275`).
- Sketch: an agent in one run lists mail of another run or mail addressed to the user; it is also a channel for cross-agent prompt-injection reconnaissance.
- Mitigation: scope to the caller's run/session when the caller is placed.
- Accepted? No. SPEC §17 says "Sessions read only their own mail".

**S-05 · med · Title stack: ~2 MiB of output becomes ~8 GiB of daemon memory** — VERIFIED by reading both sources, not run
- Why: any output can do it, and the daemon holds every session.
- Evidence: `ptyro-term/src/emulator.rs:212` `MAX_STRING = 2 << 20`; `:467-469` clips only ptyro's own copy of the title to 512 bytes. alacritty_terminal 0.26.0 keeps the unclipped title and pushes a clone per `CSI 22 t` up to depth 4096. No filter for XTWINOPS 22/23 in `ptyro-term/src`.
- Mitigation: cap title OSCs at 512 bytes in the pre-parser filter (`emulator.rs:269`) or drop `CSI 22/23 t`; add an RSS assertion to the emulator fuzz target.
- Accepted? No; SPEC §17 claims "title length capped". Related, SUSPECTED: OSC 8 URIs in scrollback are bounded per string (2 MiB) but not in total (`emulator.rs:182`).

**S-06 · med · `ptyro replay` and raw export write recorded child bytes to the real terminal** — VERIFIED
- Evidence: `crates/ptyro/src/record.rs:87-88` — `Record::Output(b) | Record::Keyframe(b) … out.write_all(b)`; `:107`, `:135` for `raw`.
- Sketch: a recording containing clipboard writes, title sets or terminal queries; the outer terminal acts on them and query replies arrive as typed input in the user's shell after the replay.
- Mitigation: replay through the emulator and paint cells, or strip OSC/DCS/APC and query CSIs.
- Accepted? No; contradicts "clients render cells, not raw bytes".

**S-07 · med · `ptyro pair --open` puts the pairing code on another program's command line** — VERIFIED
- Why: a second local user can read process arguments; the code is traded for a token (default scope `full`) over loopback, which that user can reach. The window is short (the browser claims at once) but the prize is full control.
- Evidence: `crates/ptyro/src/remote.rs:388` `let link = format!("{base}/#pair={code}");` and `:418` `ptyro_core::open::open(&Opened::Url(&link))`, which runs `open` / `xdg-open` / `explorer.exe` with the link as an argument (`ptyro-core/src/open.rs:10-32`).
- Mitigation: open a link that carries no secret (e.g. a loopback-only one-shot handoff the page fetches), or write a 0600 HTML file that redirects and open that.
- Accepted? No. The comment at `remote.rs:385-387` reasons about scrollback and server logs, not `ps`.

**S-08 · med · The CA phones (and Windows) are told to trust has no name constraints, and its key is a plain file** — VERIFIED
- Evidence: `crates/ptyro/src/tls.rs:170-174` — `IsCa::Ca(BasicConstraints::Constrained(0))` with `KeyCertSign`, no `name_constraints`; ten-year life; key at `data/serve/tls/ca-key.pem` (0600). `remote.rs:395-410` offers to add it to the Windows user's root store.
- Sketch: anything that reads that file (any agent, any same-uid process, a leaked copy) can mint a certificate for any site that the user's phone and Windows browsers accept; with a network position that is interception of everything, not just ptyro.
- Mitigation: add permitted-subtree name constraints (the machine's names and addresses) to the CA; shorter CA life; prefer the Tailscale certificate path where it exists; say so in the pairing text.
- Accepted? No. Backups do leave the CA key out by default (good).

**S-09 · med · Machine links: always `full`, token in clear, plain HTTP allowed, trust is transitive** — VERIFIED
- Evidence: `crates/ptyro/src/machines.rs:249` — `DevicePair { name: "machine", scope: Scope::Full, machine: true }` (no narrower option). `ptyro-daemon/src/machines.rs:351-371` stores the other machine's token as `data/machines/<name>.token`. `crates/ptyro/src/machines.rs:35` accepts `http://` codes and `:421-423` returns the bare TCP stream ("only sensible over Tailscale", not checked), so the code, the returned token and every later Hello go in clear. `daemon.rs:1202-1206`: a device's request for another machine's object is checked against its scope here and then forwarded over this machine's `full` link.
- Sketch: compromise of one machine (or of any same-uid process on it) is compromise of every machine it links; a device paired here with `full` is `full` everywhere. An `http://` link on a shared network leaks a full-scope token to a passive listener.
- Mitigation: refuse `http://` unless the host is loopback or a Tailscale address; a `--scope` for machine links; keep the token in the OS keychain where there is one; show linked machines' reach on the Devices page.
- Accepted? Not discussed in SPEC §17.

**S-10 · med · Device actions on the machine itself leave no record** — VERIFIED
- Evidence: `crates/ptyro/src/serve.rs:1071-1154` (`api_host`) runs `host::run` after a `DeviceList` probe with no log line; `devices.rs:395-399` audits only requests that target a session. `config.save`, `settings.apply`, `daemon.stop`, `backup.create` (with a caller-chosen output path), `machine.add`, `integrations.install` are therefore invisible after the fact.
- Mitigation: one append-only audit log for every device request at `approve` or above and every host operation (device, address, operation, outcome).
- Accepted? SPEC §17 says "remote actions in the event log"; only session actions are.

**S-11 · med · Release integrity rests on a same-origin checksum; provenance fails open** — VERIFIED
- Evidence: `scripts/install.sh:115-117` fetches the tarball and `SHA256SUMS` from the same base; `:129-136` verifies provenance only when `gh` exists and, when verification fails, prints a note and installs. Same in `scripts/install.ps1:143-152` and the self-update (`ptyro-agents/src/own.rs:624-660`). Verification pins the repository only, not the signer workflow or tag (`install.sh:130`), so an older attested build under a new name passes.
- Mitigation: a failed verification (other than "not logged in") is fatal; pin `--signer-workflow` and the tag; consider a signing key verified without `gh` and baked into the binary for self-update.
- Accepted? No; SECURITY.md:20-21 overstates what the script does.

**S-12 · med/low · ntfy sink passes agent text as curl `data =`: a body starting with `@` uploads a local file** — VERIFIED code path (curl's `@file` meaning from its documentation, not run)
- Evidence: `ptyro-daemon/src/notifier.rs:361` — `cfg.push(format!("data = {}", quote(&note.body)));`. The body is raw OSC 9/777 text (`session.rs:999`, `agents.rs:378`). Needs an ntfy sink with the `notification` kind turned on (not the default, `config.rs:491`).
- Sketch: output announces a notification whose text is `@` plus a private file's path; the file is posted to the topic.
- Mitigation: `data-raw` (push already uses `data-binary` from a private file, `push.rs:253`).

**S-13 · med/low · The approve/deny link listener has no notion of who is asking** — VERIFIED code path, impact SUSPECTED
- Evidence: HMAC key in `data/callback.key` (`notifier.rs:467-486`); a valid token answers a prompt with no caller identity (`:594-630`); links are handed to sinks and to `PTYRO_TOAST_APPROVE` (`:1029`); base defaults to `http://{listen}` (`:493`); no connection cap (`:575-590`); `safe_address` counts all of 100.64.0.0/10 as Tailscale (`config.rs:1415-1418`), which is also carrier NAT space.
- Sketch: an agent reads the key (or a link from a sink it can see) and approves its own prompt. Only when `[notifications.callback] listen` is set.
- Mitigation: key in daemon memory only (regenerated per start), refuse loopback peers placed in an agent session, connection cap.
- Accepted? Under the same-uid limit; not called out.

### Low

**S-14 · low/med · `shell_problems` misjudges quoting, so a recipe value can become shell syntax** — VERIFIED by reading, not executed. `ptyro-run` `template.rs:77-115` tracks only `'`, `"` and `\`; a `#` comment with an apostrophe, or a heredoc, flips its state so a following quoted placeholder is rendered as if bare. Mitigation: pass values as environment variables or positional parameters instead of splicing text. Contradicts the SPEC §17 "one quoted shell word" row.

**S-15 · low/med · CLI prints agent-controlled strings unsanitised** — VERIFIED. `ptyro-agents/src/adapter.rs:210-224` (`tool_summary`) has no control filter; printed raw at `crates/ptyro/src/agents.rs:87,100,103,145,217` and `cli.rs:1632`, `:676`. Mitigation: one `printable()` at the print boundary.

**S-16 · low/med · `ptyro uninstall` recursively deletes paths it does not verify as ptyro's** — VERIFIED. With `PTYRO_HOME=X` it removes `X/config`, `X/data`, `X/state`, `X/run` (`uninstall.rs:195-201`, `:877-879`, `:1052-1058`) with no marker check; worktree rows fall back to `remove_tree(&w.path)` without checking the ptyro marker or containment (`:897-919`). Mitigation: require a marker file; refuse `PTYRO_HOME` equal to `/` or `$HOME`.

**S-17 · low · File modes are narrower in the SPEC than on disk** — VERIFIED. `ptyro.db` opened with no mode (`ptyro-store/src/lib.rs:394`, unlike `chat.db`); crash reports with PTY bytes via `std::fs::write` (`watchdog.rs:52`); `serve.log` (`serve_child.rs:154-157`), `daemon.log`, scratchpad (`ptyro-core/src/scratchpad.rs:158,196,259`), `serve.json` (`remote.rs:155-158`), `ca.pem`/`server.pem`/`tls.json`. Existing config/data/state directories are never narrowed or owner-checked at start (`ptyro-daemon/src/lib.rs:124-127`); only the socket directory is.

**S-18 · low · Windows: the "private" helpers are no-ops** — VERIFIED. `ptyro-core/src/os.rs:23-31,166-188`. Databases, recordings, logs, push and machine tokens inherit the parent ACL; real narrowing exists (`ptyro_pty::acl`) but is applied to the config, shell integration and TLS keys only. Matters when `PTYRO_HOME`/`XDG_*` point outside the profile. The SPEC row "Secrets in recordings/logs: files 0600" is untrue on Windows.

**S-19 · low · Repository values outside the trust fingerprint** — VERIFIED. `[worktree] copy/share/share_mode`, `.worktreeinclude`, `[prompts]`, `start_agents` are not in `trust_fingerprint` (`ptyro-core/src/project.rs:207-216,304-312`). `[prompts]` is repository-authored prompt text delivered to agents.

**S-20 · low · Git: user-asked operations keep repo-config transports; two raw calls skip the wrapper** — hooks VERIFIED (accepted), transports SUSPECTED. `fetch` keeps `core.sshCommand`, `ext::` URLs, `credential.helper` (`ptyro-git/src/lib.rs:476-480`). `changesets.rs:1390` runs `git diff --no-index` without the hardened wrapper.

**S-21 · low · Tokens never expire and devices are not bound to anything** — VERIFIED. `devices.rs:316-329`: a token is valid until revoked; no idle timeout, rotation, or re-check. The token sits in `localStorage` and IndexedDB (`web/src/app.ts:105-108`). Default scope is `full`. Accepted ("full by default with a warning") but expiry is not discussed.

**S-22 · low · Origin fallback ignores scheme and port; no HSTS** — VERIFIED. `serve.rs:770` `names.contains(&strip_port(&authority))`: any page on a configured name at another port passes. Low because authentication is a bearer token, not a cookie. No `Strict-Transport-Security` in `secure()` (`serve.rs:634-645`).

**S-23 · low · Lockout is per source address** — VERIFIED. `serve.rs:239-254`. Behind a reverse proxy (`public_url`) every client shares one address, so ten bad logins by anyone lock everyone out for ten minutes. Irrelevant to guessing (codes and tokens are 256-bit).

**S-24 · low · `web on` picks `open` (0.0.0.0) when Tailscale is not running** — VERIFIED. `ptyro-core/src/config.rs:935-940`. Documented; exposes the pre-auth surface and a click-through certificate to the whole LAN. An agent can trigger it (S-02).

**S-25 · low · Release assets are mutable; release builds use a shared cache and unpinned tools; `install.sh` is not truncation-safe** — VERIFIED. `.github/workflows/release.yml:179-180` (`--clobber`), `:126` (rust-cache in the shipping build), `:123` (unpinned `choco install make`); `install.sh:84-160` runs at top level, and its `curl` lacks `--proto '=https'`.

**S-26 · low · Windows hook relay opens the pipe before checking who serves it, at the default impersonation level** — code VERIFIED, impact SUSPECTED. `crates/ptyro/src/relay.rs:54`, check at `:63`. Pipe-name squatting also denies service (`listen.rs:177-179`).

**S-27 · low · Smaller items** — VERIFIED unless noted.
- Unix client accepts the server when peer credentials are unavailable (`ptyro-client/src/lib.rs:489`).
- Socket fallback under `/tmp` or `$TMPDIR`: `prepare` uses `metadata` (follows links) before chmod 0700 (`listen.rs:98-114`); another user pre-creating the directory denies service.
- Placement re-resolves the pid per request; a connecting helper that exits looks like a person (SUSPECTED, `mail.rs:171-172`).
- Custom `desktop_command` substitutes agent text into argv; a body starting with `-` becomes an option (`notifier.rs:838-841`).
- `install-service` freezes the invoking shell's PATH into the login unit (`crates/ptyro/src/service.rs:29-35`).
- `/api/host` passes caller strings as CLI positionals without `--` (`host.rs:423`, `:298`); `full` scope only, so no escalation.
- `serve.json` is trusted by `ptyro pair` for the address it prints; a same-uid process can point the next pairing QR elsewhere.
- OSC 52 clipboard writes are on by default (accepted).

---

## 3. What is missing entirely

1. **Per-agent capabilities.** One socket, one authority. There is no token or handle per session; identity is inferred from the pid and, for non-agent sessions, not applied at all.
2. **Provenance of sessions.** Nothing records that a session was started by an agent, so nothing can treat it as one (root of S-01).
3. **A tamper-evident audit log.** Per-session events and `daemon.log` exist; there is no single record of who (device, agent session, local client) asked for what, and host operations are not recorded at all.
4. **Sandboxing.** Agents, scripts, verify checks and `after_turn` run as the user with the daemon's environment. Documented, not built.
5. **Secret redaction at rest.** The credential scrubber is used only for summaries (`summary.rs:208-270`). Recordings (on by default, 16 MiB, 30 days), scrollback, crash reports, inbox bodies and chat text are stored as seen. No encryption at rest.
6. **Token lifecycle.** No expiry, idle timeout, rotation, or "last used from a new address" warning.
7. **Narrow machine links.** No scope below `full`, no keychain storage.
8. **A constrained CA.** No name constraints on a root the user installs on phones and Windows.
9. **A signature that does not depend on `gh`.** Checksums are same-origin; provenance is optional and fails open.
10. **Permission checks at startup.** Existing data/state/config directories are not owner- or mode-checked.
11. **Fuzzing of several untrusted-input parsers.** HTTP routing/Host/pairing body in `serve.rs`, hook/statusline/report payloads, mail framing, `ptyro-pty/src/cmdline.rs` quoting, `serve.json`/`tls.json`, self-update archive checks, structured/ACP driver frames.
12. **Output sanitising at the CLI print boundary** as a single helper.
13. **Rate or resource limits for local clients** (connections, spawn rate, mail volume outside a policy).
14. **A SECURITY.md statement of the agent boundary as it really is**, so users know that "agents never answer for a person" is a default against mistakes, and what to do (another user, no socket) when they need more.

---

## 4. Done well

- **Local IPC:** private directory with owner check, umask around bind, peer-uid on both ends; Windows pipe with a SID-only DACL, remote clients rejected, first-instance, client-side owner check.
- **Credentials:** 256-bit tokens and pairing codes from the OS source with no fallback; hashed at rest; constant-time compare across every record; pairing codes single-use, ten minutes, never the token itself; revocation closes live connections.
- **Scopes:** `devices::needed` is an exhaustive match with no catch-all, so a new request cannot ship without a scope decision.
- **Web:** always HTTPS; Host allow-list against DNS rebinding; Origin check; no CORS; strict CSP, `frame-ancestors 'none'`, `nosniff`, `no-referrer`, `no-store`; no third-party code and no npm dependencies; **zero** `innerHTML`/`eval` sinks in `web/src`; data-derived links go through an http(s)-only filter (`web/src/kit.ts:492-505`); body limits, header and body timeouts, pending-login and connection caps, IPv6 /64 lockout keys.
- **Protocol:** 16 MiB frame cap, zstd decompression bounded, version check, test knobs compiled out of release builds.
- **Machines:** CA-fingerprint pinning with a real chain check; ssh targets cannot be options and are passed after `--`.
- **Terminal:** OSC 7501 strict allow-list; notifications stripped and capped; queries never echo; OSC 52 read denied; combining-mark and history caps; no graphics protocols; ptyro never opens URLs from terminal content.
- **Agents:** mail sender forced, framed, capped, reply depth capped; hook/status requests bound to the caller's own session by request type, not claimed kind; shipping is human-only; callback links are HMAC, expiring, single-use across restarts, POST-only, bound to the prompt.
- **Git and repos:** `core.fsmonitor=false` everywhere; hooks, external diff, filters off for background calls; `--end-of-options`; names and revs cannot start with `-`; script load, fingerprint and run in one pass; repositories cannot set `[agents]`, `[profiles]`, `[daemon]`.
- **Notifier and push:** secrets reach curl through stdin config, never argv; push endpoint host allow-list; RFC 8291 encryption; VAPID key 0600.
- **Supply chain:** actions pinned by SHA; read-only default permissions; one write job in a protected environment; no `pull_request_target`; `deny.toml` with yanked/unknown-source/wildcard denied and no ignores; rustls, no OpenSSL; `unsafe_code = "deny"` workspace-wide with unsafe confined to `ptyro-pty`; committed web bundle rebuilt and compared in CI; self-update checks archive member names and refuses links.
- **Backups:** 0600, never overwrite, leave out the CA key and recordings by default; no restore command, so no archive-extraction surface.

---

## 5. Not reached

- `orch.rs`, most of `mcp.rs`, `handle_chat` authorisation, `ProfileEnv` redaction, profile scrubbing (`Plan::strip`), whether profile secrets reach check commands.
- `ptyro-agents/src/setup.rs`, `crates/ptyro/src/setup.rs` (what `SetupRun` writes into agents' global config), Codex and other adapters' hook files.
- `finish.rs`, `review.rs`, `github.rs` push paths; worktree archive/restore/remove path handling; importers' command building.
- Soundness of `ptyro-pty/src/windows.rs` (53 unsafe sites), `winproc.rs`, `winreg.rs`, `console.rs`; `winapp.rs` (`ptyro:` link handler); `wsl*.rs` (`wsl_conf.rs:81-92` pipes text to a sudo command and deserves a read).
- `ptyro-agents/src/updates.rs` and who may trigger agent updates.
- The TUI's rendering of names and mail in overlays; `copy.rs`; `sidecar.rs`; `diag.rs`.
- Robustness of the live-upgrade image decoder beyond confirming a fuzz target exists.
- What a hostile *linked* machine can do to the machine that added it (mirror data is merged into lists and alerts; read only in outline).
- GitHub-side settings (environment reviewers, tag protection, immutable releases).
- Nothing was executed: S-05, S-12 and S-14 in particular should be confirmed with a test before fixing.
