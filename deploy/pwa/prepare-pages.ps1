param(
    [string]$PwaDirectory = '',
    [string]$OutputDirectory = 'C:/Users/Public/Temp/opencode/fintwind-pages',
    [string]$SiteDirectory = '',
    [string]$AcceptanceReport = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $PwaDirectory) {
    $PwaDirectory = Join-Path (Split-Path -Parent $repo) 'fintwind-pwa'
}
$PwaDirectory = [System.IO.Path]::GetFullPath($PwaDirectory)
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
if ($SiteDirectory) { $SiteDirectory = [System.IO.Path]::GetFullPath($SiteDirectory) }
if ($AcceptanceReport) {
    $AcceptanceReport = [System.IO.Path]::GetFullPath($AcceptanceReport)
    if (-not $SiteDirectory) { throw 'An acceptance report requires the exact prebuilt site directory.' }
}
if (-not (Test-Path -LiteralPath (Join-Path $PwaDirectory 'package.json'))) {
    throw 'The PWA checkout is unavailable. Set -PwaDirectory explicitly.'
}
$null = New-Item -ItemType Directory -Path $OutputDirectory -Force
$stamp = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N')
$archive = Join-Path $OutputDirectory "fintwind-remote-$stamp.zip"

function Get-PwaInputHashes {
    $inputs = @(& git ls-files --cached --others --exclude-standard -- 'src' 'public' 'scripts' 'index.html' 'vite.config.ts' 'tsconfig*.json' 'package.json' 'bun.lock')
    if ($LASTEXITCODE -ne 0) { throw 'PWA input enumeration failed.' }
    $hashes = [ordered]@{}
    foreach ($file in ($inputs | Sort-Object -Unique)) {
        if ($file -match '(^src/types/generated/|^src/types/wasm-crypto\.d\.ts$|^public/wasm/|^public/\.wasm-build)') { continue }
        $absolute = Join-Path $PwaDirectory $file
        if (Test-Path -LiteralPath $absolute -PathType Leaf) {
            $hashes[$file] = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash
        }
    }
    return $hashes
}

Push-Location $PwaDirectory
try {
    $sourcesBefore = Get-PwaInputHashes
    & bun run protocol:check
    if ($LASTEXITCODE -ne 0) { throw 'The Rust/PWA contract differs. Regenerate and review before publishing.' }
    if ($SiteDirectory) {
        $dist = $SiteDirectory
    }
    else {
        & bun run build:wasm
        if ($LASTEXITCODE -ne 0) { throw 'WASM build failed. No publish archive was created.' }
        & bun run build
        if ($LASTEXITCODE -ne 0) { throw 'PWA build failed. No publish archive was created.' }
        $dist = Join-Path $PwaDirectory 'dist'
    }
    $required = @('index.html', '_headers', 'sw.js', 'manifest.webmanifest', 'wasm/fintwind-crypto.js', 'wasm/fintwind-crypto_bg.wasm', 'wasm/build-manifest.json')
    foreach ($item in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $dist $item))) {
            throw "Required asset is missing: $item"
        }
    }
    $wasmManifest = Get-Content -LiteralPath (Join-Path $dist 'wasm/build-manifest.json') -Raw | ConvertFrom-Json
    if (-not $wasmManifest) { throw 'The WASM build manifest is empty.' }
    if (-not $wasmManifest.outputs) { throw 'The WASM build manifest has no asset hashes.' }
    foreach ($item in @('fintwind-crypto.js', 'fintwind-crypto_bg.wasm')) {
        $expected = $wasmManifest.outputs.$item
        if (-not $expected) { throw "The WASM manifest does not name $item" }
        $actual = (Get-FileHash -LiteralPath (Join-Path $dist "wasm/$item") -Algorithm SHA256).Hash
        if ($actual -ne $expected.sha256) { throw "The built site has a mismatched WASM asset: $item" }
    }
    $gateScript = Join-Path $PwaDirectory 'scripts/check-release-artifact.mjs'
    $gateText = @(& node $gateScript $dist)
    if ($LASTEXITCODE -ne 0) { throw 'The production release gate failed. No archive was created.' }
    $gate = ($gateText -join [Environment]::NewLine) | ConvertFrom-Json
    if (-not $gate) { throw 'The production release gate returned no report.' }
    if (-not $gate.releaseGatePassed) { throw 'The production release was not approved for packaging.' }
    $acceptance = $null
    $acceptanceHash = $null
    if ($AcceptanceReport) {
        $acceptance = Get-Content -LiteralPath $AcceptanceReport -Raw | ConvertFrom-Json
        if (-not $acceptance) { throw 'The acceptance report is empty.' }
        if (($acceptance.passed -ne $true) -or ($acceptance.allOwnedProcessesStopped -ne $true) -or ($acceptance.cleanupRevoked -ne $true)) {
            throw 'The acceptance report is not a completed, cleaned-up pass.'
        }
        $containment = $acceptance.PSObject.Properties['descendantContainmentVerified']
        if ((-not $containment) -or ($containment.Value -ne $true)) {
            throw 'The acceptance report does not verify descendant process containment.'
        }
        if (-not $acceptance.siteSha256) { throw 'The acceptance report has no tested site hashes.' }
        if (@($acceptance.siteSha256.PSObject.Properties).Count -ne @($gate.assets.PSObject.Properties).Count) {
            throw 'The accepted site and the candidate contain different assets.'
        }
        foreach ($asset in $gate.assets.PSObject.Properties) {
            $tested = $acceptance.siteSha256.PSObject.Properties[$asset.Name]
            if (-not $tested) { throw 'The acceptance report omits a candidate asset.' }
            if ($tested.Value -ne $asset.Value.sha256) { throw 'The candidate differs from the accepted site.' }
        }
        $acceptanceHash = (Get-FileHash -LiteralPath $AcceptanceReport -Algorithm SHA256).Hash
    }
    $sourcesAfter = Get-PwaInputHashes
    if (($sourcesBefore | ConvertTo-Json -Depth 10 -Compress) -ne ($sourcesAfter | ConvertTo-Json -Depth 10 -Compress)) {
        throw 'PWA sources changed during the build. No archive was created.'
    }
    & bun run protocol:check
    if ($LASTEXITCODE -ne 0) { throw 'The contract changed during the build. No archive was created.' }
    $candidate = Join-Path $OutputDirectory "fintwind-remote-$stamp"
    $null = New-Item -ItemType Directory -Path $candidate
    Copy-Item -Path (Join-Path $dist '*') -Destination $candidate -Recurse
    $candidateText = @(& node $gateScript $candidate)
    if ($LASTEXITCODE -ne 0) { throw 'The frozen release directory failed its gate.' }
    $candidateGate = ($candidateText -join [Environment]::NewLine) | ConvertFrom-Json
    if (-not $candidateGate) { throw 'The frozen release has no asset record.' }
    if (($gate | ConvertTo-Json -Depth 10 -Compress) -ne ($candidateGate | ConvertTo-Json -Depth 10 -Compress)) {
        throw 'Production assets changed while freezing the release. No archive was created.'
    }
    # Include dotfiles as well; Compress-Archive skips hidden files on Windows.
    # Entries start at the site root, not inside a dist directory.
    [System.IO.Compression.ZipFile]::CreateFromDirectory($candidate, $archive, [System.IO.Compression.CompressionLevel]::Optimal, $false)
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    $digester = [System.Security.Cryptography.SHA256]::Create()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    try {
        foreach ($entry in $zip.Entries) {
            if (-not $entry) { throw 'The archive contains an invalid entry.' }
            $name = $entry.FullName.Replace('\', '/')
            if ($name.EndsWith('/')) { continue }
            $record = $gate.assets.PSObject.Properties[$name]
            if (-not $record) { throw 'The archive contains an unrecorded asset.' }
            if (-not ($seen.Add($name))) { throw 'The archive contains duplicate assets.' }
            $stream = $entry.Open()
            try { $digest = [Convert]::ToHexString($digester.ComputeHash($stream)).ToLowerInvariant() }
            finally { $stream.Dispose() }
            if (($digest -ne $record.Value.sha256) -or ($entry.Length -ne $record.Value.bytes)) {
                throw 'An archived asset differs from the frozen candidate.'
            }
        }
        if ($seen.Count -ne @($gate.assets.PSObject.Properties).Count) {
            throw 'The archive does not include every frozen asset.'
        }
    }
    finally {
        $digester.Dispose()
        $zip.Dispose()
    }
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    $report = [ordered]@{
        builtAt = [DateTimeOffset]::UtcNow.ToString('o')
        archive = $archive
        deploymentDirectory = $candidate
        sha256 = $hash
        pwaDirectory = $PwaDirectory
        sourceStatus = @(& git status --porcelain)
        protocolVersion = $wasmManifest.protocolVersion
        wasmOutputs = $wasmManifest.outputs
        releaseGate = $gate
        archiveAssetsVerified = $true
        acceptanceReport = $AcceptanceReport
        acceptanceReportSha256 = $acceptanceHash
        testedSiteReused = $null -ne $acceptance
        fullPlanVerified = $false
        sourceSha256 = $sourcesBefore
        relay = 'https://relay.fintwind.xyz'
        initialOrigin = 'https://fintwind-remote.pages.dev'
        intendedOrigin = 'https://remote.fintwind.xyz'
        publicChainVerified = $false
        note = 'Deploy these exact assets only after behavioral validation. Packaging is not E2E acceptance.'
    }
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $OutputDirectory "fintwind-remote-$stamp.json") -Encoding utf8
    Write-Host "[OK] Pages upload archive: $archive"
    Write-Host "[SHA256] $hash"
}
finally {
    Pop-Location
}
