param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $root 'artifacts/hang-fix' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force $output | Out-Null
$mt = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin/*/x64/mt.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $mt) { throw 'Windows SDK mt.exe is required for the WebView2 test manifest.' }
Push-Location (Join-Path $root 'src-tauri')
try {
    $lines = & cargo test --lib --offline --features native-smoke --no-run --message-format=json
    if ($LASTEXITCODE -ne 0) { throw "Native test build failed: $LASTEXITCODE" }
    $exe = $lines | ForEach-Object {
        $entry = $_ | ConvertFrom-Json
        if ($entry.reason -eq 'compiler-artifact' -and $entry.profile.test -and $entry.executable) {
            $entry.executable
        }
    } | Select-Object -Last 1
    if (-not $exe) { throw 'No native test executable was produced.' }
    $copy = Join-Path $output 'native-smoke.exe'
    Copy-Item -LiteralPath $exe -Destination $copy -Force
    & $mt.FullName -nologo -manifest (Join-Path $PSScriptRoot 'native-smoke.manifest') "-outputresource:$copy;#1"
    if ($LASTEXITCODE -ne 0) { throw "Embedding test manifest failed: $LASTEXITCODE" }
    $stdout = Join-Path $output 'native-smoke.stdout.txt'
    $stderr = Join-Path $output 'native-smoke.stderr.txt'
    $process = Start-Process -FilePath $copy -WindowStyle Hidden -PassThru `
        -ArgumentList 'tray::tests::native_window_smoke --ignored --exact --nocapture --test-threads=1' `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $process.WaitForExit(45000)) {
        Stop-Process -Id $process.Id
        throw 'Native window test exceeded 45 seconds.'
    }
    $process.Refresh()
    Get-Content -LiteralPath $stdout
    Get-Content -LiteralPath $stderr
    "EXIT_CODE=$($process.ExitCode)"
    if ($process.ExitCode -ne 0) { throw "Native test failed: $($process.ExitCode)" }
} finally {
    Pop-Location
}
