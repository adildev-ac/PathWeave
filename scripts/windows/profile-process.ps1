param(
    [Parameter(Mandatory)][int]$ProcessId,
    [Parameter(Mandatory)][string]$OutputPath,
    [ValidateRange(1,60)][int]$IntervalSeconds = 1
)

$ErrorActionPreference = 'Stop'
$rows = [Collections.Generic.List[object]]::new()
while ($true) {
    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if (-not $process) { break }
    $tcpCount = @(Get-NetTCPConnection -OwningProcess $ProcessId -ErrorAction SilentlyContinue).Count
    $rows.Add([pscustomobject]@{
        timestamp_utc = [DateTime]::UtcNow.ToString('o')
        process_id = $ProcessId
        cpu_seconds = $process.TotalProcessorTime.TotalSeconds
        working_set_bytes = $process.WorkingSet64
        private_bytes = $process.PrivateMemorySize64
        peak_working_set_bytes = $process.PeakWorkingSet64
        handle_count = $process.HandleCount
        thread_count = $process.Threads.Count
        tcp_socket_count = $tcpCount
    })
    Start-Sleep -Seconds $IntervalSeconds
}
$rows | Export-Csv -LiteralPath $OutputPath -NoTypeInformation -Encoding UTF8
