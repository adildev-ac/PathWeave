param(
    [Parameter(Mandatory)][ValidateRange(1, 4294967295)][uint32[]]$InterfaceIndex,
    [Parameter(Mandatory)][string[]]$ExpectedGuid,
    [Parameter(Mandatory)][string]$DestinationIPv4,
    [Parameter(Mandatory)][string]$HostName,
    [ValidateRange(2, 16)][int]$FlowCount = 4,
    [ValidateRange(1024, 16384)][int]$MaxResponseBytes = 16384
)

$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run elevated to create the temporary TUN route and start netsh trace.' }
if ($InterfaceIndex.Count -lt 2 -or $InterfaceIndex.Count -ne $ExpectedGuid.Count -or @($InterfaceIndex | Select-Object -Unique).Count -ne $InterfaceIndex.Count) {
    throw 'Provide at least two distinct interface indexes and one matching GUID for each.'
}

$remoteIp = [Net.IPAddress]::Parse($DestinationIPv4)
if ($remoteIp.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { throw 'This validation requires an IPv4 destination.' }
$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$engine = Join-Path $workspace 'target\release\pathweave-engine.exe'
if (-not (Test-Path -LiteralPath $engine)) { throw 'Build the release engine before running this test.' }
$target = Join-Path $workspace 'target\live-validation'
New-Item -ItemType Directory -Path $target -Force | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$base = Join-Path $target "pathweave-multiflow-$stamp"
$etl = "$base.etl"
$xmlPath = "$base-packets.xml"
$engineOut = "$base-engine.log"
$engineErr = "$base-engine-error.log"
$traceStartLog = "$base-trace-start.log"
$traceStopLog = "$base-trace-stop.log"
$traceConvertLog = "$base-trace-convert.log"
$routePrefix = "$DestinationIPv4/32"
$routeAdded = $false
$captureStarted = $false
$engineProcess = $null
$tun = $null
$curlProcesses = @()
$interfaces = @()
$curlResults = @()
$routesBefore = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)

for ($i = 0; $i -lt $InterfaceIndex.Count; $i++) {
    $adapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex[$i] -ErrorAction Stop
    if ($adapter.Status -ne 'Up' -or $adapter.InterfaceGuid.ToString().Trim('{}') -ine $ExpectedGuid[$i].Trim('{}')) {
        throw "Interface $($InterfaceIndex[$i]) is down or no longer maps to expected GUID $($ExpectedGuid[$i])."
    }
    $interfaces += [pscustomobject]@{
        index = $InterfaceIndex[$i]
        guid = $ExpectedGuid[$i]
        name = $adapter.Name
        description = $adapter.InterfaceDescription
        mac = $adapter.MacAddress
        ipv4 = @(Get-NetIPAddress -InterfaceIndex $InterfaceIndex[$i] -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty IPAddress)
    }
}
$telemetry = ConvertFrom-Json -InputObject (& $engine --telemetry-once | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Pathweave could not read the Windows interface table.' }
foreach ($iface in $interfaces) {
    $current = @($telemetry | Where-Object { $_.index -eq $iface.index -and $_.stable_guid -eq $iface.guid -and $_.operational })
    if ($current.Count -ne 1) { throw "Pathweave telemetry does not confirm interface $($iface.index) GUID $($iface.guid) as operational." }
}
if (Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue) { throw 'A Pathweave adapter already exists; refusing to reuse it.' }
if (@(Get-NetRoute -DestinationPrefix $routePrefix -ErrorAction SilentlyContinue).Count -gt 0) { throw "A route for $routePrefix already exists; refusing to replace it." }
$traceStatus = (& netsh trace show status 2>&1 | Out-String)
$traceStatusExit = $LASTEXITCODE
if (($traceStatusExit -ne 0 -and $traceStatus -notmatch '(?i)no trace session') -or $traceStatus -match '(?im)^\s*Status\s*:\s*Running' -or $traceStatus -notmatch '(?i)no trace session') {
    throw 'A netsh trace session is active or its state cannot be verified.'
}

try {
    $engineArgs = @()
    foreach ($index in $InterfaceIndex) { $engineArgs += @('--interface', "$index") }
    $previousRustLog = $env:RUST_LOG
    try {
        $env:RUST_LOG = 'info'
        $engineProcess = Start-Process -FilePath $engine -WorkingDirectory (Split-Path $engine -Parent) -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $engineOut -RedirectStandardError $engineErr -ArgumentList $engineArgs
    }
    finally { $env:RUST_LOG = $previousRustLog }

    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline -and -not $tun) {
        if ($engineProcess.HasExited) { throw "Pathweave exited before TUN startup; inspect $engineErr" }
        $tun = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
        if (-not $tun) { Start-Sleep -Milliseconds 250 }
    }
    if (-not $tun) { throw 'Pathweave TUN did not appear within 20 seconds.' }

    New-NetRoute -DestinationPrefix $routePrefix -InterfaceIndex $tun.ifIndex -NextHop '0.0.0.0' -PolicyStore ActiveStore | Out-Null
    $routeAdded = $true
    $startOutput = @(& netsh trace start capture=yes report=no persistent=no maxsize=48 "tracefile=$etl" 2>&1)
    $startCode = $LASTEXITCODE
    $startOutput | Out-File -LiteralPath $traceStartLog -Encoding utf8
    if ($startCode -ne 0) { throw "netsh trace start failed ($startCode): $($startOutput -join ' ')" }
    $captureStarted = $true
    if (($startOutput -join "`n") -notmatch '(?im)^\s*Status\s*:\s*Running') { throw 'netsh trace did not confirm an active session.' }

    for ($n = 1; $n -le $FlowCount; $n++) {
        $stdout = "$base-flow$n.out"
        $stderr = "$base-flow$n.err"
        $args = @('--noproxy','*','--silent','--show-error','--max-time','12','--connect-timeout','5',
            '--max-filesize',"$MaxResponseBytes",'--limit-rate','8192','--range',"0-$([Math]::Min(8191,$MaxResponseBytes - 1))",
            '--resolve',"$HostName`:443:$DestinationIPv4",'--output','NUL',
            '--write-out','%{http_code}|%{size_download}|%{remote_ip}|%{time_total}|%{local_ip}|%{local_port}|%{remote_port}',
            "https://$HostName/")
        $process = Start-Process -FilePath "$env:WINDIR\System32\curl.exe" -ArgumentList $args -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $curlProcesses += [pscustomobject]@{ number = $n; process = $process; stdout = $stdout; stderr = $stderr; started_utc = [DateTime]::UtcNow.ToString('o') }
    }

    $curlDeadline = (Get-Date).AddSeconds(15)
    do {
        $alive = @($curlProcesses | Where-Object { -not $_.process.HasExited })
        if ($alive.Count -eq 0) { break }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $curlDeadline)
    foreach ($client in $curlProcesses) {
        if (-not $client.process.HasExited) { Stop-Process -Id $client.process.Id -Force; $client.process.WaitForExit() }
        $client.process.Refresh()
        $output = ''
        if (Test-Path $client.stdout) { $rawOutput = Get-Content $client.stdout -Raw; if ($null -ne $rawOutput) { $output = $rawOutput.Trim() } }
        $errorText = ''
        if (Test-Path $client.stderr) { $rawError = Get-Content $client.stderr -Raw; if ($null -ne $rawError) { $errorText = $rawError.Trim() } }
        $curlResults += [pscustomobject]@{ number = $client.number; exit_code = $client.process.ExitCode; result = $output; error = $errorText; started_utc = $client.started_utc }
    }
}
finally {
    try {
        if ($captureStarted) {
            $stopOutput = @(& netsh trace stop 2>&1)
            $stopCode = $LASTEXITCODE
            $stopOutput | Out-File -LiteralPath $traceStopLog -Encoding utf8
            $captureStarted = $false
            if ($stopCode -ne 0) { throw "netsh trace stop failed ($stopCode): $($stopOutput -join ' ')" }
        }
    }
    finally {
        try { if ($routeAdded -and $tun) { Remove-NetRoute -DestinationPrefix $routePrefix -InterfaceIndex $tun.ifIndex -PolicyStore ActiveStore -Confirm:$false } }
        finally {
            if ($engineProcess -and -not $engineProcess.HasExited) { Stop-Process -Id $engineProcess.Id -Force; $engineProcess.WaitForExit() }
            if (Test-Path -LiteralPath $etl) {
                $convertOutput = @(& netsh trace convert "input=$etl" "output=$xmlPath" dump=XML report=no overwrite=yes 2>&1)
                $convertCode = $LASTEXITCODE
                $convertOutput | Out-File -LiteralPath $traceConvertLog -Encoding utf8
                if ($convertCode -ne 0) { throw "netsh trace ETL conversion failed ($convertCode)." }
            }
        }
    }
}

$routesAfter = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
$routeRemaining = @(Get-NetRoute -DestinationPrefix $routePrefix -ErrorAction SilentlyContinue)
$tunRemaining = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
if ($routeRemaining.Count -ne 0 -or $tunRemaining -or (Compare-Object $routesBefore $routesAfter)) { throw 'Pathweave route/TUN cleanup or default-route integrity check failed.' }

$engineLines = @()
foreach ($logPath in @($engineOut, $engineErr)) { if (Test-Path $logPath) { $engineLines += Get-Content $logPath } }
$engineLines = @($engineLines | ForEach-Object { $_ -replace '\x1b\[[0-9;?]*[ -/]*[@-~]', '' })
$connectionLines = @($engineLines | Where-Object { $_ -match 'TCP flow connected and pinned' -and $_ -match ('outbound_peer=Some\(' + [regex]::Escape("$DestinationIPv4`:443") + '\)') })
$flows = @()
foreach ($line in $connectionLines) {
    if ($line -match 'flow_id=(?<id>\d+)') { $flowId = [uint64]$Matches.id } else { continue }
    if ($line -match 'interface_index=(?<index>\d+)') { $flowInterface = [uint32]$Matches.index } else { continue }
    if ($line -match 'source=\S+:(?<sourcePort>\d+)') { $sourcePort = [uint16]$Matches.sourcePort } else { continue }
    if ($line -match 'outbound_local=Some\((?<local>(?<localIp>(?:\d{1,3}\.){3}\d{1,3}):(?<localPort>\d+))\)') {
        $localEndpoint = $Matches.local; $localIp = $Matches.localIp; $localPort = [uint16]$Matches.localPort
    }
    else { continue }
    if ($line -match 'outbound_peer=Some\((?<peer>(?:\d{1,3}\.){3}\d{1,3}:\d+)\)') { $peerEndpoint = $Matches.peer } else { continue }
    $flows += [pscustomobject]@{ flow_id = $flowId; interface_index = $flowInterface; source_port = $sourcePort; local_endpoint = $localEndpoint; local_ip = $localIp; local_port = $localPort; remote_endpoint = $peerEndpoint }
}

$trace = [xml][IO.File]::ReadAllText($xmlPath)
$namespace = New-Object System.Xml.XmlNamespaceManager($trace.NameTable)
$namespace.AddNamespace('evt', 'http://schemas.microsoft.com/win/2004/08/events/event')
$events = $trace.SelectNodes('//evt:Event[evt:System/evt:Provider[@Name="Microsoft-Windows-NDIS-PacketCapture"] and evt:EventData]', $namespace)
foreach ($flow in $flows) {
    if ([string]::IsNullOrWhiteSpace([string]$flow.local_endpoint) -or [string]::IsNullOrWhiteSpace([string]$flow.remote_endpoint)) { continue }
    $localParts = ([string]$flow.local_endpoint).Split(':')
    $peerParts = ([string]$flow.remote_endpoint).Split(':')
    $localIp = [Net.IPAddress]::Parse($localParts[0]).ToString()
    $localPort = [int]$localParts[1]
    $peerIp = [Net.IPAddress]::Parse($peerParts[0]).ToString()
    $peerPort = [int]$peerParts[1]
    $flow.outbound_packets = 0; $flow.inbound_packets = 0; $flow.outbound_payload_bytes = [long]0; $flow.inbound_payload_bytes = [long]0
    foreach ($event in $events) {
        $data = @{}
        foreach ($item in $event.SelectNodes('./evt:EventData/evt:Data', $namespace)) { $data[$item.GetAttribute('Name')] = $item.InnerText.Trim() }
        if ($data.MiniportIfIndex -ne "$($flow.interface_index)" -or $data.LowerIfIndex -ne "$($flow.interface_index)" -or -not $data.Fragment) { continue }
        $hex = $data.Fragment -replace '^0x', ''
        if (-not $hex -or ($hex.Length % 2) -ne 0) { continue }
        $frame = New-Object byte[] ($hex.Length / 2)
        for ($n = 0; $n -lt $frame.Length; $n++) { $frame[$n] = [Convert]::ToByte($hex.Substring($n * 2, 2), 16) }
        if ($frame.Length -lt 34) { continue }
        $ipOffset = -1
        $etherType = 0
        if ($frame.Length -ge 14) {
            $etherType = ([int]$frame[12] -shl 8) -bor [int]$frame[13]
            $ipOffset = 14
            while ($etherType -eq 0x8100 -or $etherType -eq 0x88a8) {
                if ($frame.Length -lt ($ipOffset + 4)) { $ipOffset = -1; break }
                $etherType = ([int]$frame[$ipOffset + 2] -shl 8) -bor [int]$frame[$ipOffset + 3]
                $ipOffset += 4
            }
            if ($etherType -ne 0x0800) { $ipOffset = -1 }
        }
        if ($ipOffset -lt 0 -and $frame.Length -ge 32) {
            $frameControl = [int]$frame[0] -bor ([int]$frame[1] -shl 8)
            if (($frameControl -band 0x000c) -eq 0x0008) {
                $headerLength = 24
                if (($frameControl -band 0x0300) -eq 0x0300) { $headerLength += 6 }
                if (($frameControl -band 0x0080) -ne 0) {
                    $headerLength += 2
                    if (($frameControl -band 0x8000) -ne 0) { $headerLength += 4 }
                }
                if ($frame.Length -ge ($headerLength + 8) -and
                    $frame[$headerLength] -eq 0xaa -and $frame[$headerLength + 1] -eq 0xaa -and $frame[$headerLength + 2] -eq 0x03) {
                    $etherType = ([int]$frame[$headerLength + 6] -shl 8) -bor [int]$frame[$headerLength + 7]
                    if ($etherType -eq 0x0800) { $ipOffset = $headerLength + 8 }
                }
            }
        }
        if ($ipOffset -lt 0 -or $frame.Length -lt ($ipOffset + 20) -or $frame[$ipOffset + 9] -ne 6) { continue }
        $ipHeaderLength = ($frame[$ipOffset] -band 0x0f) * 4
        $tcpOffset = $ipOffset + $ipHeaderLength
        if ($ipHeaderLength -lt 20 -or $frame.Length -lt ($tcpOffset + 20)) { continue }
        $sourceAddress = ([Net.IPAddress]::new([byte[]]$frame[($ipOffset + 12)..($ipOffset + 15)])).ToString()
        $destinationAddress = ([Net.IPAddress]::new([byte[]]$frame[($ipOffset + 16)..($ipOffset + 19)])).ToString()
        $sourcePort = ([int]$frame[$tcpOffset] -shl 8) -bor [int]$frame[$tcpOffset + 1]
        $destinationPort = ([int]$frame[$tcpOffset + 2] -shl 8) -bor [int]$frame[$tcpOffset + 3]
        $direction = $null
        if ($sourceAddress -eq $localIp -and $sourcePort -eq $localPort -and $destinationAddress -eq $peerIp -and $destinationPort -eq $peerPort) { $direction = 'outbound' }
        elseif ($sourceAddress -eq $peerIp -and $sourcePort -eq $peerPort -and $destinationAddress -eq $localIp -and $destinationPort -eq $localPort) { $direction = 'inbound' }
        else { continue }
        $tcpHeaderLength = (($frame[$tcpOffset + 12] -shr 4) -band 0x0f) * 4
        if ($tcpHeaderLength -lt 20 -or $frame.Length -lt ($tcpOffset + $tcpHeaderLength)) { continue }
        $ipTotalLength = ([int]$frame[$ipOffset + 2] -shl 8) -bor [int]$frame[$ipOffset + 3]
        $payloadBytes = [Math]::Max(0, [Math]::Min($ipTotalLength - $ipHeaderLength - $tcpHeaderLength, $frame.Length - $tcpOffset - $tcpHeaderLength))
        if ($direction -eq 'outbound') { $flow.outbound_packets++; $flow.outbound_payload_bytes += $payloadBytes }
        else { $flow.inbound_packets++; $flow.inbound_payload_bytes += $payloadBytes }
    }
}

foreach ($result in $curlResults) {
    $resultText = if ($null -eq $result.result) { '' } else { [string]$result.result }
    $parts = $resultText.Split('|')
    $result | Add-Member -NotePropertyName http_code -NotePropertyValue $null
    $result | Add-Member -NotePropertyName body_bytes -NotePropertyValue 0
    $result | Add-Member -NotePropertyName local_port -NotePropertyValue $null
    if ($parts.Count -ge 7) {
        $result.http_code = [int]$parts[0]; $result.body_bytes = [int64]([double]$parts[1]); $result.local_port = [uint16]$parts[5]
    }
    $flow = $flows | Where-Object { $_.source_port -eq $result.local_port } | Select-Object -First 1
    if ($flow) { $result | Add-Member -NotePropertyName flow_id -NotePropertyValue $flow.flow_id; $result | Add-Member -NotePropertyName interface_index -NotePropertyValue $flow.interface_index }
}
$everyInterfaceVerified = $true
foreach ($iface in $interfaces) {
    $requiredBodyBytes = ($curlResults | Where-Object { $_.interface_index -eq $iface.index -and $_.http_code -ge 200 -and $_.http_code -lt 400 } | Measure-Object -Property body_bytes -Sum).Sum
    if ($null -eq $requiredBodyBytes) { $requiredBodyBytes = 0 }
    $verified = @($flows | Where-Object { $_.interface_index -eq $iface.index -and $_.outbound_packets -gt 0 -and $_.inbound_packets -gt 0 -and $_.outbound_payload_bytes -gt 0 -and $_.inbound_payload_bytes -ge $requiredBodyBytes -and $requiredBodyBytes -gt 0 })
    $iface | Add-Member -NotePropertyName verified_data_flows -NotePropertyValue $verified.Count
    if ($verified.Count -eq 0) { $everyInterfaceVerified = $false }
}
$allRequestsPassed = @($curlResults | Where-Object { $_.exit_code -eq 0 -and $_.http_code -ge 200 -and $_.http_code -lt 400 }).Count -eq $FlowCount
$resultRecord = [pscustomobject]@{
    status = if ($allRequestsPassed -and $everyInterfaceVerified) { 'PASS' } else { 'INCOMPLETE' }
    simultaneous_flow_count = $FlowCount
    configured_interfaces = $interfaces
    curl_results = $curlResults
    pathweave_flows = $flows
    ndis_packet_event_count = $events.Count
    etl = $etl
    decoded_xml = $xmlPath
    temporary_route_removed = $true
    tun_removed = $true
    default_routes_unchanged = $true
}
$resultPath = "$base-result.json"
$resultRecord | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $resultPath -Encoding UTF8
$resultRecord | ConvertTo-Json -Depth 8
Write-Output "RESULT=$resultPath"
if ($resultRecord.status -ne 'PASS') { exit 2 }
