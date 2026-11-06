<#
.SYNOPSIS
Starts the app on Windows and checks it the way a person would at a glance.

.DESCRIPTION
The Windows twin of scripts/linux/smoke.sh. It opens a game from the
command line and checks its frames show, opens the library without one,
checks the app lives without a sound device, and closes the window as its
close button would (WM_CLOSE), checking the app then quits. Screenshots,
logs, the layout probe and the timing trace of each run go in the output
folder, with a summary.md.

Each check is named run.check, such as game.quit. Those listed in the
SMOKE_KNOWN_FAILURES environment variable (space-separated) are reported
but don't fail the run, so CI can gate on the rest; one that passes says
so, to be taken off the list.

The app runs in a home of its own (DS_EMULATOR_HOME) on a copy of a test
ROM, since saves are written beside the ROM. It needs
scripts/fetch-test-roms.sh first.

.EXAMPLE
pwsh scripts/windows/smoke.ps1 -Exe target/debug/ds-emulator.exe -Out target/smoke
#>
param(
    [string]$Exe = '',
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\debug\ds-emulator.exe' }
if (-not $Out) { $Out = Join-Path $root 'target\smoke' }
$rom = Join-Path $root 'test-roms\armwrestler-ds\armwrestler.nds'
if (-not (Test-Path $Exe)) { throw "no app at $Exe; build it with cargo build -p emu-app" }
if (-not (Test-Path $rom)) { throw "no test ROM at $rom; run scripts/fetch-test-roms.sh" }
$Exe = (Resolve-Path $Exe).Path
$known = " $($env:SMOKE_KNOWN_FAILURES) "

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class SmokeWin32 {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }

    public delegate bool EnumProc(IntPtr window, IntPtr data);

    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc callback, IntPtr data);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);

    // The largest visible top-level window of process `pid`, or zero.
    public static IntPtr Window(uint pid) {
        IntPtr best = IntPtr.Zero;
        long bestArea = 0;
        EnumWindows((window, data) => {
            uint owner;
            Rect rect;
            GetWindowThreadProcessId(window, out owner);
            if (owner == pid && IsWindowVisible(window) && GetWindowRect(window, out rect)) {
                long area = (long)(rect.Right - rect.Left) * (rect.Bottom - rect.Top);
                if (area > bestArea) { best = window; bestArea = area; }
            }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    // How many colours 32-bit pixels have, in the middle `fraction` of the
    // image's width and height.
    public static int Colours(byte[] pixels, int width, int height, int stride, double fraction) {
        int w = (int)(width * fraction), h = (int)(height * fraction);
        int x0 = (width - w) / 2, y0 = (height - h) / 2;
        var seen = new HashSet<int>();
        for (int y = y0; y < y0 + h; y++)
            for (int x = x0; x < x0 + w; x++)
                seen.Add(BitConverter.ToInt32(pixels, y * stride + x * 4) & 0xFFFFFF);
        return seen.Count;
    }
}
'@
# Window rectangles in real pixels, whatever the display's scale.
[void][SmokeWin32]::SetProcessDPIAware()

# Only what an earlier run left is cleared from the output folder.
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path $Out).Path
Get-ChildItem $Out -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^(game|library|results\.txt|summary\.md)' } |
    Remove-Item -Recurse -Force
$work = Join-Path ([IO.Path]::GetTempPath()) "ds-emulator-smoke-$PID"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
$results = [Collections.Generic.List[object]]::new()
$script:failed = $false

function Check([string]$Name, [bool]$Passed, [string]$Detail) {
    $status = if ($Passed) { 'pass' } else { 'fail' }
    if ($known.Contains(" $Name ")) {
        $status = if ($Passed) { 'pass, but listed in SMOKE_KNOWN_FAILURES: take it off' } else { 'known failure' }
    } elseif (-not $Passed) {
        $script:failed = $true
    }
    $results.Add([pscustomobject]@{ Check = $Name; Result = $status; Detail = $Detail })
    Write-Host ('{0,-16} {1}: {2}' -f $Name, $status, $Detail)
}

# Saves window `$Window` as a PNG at `$Path`, drawn by the window itself
# (PrintWindow with PW_RENDERFULLCONTENT, which DirectComposition needs),
# or copied from the screen with -Screen. Returns $false if it can't.
function Save-Window([IntPtr]$Window, [string]$Path, [switch]$Screen) {
    $rect = New-Object SmokeWin32+Rect
    if (-not [SmokeWin32]::GetWindowRect($Window, [ref]$rect)) { return $false }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) { return $false }
    $bitmap = New-Object Drawing.Bitmap $width, $height, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        if ($Screen) {
            $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        } else {
            $dc = $graphics.GetHdc()
            try { [void][SmokeWin32]::PrintWindow($Window, $dc, 2) } finally { $graphics.ReleaseHdc($dc) }
        }
        $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
        return $true
    } catch {
        return $false
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# How many colours the PNG at `$Path` has, in its middle `$Fraction`.
function Get-Colours([string]$Path, [double]$Fraction = 1.0) {
    if (-not (Test-Path $Path)) { return 0 }
    $bitmap = New-Object Drawing.Bitmap $Path
    try {
        $box = New-Object Drawing.Rectangle 0, 0, $bitmap.Width, $bitmap.Height
        $data = $bitmap.LockBits($box, [Drawing.Imaging.ImageLockMode]::ReadOnly, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $bytes = New-Object byte[] ($data.Stride * $data.Height)
            [Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
            return [SmokeWin32]::Colours($bytes, $data.Width, $data.Height, $data.Stride, $Fraction)
        } finally {
            $bitmap.UnlockBits($data)
        }
    } finally {
        $bitmap.Dispose()
    }
}

# Saves window `$Window` at `$Path` as the window draws itself, or as the
# screen shows it should that come out blank. Returns how, or '' if neither
# worked.
function Save-Picture([IntPtr]$Window, [string]$Path) {
    if ((Save-Window $Window $Path) -and (Get-Colours $Path) -gt 1) { return 'PrintWindow' }
    if (Save-Window $Window $Path -Screen) { return 'screen copy' }
    return ''
}

function Get-Drawn([string]$Trace) {
    if (-not (Test-Path $Trace)) { return 0 }
    return @(Select-String -Path $Trace -SimpleMatch ' drawn ').Count
}

function Get-Run([string]$Trace) {
    if (-not (Test-Path $Trace)) { return 0 }
    return @(Select-String -Path $Trace -SimpleMatch ' run ').Count
}

# A home with libretro's box-art listings cached and naming nothing, so no
# title finds a picture and nothing is downloaded.
$home_ = Join-Path $work 'home'
$listings = Join-Path $home_ 'AppData\Local\ds-emulator\listings'
New-Item -ItemType Directory -Force -Path $listings, (Join-Path $work 'roms') | Out-Null
foreach ($system in 'Game Boy', 'Game Boy Color', 'Game Boy Advance', 'Nintendo DS') {
    foreach ($folder in 'Named_Boxarts', 'Named_Titles') {
        Set-Content -Path (Join-Path $listings "Nintendo - $system - $folder.txt") -Value 'none.png'
    }
}
$romCopy = Join-Path $work "roms\$(Split-Path $rom -Leaf)"
Copy-Item $rom $romCopy

# Starts the app, waits for its window and for it to draw, saves pictures
# of it, then closes the window and checks the app quits.
function Invoke-Run([string]$Name, [string[]]$Arguments) {
    $trace = Join-Path $Out "$Name-trace.txt"
    $probe = Join-Path $Out "$Name-probe"
    $shot = Join-Path $Out "$Name.png"
    New-Item -ItemType Directory -Force -Path $probe | Out-Null
    $env:DS_EMULATOR_HOME = $home_
    $env:DS_EMULATOR_PROBE = $probe
    $env:DS_EMULATOR_TRACE = $trace
    $env:RUST_BACKTRACE = '1'
    $start = @{
        FilePath = $Exe
        PassThru = $true
        RedirectStandardOutput = (Join-Path $Out "$Name.out.log")
        RedirectStandardError = (Join-Path $Out "$Name.log")
    }
    if ($Arguments) { $start.ArgumentList = $Arguments | ForEach-Object { '"' + $_ + '"' } }
    $process = Start-Process @start
    # Keeps the handle, so the exit code can still be read once it ends.
    $null = $process.Handle

    $window = [IntPtr]::Zero
    for ($i = 0; $i -lt 120 -and -not $process.HasExited; $i++) {
        $window = [SmokeWin32]::Window([uint32]$process.Id)
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 500
    }
    if ($window -eq [IntPtr]::Zero) {
        Check "$Name.window" $false "no window within 60 s; see $Name.log"
        if (-not $process.HasExited) { $process.Kill() }
        return
    }
    Check "$Name.window" $true "window 0x$('{0:x}' -f $window.ToInt64())"
    [void][SmokeWin32]::SetForegroundWindow($window)

    # Drawn once the window shows more than one colour, and for a game,
    # once it has shown its first 60 frames.
    $drawn = $false
    for ($i = 0; $i -lt 120 -and -not $process.HasExited; $i++) {
        Start-Sleep -Seconds 1
        if (-not (Save-Picture $window $shot)) { continue }
        if ((Get-Colours $shot) -le 1) { continue }
        if ($Arguments -and (Get-Drawn $trace) -lt 60) { continue }
        $drawn = $true
        break
    }
    $how = Save-Picture $window $shot
    [void](Save-Window $window (Join-Path $Out "$Name-screen.png") -Screen)

    Check "$Name.alive" (-not $process.HasExited) $(if ($process.HasExited) { "stopped before drawing; see $Name.log" } else { 'running after its first frames' })
    $shown = Get-Colours $shot
    if ($Arguments) {
        # The middle of the window is the game's picture.
        $picture = Get-Colours $shot 0.5
        $frames = "$(Get-Drawn $trace) frames drawn, $(Get-Run $trace) run"
        Check "$Name.frame" ($drawn -and $picture -gt 3) "$picture colours in the picture ($how); $frames"
    } else {
        Check "$Name.drawn" ($drawn -and $shown -gt 16) "$shown colours ($how)"
    }
    if ($process.HasExited) { return }

    # WM_CLOSE, as the window's close button sends.
    [void][SmokeWin32]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    if (-not $process.WaitForExit(30000)) {
        [void](Save-Window $window (Join-Path $Out "$Name-after-close.png") -Screen)
        $process.Kill()
        Check "$Name.quit" $false 'still running 30 s after its window was closed'
        return
    }
    $code = $process.ExitCode
    $log = Join-Path $Out "$Name.log"
    $panic = Select-String -Path $log -Pattern 'panicked at' -Context 0, 1 -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($code -eq 0) {
        Check "$Name.quit" $true 'exit status 0'
    } elseif ($panic) {
        $where = $panic.Line -replace '^.*panicked at (.*[\\/])?([^\\/]*[\\/]src[\\/])', '$2'
        Check "$Name.quit" $false "exit status $code, panicked at $where $($panic.Context.PostContext -join ' ')"
    } else {
        Check "$Name.quit" $false ('exit status {0} (0x{0:x8}); see {1}.log' -f $code, $Name)
    }
}

try {
    # The game first, so the library then lists it.
    Invoke-Run 'game' @($romCopy)
    Invoke-Run 'library' @()
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$results | ForEach-Object { "$($_.Check)`t$($_.Result)`t$($_.Detail)" } | Set-Content (Join-Path $Out 'results.txt')
$summary = @(
    "## Windows smoke ($env:PROCESSOR_ARCHITECTURE)"
    ''
    '| Check | Result | Detail |'
    '|---|---|---|'
    ($results | ForEach-Object { "| $($_.Check) | $($_.Result) | $($_.Detail) |" })
    ''
)
$summary | Set-Content (Join-Path $Out 'summary.md')
if ($env:GITHUB_STEP_SUMMARY) { $summary | Add-Content $env:GITHUB_STEP_SUMMARY }
if ($script:failed) { exit 1 }
exit 0
