# Real `fux` in real Windows Terminal, on a runner's desktop: CI's nearest
# to a person (WINDOWS.md, PR 8). It starts the portable Windows Terminal,
# runs fux in it from cmd, types through the keyboard, and takes screenshots
# as it goes. fux's first run here fetches the ConPTY itself.
#
#   windows-terminal.ps1 -Fux target\debug\fux.exe -Terminal ...\WindowsTerminal.exe -Out shots
param(
    [Parameter(Mandatory)][string]$Fux,
    [Parameter(Mandatory)][string]$Terminal,
    [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$Fux = (Resolve-Path $Fux).Path
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -Namespace Fux -Name Desktop -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
[DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte sc, uint f, UIntPtr x);
'@

function Shot($name) {
    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save("$Out\$name.png")
    "shot $name"
}

function Waited($what, $test, $seconds = 40) {
    $until = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $until) {
        if (& $test) { return }
        Start-Sleep -Milliseconds 250
    }
    Shot "failed"
    throw "no $what in $seconds s"
}

# What holds the foreground on a first logon: the arm image's privacy
# screen, and its WSL prompt, which also opens later (see `PressTill`).
Get-Process WWAHost, wsl, wslhost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep 2
# And the Start menu, which ending it leaves open.
[Fux.Desktop]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero)
[Fux.Desktop]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero)
Start-Sleep 1

$dir = Join-Path $Out 'home'
New-Item -ItemType Directory -Force $dir | Out-Null
$marks = Join-Path $Out 'marks'
New-Item -ItemType Directory -Force $marks | Out-Null
# Windows Terminal rebuilds a tab's environment, so a script sets fux's.
$launch = Join-Path $Out 'launch.cmd'
@"
@echo off
set LOCALAPPDATA=$dir
"$Fux"
echo after-fux> "$marks\after.txt"
"@ | Out-File -Encoding ascii $launch

Start-Process $Terminal -ArgumentList '--title', 'fuxwt', '--suppressApplicationTitle', 'cmd.exe', '/k', $launch | Out-Null
$window = $null
Waited 'a window' {
    $script:window = Get-Process WindowsTerminal -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowTitle -like '*fuxwt*' } | Select-Object -First 1
    [bool]$script:window
}
$handle = $window.MainWindowHandle
"window $handle"

function Front {
    foreach ($try in 1..20) {
        # A tap of Alt lets this process give the foreground away.
        [Fux.Desktop]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [Fux.Desktop]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][Fux.Desktop]::ShowWindow($handle, 9)
        [void][Fux.Desktop]::SetForegroundWindow($handle)
        Start-Sleep -Milliseconds 300
        if ([Fux.Desktop]::GetForegroundWindow() -eq $handle) { return }
    }
    Shot 'no-foreground'
    throw 'the window would not come to the front'
}

# Only ever into this window.
function Press($keys) {
    Front
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}

# Types `keys` till `test` holds: a window of the image's own can open and
# take the keyboard between the two.
function PressTill($keys, $what, $test) {
    foreach ($try in 1..4) {
        Get-Process wsl, wslhost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Press $keys
        $until = (Get-Date).AddSeconds(8)
        while ((Get-Date) -lt $until) {
            if (& $test) { return }
            Start-Sleep -Milliseconds 250
        }
        "typing again: no $what"
    }
    Shot 'failed'
    throw "no $what"
}

# fux up: a first run fetches the ConPTY, then a pane's cmd prompts.
Waited 'the ConPTY fetched' { Get-ChildItem $dir -Recurse -Filter OpenConsole.exe -ErrorAction SilentlyContinue } 90
Start-Sleep 5
Shot 'attached'
PressTill "echo typed-in-a-pane> `"$marks\typed.txt`"{ENTER}" 'the typed command run' { Test-Path "$marks\typed.txt" }
Get-Content "$marks\typed.txt"
Shot 'typed'

[void][Fux.Desktop]::MoveWindow($handle, 40, 40, 700, 420, $true)
Start-Sleep 2
PressTill "mode con> `"$marks\mode.txt`"{ENTER}" 'the size in the pane' { (Test-Path "$marks\mode.txt") -and ((Get-Content "$marks\mode.txt" -Raw) -match 'Columns') }
Get-Content "$marks\mode.txt" | Select-String 'Lines|Columns'
Shot 'resized'

Press '^bd'
Waited 'the detach' { Test-Path "$marks\after.txt" }
Start-Sleep 1
Shot 'detached'
PressTill "echo prompt-back> `"$marks\prompt.txt`"{ENTER}" 'the prompt back' { Test-Path "$marks\prompt.txt" }
Get-Content "$marks\prompt.txt"
Shot 'prompt'
Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Where-Object { $_.Id -eq $window.Id } | Stop-Process -Force
$env:LOCALAPPDATA = $dir
& $Fux kill-server
"real Windows Terminal: typed, resized, detached, prompt back"
