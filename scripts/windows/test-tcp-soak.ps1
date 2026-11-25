param(
    [Parameter(Mandatory)][string]$ServerAddress,
    [Parameter(Mandatory)][ValidateRange(1, 1099511627776)][long]$MaxBytes,
    [Parameter(Mandatory)][switch]$AllowExternal,
    [ValidateSet('download','upload','duplex','half-close')][string]$Mode = 'duplex',
    [long]$Bytes = 0,
    [ValidateRange(0, 3600)][int]$DurationSeconds = 0,
    [ValidateRange(1, 1000)][int]$Connections = 1,
    [ValidateRange(1, 1000)][int]$Concurrency = 1,
    [long]$RateBytesPerSec = 0,
    [string]$TokenEnvironmentVariable = 'PATHWEAVE_TEST_TOKEN'
)

$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this script elevated.' }
if (-not $AllowExternal) { throw 'WAN testing requires explicit -AllowExternal authorization.' }
if (($Bytes -gt 0) -eq ($DurationSeconds -gt 0)) { throw 'Specify exactly one of -Bytes or -DurationSeconds.' }
if ($Mode -eq 'half-close' -and $Bytes -le 0) { throw 'half-close requires -Bytes.' }
if ($Bytes -gt 0) {
    $directions = if ($Mode -in @('duplex','half-close')) { 2 } else { 1 }
    if (($Bytes * $Connections * $directions) -gt $MaxBytes) { throw 'Requested transfer exceeds -MaxBytes.' }
}
$token = [Environment]::GetEnvironmentVariable($TokenEnvironmentVariable)
if ([string]::IsNullOrWhiteSpace($token) -or $token.Length -lt 24) { throw "Set $TokenEnvironmentVariable to the remote server token before running." }

$serverParts = $ServerAddress.Split(':')
if ($serverParts.Count -ne 2) { throw 'ServerAddress must be an IPv4 address and port (address:port).' }
$serverIp = [Net.IPAddress]::Parse($serverParts[0])
if ($serverIp.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { throw 'This milestone live script supports IPv4 destinations only.' }
$serverPort = [int]$serverParts[1]
if ($serverPort -lt 1 -or $serverPort -gt 65535) { throw 'Server port is out of range.' }

$wifi = Get-NetAdapter -Name 'Wi-Fi' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Up' | Select-Object -First 1
$usb = Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.InterfaceDescription -match 'Remote NDIS' } | Select-Object -First 1
if (-not $wifi) { throw 'Active Wi-Fi adapter was not found.' }
if (-not $usb) { throw 'Active Android USB Remote NDIS adapter was not found.' }
if (Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue) { throw 'A Pathweave adapter already exists; refusing to replace it.' }
if (-not (Test-NetConnection -ComputerName $serverIp -Port $serverPort -InformationLevel Quiet)) { throw 'Remote test server is not reachable before route setup.' }
$pktmonStatus = & pktmon status 2>&1
if ($LASTEXITCODE -ne 0 -or (($pktmonStatus -join ' ') -match '(?i)(status|state)\s*:\s*running')) {
    throw 'Packet Monitor is already running or its state cannot be verified; refusing to start or stop another capture session.'
}

$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$engine = Join-Path $workspace 'target\release\pathweave-engine.exe'
$client = Join-Path $workspace 'target\release\pathweave-test.exe'
if (-not (Test-Path $engine) -or -not (Test-Path $client)) { throw 'Build release binaries first with cargo build --release --bins.' }
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$target = Join-Path $workspace 'target\m1a2'
New-Item -ItemType Directory -Path $target -Force | Out-Null
$prefix = "$serverIp/32"
$route = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
if ($route.Count -gt 0) { throw "Refusing conflicting destination route $prefix." }
$etl = Join-Path $target "tcp-soak-$stamp.etl"
$captureText = Join-Path $target "tcp-soak-$stamp.txt"
$clientJson = Join-Path $target "tcp-soak-$stamp.json"
$engineOut = Join-Path $target "engine-$stamp.log"
$engineErr = Join-Path $target "engine-$stamp-error.log"
$resultText = Join-Path $target "tcp-soak-$stamp.txt.log"
$profileCsv = Join-Path $target "engine-profile-$stamp.csv"
$addedRoute = $false
$captureStarted = $false
$engineProcess = $null
$profileJob = $null
$preexistingVirtual = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name):$($_.ifIndex):$($_.Status)" } | Sort-Object)
$defaultRoutesBefore = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex):$($_.NextHop):$($_.RouteMetric)" } | Sort-Object)

try {
    $args = @('--interface', "$($wifi.ifIndex),$($usb.ifIndex)", '--health-control-listen', '127.0.0.1:47832')
    $engineProcess = Start-Process -FilePath $engine -WorkingDirectory (Split-Path $engine -Parent) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $engineOut -RedirectStandardError $engineErr -ArgumentList $args
    $deadline = (Get-Date).AddSeconds(20)
    $tun = $null
    while ((Get-Date) -lt $deadline -and -not $tun) {
        if ($engineProcess.HasExited) { throw "Engine exited before TUN startup. See $engineErr" }
        $tun = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
        if (-not $tun) { Start-Sleep -Milliseconds 250 }
    }
    if (-not $tun) { throw 'Pathweave did not appear within 20 seconds.' }
    $profileJob = Start-Job -FilePath (Join-Path $PSScriptRoot 'profile-process.ps1') -ArgumentList $engineProcess.Id, $profileCsv
    New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -NextHop '0.0.0.0' -PolicyStore ActiveStore | Out-Null
    $addedRoute = $true
    $null = & pktmon start --capture --comp nics --pkt-size 128 --file-name $etl --file-size 128 --log-mode circular 2>&1
    if ($LASTEXITCODE -ne 0) { throw "pktmon start failed ($LASTEXITCODE)." }
    $captureStarted = $true

    $testArgs = @('--server', $ServerAddress, '--mode', $Mode, '--max-bytes', "$MaxBytes", '--connections', "$Connections", '--concurrency', "$Concurrency", '--allow-external', '--output', $clientJson)
    if ($Bytes -gt 0) { $testArgs += @('--bytes', "$Bytes") } else { $testArgs += @('--duration-secs', "$DurationSeconds") }
    if ($RateBytesPerSec -gt 0) { $testArgs += @('--rate-bytes-per-sec', "$RateBytesPerSec") }
    $clientOutput = & $client @testArgs 2>&1
    $clientExit = $LASTEXITCODE
    $clientOutput | Set-Content -LiteralPath $resultText -Encoding UTF8
    if ($clientExit -ne 0) { throw "Test client failed with exit code $clientExit. See $resultText" }
}
finally {
    try {
        if ($captureStarted) { $null = & pktmon stop 2>&1 }
    }
    finally {
        try {
            if ($addedRoute) { Remove-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -PolicyStore ActiveStore -Confirm:$false }
        }
        finally {
            if ($engineProcess -and -not $engineProcess.HasExited) { Stop-Process -Id $engineProcess.Id -Force; $engineProcess.WaitForExit() }
            if ($profileJob) {
                $null = Wait-Job -Job $profileJob -Timeout 8
                if ($profileJob.State -eq 'Running') { Stop-Job -Job $profileJob }
                Remove-Job -Job $profileJob -Force
            }
            if (Test-Path $etl) { $null = & pktmon etl2txt $etl --out $captureText --brief 2>&1 }
        }
    }
}

$routeRemaining = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
$tunRemaining = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
$virtualAfter = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name):$($_.ifIndex):$($_.Status)" } | Sort-Object)
$defaultRoutesAfter = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex):$($_.NextHop):$($_.RouteMetric)" } | Sort-Object)
if ($routeRemaining.Count -ne 0) { throw "Cleanup verification failed: $prefix remains." }
if ($tunRemaining) { throw 'Cleanup verification failed: Pathweave remains.' }
if (Compare-Object $preexistingVirtual $virtualAfter) { throw 'A pre-existing net-merge or utun8 adapter changed during the test.' }
if (Compare-Object $defaultRoutesBefore $defaultRoutesAfter) { throw 'Default route set changed during the test; inspect manually.' }
    "RESULT_JSON=$clientJson`nPACKET_CAPTURE=$etl`nPACKET_TEXT=$captureText`nPROCESS_PROFILE=$profileCsv`nROUTE_CLEANUP=verified`nTUN_CLEANUP=verified`nPREEXISTING_VIRTUAL_ADAPTERS=unchanged`nDEFAULT_ROUTES=unchanged" | Set-Content -LiteralPath (Join-Path $target "tcp-soak-$stamp-cleanup.txt")
Write-Output "PASS; results: $clientJson; packet capture: $etl"
