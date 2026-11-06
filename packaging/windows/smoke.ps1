<#
Installs the Windows packages as a user would and checks them: the
installer with /S, the installed app started on a game until it has drawn
its first frames, the portable zip, and the uninstaller. It also checks
what the GUI subsystem changes: that the exe is marked a GUI program, so
Windows opens no console window for it, and that the standard handles it
is started with still work, which the 3DS helper (the app's own exe, run
over pipes) depends on.

  pwsh packaging/windows/smoke.ps1 -Setup Imago-Emulator-*-windows-x86_64.exe `
      -Zip Imago-Emulator-*-windows-x86_64-portable.zip `
      -Rom test-roms/armwrestler-ds/armwrestler.nds [-Out target/smoke-package]

It installs into this user's %LOCALAPPDATA%\Programs and Start menu, and
uninstalls again, so run it on a test machine or CI runner. The app plays
in a home of its own (DS_EMULATOR_HOME) on a copy of the ROM. Closing the
window is reported but doesn't fail the run.
#>
param(
    [Parameter(Mandatory)] [string]$Setup,
    [Parameter(Mandatory)] [string]$Zip,
    [Parameter(Mandatory)] [string]$Rom,
    [string]$Out = 'target/smoke-package'
)
$ErrorActionPreference = 'Stop'
$Setup = (Resolve-Path $Setup).Path
$Zip = (Resolve-Path $Zip).Path
$Rom = (Resolve-Path $Rom).Path
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path $Out).Path

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class PackageWin32 {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr window, IntPtr data);

    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc callback, IntPtr data);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll")] public static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll")] public static extern bool FreeLibrary(IntPtr module);

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

    // Whether the file has resource 1 of type `type` (3 icon, 14 icon
    // group, 16 version, 24 manifest), read as data, not run.
    public static bool HasResource(string path, int type) {
        IntPtr module = LoadLibraryEx(path, IntPtr.Zero, 0x2 /* LOAD_LIBRARY_AS_DATAFILE */);
        if (module == IntPtr.Zero) return false;
        try { return FindResource(module, (IntPtr)1, (IntPtr)type) != IntPtr.Zero; }
        finally { FreeLibrary(module); }
    }
}
'@
[void][PackageWin32]::SetProcessDPIAware()

$results = [Collections.Generic.List[object]]::new()
$script:failed = $false
function Check([string]$Name, [object]$Passed, [string]$Detail) {
    # $null means a note: reported, never a failure.
    $status = if ($null -eq $Passed) { 'note' } elseif ($Passed) { 'pass' } else { 'fail' }
    if ($status -eq 'fail') { $script:failed = $true }
    $results.Add([pscustomobject]@{ Check = $Name; Result = $status; Detail = $Detail })
    Write-Host ('{0,-12} {1}: {2}' -f $Name, $status, $Detail)
}

# Runs the exe with all three standard handles redirected, as the app runs
# its 3DS helper, and returns its exit code and what it wrote.
function Invoke-Piped([string]$Exe, [string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    $process.StandardInput.Close()
    $stdout = [IO.MemoryStream]::new()
    $copy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(60000)) {
        $process.Kill()
        return [pscustomobject]@{ Code = 'timeout'; Out = [byte[]]@(); Err = '' }
    }
    $copy.Wait()
    return [pscustomobject]@{ Code = $process.ExitCode; Out = $stdout.ToArray(); Err = $stderr.Result }
}

$name = 'Imago Emulator'
$folder = Join-Path $env:LOCALAPPDATA "Programs\$name"
$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$name.lnk"
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\com.lilfourn.ds-emulator'
$work = Join-Path ([IO.Path]::GetTempPath()) "imago-smoke-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null

try {
    # The installer.
    $info = (Get-Item $Setup).VersionInfo
    Check 'setup' ($info.ProductName -eq $name) "$($info.ProductName) $($info.ProductVersion), $([math]::Round((Get-Item $Setup).Length / 1MB, 1)) MB"
    $install = Start-Process -FilePath $Setup -ArgumentList '/S' -Wait -PassThru
    $exe = Join-Path $folder 'ds-emulator.exe'
    $files = 'ds-emulator.exe', 'License.txt', 'Acknowledgements.html', 'Source\seed_tables.rs', 'Uninstall.exe'
    $missing = $files | Where-Object { -not (Test-Path (Join-Path $folder $_)) }
    Check 'install' ($install.ExitCode -eq 0 -and -not $missing) `
        "exit $($install.ExitCode) into $folder$(if ($missing) { "; missing $($missing -join ', ')" })"
    if (-not (Test-Path $exe)) { throw 'nothing was installed' }
    $target = if (Test-Path $shortcut) { (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath }
    Check 'shortcut' ($target -eq $exe) "Start menu: $target"
    $entry = Get-ItemProperty $key -ErrorAction SilentlyContinue
    Check 'entry' ($entry.DisplayName -eq $name -and (Test-Path ($entry.UninstallString -replace '"', ''))) `
        "Settings > Apps: $($entry.DisplayName) $($entry.DisplayVersion), $($entry.EstimatedSize) KB"

    # The exe: a GUI program, with its icon and version beside GPUI's
    # manifest.
    $bytes = [IO.File]::ReadAllBytes($exe)
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    $subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)
    Check 'subsystem' ($subsystem -eq 2) "$subsystem (2 is a GUI program, 3 a console one)"
    $version = (Get-Item $exe).VersionInfo
    $resources = @{ icon = [PackageWin32]::HasResource($exe, 14); version = [PackageWin32]::HasResource($exe, 16); manifest = [PackageWin32]::HasResource($exe, 24) }
    Check 'resources' ($resources.icon -and $resources.version -and $resources.manifest -and $version.ProductName -eq $name) `
        "icon $($resources.icon), version $($resources.version) ($($version.ProductName) $($version.FileVersion)), manifest $($resources.manifest)"

    # Standard handles under the GUI subsystem: the helper's usage goes to
    # its piped stderr with its exit code, and the command line's error.
    $helper = Invoke-Piped $exe @('--3ds-helper')
    Check 'pipes' ($helper.Code -eq 2 -and $helper.Err -match 'usage: --3ds-helper') `
        "--3ds-helper alone: exit $($helper.Code), stderr '$($helper.Err.Trim())'"
    $usage = Invoke-Piped $exe @('--script')
    Check 'usage' ($usage.Code -eq 2 -and $usage.Err -match '--script needs a path') `
        "--script alone: exit $($usage.Code), stderr '$(($usage.Err -split "`n")[0].Trim())'"
    # The helper's link is its stdout: with no core to load, a helper that
    # can run says so there (message 6, FAILED). Builds without 3DS games
    # on Windows refuse on stderr instead.
    $data = Join-Path $work '3DS'
    New-Item -ItemType Directory -Force -Path $data | Out-Null
    $link = Invoke-Piped $exe @('--3ds-helper', (Join-Path $work 'no-core.dll'), '', (Join-Path $work 'no-game.3dsx'), $data)
    if ($link.Err -match "aren't supported on Windows") {
        Check 'link' $null "this build has no 3DS games on Windows: '$($link.Err.Trim())'"
    } else {
        Check 'link' ($link.Code -eq 0 -and $link.Out.Length -gt 5 -and $link.Out[0] -eq 6) `
            "exit $($link.Code), $($link.Out.Length) bytes on stdout, first $(if ($link.Out.Length) { $link.Out[0] } else { 'none' })"
    }

    # The installed app plays a game.
    $home_ = Join-Path $work 'home'
    $listings = Join-Path $home_ 'AppData\Local\ds-emulator\listings'
    New-Item -ItemType Directory -Force -Path $listings, (Join-Path $work 'roms') | Out-Null
    foreach ($system in 'Game Boy', 'Game Boy Color', 'Game Boy Advance', 'Nintendo DS') {
        foreach ($kind in 'Named_Boxarts', 'Named_Titles') {
            Set-Content -Path (Join-Path $listings "Nintendo - $system - $kind.txt") -Value 'none.png'
        }
    }
    $game = Join-Path $work "roms\$(Split-Path -Leaf $Rom)"
    Copy-Item $Rom $game
    $trace = Join-Path $Out 'trace.txt'
    Remove-Item -Force $trace -ErrorAction SilentlyContinue
    $env:DS_EMULATOR_HOME = $home_
    $env:DS_EMULATOR_TRACE = $trace
    $env:RUST_BACKTRACE = '1'
    $app = Start-Process -FilePath $exe -ArgumentList "`"$game`"" -PassThru `
        -RedirectStandardOutput (Join-Path $Out 'app.out.log') -RedirectStandardError (Join-Path $Out 'app.log')
    Remove-Item Env:DS_EMULATOR_HOME, Env:DS_EMULATOR_TRACE, Env:RUST_BACKTRACE

    $window = [IntPtr]::Zero
    for ($i = 0; $i -lt 120 -and -not $app.HasExited; $i++) {
        $window = [PackageWin32]::Window([uint32]$app.Id)
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 500
    }
    Check 'window' ($window -ne [IntPtr]::Zero) "window $window"
    $frames = 0
    for ($i = 0; $i -lt 120 -and -not $app.HasExited; $i++) {
        if (Test-Path $trace) { $frames = @(Select-String -Path $trace -SimpleMatch ' drawn ').Count }
        if ($frames -ge 60) { break }
        Start-Sleep -Seconds 1
    }
    Check 'alive' (-not $app.HasExited) "$(if ($app.HasExited) { "exited with $($app.ExitCode); see app.log" } else { 'running after its first frames' })"
    Check 'frames' ($frames -ge 60) "$frames frames drawn"
    if ($window -ne [IntPtr]::Zero) {
        $rect = New-Object PackageWin32+Rect
        [void][PackageWin32]::GetWindowRect($window, [ref]$rect)
        $width = $rect.Right - $rect.Left; $height = $rect.Bottom - $rect.Top
        if ($width -gt 0 -and $height -gt 0) {
            $bitmap = New-Object Drawing.Bitmap $width, $height
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            $dc = $graphics.GetHdc()
            # PW_RENDERFULLCONTENT, which DirectComposition windows need.
            [void][PackageWin32]::PrintWindow($window, $dc, 2)
            $graphics.ReleaseHdc($dc)
            $bitmap.Save((Join-Path $Out 'game.png'), [Drawing.Imaging.ImageFormat]::Png)
            $graphics.Dispose(); $bitmap.Dispose()
        }
        # WM_CLOSE, as the close button sends.
        [void][PackageWin32]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    }
    if ($app.WaitForExit(30000)) {
        Check 'close' $(if ($app.ExitCode -eq 0) { $true } else { $null }) "exit $($app.ExitCode)"
    } else {
        $app.Kill()
        Check 'close' $null 'still running 30 s after WM_CLOSE'
    }

    # The portable zip holds the same files.
    $unzipped = Join-Path $work 'zip'
    Expand-Archive -Path $Zip -DestinationPath $unzipped
    $portable = Join-Path $unzipped "$name\ds-emulator.exe"
    $same = (Test-Path $portable) -and (Get-FileHash $portable).Hash -eq (Get-FileHash $exe).Hash
    $missing = 'License.txt', 'Acknowledgements.html', 'Source\seed_tables.rs' |
        Where-Object { -not (Test-Path (Join-Path $unzipped "$name\$_")) }
    Check 'zip' ($same -and -not $missing) "the installed exe: $same$(if ($missing) { "; missing $($missing -join ', ')" })"

    # The uninstaller copies itself away to delete the folder, and returns
    # at once.
    $uninstall = Start-Process -FilePath (Join-Path $folder 'Uninstall.exe') -ArgumentList '/S' -Wait -PassThru
    for ($i = 0; $i -lt 60 -and (Test-Path $folder); $i++) { Start-Sleep -Seconds 1 }
    $left = @((Test-Path $folder), (Test-Path $shortcut), (Test-Path $key))
    Check 'uninstall' ($uninstall.ExitCode -eq 0 -and -not ($left -contains $true)) `
        "exit $($uninstall.ExitCode); folder, shortcut, entry left: $($left -join ', ')"
} catch {
    Check 'error' $false "$_"
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    $summary = @('### Windows packages', '', '| Check | Result | Detail |', '|---|---|---|')
    $summary += $results | ForEach-Object { "| $($_.Check) | $($_.Result) | $($_.Detail -replace '\|', '/') |" }
    Set-Content -Path (Join-Path $Out 'summary.md') -Value $summary
    $results | ForEach-Object { "$($_.Check)`t$($_.Result)`t$($_.Detail)" } | Set-Content (Join-Path $Out 'results.txt')
    if ($env:GITHUB_STEP_SUMMARY) { Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value $summary }
}
if ($script:failed) { exit 1 }
