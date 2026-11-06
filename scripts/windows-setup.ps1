# Build ptyro.exe on Windows from this checkout, installing what that needs first.
#
#   powershell -ExecutionPolicy Bypass -File scripts\windows-setup.ps1            install what's missing, then build
#   powershell -ExecutionPolicy Bypass -File scripts\windows-setup.ps1 -NoInstall  build only (`make build-windows`)
#   make windows-setup / make build-windows                                        the same, once make is installed
#
# What it installs with winget, each only when missing (it checks first, so running it again is
# cheap): Git, rustup, the Visual Studio Build Tools' C++ workload (rustc links with MSVC; this
# one asks for an administrator, so expect one UAC prompt, and it's a few GB), and GNU make for
# the Makefile's other targets. Then the Rust toolchain this checkout pins (rust-toolchain.toml),
# a release ptyro.exe, and Microsoft's packaged ConPTY beside it (the version and SHA-256 that
# scripts/conpty.sh pins), which the daemon's sessions run in.
#
# It runs in Windows PowerShell 5.1 (every Windows 11 has it) and PowerShell 7.

param(
    # Build only: install nothing (what `make build-windows` runs).
    [switch]$NoInstall,
    # Skip GNU make (only the Makefile's targets need it, not the build).
    [switch]$NoMake
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Say([string]$what) { Write-Host "windows-setup: $what" }
function Fail([string]$what) { Write-Host "windows-setup: $what" -ForegroundColor Red; exit 1 }

# What a new terminal would see: installers change the registry's PATH, not this process's.
function Refresh-Path {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $cargo = Join-Path $env:USERPROFILE '.cargo\bin'
    $env:Path = (@($cargo, $machine, $user, $env:Path) | Where-Object { $_ }) -join ';'
}

function Have([string]$cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

# `winget install` one package; `$override` goes to its own installer.
function Winget-Install([string]$id, [string]$what, [string]$override = '') {
    if (-not (Have 'winget')) {
        Fail "winget is missing, so $what can't be installed for you: install `"App Installer`" from the Microsoft Store (https://apps.microsoft.com/detail/9NBLGGH4NNS1), then run this again"
    }
    Say "installing $what (winget $id)"
    $wargs = @('install', '--id', $id, '--exact', '--silent', '--accept-package-agreements', '--accept-source-agreements')
    if ($override) { $wargs += @('--override', $override) }
    & winget @wargs
    # 0x8A15002B: already installed, nothing newer.
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
        Fail "installing $what failed (winget exit code $LASTEXITCODE): run ``winget install --id $id`` yourself to see why, then run this again"
    }
    Refresh-Path
}

# This machine's CPU: PROCESSOR_ARCHITECTURE names the shell's, which is AMD64 for an x64
# PowerShell emulated on ARM64; PROCESSOR_IDENTIFIER names the CPU.
$cpu = "$env:PROCESSOR_ARCHITECTURE $env:PROCESSOR_ARCHITEW6432 $env:PROCESSOR_IDENTIFIER"
if ($cpu -match 'ARM') { $rid = 'arm64'; $vc = 'ARM64'; $arch = 'aarch64' }
elseif ($cpu -match 'AMD64|x86_64|Intel64') { $rid = 'x64'; $vc = 'x86.x64'; $arch = 'x86_64' }
else { Fail "unsupported CPU ($cpu): ptyro builds for x64 and ARM64" }

# The MSVC C++ tools for this CPU: Visual Studio's own finder says where they are.
$installerDir = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer'
$vswhere = Join-Path $installerDir 'vswhere.exe'
function Msvc-Path {
    if (-not (Test-Path $vswhere)) { return $null }
    $p = & $vswhere -nologo -latest -products '*' -requires "Microsoft.VisualStudio.Component.VC.Tools.$vc" -property installationPath
    if ($p) { return "$p".Trim() } else { return $null }
}

Refresh-Path

if (-not $NoInstall) {
    if (-not (Have 'git')) { Winget-Install 'Git.Git' 'Git' }
    if (-not (Have 'rustup')) { Winget-Install 'Rustlang.Rustup' 'rustup (Rust)' }
    if (-not (Have 'rustup')) {
        Fail "rustup was installed but isn't found: open a new PowerShell and run this again"
    }
    if (-not (Msvc-Path)) {
        $workload = "--add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.$vc --includeRecommended"
        $existing = $null
        if (Test-Path $vswhere) {
            $existing = & $vswhere -nologo -latest -products '*' -property installationPath
        }
        if ($existing) {
            # Visual Studio or its Build Tools are there without the C++ workload: add it to them.
            Say "adding the C++ build tools to $existing (Windows asks for an administrator)"
            $p = Start-Process -FilePath (Join-Path $installerDir 'setup.exe') -Wait -PassThru -Verb RunAs `
                -ArgumentList "modify --installPath `"$("$existing".Trim())`" $workload --passive --norestart"
            if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) {
                Fail "the Visual Studio Installer exited with $($p.ExitCode): add `"Desktop development with C++`" in the Visual Studio Installer, then run this again"
            }
        } else {
            Say 'the C++ build tools are a few GB; Windows asks for an administrator once'
            Winget-Install 'Microsoft.VisualStudio.BuildTools' 'the Visual Studio Build Tools (C++)' "--passive --wait $workload"
        }
        if (-not (Msvc-Path)) {
            Fail "the MSVC C++ build tools still aren't found: add `"Desktop development with C++`" in the Visual Studio Installer, then run this again"
        }
    }
    if (-not $NoMake -and -not (Have 'make')) { Winget-Install 'ezwinports.make' 'GNU make' }
}

foreach ($need in 'git', 'rustup', 'cargo') {
    if (-not (Have $need)) {
        Fail "$need isn't found: run this without -NoInstall to install what's missing (or open a new PowerShell if you just installed it)"
    }
}
if (-not (Msvc-Path)) {
    Fail "the MSVC C++ build tools aren't found: run this without -NoInstall, or add `"Desktop development with C++`" in the Visual Studio Installer"
}

Push-Location $root
try {
    # The toolchain rust-toolchain.toml pins (a no-op once it's there).
    Say 'the Rust toolchain this checkout pins'
    & rustup toolchain install
    if ($LASTEXITCODE -ne 0) { Fail 'rustup toolchain install failed (see above)' }

    Say 'building ptyro.exe (release; the first build takes a few minutes)'
    & cargo build --release --locked -p ptyro
    if ($LASTEXITCODE -ne 0) { Fail 'cargo build failed (see above)' }
    $out = Join-Path $root 'target\release'
    $exe = Join-Path $out 'ptyro.exe'

    # Microsoft's packaged ConPTY, pinned in scripts/conpty.sh (one place for its version and hash).
    $sh = Get-Content -Raw (Join-Path $root 'scripts\conpty.sh')
    $version = [regex]::Match($sh, '(?m)^version=(\S+)').Groups[1].Value
    $sha256 = [regex]::Match($sh, '(?m)^sha256=(\S+)').Groups[1].Value
    if (-not $version -or -not $sha256) { Fail "can't read the ConPTY pin from scripts/conpty.sh" }
    $dir = Join-Path $root 'target\conpty'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $pkg = Join-Path $dir "microsoft.windows.console.conpty.$version.nupkg"
    $ok = (Test-Path $pkg) -and ((Get-FileHash -Algorithm SHA256 $pkg).Hash.ToLower() -eq $sha256)
    if (-not $ok) {
        Say "fetching Microsoft.Windows.Console.ConPTY $version"
        $url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg"
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile "$pkg.part"
        $got = (Get-FileHash -Algorithm SHA256 "$pkg.part").Hash.ToLower()
        if ($got -ne $sha256) {
            Remove-Item "$pkg.part" -Force
            Fail "the ConPTY package's SHA-256 is $got, not the pinned ${sha256}: refusing it"
        }
        Move-Item -Force "$pkg.part" $pkg
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($pkg)
    try {
        foreach ($entry in "runtimes/win-$rid/native/conpty.dll", "build/native/runtimes/$rid/OpenConsole.exe") {
            $e = $zip.GetEntry($entry)
            if (-not $e) { Fail "the ConPTY package has no $entry" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($e, (Join-Path $out (Split-Path -Leaf $entry)), $true)
        }
    } finally { $zip.Dispose() }

    # Does Windows let it run? Smart App Control refuses some unsigned programs.
    try { $ran = & $exe --version 2>&1 } catch { $ran = "$_" }
    if ("$ran" -notmatch '^ptyro ') {
        if ("$ran" -match 'Application Control|blocked') {
            Write-Warning ("Windows blocked the ptyro.exe you built: Smart App Control runs only signed programs it knows, " +
                "and a build of your own isn't signed. Some builds pass, some don't. The client in WSL works meanwhile.")
        } else {
            Write-Warning "ptyro.exe didn't run: $ran"
        }
        exit 1
    }
    if (-not (Test-Path (Join-Path $out 'ptyrow.exe'))) { Fail "the build made no ptyrow.exe beside $exe" }
    Say "built $exe ($ran), with ptyrow.exe, conpty.dll and OpenConsole.exe beside it"
    Say "run it: & '$exe'   (the Makefile's other targets run from Git Bash: they're POSIX shell)"
} finally {
    Pop-Location
}
