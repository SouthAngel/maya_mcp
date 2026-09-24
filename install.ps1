# maya_mcp installer (PowerShell)
#
# 1. Build Rust MCP server (cargo build --release if binary missing)
# 2. Register MCP server to clients:
#    - Trae (~/.trae-cn/mcp.json or ~/.trae/mcp.json)
#    - CodeBuddy (~/.codebuddy/mcp.json)
#    - OpenCode (~/.config/opencode/opencode.json)
#    - Codex (~/.codex/config.toml)
# 3. Register Maya listener to per-version userSetup.py (Documents\maya\<ver>\scripts)
#
# Usage: powershell -ExecutionPolicy Bypass -File install.ps1
# Idempotent; timestamped backup before every write.

$ErrorActionPreference = 'Stop'

$ProjectRoot = $PSScriptRoot
$ExePath = Join-Path $ProjectRoot 'target\release\maya_mcp.exe'
$ListenerPath = Join-Path $ProjectRoot 'maya\maya_mcp_listener.py'
$McpName = 'maya_mcp'

function Log($msg) { Write-Host $msg }

function Backup-File($path) {
    if (Test-Path $path) {
        $bak = "$path.bak-$(Get-Date -Format 'yyyyMMddHHmmss')"
        Copy-Item $path $bak
        Log "[backup] $path -> $bak"
    }
}

function Write-Utf8NoBom($path, $text) {
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

# --- 1. build ---
if (-not (Test-Path $ExePath)) {
    Log "[build] binary missing, running cargo build --release ..."
    Push-Location $ProjectRoot
    try { cargo build --release } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $ExePath)) {
        throw "[build] cargo build failed; build it manually and re-run"
    }
} else {
    Log "[build] server binary exists: $ExePath"
}

# --- 2a. register to mcpServers-style JSON clients (Trae / CodeBuddy) ---
function Add-StdMcpJson($path, $label) {
    $created = -not (Test-Path $path)
    $config = $null
    if (-not $created) {
        try { $config = Get-Content $path -Raw -Encoding UTF8 | ConvertFrom-Json }
        catch {
            Log "[warn] $path is not valid JSON ($($_.Exception.Message)); backup then recreate"
            Backup-File $path
            $config = $null
        }
    }
    if ($null -eq $config) {
        $config = [ordered]@{ mcpServers = [ordered]@{} }
    } elseif (-not $config.PSObject.Properties['mcpServers']) {
        $config | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([ordered]@{})
    }
    $entry = [ordered]@{ command = $ExePath; args = @(); env = [ordered]@{} }
    $servers = $config.mcpServers
    if ($servers.PSObject.Properties[$McpName]) { $servers.$McpName = $entry }
    else { $servers | Add-Member -NotePropertyName $McpName -NotePropertyValue $entry }

    Backup-File $path
    $dir = Split-Path $path -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    # collapse empty arrays/objects emitted as multi-line noise by PS5.1 ConvertTo-Json
    $json = ($config | ConvertTo-Json -Depth 10) -replace '\[\s+\]', '[]' -replace '\{\s+\}', '{}'
    Write-Utf8NoBom $path ($json + "`n")
    $state = if ($created) { 'created' } else { 'updated' }
    Log "[$label] $state $path (server: $McpName)"
}

$stdTargets = @(
    @{ path = "$env:USERPROFILE\.trae-cn\mcp.json"; label = 'trae' },
    @{ path = "$env:USERPROFILE\.trae\mcp.json"; label = 'trae' },
    @{ path = "$env:USERPROFILE\.codebuddy\mcp.json"; label = 'codebuddy' }
)
foreach ($t in $stdTargets) { Add-StdMcpJson $t.path $t.label }

# --- 2b. register to OpenCode (~/.config/opencode/opencode.json) ---
function Add-OpenCodeMcp($path) {
    $created = -not (Test-Path $path)
    $config = $null
    if (-not $created) {
        try { $config = Get-Content $path -Raw -Encoding UTF8 | ConvertFrom-Json }
        catch {
            Log "[warn] $path is not valid JSON ($($_.Exception.Message)); backup then recreate"
            Backup-File $path
            $config = $null
        }
    }
    if ($null -eq $config) {
        $config = [ordered]@{ }
    }
    if (-not $config.PSObject.Properties['mcp']) {
        $config | Add-Member -NotePropertyName mcp -NotePropertyValue ([ordered]@{})
    }
    $entry = [ordered]@{
        type = 'local'
        command = @($ExePath)
        enabled = $true
        environment = [ordered]@{}
    }
    $mcp = $config.mcp
    if ($mcp.PSObject.Properties[$McpName]) { $mcp.$McpName = $entry }
    else { $mcp | Add-Member -NotePropertyName $McpName -NotePropertyValue $entry }

    Backup-File $path
    $dir = Split-Path $path -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $json = ($config | ConvertTo-Json -Depth 10) -replace '\[\s+\]', '[]' -replace '\{\s+\}', '{}'
    Write-Utf8NoBom $path ($json + "`n")
    $state = if ($created) { 'created' } else { 'updated' }
    Log "[opencode] $state $path (server: $McpName)"
}

# prefer existing opencode.json/jsonc; else create opencode.json
$ocJson = "$env:USERPROFILE\.config\opencode\opencode.json"
$ocJsonc = "$env:USERPROFILE\.config\opencode\opencode.jsonc"
$ocPath = if (Test-Path $ocJsonc) { $ocJsonc } else { $ocJson }
Add-OpenCodeMcp $ocPath

# --- 2c. register to Codex (~/.codex/config.toml) ---
function Add-CodexToml($path) {
    $text = ''
    if (Test-Path $path) { $text = [System.IO.File]::ReadAllText($path) }
    # TOML basic string: escape backslashes (String.Replace avoids regex substitution semantics)
    $cmd = $ExePath.Replace('\', '\\')
    $block = "[mcp_servers.$McpName]`ncommand = `"$cmd`"`nargs = []`n"
    if ($text -match '(?m)^\[mcp_servers\.' + [regex]::Escape($McpName) + '\]') {
        $pattern = '(?ms)^\[mcp_servers\.' + [regex]::Escape($McpName) + '\].*?(?=^\[|\z)'
        $new = [regex]::Replace($text, $pattern, $block)
        if ($new -eq $text) { Log "[codex] skipped $path (already registered)"; return }
        Backup-File $path
        Write-Utf8NoBom $path $new
        Log "[codex] updated $path (server: $McpName)"
    } else {
        $sep = if ($text -and -not $text.EndsWith("`n")) { "`n`n" } else { '' }
        Backup-File $path
        Write-Utf8NoBom $path ($text + $sep + $block)
        Log "[codex] appended $path (server: $McpName)"
    }
}

Add-CodexToml "$env:USERPROFILE\.codex\config.toml"

# --- 3. register to Maya userSetup ---
$listenerBlock = @"

# >>> maya_mcp (managed block - auto-generated by maya_mcp/install.ps1) >>>
try:
    exec(compile(open(r"$ListenerPath", "rb").read(), r"$ListenerPath", "exec"))
except Exception as _e:
    print("[maya-mcp] listener start failed: %s" % _e)
# <<< maya_mcp (managed block) <<<
"@

function Add-ListenerBlock($userSetup) {
    $dir = Split-Path $userSetup -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    if (Test-Path $userSetup) {
        $existing = [System.IO.File]::ReadAllText($userSetup)
        if ($existing.Contains('maya_mcp (managed block)')) { return 'skipped' }
    }
    Backup-File $userSetup
    [System.IO.File]::AppendAllText($userSetup, $listenerBlock)
    return 'appended'
}

$mayaRoot = if ($env:MAYA_APP_DIR) { $env:MAYA_APP_DIR } else { Join-Path $env:USERPROFILE 'Documents\maya' }
if (-not (Test-Path $mayaRoot)) { throw "[maya] Maya home dir not found: $mayaRoot" }

# Shared scripts dir may be ACL-protected (DENY) on some setups -> fall back to per-version dirs
$shared = Join-Path $mayaRoot 'scripts'
try {
    $result = Add-ListenerBlock (Join-Path $shared 'userSetup.py')
    Log "[maya] $(Join-Path $shared 'userSetup.py'): $result"
} catch [System.UnauthorizedAccessException] {
    Log "[maya] shared scripts dir is write-protected (DENY ACL); using per-version dirs"
}

$versions = Get-ChildItem $mayaRoot -Directory |
    Where-Object { $_.Name -match '^20\d\d$' } |
    Sort-Object Name
foreach ($v in $versions) {
    $us = Join-Path $v.FullName 'scripts\userSetup.py'
    Log "[maya] $us : $(Add-ListenerBlock $us)"
}

Log "=== done ==="
Log "next steps:"
Log "  1. restart Maya -> listener starts automatically (see [maya-mcp] banner)"
Log "  2. restart/reload your MCP client (Trae / CodeBuddy / OpenCode / Codex)"
Log "  3. note: if multiple Maya versions run simultaneously, only the first binds port 5055"
