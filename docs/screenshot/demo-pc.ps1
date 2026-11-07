# Puts fake hosts in shcp for the README screenshot, and puts everything back. Close shcp first.
#   .\demo-pc.ps1 -Server ubuntu@203.0.113.10 [-Port 22] [-Identity C:\path\to\key] [-ProxyCommand "..."]
#   .\demo-pc.ps1 -Restore
# Your settings, hosts and tabs move aside, and so does ~\.ssh\config, whose hosts would show too.
# shcp then opens on shop-web-1 with the terminal next to its files, and in the terminal it runs
# /tmp/demo-server.sh, which has to be on the server already.
param(
    [string]$Server,
    [int]$Port = 22,
    [string]$Identity,
    [string]$ProxyCommand,
    [switch]$Restore
)
$ErrorActionPreference = 'Stop'

$config = Join-Path $env:APPDATA 'shcp\shcp\config'
$saved = "$config.before-screenshot"
$sshConfig = Join-Path $HOME '.ssh\config'
$sshSaved = "$sshConfig.before-screenshot"

if (Get-Process shcp -ErrorAction SilentlyContinue) {
    throw 'Close shcp first.'
}

if ($Restore) {
    if (-not (Test-Path $saved)) {
        throw 'Nothing to restore.'
    }
    if (-not (Test-Path (Join-Path $config '.shcp-demo'))) {
        throw "$config is not the demo one: left as it is."
    }
    Remove-Item -Recurse -Force $config
    Rename-Item $saved $config
    if (Test-Path $sshSaved) {
        Rename-Item $sshSaved $sshConfig
    }
    Write-Output 'Your settings, hosts, tabs and ~\.ssh\config are back.'
    return
}

if ($Server -notmatch '^[^@\s]+@[^@\s]+$') {
    throw 'Say which server to use: -Server user@address'
}
if (Test-Path $saved) {
    throw "A demo is already set up. Run -Restore first, or look in $saved."
}
$user, $address = $Server -split '@'

if (Test-Path $config) {
    Rename-Item $config $saved
}
if (Test-Path $sshConfig) {
    Rename-Item $sshConfig $sshSaved
}
New-Item -ItemType Directory -Force $config | Out-Null
New-Item -ItemType File (Join-Path $config '.shcp-demo') | Out-Null

function Host-Entry($alias, $hostname, $hostUser, $hostPort, $key, $proxy) {
    $entry = "[[hosts]]`nalias = `"$alias`"`nhostname = `"$hostname`"`nport = $hostPort`nuser = `"$hostUser`"`n"
    if ($key) {
        $entry += "identity_files = ['$key']`n"
    }
    if ($proxy) {
        $entry += "proxy_command = '$proxy'`n"
    }
    return "$entry`n"
}

# Only the first one connects, to the real server. The others are never opened.
$hosts = Host-Entry 'shop-web-1' $address $user $Port $Identity $ProxyCommand
foreach ($demo in @(
        @('shop-web-2', '10.20.0.12', 'deploy'),
        @('gateway', '10.20.0.5', 'deploy'),
        @('postgres-main', '10.20.1.4', 'postgres'),
        @('redis', '10.20.1.9', 'deploy'),
        @('jump', 'jump.example.com', 'ops'),
        @('staging', 'staging.example.com', 'deploy'),
        @('nas', '192.168.1.20', 'admin')
    )) {
    $hosts += Host-Entry $demo[0] $demo[1] $demo[2] 22 $null $null
}

# Keeping terminals is off on the real server: its running terminals would open by themselves.
$settings = @'
plain_terminal_hosts = ["shop-web-1"]
host_order = ["jump", "staging", "nas", "postgres-main", "redis", "gateway", "shop-web-1", "shop-web-2"]
group_order = ["Data", "Production"]

[startup_commands]
shop-web-1 = "bash /tmp/demo-server.sh"

[host_groups]
postgres-main = "Data"
redis = "Data"
gateway = "Production"
shop-web-1 = "Production"
shop-web-2 = "Production"

[host_appearance.shop-web-1]
color = "blue"

[host_appearance.shop-web-2]
color = "blue"

[host_appearance.postgres-main]
color = "teal"

[host_appearance.redis]
color = "red"

[host_appearance.staging]
color = "amber"
'@

$tabs = @'
active = 0

[[tabs]]
kind = "terminal"
host = "shop-web-1"
both = true
ratio = 0.58

[tabs.layout]
pane = "terminal"
'@

# Without a byte order mark, which Windows PowerShell would add and a TOML reader may refuse.
[IO.File]::WriteAllText((Join-Path $config 'hosts.toml'), $hosts)
[IO.File]::WriteAllText((Join-Path $config 'settings.toml'), $settings)
[IO.File]::WriteAllText((Join-Path $config 'tabs.toml'), $tabs)
Write-Output 'Ready. Open shcp, take the screenshot, close shcp, run -Restore.'
