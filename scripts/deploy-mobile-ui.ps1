param(
    [Parameter(Mandatory = $true)]
    [string]$ConfigPath,
    [ValidatePattern('^[a-f0-9]{64}$')]
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSiteHash,
    [switch]$CheckOnly
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$resolvedConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
$repoPrefix = [IO.Path]::GetFullPath($repo).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if ($resolvedConfig.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Keep deployment configuration outside the repository.' }
$config = Get-Content -LiteralPath $resolvedConfig -Raw | ConvertFrom-Json
if (-not $config) { throw 'Deployment configuration is empty.' }
foreach ($name in @('SshTarget', 'IdentityFile', 'Domain', 'SitePath')) {
    if (-not ($config.PSObject.Properties.Name -contains $name)) { throw ('Missing deployment setting: ' + $name) }
    if ($config.$name -isnot [string] -or [string]::IsNullOrWhiteSpace($config.$name)) { throw ('Invalid deployment setting: ' + $name) }
}
$target = $config.SshTarget
$key = $config.IdentityFile
$domain = $config.Domain
$site = $config.SitePath
if ($target -notmatch '^[A-Za-z0-9_.-]+@[A-Za-z0-9_.:-]+$') { throw 'Invalid SSH target.' }
if ($domain.Length -gt 253 -or $domain -notmatch '^(?=.{1,253}$)(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,63}$') { throw 'Invalid deployment domain.' }
if ($site -notmatch '^/etc/caddy/[A-Za-z0-9_.-]+\.caddy$') { throw 'Invalid Caddy site path.' }
if (-not (Test-Path -LiteralPath $key -PathType Leaf)) { throw 'SSH identity file does not exist.' }
if ($CheckOnly) { Write-Output 'Deployment configuration is valid. No build or network action was run.'; return }
$id = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$work = Join-Path ([IO.Path]::GetTempPath()) ('fintwind-mobile-ui-' + $id)
$remote = '/var/tmp/fintwind-mobile-ui-' + $id
$sshArgs = @('-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ConnectTimeout=10', '-i', $key)

function Assert-NativeSuccess([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw ($Operation + ' failed.') }
}

& bun run --cwd (Join-Path $repo 'mobile') build
Assert-NativeSuccess 'Mobile production build'
New-Item -ItemType Directory -Path $work | Out-Null
$release = Join-Path $work 'release'
New-Item -ItemType Directory -Path $release | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'mobile/dist') -Destination (Join-Path $release 'mobile') -Recurse
$caddyTemplate = [IO.File]::ReadAllText((Join-Path $repo 'deploy/remote-control/Caddyfile'))
$siteConfig = $caddyTemplate.Replace('{$FINTWIND_MOBILE_DOMAIN}', $domain)
[IO.File]::WriteAllText((Join-Path $release 'Caddyfile'), $siteConfig, [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $repo 'deploy/remote-control/publish-mobile-ui.sh') -Destination $release
$checksums = [System.Collections.Generic.List[string]]::new()
foreach ($file in Get-ChildItem -LiteralPath $release -Recurse -File) {
    $relative = [IO.Path]::GetRelativePath($release, $file.FullName).Replace([IO.Path]::DirectorySeparatorChar, '/')
    $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksums.Add($hash + '  ' + $relative)
}
[IO.File]::WriteAllLines((Join-Path $release 'SHA256SUMS'), $checksums, [Text.UTF8Encoding]::new($false))
$archive = Join-Path $work 'release.tar.gz'
& tar -czf $archive -C $work release
Assert-NativeSuccess 'Release archive'
$archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
& ssh @sshArgs $target ('mkdir -m 700 ' + $remote)
Assert-NativeSuccess 'Remote staging directory'
& scp @sshArgs $archive ($target + ':' + $remote + '/release.tar.gz')
Assert-NativeSuccess 'Static release upload'
$activate = "printf '%s  %s\n' '$archiveHash' '$remote/release.tar.gz' | sha256sum -c - && tar -xzf '$remote/release.tar.gz' -C '$remote' && sh '$remote/release/publish-mobile-ui.sh' '$remote/release' '$id' '$ExpectedSiteHash' '$site'"
& ssh @sshArgs $target $activate
Assert-NativeSuccess 'Static release activation'
Write-Output ('Release: ' + $id)
Write-Output ('Archive SHA256: ' + $archiveHash)
Write-Output ('Local manifest: ' + (Join-Path $release 'SHA256SUMS'))
Write-Output 'No endpoint, page or feature test was run. The relay was not restarted.'
