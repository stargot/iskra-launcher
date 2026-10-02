# RAM acceptance: private WS of iskra.exe process tree (iskra + descendants).
# 3 samples, 3s interval, after 10s idle. Does NOT kill the app. ASCII only (PS5.1).
$ErrorActionPreference = 'Stop'

$log = Join-Path $env:APPDATA 'iskra\logs\runtime.log'
$logLen = 0
if (Test-Path $log) { $logLen = (Get-Item $log).Length }
$sw = [System.Diagnostics.Stopwatch]::StartNew()

$exe = Join-Path $PSScriptRoot '..\target\release\iskra.exe'
Start-Process -FilePath $exe
Write-Output ("launched at {0:s}" -f (Get-Date))

$ready = $false
while ($sw.Elapsed.TotalSeconds -lt 40) {
    Start-Sleep -Milliseconds 1500
    if (Test-Path $log) {
        $fs = [System.IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
        try {
            if ($fs.Length -gt $logLen) {
                $fs.Seek($logLen, 'Begin') | Out-Null
                $sr = New-Object System.IO.StreamReader($fs)
                $txt = $sr.ReadToEnd()
                if ($txt -match 'setup done' -or $txt -match 'indexing.*done' -or $txt -match 'Indexed') {
                    Write-Output ("log-ready marker after {0:n1}s" -f $sw.Elapsed.TotalSeconds)
                    $ready = $true
                    break
                }
            }
        } finally { $fs.Close() }
    }
}
if (-not $ready) { Write-Output ("no explicit marker in 40s, proceeding (elapsed {0:n1}s)" -f $sw.Elapsed.TotalSeconds) }

Start-Sleep -Seconds 10
Write-Output ("idle done, total elapsed {0:n1}s" -f $sw.Elapsed.TotalSeconds)

function Get-Tree {
    $all = Get-CimInstance Win32_Process
    $roots = @($all | Where-Object { $_.Name -like 'iskra*' })
    $ids = @{}
    foreach ($r in $roots) { $ids[[uint32]$r.ProcessId] = $true }
    $changed = $true
    while ($changed) {
        $changed = $false
        foreach ($p in $all) {
            $pp = [uint32]$p.ParentProcessId
            $pidI = [uint32]$p.ProcessId
            if (-not $ids.ContainsKey($pidI) -and $ids.ContainsKey($pp)) {
                $ids[$pidI] = $true; $changed = $true
            }
        }
    }
    $perf = Get-CimInstance -ClassName Win32_PerfFormattedData_PerfProc_Process |
        Where-Object { $ids.ContainsKey([uint32]$_.IDProcess) }
    $sum = ($perf | Measure-Object -Property WorkingSetPrivate -Sum).Sum
    $sumWS = ($perf | Measure-Object -Property WorkingSet -Sum).Sum
    $names = ($perf | Group-Object Name | ForEach-Object { "{0}x{1}" -f $_.Count, $_.Name }) -join ', '
    [pscustomobject]@{ PrivateWS_MB = [math]::Round($sum/1MB, 2); WS_MB = [math]::Round($sumWS/1MB, 2); Procs = $names }
}

for ($i = 1; $i -le 3; $i++) {
    $t = Get-Tree
    Write-Output ("sample {0}: privateWS={1} MB  WS={2} MB  [{3}]" -f $i, $t.PrivateWS_MB, $t.WS_MB, $t.Procs)
    if ($i -lt 3) { Start-Sleep -Seconds 3 }
}
Write-Output ("done at {0:s}, app left RUNNING" -f (Get-Date))
