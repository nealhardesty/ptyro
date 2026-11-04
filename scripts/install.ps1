# Install a ptyro release on Windows (ROADMAP WIN3): download the zip for this machine, check its
# SHA-256 against the release's SHA256SUMS (and its signed provenance when `gh` is installed), put
# ptyro.exe in %LOCALAPPDATA%\Programs\ptyro\bin and that folder on your PATH. The Windows build is
# the client of the daemon in WSL2: install ptyro in your distro too (README, Windows).
#
#   irm https://raw.githubusercontent.com/nealhardesty/ptyro/master/scripts/install.ps1 | iex
#   powershell -ExecutionPolicy Bypass -File install.ps1
#   $env:PTYRO_VERSION = '0.0.50'; irm .../install.ps1 | iex
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
#
# Works in Windows PowerShell 5.1 and PowerShell 7. To remove it again: `ptyro uninstall`.

function Install-Ptyro {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest's progress bar slows it down a lot
    $repo = 'nealhardesty/ptyro'
    # Windows PowerShell 5.1 may still offer TLS 1.0 first; GitHub needs 1.2.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    # The machine's own architecture, also from a 32-bit or emulated PowerShell.
    $arch = $env:PROCESSOR_ARCHITEW6432
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
        catch { throw "install: no download at $base/$name.zip (is $version released for $triple?)" }
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
        # A running ptyro.exe can't be overwritten, but it can be renamed out of the way.
        if (Test-Path -LiteralPath $exe) {
            $old = "$exe.old"
            Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
            try { Copy-Item -LiteralPath (Join-Path $src 'ptyro.exe') -Destination $exe -Force }
            catch { Move-Item -LiteralPath $exe -Destination $old -Force; Copy-Item -LiteralPath (Join-Path $src 'ptyro.exe') -Destination $exe }
        } else {
            Copy-Item -LiteralPath (Join-Path $src 'ptyro.exe') -Destination $exe
        }
        Unblock-File -LiteralPath $exe -ErrorAction SilentlyContinue
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
        Write-Host '  curl -fsSL https://raw.githubusercontent.com/nealhardesty/ptyro/master/scripts/install.sh | sh'
        Write-Host 'Then run `ptyro` in Windows Terminal. Tab completion: add this line to $PROFILE'
        Write-Host '  ptyro completions powershell | Out-String | Invoke-Expression'
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Ptyro
