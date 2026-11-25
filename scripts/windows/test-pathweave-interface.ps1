param(
    [Parameter(Mandatory)][ValidateRange(1, 4294967295)][uint32]$InterfaceIndex,
    [Parameter(Mandatory)][string]$ExpectedGuid,
    [Parameter(Mandatory)][string]$DestinationIPv4,
    [Parameter(Mandatory)][string]$HostName,
    [ValidateRange(1024, 16384)][int]$MaxResponseBytes = 16384,
    [ValidateSet('PacketMonitor', 'NetshTrace')][string]$CaptureBackend = 'PacketMonitor',
    [ValidateRange(1, 512)][int]$MaxConnections = 512,
    [ValidateRange(1, 60)][int]$ConnectTimeoutSecs = 12
)

$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run elevated to create the temporary TUN route and start the selected capture backend.' }

$remoteIp = [Net.IPAddress]::Parse($DestinationIPv4)
if ($remoteIp.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { throw 'This validation script requires an IPv4 destination.' }
$adapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex -ErrorAction Stop
if ($adapter.Status -ne 'Up') { throw "Interface $InterfaceIndex is not active." }

$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$engine = Join-Path $workspace 'target\release\pathweave-engine.exe'
if (-not (Test-Path -LiteralPath $engine)) { throw 'Build the release engine before running this test.' }
$telemetryJson = (& $engine --telemetry-once | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Pathweave could not read the Windows interface table.' }
$telemetry = ConvertFrom-Json -InputObject $telemetryJson
$identity = $telemetry | Where-Object { $_.index -eq $InterfaceIndex } | Select-Object -First 1
if (-not $identity -or $identity.stable_guid -ne $ExpectedGuid -or -not $identity.operational) { throw 'The selected index no longer resolves to the expected operational GUID.' }
$adapterAddresses = @($identity.ipv4_addresses | ForEach-Object { ($_ -split '/')[0] } | Where-Object { $_ -notlike '169.254.*' })
if ($adapterAddresses.Count -eq 0) { throw "The selected interface $InterfaceIndex has no usable IPv4 source address." }
$selectedSourceIPv4 = $adapterAddresses[0]
$routeBefore = Find-NetRoute -RemoteIPAddress $DestinationIPv4 -ErrorAction Stop | Select-Object InterfaceAlias,InterfaceIndex,NextHop,DestinationPrefix,RouteMetric

$directSocketResult = $null
$directSocket = [Net.Sockets.Socket]::new([Net.Sockets.AddressFamily]::InterNetwork, [Net.Sockets.SocketType]::Stream, [Net.Sockets.ProtocolType]::Tcp)
$directWatch = [Diagnostics.Stopwatch]::StartNew()
try {
    $directSocket.Bind([Net.IPEndPoint]::new([Net.IPAddress]::Parse($selectedSourceIPv4), 0))
    $directLocal = $directSocket.LocalEndPoint.ToString()
    $directAsync = $directSocket.BeginConnect([Net.IPEndPoint]::new($remoteIp, 443), $null, $null)
    if (-not $directAsync.AsyncWaitHandle.WaitOne([TimeSpan]::FromSeconds(5))) { throw [TimeoutException]::new('Direct Wi-Fi TCP connect exceeded 5000 ms.') }
    $directSocket.EndConnect($directAsync)
    $directWatch.Stop()
    $directSocketResult = [pscustomobject]@{
        bind_succeeded = $true
        connect_succeeded = $true
        local_endpoint = $directSocket.LocalEndPoint.ToString()
        remote_endpoint = $directSocket.RemoteEndPoint.ToString()
        interface_index = $InterfaceIndex
        duration_ms = $directWatch.ElapsedMilliseconds
        error = $null
    }
}
catch {
    $directWatch.Stop()
    $directError = $_.Exception
    while ($directError.InnerException) { $directError = $directError.InnerException }
    $directSocketResult = [pscustomobject]@{
        bind_succeeded = [bool]$directSocket.LocalEndPoint
        connect_succeeded = $false
        local_endpoint = if ($directSocket.LocalEndPoint) { $directSocket.LocalEndPoint.ToString() } else { $null }
        remote_endpoint = "$DestinationIPv4`:443"
        interface_index = $InterfaceIndex
        duration_ms = $directWatch.ElapsedMilliseconds
        error = [pscustomobject]@{ message = $directError.Message; type = $directError.GetType().FullName; socket_error = if ($directError -is [Net.Sockets.SocketException]) { $directError.SocketErrorCode.ToString() } else { $null }; native_error = if ($directError -is [Net.Sockets.SocketException]) { $directError.NativeErrorCode } else { $null } }
    }
}
finally { $directSocket.Dispose() }

if (Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue) { throw 'A Pathweave adapter already exists; refusing to reuse it.' }
$prefix = "$DestinationIPv4/32"
if (@(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue).Count -gt 0) { throw "A route for $prefix already exists; refusing to replace it." }
if ($CaptureBackend -eq 'PacketMonitor') {
    $pktmonStatus = (& pktmon status 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0 -or $pktmonStatus -match '(?i)(status|state)\s*:\s*running') { throw 'Packet Monitor is active or its state cannot be verified.' }
}
else {
    $traceStatus = (& netsh trace show status 2>&1 | Out-String)
    $traceStatusExit = $LASTEXITCODE
    $noTraceSession = $traceStatus -match '(?i)no trace session'
    if (($traceStatusExit -ne 0 -and -not $noTraceSession) -or
        $traceStatus -match '(?im)^\s*Status\s*:\s*Running' -or -not $noTraceSession) {
        throw 'A netsh trace session is active or its state cannot be verified.'
    }
}

$target = Join-Path $workspace 'target\live-validation'
New-Item -ItemType Directory -Path $target -Force | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$etl = Join-Path $target "pathweave-if$InterfaceIndex-$stamp.etl"
$packetText = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-packets.txt"
$packetXml = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-packets.xml"
$componentMap = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-components.txt"
$captureStartLog = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-capture-start.log"
$captureStopLog = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-capture-stop.log"
$captureConvertLog = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-capture-convert.log"
$engineOut = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-engine.log"
$engineErr = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-engine-error.log"
$curlErrorPath = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-curl-error.log"
$resultPath = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-result.json"
$routeAdded = $false
$routeDuring = @()
$captureStarted = $false
$engineProcess = $null
$curlProcess = $null
$previousPathweaveLogFormat = $env:PATHWEAVE_LOG_FORMAT
$curlOutputPath = Join-Path $target "pathweave-if$InterfaceIndex-$stamp-curl-output.log"
$routesBefore = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
$virtualBefore = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name)|$($_.ifIndex)|$($_.Status)" } | Sort-Object)
$curlResult = $null
$curlExitCode = $null
$pathweaveSocketSamples = New-Object System.Collections.Generic.List[object]
$failureMessage = $null
$componentId = $null
$outboundLocal = $null
$outboundPeer = $null
$captureStopError = $null

try {
    $previousRustLog = $env:RUST_LOG
    $previousPathweaveLogFormat = $env:PATHWEAVE_LOG_FORMAT
    try {
        $env:RUST_LOG = 'info'
        $env:PATHWEAVE_LOG_FORMAT = 'json'
        $engineProcess = Start-Process -FilePath $engine -WorkingDirectory (Split-Path $engine -Parent) -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $engineOut -RedirectStandardError $engineErr -ArgumentList @('--interface', "$InterfaceIndex", '--max-connections', "$MaxConnections", '--connect-timeout-secs', "$ConnectTimeoutSecs")
    }
    finally {
        $env:RUST_LOG = $previousRustLog
        if ($null -eq $previousPathweaveLogFormat) { Remove-Item Env:PATHWEAVE_LOG_FORMAT -ErrorAction SilentlyContinue } else { $env:PATHWEAVE_LOG_FORMAT = $previousPathweaveLogFormat }
    }
    $deadline = (Get-Date).AddSeconds(20)
    $tun = $null
    while ((Get-Date) -lt $deadline -and -not $tun) {
        if ($engineProcess.HasExited) { throw "Pathweave exited before TUN startup; inspect $engineErr" }
        $tun = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
        if (-not $tun) { Start-Sleep -Milliseconds 250 }
    }
    if (-not $tun) { throw 'Pathweave TUN did not appear within 20 seconds.' }

    if ($CaptureBackend -eq 'PacketMonitor') {
        $componentRows = @(& pktmon list 2>&1)
        if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate Packet Monitor components.' }
        $componentRows | Out-File -LiteralPath $componentMap -Encoding utf8
        $adapterMac = ($adapter.MacAddress -replace '[:-]', '').ToUpperInvariant()
        $macOctets = [regex]::Matches($adapterMac, '..') | ForEach-Object { [regex]::Escape($_.Value) }
        $macPattern = '(?i)' + ($macOctets -join '[:-]?')
        $adapterComponent = @($componentRows | Where-Object { $_.ToString() -match $macPattern })
        if ($adapterComponent.Count -ne 1 -or $adapterComponent[0].ToString() -notmatch '^\s*(\d+)\s+') { throw 'Could not uniquely resolve the selected tether NIC Packet Monitor component by MAC address.' }
        $componentId = [int]$Matches[1]
    }

    New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -NextHop '0.0.0.0' -PolicyStore ActiveStore | Out-Null
    $routeAdded = $true
    $routeDuring = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction Stop | Select-Object InterfaceAlias,InterfaceIndex,NextHop,DestinationPrefix,RouteMetric)
    if ($CaptureBackend -eq 'PacketMonitor') {
        $captureOutput = @(& pktmon start --capture --comp $componentId --pkt-size 128 --file-name $etl --file-size 16 --log-mode circular 2>&1)
    }
    else {
        $captureOutput = @(& netsh trace start capture=yes report=no persistent=no maxsize=32 "tracefile=$etl" 2>&1)
    }
    $captureExitCode = $LASTEXITCODE
    $captureOutput | Out-File -LiteralPath $captureStartLog -Encoding utf8
    if ($captureExitCode -ne 0) { throw "$CaptureBackend capture start failed ($captureExitCode): $($captureOutput -join ' ')" }
    $captureStarted = $true
    if ($CaptureBackend -eq 'NetshTrace' -and ($captureOutput -join "`n") -notmatch '(?im)^\s*Status\s*:\s*Running') {
        throw 'netsh trace did not confirm an active packet-capture session.'
    }

    $curlArguments = @('--noproxy','*','--silent','--show-error','--max-time','12','--connect-timeout','5',
        '--max-filesize',"$MaxResponseBytes",'--limit-rate','8192','--range',"0-$([Math]::Min(8191,$MaxResponseBytes - 1))",
        '--resolve',"$HostName`:443:$DestinationIPv4",'--output','NUL','--write-out','%{http_code}|%{size_download}|%{remote_ip}|%{time_total}|%{local_ip}|%{local_port}|%{remote_port}',
        "https://$HostName/")
    $curlProcess = Start-Process -FilePath 'curl.exe' -WindowStyle Hidden -PassThru -RedirectStandardOutput $curlOutputPath -RedirectStandardError $curlErrorPath -ArgumentList $curlArguments
    while (-not $curlProcess.HasExited) {
        $liveSockets = @(Get-NetTCPConnection -OwningProcess $engineProcess.Id -ErrorAction SilentlyContinue | Where-Object { $_.RemoteAddress -eq $DestinationIPv4 -and $_.RemotePort -eq 443 })
        foreach ($socket in $liveSockets) {
            $sampleKey = "$($socket.LocalAddress):$($socket.LocalPort)|$($socket.RemoteAddress):$($socket.RemotePort)|$($socket.State)"
            if (-not ($pathweaveSocketSamples | Where-Object { $_.key -eq $sampleKey })) {
                $localAddressOwner = Get-NetIPAddress -IPAddress $socket.LocalAddress -ErrorAction SilentlyContinue | Select-Object -First 1
                $pathweaveSocketSamples.Add([pscustomobject]@{ key=$sampleKey; local_endpoint="$($socket.LocalAddress):$($socket.LocalPort)"; remote_endpoint="$($socket.RemoteAddress):$($socket.RemotePort)"; state=$socket.State.ToString(); local_address_interface_index=if($localAddressOwner){$localAddressOwner.InterfaceIndex}else{$null}; owning_process_id=$engineProcess.Id; observed_at=(Get-Date).ToString('o') })
            }
        }
        Start-Sleep -Milliseconds 50
    }
    $curlProcess.WaitForExit()
    $curlProcess.Refresh()
    $curlExitCode = [int]$curlProcess.ExitCode
    $curlResult = if (Test-Path -LiteralPath $curlOutputPath) { (Get-Content -LiteralPath $curlOutputPath -Raw).Trim() } else { '' }
    if ($curlExitCode -ne 0) {
        $curlErrorText = if (Test-Path -LiteralPath $curlErrorPath) { (Get-Content -LiteralPath $curlErrorPath | Out-String).Trim() } else { $curlResult }
        throw "Bounded HTTPS request failed with curl exit ${curlExitCode}: $curlErrorText"
    }
    $parts = $curlResult.Split('|')
    if ($parts.Count -lt 4) { throw "Could not parse bounded HTTPS result: $curlResult" }
    $httpCode = [int]$parts[0]
    $downloaded = [int64]([double]$parts[1])
    if ($parts[2] -ne $DestinationIPv4) { throw "curl connected to unexpected remote IP $($parts[2]); expected $DestinationIPv4." }
    $curlLocalEndpoint = if ($parts.Count -ge 6) { "$($parts[4]):$($parts[5])" } else { $null }
    $curlRemoteEndpoint = if ($parts.Count -ge 7) { "$($parts[2]):$($parts[6])" } else { "$($parts[2]):443" }
    if ($httpCode -lt 200 -or $httpCode -ge 400 -or $downloaded -gt $MaxResponseBytes) { throw "Unexpected HTTP result or response exceeded cap: $curlResult" }
}
catch {
    $failureMessage = $_.Exception.Message
}
finally {
    try {
        if ($captureStarted) {
            if ($CaptureBackend -eq 'PacketMonitor') { $stopOutput = @(& pktmon stop 2>&1) }
            else { $stopOutput = @(& netsh trace stop 2>&1) }
            $stopExitCode = $LASTEXITCODE
            $stopOutput | Out-File -LiteralPath $captureStopLog -Encoding utf8
            $captureStarted = $false
            if ($stopExitCode -ne 0) { $captureStopError = "$CaptureBackend capture stop failed ($stopExitCode): $($stopOutput -join ' ')" }
        }
    }
    finally {
        try {
            if ($routeAdded -and $tun) { Remove-NetRoute -DestinationPrefix $prefix -InterfaceIndex $tun.ifIndex -PolicyStore ActiveStore -Confirm:$false }
        }
        finally {
            if ($engineProcess -and -not $engineProcess.HasExited) { Stop-Process -Id $engineProcess.Id -Force; $engineProcess.WaitForExit() }
            if (Test-Path -LiteralPath $etl) {
                if ($CaptureBackend -eq 'PacketMonitor') {
                    $convertOutput = @(& pktmon etl2txt $etl --out $packetText --brief 2>&1)
                    $convertExitCode = $LASTEXITCODE
                }
                else {
                    $convertOutput = @(& netsh trace convert "input=$etl" "output=$packetXml" dump=XML report=no overwrite=yes 2>&1)
                    $convertExitCode = $LASTEXITCODE
                }
                $convertOutput | Out-File -LiteralPath $captureConvertLog -Encoding utf8
                if ($convertExitCode -ne 0) { throw "$CaptureBackend ETL conversion failed ($convertExitCode): $($convertOutput -join ' ')" }
            }
        }
    }
}

if ($failureMessage) {
    $failedRouteRemaining = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
    $failedTunRemaining = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
    $failedRoutesAfter = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
    $failedVirtualAfter = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name)|$($_.ifIndex)|$($_.Status)" } | Sort-Object)
    $ndisEventCount = 0
    $targetAddressSeen = $false
    if ($CaptureBackend -eq 'NetshTrace' -and (Test-Path -LiteralPath $packetXml)) {
        $ndisEventCount = (Select-String -LiteralPath $packetXml -Pattern 'Microsoft-Windows-NDIS-PacketCapture').Count
        $targetAddressSeen = [bool](Select-String -LiteralPath $packetXml -Pattern 'AC4293F3' -SimpleMatch -Quiet)
    }
    $acceptedCount = if (Test-Path -LiteralPath $engineOut) { (Select-String -LiteralPath $engineOut -Pattern '"message":"TCP flow accepted"').Count } else { 0 }
    $capacityRejectedCount = if (Test-Path -LiteralPath $engineOut) { (Select-String -LiteralPath $engineOut -Pattern '"stage":"tcp_flow_rejected_capacity"').Count } else { 0 }
    $connectTimeoutCount = if (Test-Path -LiteralPath $engineOut) { (Select-String -LiteralPath $engineOut -Pattern '"stage":"tcp_connect_timed_out"').Count } else { 0 }
    $traceStatusAfter = netsh trace show status 2>&1 | Out-String
    $failedReport = [pscustomobject]@{
        status = 'failed'
        failure = $failureMessage
        adapter_alias = $adapter.Name
        interface_guid = $ExpectedGuid
        interface_index = $InterfaceIndex
        interface_luid = $identity.luid
        operational = $identity.operational
        ipv4_addresses = $identity.ipv4_addresses
        default_gateways = $identity.default_gateways
        route_to_destination_before_test = @($routeBefore)
        temporary_route_during_test = @($routeDuring)
        selected_source_ipv4 = $selectedSourceIPv4
        direct_socket = $directSocketResult
        max_connections = $MaxConnections
        connect_timeout_seconds = $ConnectTimeoutSecs
        curl_result = $curlResult
        curl_exit_code = $curlExitCode
        curl_error = if (Test-Path -LiteralPath $curlErrorPath) { (Get-Content -LiteralPath $curlErrorPath -Raw).Trim() } else { $null }
        pathweave_tcp_socket_samples = $pathweaveSocketSamples.ToArray()
        tcp_flow_accepted_count = $acceptedCount
        tcp_capacity_rejected_count = $capacityRejectedCount
        tcp_connect_timeout_count = $connectTimeoutCount
        capture_backend = $CaptureBackend
        etl = $etl
        decoded_xml = if (Test-Path -LiteralPath $packetXml) { $packetXml } else { $null }
        ndis_provider_lines = $ndisEventCount
        destination_ipv4_bytes_seen = $targetAddressSeen
        capture_stop_error = $captureStopError
        route_removed = ($failedRouteRemaining.Count -eq 0)
        tun_removed = (-not $failedTunRemaining)
        default_routes_unchanged = (-not [bool](Compare-Object $routesBefore $failedRoutesAfter))
        existing_virtual_adapters_unchanged = (-not [bool](Compare-Object $virtualBefore $failedVirtualAfter))
        netsh_trace_stopped = (-not [bool]($traceStatusAfter -match '(?im)^\s*Status\s*:\s*Running'))
        engine_log = $engineOut
        engine_error_log = $engineErr
    }
    $failedReport | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    throw "Bounded Pathweave validation failed: $failureMessage (evidence: $resultPath)"
}

if ($captureStopError) { throw $captureStopError }

$routeRemaining = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
$tunRemaining = Get-NetAdapter -Name 'Pathweave' -ErrorAction SilentlyContinue
$routesAfter = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.InterfaceIndex)|$($_.NextHop)|$($_.RouteMetric)|$($_.PolicyStore)" } | Sort-Object)
$virtualAfter = @(Get-NetAdapter -Name 'net-merge','utun8' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name)|$($_.ifIndex)|$($_.Status)" } | Sort-Object)
if ($routeRemaining.Count -ne 0) { throw "Cleanup failed: $prefix remains." }
if ($tunRemaining) { throw 'Cleanup failed: Pathweave TUN remains.' }
if (Compare-Object $routesBefore $routesAfter) { throw 'Default routes changed during the test.' }
if (Compare-Object $virtualBefore $virtualAfter) { throw 'A pre-existing virtual adapter changed during the test.' }
if (-not $curlResult) { throw 'No HTTPS result was recorded.' }
$captureEvidence = $null
$pathweaveSocket = $null
if ($CaptureBackend -eq 'PacketMonitor') {
    $captureStats = @(& pktmon etl2txt $etl --stats 2>&1)
    $captureStats | Out-File -LiteralPath (Join-Path $target "pathweave-if$InterfaceIndex-$stamp-packet-stats.txt") -Encoding utf8
    $packetCountLine = $captureStats | Where-Object { $_.ToString() -match 'Packets total\s*:?\s*(\d+)' } | Select-Object -First 1
    if (-not $packetCountLine -or $packetCountLine.ToString() -notmatch 'Packets total\s*:?\s*(\d+)') { throw 'Packet Monitor did not provide a packet count.' }
    $capturedPackets = [int64]$Matches[1]
    if ($capturedPackets -eq 0) { throw 'HTTPS succeeded, but Packet Monitor captured zero packets on the selected physical NIC; physical egress is not verified.' }
    $captureEvidence = [pscustomobject]@{ packet_count = $capturedPackets; packet_text = $packetText }
}
else {
    $engineEvents = @()
    foreach ($logPath in @($engineOut, $engineErr)) {
        if (Test-Path -LiteralPath $logPath) {
            foreach ($line in Get-Content -LiteralPath $logPath) {
                try { $engineEvents += (ConvertFrom-Json -InputObject $line -ErrorAction Stop) } catch { }
            }
        }
    }
    $connectedEvents = @($engineEvents | Where-Object {
        $_.message -eq 'TCP flow connected and pinned' -and
        [int]$_.interface_index -eq $InterfaceIndex -and
        [string]$_.outbound_peer -match ('Some\(' + [regex]::Escape("$DestinationIPv4`:443") + '\)')
    })
    if ($connectedEvents.Count -ne 1) { throw "Expected one logged Pathweave TCP flow to $DestinationIPv4`:443 on interface $InterfaceIndex; found $($connectedEvents.Count)." }
    $flowEvent = $connectedEvents[0]
    if ([string]$flowEvent.outbound_local -notmatch 'Some\((?<endpoint>(?<ip>(?:\d{1,3}\.){3}\d{1,3}):(?<port>\d+))\)') { throw 'Pathweave did not log a parseable outbound local IPv4 endpoint.' }
    $outboundLocal = $Matches.endpoint
    if ([string]$flowEvent.outbound_peer -notmatch 'Some\((?<endpoint>(?<ip>(?:\d{1,3}\.){3}\d{1,3}):(?<port>\d+))\)') { throw 'Pathweave did not log a parseable outbound peer IPv4 endpoint.' }
    $outboundPeer = $Matches.endpoint
    $localParts = $outboundLocal.Split(':')
    $peerParts = $outboundPeer.Split(':')
    $localEndpoint = New-Object Net.IPEndPoint ([Net.IPAddress]::Parse($localParts[0]), [int]$localParts[1])
    $peerEndpoint = New-Object Net.IPEndPoint ([Net.IPAddress]::Parse($peerParts[0]), [int]$peerParts[1])
    $physicalAddresses = @(Get-NetIPAddress -InterfaceIndex $InterfaceIndex -AddressFamily IPv4 -ErrorAction Stop | Select-Object -ExpandProperty IPAddress)
    if ($physicalAddresses -notcontains $localEndpoint.Address.ToString()) { throw "Pathweave socket local address $($localEndpoint.Address) is not assigned to interface $InterfaceIndex." }
    if ($peerEndpoint.Address.ToString() -ne $DestinationIPv4 -or $peerEndpoint.Port -ne 443) { throw "Pathweave logged an unexpected remote endpoint: $outboundPeer." }

    $currentAdapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex -ErrorAction Stop
    if ($currentAdapter.Status -ne 'Up' -or $currentAdapter.InterfaceGuid.ToString().Trim('{}') -ine $ExpectedGuid.Trim('{}')) { throw 'The selected interface changed identity or went down during the capture.' }

    [xml]$traceDocument = [IO.File]::ReadAllText($packetXml)
    $namespace = New-Object System.Xml.XmlNamespaceManager($traceDocument.NameTable)
    $namespace.AddNamespace('evt', 'http://schemas.microsoft.com/win/2004/08/events/event')
    $ndisEvents = $traceDocument.SelectNodes('//evt:Event[evt:System/evt:Provider[@Name="Microsoft-Windows-NDIS-PacketCapture"] and evt:EventData]', $namespace)
    if (-not $ndisEvents -or $ndisEvents.Count -eq 0) { throw 'The netsh trace ETL contains no NDIS packet events.' }

    $outboundPackets = 0
    $inboundPackets = 0
    [long]$outboundPayloadBytes = 0
    [long]$inboundPayloadBytes = 0
    foreach ($event in $ndisEvents) {
        $eventData = @{}
        foreach ($item in $event.SelectNodes('./evt:EventData/evt:Data', $namespace)) { $eventData[$item.GetAttribute('Name')] = $item.InnerText.Trim() }
        if ($eventData.MiniportIfIndex -ne "$InterfaceIndex" -or $eventData.LowerIfIndex -ne "$InterfaceIndex" -or -not $eventData.Fragment) { continue }
        $hex = $eventData.Fragment -replace '^0x', ''
        if (-not $hex -or ($hex.Length % 2) -ne 0) { continue }
        $frame = New-Object byte[] ($hex.Length / 2)
        for ($i = 0; $i -lt $frame.Length; $i++) { $frame[$i] = [Convert]::ToByte($hex.Substring($i * 2, 2), 16) }
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
        if ($ipOffset -lt 0 -or $frame.Length -lt ($ipOffset + 20)) { continue }
        $ipHeaderLength = ($frame[$ipOffset] -band 0x0f) * 4
        $ipTotalLength = ([int]$frame[$ipOffset + 2] -shl 8) -bor [int]$frame[$ipOffset + 3]
        $fragmentOffset = (([int]$frame[$ipOffset + 6] -shl 8) -bor [int]$frame[$ipOffset + 7]) -band 0x1fff
        if ($frame[$ipOffset + 9] -ne 6 -or $fragmentOffset -ne 0 -or $ipHeaderLength -lt 20 -or $frame.Length -lt ($ipOffset + $ipHeaderLength + 20)) { continue }

        $packetSourceIp = ([Net.IPAddress]::new([byte[]]$frame[($ipOffset + 12)..($ipOffset + 15)])).ToString()
        $destinationAddress = ([Net.IPAddress]::new([byte[]]$frame[($ipOffset + 16)..($ipOffset + 19)])).ToString()
        $tcpOffset = $ipOffset + $ipHeaderLength
        $sourcePort = ([int]$frame[$tcpOffset] -shl 8) -bor [int]$frame[$tcpOffset + 1]
        $destinationPort = ([int]$frame[$tcpOffset + 2] -shl 8) -bor [int]$frame[$tcpOffset + 3]
        $direction = $null
        if ($packetSourceIp -eq $localEndpoint.Address.ToString() -and $sourcePort -eq $localEndpoint.Port -and
            $destinationAddress -eq $peerEndpoint.Address.ToString() -and $destinationPort -eq $peerEndpoint.Port) { $direction = 'outbound' }
        elseif ($packetSourceIp -eq $peerEndpoint.Address.ToString() -and $sourcePort -eq $peerEndpoint.Port -and
            $destinationAddress -eq $localEndpoint.Address.ToString() -and $destinationPort -eq $localEndpoint.Port) { $direction = 'inbound' }
        else { continue }

        $tcpHeaderLength = (($frame[$tcpOffset + 12] -shr 4) -band 0x0f) * 4
        if ($tcpHeaderLength -lt 20 -or $frame.Length -lt ($tcpOffset + $tcpHeaderLength)) { continue }
        $capturedPayloadLength = [Math]::Max(0, [Math]::Min($ipTotalLength - $ipHeaderLength - $tcpHeaderLength, $frame.Length - $tcpOffset - $tcpHeaderLength))
        if ($direction -eq 'outbound') { $outboundPackets++; $outboundPayloadBytes += $capturedPayloadLength }
        else { $inboundPackets++; $inboundPayloadBytes += $capturedPayloadLength }
    }

    if ($outboundPackets -eq 0 -or $inboundPackets -eq 0) { throw "No bidirectional NDIS packets matched Pathweave socket $outboundLocal -> $outboundPeer on interface $InterfaceIndex." }
    if ($downloaded -le 0 -or $inboundPayloadBytes -lt $downloaded) { throw "The capture did not prove transferred response data: HTTP body=$downloaded bytes, matching inbound TCP payload=$inboundPayloadBytes bytes." }
    if ($outboundPayloadBytes -le 0) { throw 'The capture matched only inbound data; no outbound TCP payload was verified.' }

    $captureEvidence = [pscustomobject]@{
        etl = $etl
        decoded_xml = $packetXml
        ndis_packet_events = $ndisEvents.Count
        capture_interface_index = $InterfaceIndex
        matched_outbound_packets = $outboundPackets
        matched_inbound_packets = $inboundPackets
        matched_outbound_tcp_payload_bytes = $outboundPayloadBytes
        matched_inbound_tcp_payload_bytes = $inboundPayloadBytes
        required_http_body_bytes = $downloaded
    }
    $flowId = $null
    if ($flowEvent.flow_id) { $flowId = [uint64]$flowEvent.flow_id }
    $pathweaveSocket = [pscustomobject]@{
        flow_id = $flowId
        interface_index = $InterfaceIndex
        local_endpoint = $outboundLocal
        remote_endpoint = $outboundPeer
    }
}

$report = [pscustomobject]@{
    interface_alias = $adapter.Name
    interface_description = $adapter.InterfaceDescription
    interface_index = $InterfaceIndex
    interface_guid = $ExpectedGuid
    interface_mac = $adapter.MacAddress
    source_ipv4 = ($identity.ipv4_addresses | Select-Object -First 1)
    destination = "$HostName ($DestinationIPv4):443"
    route_to_destination_before_test = @($routeBefore)
    temporary_route_during_test = @($routeDuring)
    direct_socket = $directSocketResult
    selected_source_ipv4 = $selectedSourceIPv4
    max_connections = $MaxConnections
    connect_timeout_seconds = $ConnectTimeoutSecs
    pathweave_tcp_socket_samples = @($pathweaveSocketSamples)
    http_result = $curlResult
    curl_exit_code = $curlExitCode
    curl_frontend_socket = [pscustomobject]@{ local_endpoint = $curlLocalEndpoint; remote_endpoint = $curlRemoteEndpoint }
    response_byte_limit = $MaxResponseBytes
    capture_backend = $CaptureBackend
    packet_capture = $etl
    packet_text = if ($CaptureBackend -eq 'PacketMonitor') { $packetText } else { $null }
    packet_xml = if ($CaptureBackend -eq 'NetshTrace') { $packetXml } else { $null }
    packet_capture_evidence = $captureEvidence
    pathweave_outbound_socket = $pathweaveSocket
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
