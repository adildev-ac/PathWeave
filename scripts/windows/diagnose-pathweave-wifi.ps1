param(
    [string]$AdapterAlias = 'Wi-Fi',
    [string]$DestinationIPv4 = '172.66.147.243',
    [string]$HostName = 'example.com',
    [ValidateRange(1024, 16384)][int]$MaxResponseBytes = 8192,
    [ValidateRange(1, 60)][int]$ConnectTimeoutSecs = 12
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$engine = Join-Path $workspace 'target\release\pathweave-engine.exe'
$test = Join-Path $PSScriptRoot 'test-pathweave-interface.ps1'
if (-not (Test-Path -LiteralPath $engine)) { throw 'Build the release engine before running this diagnostic.' }

$usbAdapters = @(Get-NetAdapter | Where-Object {
    $_.Status -eq 'Up' -and $_.InterfaceDescription -match '(?i)Remote NDIS|USB.*(Ethernet|Network)'
})
if ($usbAdapters.Count -gt 0) {
    throw "USB tethering must remain disconnected for this Wi-Fi-only diagnostic. Active USB/Remote-NDIS adapter(s): $($usbAdapters.Name -join ', ')."
}

$adapter = Get-NetAdapter -Name $AdapterAlias -ErrorAction Stop
if ($adapter.Status -ne 'Up') { throw "Adapter '$AdapterAlias' is not operational." }
$telemetryJson = (& $engine --telemetry-once | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Pathweave could not read the Windows interface table.' }
$telemetry = ConvertFrom-Json -InputObject $telemetryJson
$expectedGuid = $adapter.InterfaceGuid.ToString().Trim('{}').ToLowerInvariant()
$identity = @($telemetry | Where-Object { $_.stable_guid -eq $expectedGuid })
if ($identity.Count -ne 1 -or -not $identity[0].operational) { throw "Could not uniquely resolve an operational Pathweave identity for '$AdapterAlias' GUID $expectedGuid." }
$identity = $identity[0]
if ([uint32]$identity.index -ne [uint32]$adapter.ifIndex) { throw 'Adapter index changed while resolving the current Wi-Fi identity; rerun the diagnostic.' }

Write-Output ("Resolved Wi-Fi: alias={0}; GUID={1}; LUID={2}; ifIndex={3}; IPv4={4}; gateways={5}" -f `
    $adapter.Name, $identity.stable_guid, $identity.luid, $identity.index, ($identity.ipv4_addresses -join ','), ($identity.default_gateways -join ','))

& $test `
    -InterfaceIndex ([uint32]$identity.index) `
    -ExpectedGuid $identity.stable_guid `
    -DestinationIPv4 $DestinationIPv4 `
    -HostName $HostName `
    -MaxResponseBytes $MaxResponseBytes `
    -CaptureBackend NetshTrace `
    -MaxConnections 1 `
    -ConnectTimeoutSecs $ConnectTimeoutSecs
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
