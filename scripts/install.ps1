# Install a ptyro release on Windows (ROADMAP WIN3): download the zip for this machine, check its
# SHA-256 against the release's SHA256SUMS (and its signed provenance when `gh` is installed), put
# ptyro.exe in %LOCALAPPDATA%\Programs\ptyro\bin and that folder on your PATH. The Windows build is
# the client of the daemon in WSL2: install ptyro in your distro too (README, Windows). Every
# release carries this script as an asset, so the URL below is always the newest one.
#
#   irm https://github.com/nealhardesty/ptyro/releases/latest/download/install.ps1 | iex
#   powershell -ExecutionPolicy Bypass -File install.ps1
#   $env:PTYRO_VERSION = '0.0.50'; irm .../install.ps1 | iex
#
# Removing it hands over to `ptyro uninstall` (it lists everything it removes and asks first);
# `iex` takes no arguments, so either set PTYRO_UNINSTALL or run the script as a script block:
#   $env:PTYRO_UNINSTALL = '1'; irm .../install.ps1 | iex
#   & ([scriptblock]::Create((irm .../install.ps1))) -Uninstall --dry-run   # what it would remove
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall
# Arguments after -Uninstall go to `ptyro uninstall` (--dry-run, --yes, --backup FILE…). If
# ptyro.exe won't run, only what this script installed is removed, and its PATH entry.
#
# Environment (the same names as install.sh):
#   PTYRO_VERSION          the version to install, like 0.0.50 (default: the latest release)
#   PTYRO_PREFIX           where to install (default %LOCALAPPDATA%\Programs\ptyro): ptyro.exe goes
#                          in $PREFIX\bin
#   PTYRO_RELEASE_URL      a directory (URL, file:// too) with the zip and SHA256SUMS instead of
#                          GitHub's release (for tests and mirrors)
#   PTYRO_VERIFY=0         don't try `gh attestation verify`
#   PTYRO_REQUIRE_ATTESTATION=1   fail unless the provenance verifies (needs `gh`)
#   PTYRO_NO_MODIFY_PATH=1 leave your PATH alone
#   PTYRO_UNINSTALL=1      remove ptyro instead, like -Uninstall
#
# Works in Windows PowerShell 5.1 and PowerShell 7. To remove it again: `ptyro uninstall`.
param([switch]$Uninstall)

# Never `exit` in here: under `irm | iex` that would close the user's PowerShell.
function Uninstall-Ptyro {
    $ErrorActionPreference = 'Stop'
    $prefix = $env:PTYRO_PREFIX
    if (-not $prefix) { $prefix = Join-Path $env:LOCALAPPDATA 'Programs\ptyro' }
    $bin = Join-Path $prefix 'bin'
    $exe = Join-Path $bin 'ptyro.exe'
    if (-not (Test-Path -LiteralPath $exe)) {
        $found = Get-Command ptyro.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { $exe = $found.Source }
    }
    $runs = $false
    if (Test-Path -LiteralPath $exe) {
        try { $ran = & $exe --version 2>&1; $runs = ($LASTEXITCODE -eq 0 -and "$ran" -match '^ptyro ') } catch { }
    }
    # ptyro uninstall knows its directories, worktrees, rc-file lines, ConPTY files and PATH entry.
    if ($runs) {
        Write-Host "handing over to $exe uninstall"
        & $exe uninstall @args
        return
    }
    $mine = Join-Path $bin 'ptyro.exe'
    if (-not (Test-Path -LiteralPath $mine)) {
        Write-Host "ptyro isn't installed in $bin or on your PATH: nothing to remove"
        return
    }
    if ($args -contains '--dry-run') {
        Write-Host "$mine doesn't run, so ptyro uninstall can't. Without --dry-run, this removes what this script installed: ptyro.exe, conpty.dll and OpenConsole.exe from $bin, and $bin from your PATH"
        return
    }
    Write-Host "$mine doesn't run, so ptyro uninstall can't: removing what this script installed"
    foreach ($file in 'ptyro.exe', 'conpty.dll', 'OpenConsole.exe') {
        Remove-Item -LiteralPath (Join-Path $bin $file), (Join-Path $bin "$file.old") -Force -ErrorAction SilentlyContinue
    }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ -and $_ -ne $bin })
    if (($parts -join ';') -ne $userPath) {
        [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
        Write-Host "removed $bin from your PATH"
    }
    # The folders too, when nothing else is in them (Remove-Item would ask about a full one).
    foreach ($dir in $bin, $prefix) {
        if ((Test-Path -LiteralPath $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force)) {
            Remove-Item -LiteralPath $dir -Force
        }
    }
    Write-Host "removed ptyro.exe from $bin; ptyro's config and data, if any, stay"
}

function Install-Ptyro {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest's progress bar slows it down a lot
    $repo = 'nealhardesty/ptyro'
    # Windows PowerShell 5.1 may still offer TLS 1.0 first; GitHub needs 1.2.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    # The machine's own architecture. A process's environment names its own: an x64 PowerShell
    # (or Git Bash) emulated on ARM64 says AMD64. The system's environment in the registry doesn't.
    $arch = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -ErrorAction SilentlyContinue).PROCESSOR_ARCHITECTURE
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { $triple = 'x86_64-pc-windows-msvc' }
        'ARM64' { $triple = 'aarch64-pc-windows-msvc' }
        default { throw "install: $arch isn't a platform ptyro ships for (x64 and ARM64 are)" }
    }

    $version = $env:PTYRO_VERSION
    if (-not $version) {
        if ($env:PTYRO_RELEASE_URL) { throw 'install: set PTYRO_VERSION with PTYRO_RELEASE_URL' }
        try {
            $latest = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases/latest"
        } catch {
            throw "install: couldn't find the latest release (no network? a rate limit? set `$env:PTYRO_VERSION): $_"
        }
        $version = $latest.tag_name
    }
    $version = $version -replace '^v', ''
    $base = $env:PTYRO_RELEASE_URL
    if (-not $base) { $base = "https://github.com/$repo/releases/download/v$version" }
    $name = "ptyro-$version-$triple"

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("ptyro-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        # PowerShell 7's Invoke-WebRequest doesn't take file:// URLs, so those are copied.
        function Get-Release($file) {
            $uri = [Uri]"$base/$file"
            $to = Join-Path $tmp $file
            if ($uri.IsFile) { Copy-Item -LiteralPath $uri.LocalPath -Destination $to }
            else { Invoke-WebRequest -UseBasicParsing -Uri $uri -OutFile $to }
            $to
        }
        Write-Host "downloading ptyro $version for $triple"
        try { $zip = Get-Release "$name.zip" }
        catch { throw "install: no download at $base/$name.zip (is $version released for ${triple}?)" }
        try { $sums = Get-Release 'SHA256SUMS' }
        catch { throw "install: no checksums at $base/SHA256SUMS" }

        $want = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            $f = $line -split '\s+', 2
            if ($f.Count -eq 2 -and ($f[1] -eq "$name.zip" -or $f[1] -eq "*$name.zip")) { $want = $f[0].ToLower() }
        }
        if (-not $want) { throw "install: SHA256SUMS has no entry for $name.zip" }
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
        if ($got -ne $want) { throw "install: the download's checksum is $got, the release says ${want}: not installing" }
        Write-Host 'checksum ok'

        # Provenance: the release's archives are attested by the workflow that built them.
        $gh = Get-Command gh -ErrorAction SilentlyContinue
        if ($env:PTYRO_VERIFY -ne '0' -and $gh) {
            & $gh.Source attestation verify $zip --repo $repo *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "provenance ok (built by $repo's release workflow)" }
            elseif ($env:PTYRO_REQUIRE_ATTESTATION -eq '1') { throw "install: the provenance didn't verify" }
            else { Write-Host "note: couldn't verify the provenance with gh (not logged in, or an old release); the checksum matched" }
        } elseif ($env:PTYRO_REQUIRE_ATTESTATION -eq '1') {
            throw 'install: PTYRO_REQUIRE_ATTESTATION=1 needs gh (https://cli.github.com) to check the provenance'
        }

        Expand-Archive -LiteralPath $zip -DestinationPath $tmp
        $src = Join-Path $tmp $name
        $prefix = $env:PTYRO_PREFIX
        if (-not $prefix) { $prefix = Join-Path $env:LOCALAPPDATA 'Programs\ptyro' }
        $bin = Join-Path $prefix 'bin'
        New-Item -ItemType Directory -Force -Path $bin | Out-Null
        $exe = Join-Path $bin 'ptyro.exe'
        # ptyro.exe, and the packaged ConPTY its sessions run in (WIN0.5 g; older zips lack it).
        # A running file can't be overwritten, but it can be renamed out of the way.
        foreach ($file in 'ptyro.exe', 'conpty.dll', 'OpenConsole.exe') {
            $from = Join-Path $src $file
            if (-not (Test-Path -LiteralPath $from)) { continue }
            $to = Join-Path $bin $file
            if (Test-Path -LiteralPath $to) {
                $old = "$to.old"
                Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
                try { Copy-Item -LiteralPath $from -Destination $to -Force }
                catch { Move-Item -LiteralPath $to -Destination $old -Force; Copy-Item -LiteralPath $from -Destination $to }
            } else {
                Copy-Item -LiteralPath $from -Destination $to
            }
            Unblock-File -LiteralPath $to -ErrorAction SilentlyContinue
        }
        Write-Host "installed $exe"

        if ($env:PTYRO_NO_MODIFY_PATH -ne '1') {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $parts = @($userPath -split ';' | Where-Object { $_ })
            if ($parts -notcontains $bin) {
                [Environment]::SetEnvironmentVariable('Path', (($parts + $bin) -join ';'), 'User')
                Write-Host "added $bin to your PATH (new terminals see it)"
            }
            if (@($env:Path -split ';') -notcontains $bin) { $env:Path = "$env:Path;$bin" }
        }

        try { $ran = & $exe --version 2>&1 } catch { $ran = $_ }
        if ($LASTEXITCODE -ne 0 -or "$ran" -notmatch '^ptyro ') {
            if ("$ran" -match 'Application Control|blocked') {
                Write-Warning ("Windows blocked ptyro.exe: Smart App Control runs only signed programs it knows, " +
                    "and ptyro.exe isn't signed yet. The client in WSL works meanwhile: run ptyro inside wsl.")
            } else {
                Write-Warning "ptyro.exe didn't run: $ran"
            }
            return
        }
        Write-Host "$ran"
        Write-Host ''
        Write-Host 'Next: ptyro.exe is the client of the daemon in WSL2. If your distro has no ptyro yet,'
        Write-Host 'ptyro.exe offers to install it the first time you run it; or, in WSL:'
        Write-Host '  curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh'
        Write-Host 'Then run `ptyro` in Windows Terminal. Tab completion: add this line to $PROFILE'
        Write-Host '  ptyro completions powershell | Out-String | Invoke-Expression'
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($Uninstall -or $env:PTYRO_UNINSTALL -eq '1') { Uninstall-Ptyro @args } else { Install-Ptyro }
