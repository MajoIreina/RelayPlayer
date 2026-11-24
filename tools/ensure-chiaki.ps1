#requires -Version 5.1
<# Prepare the unmodified upstream runtime, without starting it. See notices/CHIAKI-NG.md. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][ValidateNotNullOrEmpty()][string]$InstallRoot,
    [string]$ArchivePath,
    [switch]$VerifyOnly,
    [switch]$CommunityNr,
    [switch]$RelayRender
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$archiveUrl = 'https://github.com/chiaki-ng/chiaki-ng/releases/download/v1.10.1/chiaki-ng-win_x64-MSYS2-portable-1.10.1.zip'
$archiveSize = [long]118676648
$archiveHash = '6ca3f5e6780608104d79345ab7755cfa77fefc33a41ae10651a874c43f54ea57'
$treeHash = 'f18019dca0124041de066cde3558c0d3ed7a9366ab37c30e2fce327ab3db506b'
$fileNamesHash = '600a00d691e798b94f56d4df894f5410e5cb964e4cd1294182d8f838fcddf863'
$dirNamesHash = 'a4bb3d6c45a3f26a1d2146c14f893d5cb766cc09c616f3292a45f0fc00a3fb07'
$fileCount = 1497
$dirCount = 64 # Excludes the root directory itself.
$treeSize = [long]315854122
$originalSize = 6543375
$originalHash = 'c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec'
$profileOffset = 0x221dbb
if ($RelayRender) { $CommunityNr = $true }
$runtimeName = 'chiaki-ng'
$archiveEntries = $fileCount + $dirCount + 1
if ($CommunityNr) {
    $runtimeName = 'chiaki-nr'
    $archiveUrl = 'https://github.com/windystrife/chiaki-ng-Ultimate/releases/download/1.10.0-dlss5.7/chiaki-ng-1.10.0-dlss5.7-win_x64-portable.zip'
    $archiveSize = [long]271677844
    $archiveHash = 'b07e58000ebf38545518c955ad9462ad8fac30dc8e0438ff44d5119d93c91ebe'
    $treeHash = '096fa9ded30152d4d800fb5d8c48ef3c8175b7b764e7b9b5e0c3078e099efad4'
    $fileNamesHash = 'ce702af0fc2631280afc04c7636bb2314d5eea41c2439f2302decd77d4ce9456'
    $dirNamesHash = '8f38c86eb10ef8e66f69e61cfe1b4550adbb561a218b1f878419a7d6a3234a63'
    $fileCount = 1438; $dirCount = 69; $archiveEntries = 1452
    $treeSize = [long]629530922
    $originalSize = 5536256
    $originalHash = '3488b8a2c6564f006b1447dc42b67e0fbafc5293963ee3af073a3f95e7db3e26'
    $profileOffset = 0x45d124
}
$utf = [Text.UTF8Encoding]::new($false)
$cancelEvent = $null
$ioBuffer = [byte[]]::new(524288)

# The Rust caller owns a manual-reset event for this one request. Direct CLI users need no event.
function Assert-Preparing([Threading.CancellationTokenSource]$Request = $null) {
    if ($null -ne $cancelEvent -and $cancelEvent.WaitOne(0)) {
        if ($null -ne $Request) { $Request.Cancel() }
        throw [OperationCanceledException]::new('Runtime preparation cancelled.')
    }
}
# HttpClient's body stream can stall after headers; waiting on BOTH handles lets the caller
# interrupt DNS/connect/TLS, response headers and each pending body read without a timeout-sized delay.
function Wait-Transfer([Threading.Tasks.Task]$Task, [DateTime]$Deadline, [Threading.CancellationTokenSource]$Request) {
    Assert-Preparing $Request
    $ms = [int][Math]::Max(0, [Math]::Ceiling(($Deadline - [DateTime]::UtcNow).TotalMilliseconds))
    if ($ms -gt 0 -and !$Task.IsCompleted) {
        $io = ([IAsyncResult]$Task).AsyncWaitHandle
        if ($null -ne $cancelEvent) {
            $which = [Threading.WaitHandle]::WaitAny([Threading.WaitHandle[]]@($cancelEvent, $io), $ms)
            if ($which -eq 0) { Assert-Preparing $Request }
        } else { [void]$io.WaitOne($ms) }
    }
    Assert-Preparing $Request
    if (!$Task.IsCompleted -or $ms -le 0) { $Request.Cancel(); throw 'The upstream release download timed out.' }
}
# Limit the largest non-interruptible read on a slow drive. All callers are synchronous and
# reuse this buffer. Small verified files use the same underlying SHA256 without extra copies.
function Stream-Digest([IO.Stream]$Stream, $Sha) {
    Assert-Preparing
    if ($null -eq $cancelEvent -or $Stream.Length -le $ioBuffer.Length) { return (Hex ($Sha.ComputeHash($Stream))) }
    # On .NET Framework TransformFinalBlock consumes the native hashing state. Use a distinct
    # incremental hash so following small files can reuse the ordinary ComputeHash instance.
    $chunked = [Security.Cryptography.SHA256]::Create()
    try {
        while (($n = $Stream.Read($ioBuffer,0,$ioBuffer.Length)) -gt 0) {
            Assert-Preparing
            [void]$chunked.TransformBlock($ioBuffer,0,$n,$ioBuffer,0)
        }
        [void]$chunked.TransformFinalBlock($ioBuffer,0,0)
        return (Hex $chunked.Hash)
    } finally { $chunked.Dispose() }
}

function Report([string]$Phase, [long]$Done, [long]$Total) {
    [Console]::Out.WriteLine("RELAY $Phase $Done $Total")
    [Console]::Out.Flush()
}
function Hex([byte[]]$Bytes) { [BitConverter]::ToString($Bytes).Replace('-','').ToLowerInvariant() }
function Assert-Unlinked([string]$Path) {
    # GetAttributes also sees a reparse point whose target is inaccessible. Do not follow it.
    for ($p = [IO.Path]::GetFullPath($Path); $p; $p = [IO.Path]::GetDirectoryName($p)) {
        try { $attr = [IO.File]::GetAttributes($p) }
        catch [IO.FileNotFoundException] { continue }
        catch [IO.DirectoryNotFoundException] { continue }
        if ($attr -band [IO.FileAttributes]::ReparsePoint) { throw 'A runtime path is a reparse point; use a regular local directory.' }
    }
}
function Child-Path([string]$Parent, [string]$Name) {
    $p = [IO.Path]::GetFullPath($Parent).TrimEnd('\')
    $child = [IO.Path]::GetFullPath([IO.Path]::Combine($p, $Name))
    if (!$child.StartsWith($p + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid path outside the installation folder.' }
    return $child
}
function Name-Digest([string[]]$Names, $Sha) {
    return (Hex ($Sha.ComputeHash($utf.GetBytes([string]::Join("`n", $Names) + "`n"))))
}
function Assert-NrModel([string]$Path) {
    Assert-Unlinked $Path
    $file = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        # This original 310.9.16 model is unsigned. Pin its complete bytes instead of
        # accepting a version string, trusting a modified legacy DLL, or claiming a signature.
        if ($file.Length -ne 276162560 -or
            (Stream-Digest $file $sha) -cne 'cdfa6c1ec784867182164c7836f324f087b5c9f374d0c9fe46f54999feabf3ed') {
            throw 'The DLSS NR model must be the verified original 310.9.16 build. Modified or unknown runtime files are not accepted.'
        }
    } finally { $sha.Dispose(); $file.Dispose() }
}
# These local, hash-bound changes are the compiled equivalent of
# docs/patches/dualsense-motion.patch. They remove quaternion deadband and
# preserve raw SDL gravity after a motion reset. No protocol or credentials change.
function Update-MotionImage([byte[]]$Image, [bool]$Restore = $false) {
    $fixes = @(
        @(0x4823ec,'3480b739','00000000'), @(0x4823f0,'3480373a','00000000'),
        @(0x4823fc,'3480b73a','00000000'), @(0x66ce7,'00','01')
    )
    if ($CommunityNr) {
        $fixes = @(
            @(0x44e6c4,'3480b739','00000000'), @(0x44e6c8,'3480b7b9','00000000'),
            @(0x44e6cc,'3480373a','00000000'), @(0x44e6d0,'348037ba','00000000'),
            @(0x44e6d4,'3480b73a','00000000'), @(0x44e6d8,'3480b7ba','00000000'),
            @(0x65cfa,'00','01')
        )
    }
    $old = $true; $new = $true
    foreach ($fix in $fixes) {
        $offset = [int]$fix[0]; $before = [string]$fix[1]; $after = [string]$fix[2]
        for ($i=0; $i -lt $before.Length/2; $i++) {
            $old = $old -and ($Image[$offset+$i] -eq [Convert]::ToByte($before.Substring($i*2,2),16))
            $new = $new -and ($Image[$offset+$i] -eq [Convert]::ToByte($after.Substring($i*2,2),16))
        }
    }
    if ($Restore -and $old) { return } # Accept a verified v0.1.8 local profile too.
    if ((!$Restore -and !$old) -or ($Restore -and !$new)) { throw 'Motion patch bytes differ from the reviewed runtime.' }
    foreach ($fix in $fixes) {
        $offset = [int]$fix[0]; $value = [string]$fix[$(if ($Restore) { 1 } else { 2 })]
        for ($i=0; $i -lt $value.Length/2; $i++) { $Image[$offset+$i] = [Convert]::ToByte($value.Substring($i*2,2),16) }
    }
}

function Test-ProfileImage([IO.FileInfo]$File) {
    # This one optional derivative is never a replacement for an upstream file. Undo only the
    # six-byte local profile name and compare the whole image with the pinned original. The
    # initializer and Rust will still make a separate profile-specific proof at launch time.
    if ($File.Length -ne $originalSize) { return $false }
    $link = Microsoft.PowerShell.Management\Get-Item -Force -LiteralPath $File.FullName
    if ($link.LinkType) { return $false }
    $image = [IO.File]::Open($File.FullName,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        if ($image.Length -ne $originalSize) { return $false }
        $bytes = [byte[]]::new($originalSize); $offset = 0
        while ($offset -lt $bytes.Length) {
            Assert-Preparing
            $n = $image.Read($bytes, $offset, [Math]::Min($ioBuffer.Length, $bytes.Length-$offset))
            if ($n -le 0) { return $false }; $offset += $n
        }
    } finally { $image.Dispose() }
    if ($bytes.Length -ne $originalSize -or [Text.Encoding]::ASCII.GetString($bytes,$profileOffset,6) -cnotmatch '^R[A-Z2-7]{5}$') { return $false }
    try { Update-MotionImage $bytes $true } catch { return $false }
    [Array]::Copy([Text.Encoding]::ASCII.GetBytes('Chiaki'), 0, $bytes, $profileOffset, 6)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ((Hex ($sha.ComputeHash($bytes))) -ceq $originalHash) } finally { $sha.Dispose() }
}
function Test-KnownRuntime([string]$Path) {
    # Distinguish a damaged installation from an unrelated folder. These are membership probes,
    # NOT sufficient for launching. Do not traverse links or read arbitrary paths inside it.
    foreach ($probe in @(
        @('chiaki.exe',$originalSize,$originalHash),
        $(if ($CommunityNr) { @('platforms/qwindows.dll',925920,'133983755958a0f4d955b4a6f3712a014ac62351ec5edd3c32b802094f66ac96') } else { @('platforms/qwindows.dll',1527707,'b1cab650819e13f13427215558b3bc714e05a628b78dcaa4c2565991952fb3de') }),
        $(if ($CommunityNr) { @('imageformats/qjpeg.dll',576736,'43ca4ed440f9f03e79f75fd76905e3350ec4008650d02d7041fdd2ac4043dfd2') } else { @('imageformats/qjpeg.dll',66404,'891d27e92f807fffd894b6016f055aa26302f1c50062aaff8ed7e5949b16f409') })
    )) {
        Assert-Preparing
        $probePath = Child-Path $Path $probe[0]
        try { Assert-Unlinked $probePath } catch { continue }
        if (![IO.File]::Exists($probePath)) { continue }
        $stream = $null; $sha = $null
        try {
            $stream = [IO.File]::Open($probePath,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
            if ($stream.Length -ne $probe[1]) { continue }
            $sha = [Security.Cryptography.SHA256]::Create()
            if ((Stream-Digest $stream $sha) -ceq $probe[2]) { return $true }
        } catch { Assert-Preparing; continue }
        finally { if ($sha) { $sha.Dispose() }; if ($stream) { $stream.Dispose() } }
    }
    return $false
}
function Verify-Tree([string]$Path, [bool]$AllowProfile = $false) {
    Assert-Unlinked $Path
    if (![IO.Directory]::Exists($Path)) { throw 'The runtime folder is missing.' }
    $prefix = [IO.Path]::GetFullPath($Path).TrimEnd('\') + '\'
    $dirs = [Collections.Generic.List[string]]::new()
    $files = [Collections.Generic.List[string]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($Path)
    $total = [long]0
    while ($pending.Count) {
        foreach ($entry in [IO.DirectoryInfo]::new($pending.Pop()).EnumerateFileSystemInfos()) {
            Assert-Preparing
            if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Runtime contains a redirected file or directory.' }
            $rel = $entry.FullName.Substring($prefix.Length).Replace('\','/')
            if ($CommunityNr -and $rel -ceq 'nvngx_dlssnr.dll') {
                if ($entry -isnot [IO.FileInfo]) { throw 'The DLSS NR model must be a regular file.' }
                Assert-NrModel $entry.FullName
                continue
            }
            if ($AllowProfile -and $rel -ceq 'relay-profile.exe') {
                if ($entry -isnot [IO.FileInfo] -or !(Test-ProfileImage $entry)) { throw 'The isolated player image failed verification.' }
                continue
            }
            if ($entry.Attributes -band [IO.FileAttributes]::Directory) {
                $dirs.Add($rel + '/'); $pending.Push($entry.FullName)
            } else {
                $files.Add($rel); $total += $entry.Length
            }
            if ($files.Count -gt $fileCount -or $dirs.Count -gt $dirCount -or $total -gt $treeSize) { throw 'Unexpected files in the runtime.' }
        }
    }
    if ($files.Count -ne $fileCount -or $dirs.Count -ne $dirCount -or $total -ne $treeSize) { throw 'Runtime file list or size differs from the upstream package.' }
    $files.Sort([StringComparer]::Ordinal); $dirs.Sort([StringComparer]::Ordinal)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        if ((Name-Digest $files.ToArray() $sha) -cne $fileNamesHash -or (Name-Digest $dirs.ToArray() $sha) -cne $dirNamesHash) { throw 'Unexpected runtime paths.' }
        # Verify the names first. Never open unrelated files just because they were put here.
        $manifest = [Text.StringBuilder]::new()
        $n = 0; Report 'verify' 0 $fileCount
        foreach ($rel in $files) {
            $inFile = [IO.File]::Open((Child-Path $Path $rel), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
            try { $digest = Stream-Digest $inFile $sha } finally { $inFile.Dispose() }
            [void]$manifest.Append($digest).Append('  ').Append($rel).Append("`n")
            $n++; if ($n % 128 -eq 0) { Report 'verify' $n $fileCount }
        }
        if ((Hex ($sha.ComputeHash($utf.GetBytes($manifest.ToString())))) -cne $treeHash) { throw 'A runtime file failed SHA-256 verification.' }
        Report 'verify' $fileCount $fileCount
    } finally { $sha.Dispose() }
}

function Get-OfficialArchive([string]$Destination, [string]$Url = $archiveUrl, [long]$ExpectedSize = $archiveSize) {
    Add-Type -AssemblyName System.Net.Http
    # Automatic redirects on older .NET versions can downgrade HTTPS. Follow only GitHub release asset hosts.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $handler.UseCookies = $false
    $handler.UseDefaultCredentials = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(90)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('RelayPlayer/0.1.9')
    $uri = [Uri]$Url
    $allowed = @('github.com','release-assets.githubusercontent.com','objects.githubusercontent.com','github-releases.githubusercontent.com')
    $response = $null; $request = [Threading.CancellationTokenSource]::new()
    try {
        for ($hop = 0; ; $hop++) {
            if ($hop -gt 5 -or $uri.Scheme -cne 'https' -or !$uri.IsDefaultPort -or $uri.UserInfo -or $uri.IdnHost -notin $allowed) { throw 'Unexpected upstream release redirect.' }
            Assert-Preparing $request
            $pending = $client.GetAsync($uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $request.Token)
            Wait-Transfer $pending ([DateTime]::UtcNow.AddSeconds(90)) $request
            $response = $pending.GetAwaiter().GetResult()
            $status = [int]$response.StatusCode
            if ($status -eq 200) { break }
            if ($status -notin @(301,302,303,307,308) -or $null -eq $response.Headers.Location) { throw "GitHub release download returned HTTP $status." }
            $uri = [Uri]::new($uri, $response.Headers.Location)
            $response.Dispose(); $response = $null
        }
        $size = $response.Content.Headers.ContentLength
        if ($null -ne $size -and $size -ne $ExpectedSize) { throw 'Unexpected upstream release download size.' }
        $pending = $response.Content.ReadAsStreamAsync()
        Wait-Transfer $pending ([DateTime]::UtcNow.AddSeconds(90)) $request
        $source = $pending.GetAwaiter().GetResult()
        try {
            if ($source.CanTimeout) { $source.ReadTimeout = 90000 }
            $dest = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $buffer = [byte[]]::new(262144); $received = [long]0; $next = [long]0
                $deadline = [DateTime]::UtcNow.AddMinutes(30)
                Report 'download' 0 $ExpectedSize
                while ($true) {
                    Assert-Preparing $request
                    $pending = $source.ReadAsync($buffer,0,$buffer.Length,$request.Token)
                    $readDeadline = [DateTime]::UtcNow.AddSeconds(90)
                    if ($deadline -lt $readDeadline) { $readDeadline = $deadline }
                    Wait-Transfer $pending $readDeadline $request
                    $read = $pending.GetAwaiter().GetResult(); if ($read -eq 0) { break }
                    $received += $read
                    if ($received -gt $ExpectedSize -or [DateTime]::UtcNow -gt $deadline) { throw 'Upstream download exceeded its size or time limit.' }
                    $dest.Write($buffer,0,$read)
                    if ($received -ge $next) { Report 'download' $received $ExpectedSize; $next = $received + 2097152 }
                }
                if ($received -ne $ExpectedSize) { throw 'The upstream release download was incomplete.' }
                Report 'download' $received $ExpectedSize
            } finally { $dest.Dispose() }
        } finally { $source.Dispose() }
    } finally { if ($response) { $response.Dispose() }; $client.Dispose(); $request.Dispose() }
}
function Install-NrModel([string]$Stage, [string]$Payload) {
    $targetModel = Child-Path $Payload 'nvngx_dlssnr.dll'
    if ($env:RELAY_NR_MODEL) {
        $model = [IO.Path]::GetFullPath($env:RELAY_NR_MODEL)
        Assert-NrModel $model
        $inputModel = [IO.File]::Open($model,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        try {
            $outputModel = [IO.File]::Open($targetModel,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
            try { $inputModel.CopyTo($outputModel,262144) } finally { $outputModel.Dispose() }
        } finally { $inputModel.Dispose() }
    } else {
        $modelZip = Child-Path $Stage 'nr-model.zip'
        Get-OfficialArchive $modelZip 'https://github.com/RankFTW/rhi-repo/releases/download/dlssnr-310.9.16/nvngx_dlssnr_310.9.16.zip' 192378839
        $inputModel = [IO.File]::Open($modelZip,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            if ($inputModel.Length -ne 192378839 -or (Stream-Digest $inputModel $sha) -cne 'dbeb1f2790b5c621887db99b1a56ac8bd5cb35207e8c074cbb806710f7460cab') { throw 'The DLSS NR model archive failed verification.' }
            $inputModel.Position = 0
            $zipModel = [IO.Compression.ZipArchive]::new($inputModel,[IO.Compression.ZipArchiveMode]::Read,$true)
            try {
                $entries = @($zipModel.Entries | Where-Object { $_.FullName -ceq 'nvngx_dlssnr.dll' })
                if ($entries.Count -ne 1 -or $entries[0].Length -ne 276162560) { throw 'The DLSS NR model archive has unexpected contents.' }
                $sourceModel = $entries[0].Open()
                $outputModel = [IO.File]::Open($targetModel,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
                try {
                    while (($n = $sourceModel.Read($ioBuffer,0,$ioBuffer.Length)) -gt 0) { Assert-Preparing; $outputModel.Write($ioBuffer,0,$n) }
                } finally { $outputModel.Dispose(); $sourceModel.Dispose() }
            } finally { $zipModel.Dispose() }
        } finally { $sha.Dispose(); $inputModel.Dispose() }
    }
    Assert-NrModel $targetModel
}
function Expand-OfficialArchive([string]$Archive, [string]$Destination) {
    Add-Type -AssemblyName System.IO.Compression
    Assert-Unlinked $Archive
    $source = [IO.File]::Open($Archive, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($source.Length -ne $archiveSize) { throw 'The release archive has the wrong size.' }
        $sha = [Security.Cryptography.SHA256]::Create()
        try { if ((Stream-Digest $source $sha) -cne $archiveHash) { throw 'The release archive failed SHA-256 verification.' } } finally { $sha.Dispose() }
        $source.Position = 0 # The same locked, verified file is used for extraction.
        $zip = [IO.Compression.ZipArchive]::new($source, [IO.Compression.ZipArchiveMode]::Read, $true)
        try {
            $zipPrefix = 'chiaki-ng-Win/'
            if ($zip.Entries.Count -ne $archiveEntries) { throw 'Unexpected number of release archive entries.' }
            $count = 0; $sum = [long]0
            Report 'extract' 0 $fileCount
            foreach ($entry in $zip.Entries) {
                Assert-Preparing
                $name = $entry.FullName
                if ($CommunityNr) { $name = $name.Replace('\','/') }
                if (!$name.StartsWith($zipPrefix, [StringComparison]::Ordinal)) { throw 'Unexpected archive root.' }
                $rel = $name.Substring($zipPrefix.Length)
                if ($rel -ceq '') { continue }
                $directory = $rel.EndsWith('/')
                $rel = $rel.TrimEnd('/')
                if ($rel -cnotmatch '^[A-Za-z0-9_+.@-]+(/[A-Za-z0-9_+.@-]+)*$' -or $rel -match '(^|/)(\.{1,2}|[^/]+\.|(?:CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\.[^/]*)?)(/|$)') { throw 'Unusable archive path.' }
                $kind = ($entry.ExternalAttributes -shr 16) -band 0xf000
                if (!($CommunityNr -and $entry.ExternalAttributes -eq 0) -and (($directory -and $kind -ne 0x4000) -or (!$directory -and $kind -ne 0x8000))) { throw 'The archive contains an unexpected file type.' }
                $out = Child-Path $Destination $rel
                if ($directory) { [void][IO.Directory]::CreateDirectory($out); continue }
                $sum += $entry.Length
                if ($sum -gt $treeSize) { throw 'Archive exceeds the expected uncompressed size.' }
                [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($out))
                $output = [IO.File]::Open($out, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $inputStream = $entry.Open()
                    try {
                        if ($null -eq $cancelEvent -or $entry.Length -le $ioBuffer.Length) { $inputStream.CopyTo($output, 131072) }
                        else {
                            while (($n = $inputStream.Read($ioBuffer,0,$ioBuffer.Length)) -gt 0) {
                                Assert-Preparing; $output.Write($ioBuffer,0,$n)
                            }
                        }
                    } finally { $inputStream.Dispose() }
                    if ($output.Length -ne $entry.Length) { throw 'Incomplete runtime file.' }
                } finally { $output.Dispose() }
                $count++; if ($count % 128 -eq 0) { Report 'extract' $count $fileCount }
            }
            if ($sum -ne $treeSize -or $count -ne $fileCount) { throw 'Missing upstream runtime data.' }
            Report 'extract' $count $fileCount
        } finally { $zip.Dispose() }
    } finally { $source.Dispose() }
}

# Only our newly created private staging tree may be cleaned. Do not traverse any links.
function Clear-Stage([string]$Parent, [string]$Path) {
    $absolute = [IO.Path]::GetFullPath($Path)
    $leaf = [IO.Path]::GetFileName($absolute)
    if ($leaf -cnotmatch '^\.chiaki-stage-[a-f0-9]{32}$' -or ![string]::Equals($absolute,(Child-Path $Parent $leaf),[StringComparison]::OrdinalIgnoreCase)) { throw 'Refusing to clean an unmanaged directory.' }
    Assert-Unlinked $absolute
    $pending = [Collections.Generic.Stack[string]]::new(); $pending.Push($absolute)
    $empty = [Collections.Generic.Stack[string]]::new()
    while ($pending.Count) {
        $pathHere = $pending.Pop(); $empty.Push($pathHere)
        foreach ($child in [IO.DirectoryInfo]::new($pathHere).EnumerateFileSystemInfos()) {
            if ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing to clean a redirected staging path.' }
            if ($child.Attributes -band [IO.FileAttributes]::Directory) { $pending.Push($child.FullName) } else { $child.Delete() }
        }
    }
    while ($empty.Count) { [IO.Directory]::Delete($empty.Pop(), $false) }
}
function Prepare {
    if ($InstallRoot.StartsWith('\\?\') -and $InstallRoot.Substring(4) -match '^[A-Za-z]:[\\/]') { $InstallRoot = $InstallRoot.Substring(4) }
    if ($InstallRoot -notmatch '^[A-Za-z]:[\\/]' -or $InstallRoot.Substring(2).Contains(':')) { throw 'Use an absolute local installation directory.' }
    $root = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\')
    if ($root.Length -le 3) { throw 'Use a dedicated installation directory, not a drive root.' }
    Assert-Unlinked $root
    $target = Child-Path $root $runtimeName
    if ($VerifyOnly -and ![IO.Directory]::Exists($target)) { throw 'The runtime has not been installed.' }
    [void][IO.Directory]::CreateDirectory($root)
    $lockPath = Child-Path $root '.chiaki-install.lock'
    Assert-Unlinked $lockPath
    $deadline = [DateTime]::UtcNow.AddMinutes(32); $reported = $false; $lock = $null; $stage = $null
    try {
        while (!$lock) {
            Assert-Preparing
            try { $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
            catch [IO.IOException] {
                if (($_.Exception.GetBaseException().HResult -band 0xffff) -notin @(32,33)) { throw }
                if ([DateTime]::UtcNow -ge $deadline) { throw 'Another installation is still running; retry later.' }
                if (!$reported) { Report 'waiting' 0 1; $reported = $true }
                if ($null -ne $cancelEvent) { [void]$cancelEvent.WaitOne(250) } else { [Threading.Thread]::Sleep(250) }
            }
        }
        Assert-Unlinked $target
        $repair = $false
        if ([IO.Directory]::Exists($target)) {
            try { Verify-Tree $target $true; if ($CommunityNr) { Assert-NrModel (Child-Path $target 'nvngx_dlssnr.dll') }; Assert-Preparing; Report 'ready' 1 1; return }
            catch {
                Assert-Preparing # Cancellation is not corruption and must never start a repair.
                if ($VerifyOnly) { throw 'The installed runtime failed verification. VerifyOnly does not change it.' }
                if (!(Test-KnownRuntime $target)) { throw 'Unrecognized runtime folder. Back it up under a different name before retrying.' }
                $repair = $true; Report 'repair' 0 1
            }
        }
        if ($VerifyOnly) { throw 'The runtime has not been installed.' }
        if ([IO.File]::Exists($target)) { throw 'A file is occupying the runtime directory.' }
        Assert-Preparing
        $fresh = Child-Path $root ('.chiaki-stage-' + [Guid]::NewGuid().ToString('N'))
        Assert-Unlinked $fresh
        if ([IO.Directory]::Exists($fresh) -or [IO.File]::Exists($fresh)) { throw 'Staging name collision; please retry.' }
        [void][IO.Directory]::CreateDirectory($fresh); $stage = $fresh
        $payload = Child-Path $stage 'payload'; [void][IO.Directory]::CreateDirectory($payload)
        if ($ArchivePath) { $archive = [IO.Path]::GetFullPath($ArchivePath) }
        else { $archive = Child-Path $stage 'upstream.zip'; Get-OfficialArchive $archive }
        Expand-OfficialArchive $archive $payload
        Verify-Tree $payload
        if ($CommunityNr) {
            Install-NrModel $stage $payload
            Verify-Tree $payload
        }
        # Rename whole trees on one volume. Old contents (including any internal links) are
        # preserved as-is; no recursive move, delete or overlay of the damaged installation.
        # Complete and verify the replacement BEFORE touching the old installation.
        Assert-Unlinked $payload; Assert-Unlinked $target
        $backup = $null
        if ($repair) {
            if (!(Test-KnownRuntime $target)) { throw 'The existing runtime changed while preparing. Retry after closing other players.' }
            $backup = Child-Path $root ($runtimeName + '.backup-' + [Guid]::NewGuid().ToString('N'))
            Assert-Unlinked $backup
            if ([IO.File]::Exists($backup) -or [IO.Directory]::Exists($backup)) { throw 'Backup name collision. Retry without changing the original.' }
        }
        # Last cancellation point before publication. The two renames and possible rollback must
        # run to completion together. Cleanup likewise runs even if the user has closed Relay.
        Assert-Preparing
        if ($repair) {
            try { [IO.Directory]::Move($target, $backup) }
            catch { throw 'Cannot move the old runtime aside. Close other players and retry; the existing directory was not changed.' }
        }
        try { [IO.Directory]::Move($payload, $target) }
        catch {
            if ($backup) {
                try {
                    Assert-Unlinked $target; Assert-Unlinked $backup
                    if ([IO.Directory]::Exists($target) -or [IO.File]::Exists($target)) { throw 'Target occupied.' }
                    [IO.Directory]::Move($backup, $target)
                } catch { throw 'Repair could not switch or restore the active path. The original runtime is preserved under the runtime backup folder.' }
                throw 'Repair could not switch the runtime. The original directory was restored; close other players and retry.'
            }
            throw
        }
        if ($repair) { Report 'repair' 1 1 }
        Report 'ready' 1 1
    } finally {
        try { if ($stage -and [IO.Directory]::Exists($stage)) { Clear-Stage $root $stage } } finally { if ($lock) { $lock.Dispose() } }
    }
}
try {
    if ($env:RELAY_PREPARE_CANCEL) {
        if ($env:RELAY_PREPARE_CANCEL -cnotmatch '^Local\\RelayPlayer\.Prepare\.[0-9]{1,10}\.[0-9]{1,20}$') { throw 'Invalid preparation cancellation signal.' }
        try { $cancelEvent = [Threading.EventWaitHandle]::OpenExisting($env:RELAY_PREPARE_CANCEL,[Security.AccessControl.EventWaitHandleRights]::Synchronize) }
        catch { throw 'Cannot open the current preparation cancellation signal.' }
    }
    Assert-Preparing; Prepare; exit 0
} catch {
    if ($null -ne $cancelEvent -and $cancelEvent.WaitOne(0)) { [Console]::Error.WriteLine('Runtime preparation cancelled.'); exit 2 }
    # Redirect locations, personal paths and response bodies must not be emitted to the GUI/logs.
    # All explicit messages are static, except for the numeric HTTP status.
    $exception = $_.Exception
    if ($exception -is [Management.Automation.RuntimeException] -and $exception.InnerException -eq $null) { [Console]::Error.WriteLine($exception.Message) }
    else { [Console]::Error.WriteLine('Could not prepare the official chiaki-ng runtime. Check GitHub access, free disk space and folder permissions; then retry.') }
    exit 1
} finally { if ($null -ne $cancelEvent) { $cancelEvent.Dispose() } }
