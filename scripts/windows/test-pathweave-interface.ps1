param(
    [Parameter(Mandatory)][ValidateRange(1, 4294967295)][uint32]$InterfaceIndex,
    [Parameter(Mandatory)][string]$ExpectedGuid,
    [Parameter(Mandatory)][string]$DestinationIPv4,
    [Parameter(Mandatory)][string]$HostName,
    [ValidateRange(1024, 16384)][int]$MaxResponseBytes = 16384
)

$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run elevated to create the temporary TUN route and start Packet Monitor.' }

$remoteIp = [Net.IPAddress]::Parse($DestinationIPv4)
if ($remoteIp.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { throw 'This validation script requires an IPv4 destination.' }
$adapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex -ErrorAction Stop
if ($adapter.Status -ne 'Up' -or $adapter.InterfaceDescription -notmatch 'Remote NDIS') { throw "Interface $InterfaceIndex is not an active Remote NDIS tether adapter." }

$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$engine = Join-Path $workspace 'target\release\pathweave-engine.exe'
if (-not (Test-Path -LiteralPath $engine)) { throw 'Build the release engine before running this test.' }
$telemetryJson = (& $engine --telemetry-once | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Pathweave could not read the Windows interface table.' }
$telemetry = ConvertFrom-Json -InputObject $telemetryJson
$identity = $telemetry | Where-Object { $_.index -eq $InterfaceIndex } | Select-Object -First 1
if (-not $identity -or $identity.stable_guid -ne $ExpectedGuid -or -not $identity.operational) { throw 'The selected index no longer resolves to the expected operational GUID.' }

if (Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue) { throw 'A Pathweave adapter already exists; refusing to reuse it.' }
$prefix = "$DestinationIPv4/32"
if (@(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue).Count -gt 0) { throw "A route for $prefix already exists; refusing to replace it." }
$pktmonStatus = (& pktmon status 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0 -or $pktmonStatus -match '(?i)(status|state)\s*:\s*running') { throw 'Packet Monitor is active or its state cannot be verified.' }

$target = Join-Path $workspace 'target\live-validation'
New-Item -ItemType Directory -Path $target -Force | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$etl = Join-Path $target "pathweave-if$InterfaceIndex-$stamp.etl"
$packetText = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-packets.txt"
$componentMap = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-components.txt"
$engineOut = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-engine.log"
$engineErr = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-engine-error.log"
$resultPath = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-result.json"
$routeAdded = $false
$captureStarted = $false
$engineProcess = $null
$routesBefore = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
$virtualBefore = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name)|$($_.ifIndex)|$($_.Status)" } | Sort-Object)
$curlResult = $null
$componentId = $null

try {
    $engineProcess = Start-Process -FilePath $engine -WorkingDirectory (Split-Path $engine -Parent) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $engineOut -RedirectStandardError $engineErr -ArgumentList @('--interface', "$InterfaceIndex")
    $deadline = (Get-Date).AddSeconds(20)
    $tun = $null
    while ((Get-Date) -lt $deadline -and -not $tun) {
        if ($engineProcess.HasExited) { throw "Pathweave exited before TUN startup; inspect $engineErr" }
        $tun = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
        if (-not $tun) { Start-Sleep -Milliseconds 250 }
    }
    if (-not $tun) { throw 'Pathweave TUN did not appear within 20 seconds.' }

    $componentRows = @(& pktmon list 2>&1)
    if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate Packet Monitor components.' }
    $componentRows | Out-File -LiteralPath $componentMap -Encoding utf8
    $adapterComponent = @($componentRows | Where-Object { $_.ToString() -match 'Remote NDIS based Internet Sharing Device' })
    if ($adapterComponent.Count -ne 1 -or $adapterComponent[0].ToString() -notmatch '^\s*(\d+)\s+') { throw 'Could not uniquely resolve the tether NIC Packet Monitor component.' }
    $componentId = [int]$Matches[1]

    New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -NextHop '0.0.0.0' -PolicyStore ActiveStore | Out-Null
    $routeAdded = $true
    $null = & pktmon start --capture --comp $componentId --pkt-size 128 --file-name $etl --file-size 16 --log-mode circular 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Packet Monitor start failed ($LASTEXITCODE)." }
    $captureStarted = $true

    $curlOutput = & curl.exe --noproxy '*' --silent --show-error --max-time 12 --connect-timeout 5 `
        --max-filesize $MaxResponseBytes --limit-rate 8192 --range "0-$([Math]::Min(8191,$MaxResponseBytes - 1))" `
        --resolve "$HostName`:443:$DestinationIPv4" --output NUL --write-out '%{http_code}|%{size_download}|%{remote_ip}|%{time_total}' `
        "https://$HostName/" 2>&1
    $curlExit = $LASTEXITCODE
    $curlResult = ($curlOutput | Out-String).Trim()
    if ($curlExit -ne 0) { throw "Bounded HTTPS request failed with curl exit ${curlExit}: $curlResult" }
    $parts = $curlResult.Split('|')
    if ($parts.Count -lt 4) { throw "Could not parse bounded HTTPS result: $curlResult" }
    $httpCode = [int]$parts[0]
    $downloaded = [int64]([double]$parts[1])
    if ($httpCode -lt 200 -or $httpCode -ge 400 -or $downloaded -gt $MaxResponseBytes) { throw "Unexpected HTTP result or response exceeded cap: $curlResult" }
}
finally {
    try {
        if ($captureStarted) { $null = & pktmon stop 2>&1 }
    }
    finally {
        try {
            if ($routeAdded -and $tun) { Remove-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -PolicyStore ActiveStore -Confirm:$false }
        }
        finally {
            if ($engineProcess -and -not $engineProcess.HasExited) { Stop-Process -Id $engineProcess.Id -Force; $engineProcess.WaitForExit() }
            if (Test-Path -LiteralPath $etl) { $null = & pktmon etl2txt $etl --out $packetText --brief 2>&1 }
        }
    }
}

$routeRemaining = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
$tunRemaining = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
$routesAfter = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
$virtualAfter = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name)|$($_.ifIndex)|$($_.Status)" } | Sort-Object)
if ($routeRemaining.Count -ne 0) { throw "Cleanup failed: $prefix remains." }
if ($tunRemaining) { throw 'Cleanup failed: Pathweave TUN remains.' }
if (Compare-Object $routesBefore $routesAfter) { throw 'Default routes changed during the test.' }
if (Compare-Object $virtualBefore $virtualAfter) { throw 'A pre-existing virtual adapter changed during the test.' }
if (-not $curlResult) { throw 'No HTTPS result was recorded.' }
$captureStats = @(& pktmon etl2txt $etl --stats 2>&1)
$captureStats | Out-File -LiteralPath (Join-Path $target "pathweave-if$InterfaceIndex-$stamp-packet-stats.txt") -Encoding utf8
$packetCountLine = $captureStats | Where-Object { $_.ToString() -match 'Packets total\s*:?\s*(\d+)' } | Select-Object -First 1
if (-not $packetCountLine -or $packetCountLine.ToString() -notmatch 'Packets total\s*:?\s*(\d+)') { throw 'Packet Monitor did not provide a packet count.' }
$capturedPackets = [int64]$Matches[1]
if ($capturedPackets -eq 0) { throw 'HTTPS succeeded, but Packet Monitor captured zero packets on the selected physical NIC; physical egress is not verified.' }

$report = [pscustomobject]@{
    interface_alias = $adapter.Name
    interface_description = $adapter.InterfaceDescription
    interface_index = $InterfaceIndex
    interface_guid = $ExpectedGuid
    source_ipv4 = ($identity.ipv4_addresses | Select-Object -First 1)
    destination = "$HostName ($DestinationIPv4):443"
    http_result = $curlResult
    response_byte_limit = $MaxResponseBytes
    packet_capture = $etl
    packet_text = $packetText
    captured_packets = $capturedPackets
    packet_monitor_component_id = $componentId
    packet_component_map = $componentMap
    route_cleanup = 'verified'
    tun_cleanup = 'verified'
    default_routes_unchanged = $true
    existing_virtual_adapters_unchanged = $true
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
$report | Format-List
Write-Output "RESULT=$resultPath"
