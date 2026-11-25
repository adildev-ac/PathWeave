param(
    [Parameter(Mandatory)][ValidateSet('GET','SET')][string]$Command,
    [int]$InterfaceIndex = 0,
    [ValidateSet('healthy','degraded','unavailable','recovering')][string]$Status,
    [string]$ControlAddress = '127.0.0.1:47832'
)

$ErrorActionPreference = 'Stop'
$parts = $ControlAddress.Split(':')
if ($parts.Count -ne 2 -or $parts[0] -ne '127.0.0.1') { throw 'Health control must use the configured IPv4 loopback endpoint.' }
if ($Command -eq 'SET' -and $InterfaceIndex -le 0) { throw 'SET requires a positive -InterfaceIndex.' }
if ($Command -eq 'SET' -and -not $Status) { throw 'SET requires -Status.' }
$client = [Net.Sockets.TcpClient]::new()
try {
    $client.Connect($parts[0], [int]$parts[1])
    $stream = $client.GetStream()
    $commandLine = if ($Command -eq 'GET') { "GET`n" } else { "SET $InterfaceIndex $Status`n" }
    $request = [Text.Encoding]::ASCII.GetBytes($commandLine)
    $stream.Write($request, 0, $request.Length)
    $stream.Flush()
    $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $false, 1024, $true)
    while ($null -ne ($line = $reader.ReadLine())) { Write-Output $line }
}
finally {
    $client.Dispose()
}
