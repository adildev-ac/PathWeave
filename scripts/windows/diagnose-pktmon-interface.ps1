param(
    [Parameter(Mandatory)][ValidateRange(1, 4294967295)][uint32]$InterfaceIndex,
    [Parameter(Mandatory)][string]$ExpectedGuid,
    [Parameter(Mandatory)][string]$ExpectedMacAddress,
    [Parameter(Mandatory)][string]$DestinationIPv4,
    [ValidateRange(1, 65535)][int]$DestinationPort = 443
)

$ErrorActionPreference = 'Stop'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run elevated to control Packet Monitor.' }

$adapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex -ErrorAction Stop
$normalizedMac = ($adapter.MacAddress -replace '[:-]', '').ToUpperInvariant()
$expectedMac = ($ExpectedMacAddress -replace '[:-]', '').ToUpperInvariant()
if ($adapter.Status -ne 'Up' -or $adapter.InterfaceGuid.Trim('{}') -ine $ExpectedGuid.Trim('{}') -or $normalizedMac -ne $expectedMac) {
    throw 'The interface index, expected GUID, MAC address, and operational state do not match.'
}
$sourceAddress = Get-NetIPAddress -InterfaceIndex $InterfaceIndex -AddressFamily IPv4 -ErrorAction Stop |
    Where-Object { $_.IPAddress -notlike '169.254.*' -and $_.IPAddress -ne '127.0.0.1' } |
    Select-Object -First 1 -ExpandProperty IPAddress
if (-not $sourceAddress) { throw 'No usable IPv4 source address was found on the selected interface.' }
$destination = [Net.IPAddress]::Parse($DestinationIPv4)
if ($destination.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { throw 'DestinationIPv4 must be IPv4.' }

$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$outputDir = Join-Path $workspace 'target\capture-diagnostics'
New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$prefix = Join-Path $outputDir "pktmon-if$InterfaceIndex-$stamp"

function Get-PktmonStatusText {
    $text = @(& pktmon status 2>&1)
    $exit = $LASTEXITCODE
    if ($exit -ne 0) { throw "pktmon status failed ($exit): $($text -join ' ')" }
    return ($text -join "`n")
}
function Test-PktmonRunning([string]$statusText) {
    if ($statusText -match '(?i)\bnot\s+running\b') { return $false }
    return ($statusText -match '(?i)\bPacket Monitor is running\b' -or $statusText -match '(?im)^\s*Logger Parameters\s*:')
}

$initialStatus = Get-PktmonStatusText
if (Test-PktmonRunning -statusText ($initialStatus -join "`n")) { throw 'Packet Monitor is already running; refusing to stop or replace another capture.' }

$filters = @(& pktmon filter list 2>&1)
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect Packet Monitor filters.' }
$filters | Out-File -LiteralPath "$prefix-filters.txt" -Encoding utf8

$componentLines = @(& pktmon list 2>&1)
if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate Packet Monitor components.' }
$componentLines | Out-File -LiteralPath "$prefix-components.txt" -Encoding utf8
$macOctets = [regex]::Matches($normalizedMac, '..') | ForEach-Object { [regex]::Escape($_.Value) }
$adapterMacPattern = '(?i)' + ($macOctets -join '[:-]?')
$matchingLine = @($componentLines | Where-Object {
    $line = $_.ToString()
    $line -match $adapterMacPattern
})
if ($matchingLine.Count -ne 1 -or $matchingLine[0].ToString() -notmatch '^\s*(\d+)\s+') {
    throw 'PktMon did not expose exactly one component matching the adapter description and MAC.'
}
$componentId = [int]$Matches[1]

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PathweaveCaptureDiagnosticNative {
    [DllImport("Ws2_32.dll", SetLastError=true)]
    public static extern int setsockopt(IntPtr socket, int level, int option, byte[] value, int valueLength);
}
'@

function Invoke-UsbTcpHandshake {
    param([string]$SelectorName)
    $socket = [Net.Sockets.Socket]::new([Net.Sockets.AddressFamily]::InterNetwork, [Net.Sockets.SocketType]::Stream, [Net.Sockets.ProtocolType]::Tcp)
    try {
        $indexBytes = [BitConverter]::GetBytes([uint32]$InterfaceIndex)
        if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($indexBytes) }
        $result = [PathweaveCaptureDiagnosticNative]::setsockopt($socket.Handle, 0, 31, $indexBytes, 4)
        if ($result -ne 0) { throw "IP_UNICAST_IF failed with WSA $([Runtime.InteropServices.Marshal]::GetLastWin32Error())." }
        $socket.Bind([Net.IPEndPoint]::new([Net.IPAddress]::Parse($sourceAddress), 0))
        $remote = [Net.IPEndPoint]::new($destination, $DestinationPort)
        $asyncConnect = $socket.BeginConnect($remote, $null, $null)
        if (-not $asyncConnect.AsyncWaitHandle.WaitOne(6000)) { throw "TCP connect timed out on interface $InterfaceIndex." }
        $socket.EndConnect($asyncConnect)
        if (-not $socket.Connected) { throw 'TCP connect did not establish a socket.' }
        $local = $socket.LocalEndPoint.ToString()
        Start-Sleep -Milliseconds 200
        return [pscustomobject]@{ Selector = $SelectorName; Connected = $true; LocalEndpoint = $local; Destination = $remote.ToString() }
    }
    finally { $socket.Dispose() }
}

$selectors = @(@{ Name = "component-$componentId"; Args = @('--comp', "$componentId") },
    @{ Name = 'nics'; Args = @('--comp', 'nics') },
    @{ Name = 'all'; Args = @('--comp', 'all') })
$results = @()
foreach ($selector in $selectors) {
    $etl = "$prefix-$($selector.Name).etl"
    $textPath = "$prefix-$($selector.Name).txt"
    $startPath = "$prefix-$($selector.Name)-start.txt"
    $statusPath = "$prefix-$($selector.Name)-status-running.txt"
    $stopPath = "$prefix-$($selector.Name)-stop.txt"
    $captureActive = $false
    try {
        $status = Get-PktmonStatusText
        if (Test-PktmonRunning -statusText ($status -join "`n")) { throw 'PktMon unexpectedly remained active before this selector.' }
        $startArgs = @('start', '--capture') + $selector.Args + @('--type', 'all', '--pkt-size', '0', '--flags', '0x03f', '--file-name', $etl, '--file-size', '16', '--log-mode', 'circular')
        $startOutput = @(& pktmon @startArgs 2>&1)
        $startCode = $LASTEXITCODE
        $startOutput | Out-File -LiteralPath $startPath -Encoding utf8
        if ($startCode -ne 0) { throw "pktmon start failed for $($selector.Name) ($startCode)." }
        $captureActive = $true
        $runningStatus = Get-PktmonStatusText
        $runningStatus | Set-Content -LiteralPath $statusPath -Encoding utf8

        $handshake = Invoke-UsbTcpHandshake -SelectorName $selector.Name
        $stopOutput = @(& pktmon stop 2>&1)
        $stopCode = $LASTEXITCODE
        $captureActive = $false
        $stopOutput | Out-File -LiteralPath $stopPath -Encoding utf8
        if ($stopCode -ne 0) { throw "pktmon stop failed for $($selector.Name) ($stopCode)." }

        $convertOutput = @(& pktmon etl2txt $etl --out $textPath --verbose 2>&1)
        $convertCode = $LASTEXITCODE
        $convertOutput | Out-File -LiteralPath "$prefix-$($selector.Name)-convert.txt" -Encoding utf8
        if ($convertCode -ne 0) { throw "etl2txt failed for $($selector.Name) ($convertCode)." }
        $stats = @(& pktmon etl2txt $etl --stats 2>&1)
        $stats | Out-File -LiteralPath "$prefix-$($selector.Name)-stats.txt" -Encoding utf8
        $packetLine = $stats | Where-Object { $_.ToString() -match 'Packets total\s*:?\s*(\d+)' } | Select-Object -First 1
        $packetCount = if ($packetLine -and $packetLine.ToString() -match 'Packets total\s*:?\s*(\d+)') { [int64]$Matches[1] } else { 0 }
        $containsDestination = Select-String -LiteralPath $textPath -SimpleMatch $DestinationIPv4 -Quiet
        $results += [pscustomobject]@{
            selector = $selector.Name
            component_id = $componentId
            start_state = $runningStatus.Trim()
            handshake = $handshake
            packet_count = $packetCount
            destination_visible_in_text = [bool]$containsDestination
            etl = $etl
            decoded_text = $textPath
            statistics = "$prefix-$($selector.Name)-stats.txt"
        }
    }
    finally {
        if ($captureActive) { $null = & pktmon stop 2>&1 }
    }
}

$summary = [pscustomobject]@{
    interface = $adapter.Name
    description = $adapter.InterfaceDescription
    interface_index = $InterfaceIndex
    interface_guid = $ExpectedGuid
    interface_mac = $adapter.MacAddress
    source_ipv4 = $sourceAddress
    destination = "$DestinationIPv4`:$DestinationPort"
    filters = $filters -join "`n"
    selectors = $results
}
$summaryPath = "$prefix-summary.json"
$summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath -Encoding utf8
$summary | ConvertTo-Json -Depth 8
Write-Output "SUMMARY=$summaryPath"
