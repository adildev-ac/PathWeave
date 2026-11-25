param(
    [string]$ServerAddress = '127.0.0.1:47800'
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path $PSScriptRoot -Parent
$target = Join-Path $workspace 'target'
$server = Join-Path $target 'debug\netmerge-test-server.exe'
$client = Join-Path $target 'debug\netmerge-test.exe'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$serverLog = Join-Path $target "local-test-server-$stamp.log"
$serverError = Join-Path $target "local-test-server-error-$stamp.log"
$results = Join-Path $target "local-protocol-results-$stamp"
$hostPart, $portPart = $ServerAddress.Split(':')
if ($hostPart -ne '127.0.0.1' -or [int]$portPart -lt 1 -or [int]$portPart -gt 65535) {
    throw 'The local integration test binds only to 127.0.0.1 and a valid port.'
}
if (-not (Test-Path -LiteralPath $server) -or -not (Test-Path -LiteralPath $client)) {
    throw 'Build the binaries first with cargo build --bins.'
}
New-Item -ItemType Directory -Path $results | Out-Null
$script:testCounter = 0

$token = ((1..4 | ForEach-Object { [guid]::NewGuid().ToString('N') }) -join '')
$env:NETMERGE_TEST_TOKEN = $token
$process = $null
try {
    $process = Start-Process -FilePath $server -WorkingDirectory $workspace -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $serverLog -RedirectStandardError $serverError `
        -ArgumentList @('--listen', $ServerAddress, '--max-connections', '64', '--max-bytes', '1048576', '--max-session-secs', '30')
    $ready = $false
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline -and -not $ready) {
        if ($process.HasExited) { throw "Test server exited early. See $serverError" }
        $probe = [Net.Sockets.TcpClient]::new()
        try { $probe.Connect($hostPart, [int]$portPart); $ready = $true } catch { Start-Sleep -Milliseconds 100 } finally { $probe.Dispose() }
    }
    if (-not $ready) { throw 'Local test server did not start listening within 10 seconds.' }

    function Invoke-LocalMode([string]$Mode, [string[]]$ExtraArgs) {
        $script:testCounter++
        $output = & $client --server $ServerAddress --mode $Mode @ExtraArgs
        if ($LASTEXITCODE -ne 0) { throw "Client mode '$Mode' failed with exit code $LASTEXITCODE." }
        $outFile = Join-Path $results ("{0}-{1:D2}.json" -f $Mode, $script:testCounter)
        $output | Set-Content -LiteralPath $outFile -Encoding UTF8
        "PASS $Mode"
    }

    Invoke-LocalMode 'download' @('--bytes','131072','--max-bytes','131072') | Out-Host
    Invoke-LocalMode 'upload' @('--bytes','131072','--max-bytes','131072') | Out-Host
    Invoke-LocalMode 'duplex' @('--bytes','65536','--max-bytes','131072') | Out-Host
    Invoke-LocalMode 'half-close' @('--bytes','131072','--max-bytes','262144') | Out-Host
    Invoke-LocalMode 'duplex' @('--bytes','32768','--max-bytes','262144','--connections','4','--concurrency','4') | Out-Host
    Invoke-LocalMode 'duplex' @('--bytes','512','--max-bytes','65536','--connections','50','--concurrency','50') | Out-Host
    Invoke-LocalMode 'duplex' @('--duration-secs','2','--max-bytes','2097152','--rate-bytes-per-sec','131072') | Out-Host
    Invoke-LocalMode 'duplex' @('--bytes','65536','--max-bytes','131072','--receive-rate-bytes-per-sec','32768') | Out-Host
    Invoke-LocalMode 'upload' @('--bytes','1024','--max-bytes','1024000','--connections','1000','--concurrency','1') | Out-Host

    $null = & $client --server $ServerAddress --mode download --bytes 524288 --max-bytes 262144 2>&1
    if ($LASTEXITCODE -eq 0) { throw 'Client accepted a fixed transfer that exceeded its explicit total byte budget.' }
    'PASS byte-budget rejection' | Out-Host

    $null = & $client --server $ServerAddress --mode download --bytes 2097152 --max-bytes 2097152 2>&1
    if ($LASTEXITCODE -eq 0) { throw 'Server accepted a transfer exceeding its configured per-connection cap.' }
    'PASS server transfer cap' | Out-Host

    $savedToken = $env:NETMERGE_TEST_TOKEN
    try {
        $env:NETMERGE_TEST_TOKEN = 'invalid-token-that-is-long-enough-000000'
        $null = & $client --server $ServerAddress --mode download --bytes 1024 --max-bytes 1024 2>&1
        if ($LASTEXITCODE -eq 0) { throw 'Server accepted an invalid authentication token.' }
        'PASS authentication rejection' | Out-Host
    }
    finally {
        $env:NETMERGE_TEST_TOKEN = $savedToken
    }
}
finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit()
    }
    Remove-Item Env:\NETMERGE_TEST_TOKEN -ErrorAction SilentlyContinue
}

if ($process -and -not (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
    'SERVER_CLEANUP=verified' | Set-Content -LiteralPath (Join-Path $results 'cleanup.txt')
}
