param(
    [Parameter(Mandatory)]
    [int]$TunInterfaceIndex,
    [Parameter(Mandatory)]
    [string]$PhysicalInterfaceIndexes,
    [string]$Destination = '1.1.1.1',
    [string]$TlsHost = 'cloudflare-dns.com',
    [ValidateRange(1, 64)][int]$Requests = 8
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path $PSScriptRoot -Parent
$targetDir = Join-Path $workspace 'target'
$prefix = "$Destination/32"
$etl = Join-Path $targetDir 'm1a-egress.etl'
$txt = Join-Path $targetDir 'm1a-egress.txt'
$results = Join-Path $targetDir 'm1a-live-results.txt'
$admin = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this test script elevated.'
}

Set-Content -LiteralPath $results -Value "Destination=$Destination/443; TUN ifIndex=$TunInterfaceIndex; physical ifIndexes=$($PhysicalInterfaceIndexes -join ',')"
$existingExactRoute = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
if ($existingExactRoute.Count -ne 0) {
    throw "Refusing to replace pre-existing exact route $prefix."
}
$routeAdded = $false
$captureStarted = $false
try {
    New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $TunInterfaceIndex -NextHop '0.0.0.0' -PolicyStore ActiveStore | Out-Null
    $routeAdded = $true
    Add-Content -LiteralPath $results -Value "TEMP_ROUTE=added via interface $TunInterfaceIndex"

    $null = & pktmon start --capture --comp nics --pkt-size 96 --file-name $etl --file-size 32 --log-mode circular 2>&1
    if ($LASTEXITCODE -ne 0) { throw "pktmon start failed with exit code $LASTEXITCODE" }
    $captureStarted = $true
    Add-Content -LiteralPath $results -Value 'PKTMON=started, NICs only, 96-byte packet prefix'

    $elapsed = [System.Diagnostics.Stopwatch]::StartNew()
    $jobs = for ($request = 1; $request -le $Requests; $request++) {
        Start-Job -ArgumentList $request, $TlsHost, $Destination -ScriptBlock {
            param($requestId, $tlsName, $ipAddress)
            $output = & curl.exe --noproxy '*' --http1.1 --no-keepalive --connect-timeout 12 --max-time 20 `
                --resolve "$tlsName`:443`:$ipAddress" -H 'accept: application/dns-json' `
                -sS -o NUL -w 'http=%{http_code} remote=%{remote_ip} local=%{local_ip} seconds=%{time_total} bytes=%{size_download}' `
                "https://$tlsName/dns-query?name=example.com&type=A" 2>&1
            [pscustomobject]@{ Request = $requestId; ExitCode = $LASTEXITCODE; Output = ($output -join ' ') }
        }
    }
    $requestResults = Receive-Job -Job $jobs -Wait
    $elapsed.Stop()
    $requestResults | Sort-Object Request | ForEach-Object {
        Add-Content -LiteralPath $results -Value "CURL=$($_.Request) exit=$($_.ExitCode) $($_.Output)"
    }
    Remove-Job -Job $jobs -Force
    Add-Content -LiteralPath $results -Value "PARALLEL_REQUESTS=$Requests elapsed_ms=$($elapsed.ElapsedMilliseconds)"
    if (@($requestResults | Where-Object { $_.ExitCode -ne 0 -or $_.Output -notmatch 'http=200' }).Count -ne 0) {
        throw 'One or more parallel TCP requests failed.'
    }
}
finally {
    if ($captureStarted) {
        $null = & pktmon stop 2>&1
        Add-Content -LiteralPath $results -Value "PKTMON_STOP_EXIT=$LASTEXITCODE"
    }
    if ($routeAdded) {
        Remove-NetRoute -DestinationPrefix $prefix -InterfaceIndex $TunInterfaceIndex -PolicyStore ActiveStore -Confirm:$false
        Add-Content -LiteralPath $results -Value 'TEMP_ROUTE=removed'
    }
}

if (Test-Path -LiteralPath $etl) {
    $null = & pktmon etl2txt $etl --out $txt --brief 2>&1
    Add-Content -LiteralPath $results -Value "ETL2TXT_EXIT=$LASTEXITCODE"
    if (Test-Path -LiteralPath $txt) {
        Select-String -LiteralPath $txt -Pattern ([regex]::Escape($Destination)) | ForEach-Object {
            Add-Content -LiteralPath $results -Value "PACKET=$($_.Line)"
        }
    }
}
$remaining = @(Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue)
if ($remaining.Count -ne 0) { throw "Temporary route cleanup failed; $prefix is still present." }
Add-Content -LiteralPath $results -Value 'ROUTE_CLEANUP=verified'



