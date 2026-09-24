param([Parameter(Mandatory = $true)][string]$ReferenceRoot)

# Read-only inventory. A scanned file is NOT a manually reviewed file.
$ErrorActionPreference = 'Stop'
$auditRoot = (Resolve-Path -LiteralPath $ReferenceRoot).Path
$tracked = @(& git -C $auditRoot -c core.quotepath=false ls-files)
if ($LASTEXITCODE -ne 0) { throw 'Cannot enumerate reference checkout' }
$revision = (& git -C $auditRoot rev-parse HEAD).Trim()
$textExtensions = @('.rs', '.py', '.sh', '.js', '.css', '.html', '.md', '.json', '.toml', '.yml', '.yaml', '.txt', '.license', '.srcinfo', '.arch', '.gnu', '.rb')
$sourceExtensions = @('.rs', '.py', '.sh', '.js', '.css', '.html', '.rb')
$scriptRoots = @(
    'src/personas/default/scripts/',
    'src/personas/',
    'src/skills/'
)
$signals = [ordered]@{
    process = '\b(Command::new|subprocess\.|os\.system|exec\(|spawn\()'
    destructive = '\b(remove_dir_all|remove_file|unlink|rmtree|DELETE FROM|DROP TABLE)\b'
    network = '\b(reqwest|httpx|requests\.|urllib|fetch\(|TcpListener|WebSocket)'
    database = '\b(rusqlite|sqlite3|CREATE TABLE|BEGIN TRANSACTION)\b'
    concurrency = '\b(tokio::spawn|thread::spawn|Mutex|RwLock|AtomicBool|broadcast::|mpsc::)'
    unsafe = '\bunsafe\b'
    resource = '(include_str!|include_bytes!|env!|MIYU_WORKSPACE_ROOT)'
}
$rows = foreach ($relative in $tracked) {
    $path = Join-Path $auditRoot $relative
    $item = Get-Item -LiteralPath $path
    $extension = $item.Extension.ToLowerInvariant()
    $knownScriptPath = $scriptRoots | Where-Object { $relative.StartsWith($_, [System.StringComparison]::OrdinalIgnoreCase) }
    $textFile = $extension -in $textExtensions -or $item.Name -in @('Cargo.lock', 'PKGBUILD', 'LICENSE', 'Dockerfile', 'build.rs') -or $knownScriptPath
    $body = if ($textFile) { [IO.File]::ReadAllText($path) } else { '' }
    $hasShebang = $body -match '^#!'
    $group = if ($relative -match '^crates/([^/]+)/src/([^/.]+)') { "$($Matches[1])/$($Matches[2])" }
        elseif ($relative -match '^src/([^/.]+)') { "entry/$($Matches[1])" }
        elseif ($relative -match '^([^/]+)/') { $Matches[1] } else { 'root' }
    $found = @($signals.Keys | Where-Object { $body -match $signals[$_] })
    $imports = @([regex]::Matches($body, '\bmiyu_(?:base|core|engine|hosts)::([A-Za-z_][A-Za-z0-9_]*)') | ForEach-Object { $_.Value.TrimEnd(':') } | Sort-Object -Unique)
    [pscustomobject][ordered]@{
        path = $relative
        group = $group
        kind = if ($extension -in $sourceExtensions -or $hasShebang) { 'source' } elseif ($textFile) { 'text-resource' } else { 'binary-or-other' }
        lines = if ($body.Length) { [regex]::Matches($body, '\n').Count + [int](!$body.EndsWith("`n")) } else { 0 }
        bytes = $item.Length
        scan = if ($textFile) { 'content-scanned-not-full-reviewed' } else { 'catalogued-only' }
        signals = $found -join '|'
        imports = $imports -join '|'
    }
}
[pscustomobject]@{ revision = $revision; count = $rows.Count; files = @($rows) } | ConvertTo-Json -Depth 4 -Compress
