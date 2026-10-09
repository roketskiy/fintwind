# Deploy the fintwind-pwa build to the UCloud box: build, pack, upload,
# extract into /var/www/pwa, and verify over the public endpoint.
# The PWA is served by Caddy from the relay origin itself (same-origin
# deployment; see deploy/relay/Caddyfile).
# ASCII only: [OK] [X] markers.

$ErrorActionPreference = 'Stop'

# Historical same-origin publication is no longer a supported operation.
# Fail before building, uploading, or deleting the live site's existing files.
throw 'Same-origin PWA deployment is retired. Use prepare-pages.ps1 and the independent fintwind-remote Pages project.'

$pwa = 'E:\work\Opencode-Desktop\fintwind-pwa'
$pack = 'C:\Users\Public\Temp\opencode\fintwind-pwa.tar.gz'
$key = 'C:\Users\潘雷\.ssh\ucloud_root_ed25519'
$sshArgs = @('-i', $key, '-o', 'ConnectTimeout=10', '-o', 'StrictHostKeyChecking=accept-new', 'root@165.154.64.159')

function Invoke-Remote([string]$Command) {
    & ssh @sshArgs $Command
    if ($LASTEXITCODE -ne 0) { throw "remote command failed: $Command" }
}

Write-Host '[1/5] building PWA...'
Push-Location $pwa
try {
    # bun prints the script line to stderr; route the whole build through
    # cmd so PowerShell never turns native stderr into a terminating error.
    & cmd /c "bun run build >nul 2>&1"
    if ($LASTEXITCODE -ne 0) { throw 'bun run build failed' }
} finally {
    Pop-Location
}

Write-Host '[2/5] packing...'
if (Test-Path $pack) { Remove-Item $pack -Force }
# GNU tar (Git's) reads "C:\..." as host:path; bsdtar ships with Windows
# and handles drive letters natively.
& C:\Windows\System32\tar.exe -czf $pack -C (Join-Path $pwa 'dist') .
if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
$size = [math]::Round((Get-Item $pack).Length / 1KB, 0)
Write-Host "      packed $size KB"

Write-Host '[3/5] uploading...'
$scpArgs = @('-i', $key, '-o', 'ConnectTimeout=10', '-o', 'StrictHostKeyChecking=accept-new')
& scp @scpArgs $pack 'root@165.154.64.159:/tmp/fintwind-pwa.tar.gz'
if ($LASTEXITCODE -ne 0) { throw 'scp failed' }

Write-Host '[4/5] extracting to /var/www/pwa...'
Invoke-Remote 'mkdir -p /var/www/pwa && rm -rf /var/www/pwa/* && tar -xzf /tmp/fintwind-pwa.tar.gz -C /var/www/pwa && rm /tmp/fintwind-pwa.tar.gz && ls /var/www/pwa | head -5'

Write-Host '[5/5] verifying public endpoint...'
$code = & C:\Windows\System32\curl.exe -s -o NUL -w '%{http_code}' https://relay.fintwind.xyz/
if ($code -ne '200') { throw "public verification failed: HTTP $code" }
Write-Host "[OK] PWA served at https://relay.fintwind.xyz/"
