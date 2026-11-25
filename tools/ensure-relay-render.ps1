#requires -Version 5.1
# Verified source-built render adapter over the pinned community runtime. No user data is read here.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$InstallRoot,[string]$ProfileRoot,[switch]$RelayRender)
Set-StrictMode -Version 3
$ErrorActionPreference='Stop'
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
    $allowed = @('github.com','release-assets.githubusercontent.com','objects.githubusercontent.com','github-releases.githubusercontent.com','pypi.nvidia.com')
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

$root=[IO.Path]::GetFullPath($InstallRoot).TrimEnd('\')
Assert-Unlinked $root

function File-Digest([string]$Path){
 Assert-Unlinked $Path;$f=[IO.File]::OpenRead($Path);$sha=[Security.Cryptography.SHA256]::Create()
 try{return (Stream-Digest $f $sha)}finally{$sha.Dispose();$f.Dispose()}
}
$manifestPath=Child-Path ([IO.Path]::GetDirectoryName($root)) 'tools/relay-render-manifest.json'
Assert-Unlinked $manifestPath
if((File-Digest $manifestPath) -cne 'aa916eda18bc01647cf6ca9905958e6aba6d3132932ea8d649fecb8aa9a3231d'){throw 'Native manifest failed verification.'}
$manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
function Verify-Native([string]$Path){
 Assert-Unlinked $Path
 foreach($dir in Get-ChildItem -LiteralPath $Path -Recurse -Force -Directory){Assert-Unlinked $dir.FullName}
 $actual=@(Get-ChildItem -LiteralPath $Path -Recurse -Force -File)
 foreach($f in $actual){
  Assert-Preparing;Assert-Unlinked $f.FullName
  $name=$f.FullName.Substring($Path.Length+1).Replace('\','/')
  if($name -ceq 'relay-profile.exe'){$item=$manifest.'chiaki.exe'}
  else {$prop=$manifest.PSObject.Properties[$name];if(!$prop){throw 'Unexpected native runtime file.'};$item=$prop.Value}
  if($f.Length -ne $item.size){throw 'Native runtime size mismatch.'}
  $stream=[IO.File]::OpenRead($f.FullName);$sha=[Security.Cryptography.SHA256]::Create()
  try{if((Stream-Digest $stream $sha) -cne $item.sha256){throw 'Native runtime SHA-256 mismatch.'}}finally{$sha.Dispose();$stream.Dispose()}
 }
 foreach($p in $manifest.PSObject.Properties){if(![IO.File]::Exists((Child-Path $Path $p.Name))){throw 'Native runtime component missing.'}}
}
function Copy-Tree([string]$From,[string]$To){
 foreach($f in Get-ChildItem -LiteralPath $From -Recurse -Force){
  Assert-Preparing;Assert-Unlinked $f.FullName
  $name=$f.FullName.Substring($From.Length+1)
  if($name -ceq 'relay-profile.exe' -or $name -ceq 'chiaki-fgdx12test.exe'){continue}
  $dest=Child-Path $To $name
  if($f.PSIsContainer){[void][IO.Directory]::CreateDirectory($dest)}else{[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest));[IO.File]::Copy($f.FullName,$dest,$false)}
 }
}
$target=Child-Path $root 'relay-render'
$stage=$null;$lock=$null
try{
 if($env:RELAY_PREPARE_CANCEL){$cancelEvent=[Threading.EventWaitHandle]::OpenExisting($env:RELAY_PREPARE_CANCEL)}
 $lockPath=Child-Path $root '.relay-render-install.lock';Assert-Unlinked $lockPath
 while(!$lock){Assert-Preparing;try{$lock=[IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)}catch [IO.IOException]{if(($_.Exception.HResult -band 0xffff) -notin @(32,33)){throw};Report 'waiting' 0 1;if($cancelEvent){[void]$cancelEvent.WaitOne(250)}else{[Threading.Thread]::Sleep(250)}}}
 if([IO.Directory]::Exists($target)){Verify-Native $target;Report 'ready' 1 1;return}
 $overlay=Child-Path ([IO.Path]::GetDirectoryName($root)) 'runtime/relay-render-overlay.zip';Assert-Unlinked $overlay
 if((Get-Item -LiteralPath $overlay).Length -ne 24795745 -or (File-Digest $overlay) -cne '4edfe5dae8cc7c6fea4efc5ab94fd59c58f1e2562b279e4bb7b023dd6100cd10'){throw 'Native renderer bundle failed verification.'}
 $stage=Child-Path $root ('.relay-render-stage-'+[Guid]::NewGuid().ToString('N'));[void][IO.Directory]::CreateDirectory($stage)
 Copy-Tree (Child-Path $root 'chiaki-nr') $stage
 Add-Type -AssemblyName System.IO.Compression.FileSystem
 $zip=[IO.Compression.ZipFile]::OpenRead($overlay)
 try{
  foreach($e in $zip.Entries){
   Assert-Preparing
   if(!$e.Name){continue};$prop=$manifest.PSObject.Properties[$e.FullName]
   if(!$prop -or $e.Length -ne $prop.Value.size -or $e.FullName -like 'vfx/*' -or $e.FullName -ceq 'nvngx_dlssnr.dll'){throw 'Unrecognized overlay entry.'}
   $dest=Child-Path $stage $e.FullName;[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest));[IO.Compression.ZipFileExtensions]::ExtractToFile($e,$dest,$true)
  }
 }finally{$zip.Dispose()}
 # NVIDIA binaries are fetched from NVIDIA and retain their own license terms.
 $wheel=Child-Path $root '.relay-nvidia-vfx.whl'
 if(![IO.File]::Exists($wheel)){
  $download=Child-Path $stage 'nvidia-vfx.download'
  Get-OfficialArchive $download 'https://pypi.nvidia.com/nvidia-vfx/nvidia_vfx-0.2.0.0-cp312-abi3-win_amd64.whl' 435807359
  if((File-Digest $download) -cne 'cebaa12411f7e22b39293205c5c4d07b8073d14ca1e0e7727ee4375207a36445'){throw 'NVIDIA download failed verification.'}
  [IO.File]::Move($download,$wheel)
 }
 Assert-Unlinked $wheel
 if((Get-Item -LiteralPath $wheel).Length -ne 435807359 -or (File-Digest $wheel) -cne 'cebaa12411f7e22b39293205c5c4d07b8073d14ca1e0e7727ee4375207a36445'){throw 'NVIDIA runtime failed verification.'}
 $zip=[IO.Compression.ZipFile]::OpenRead($wheel)
 try{
  foreach($p in $manifest.PSObject.Properties){if(!$p.Name.StartsWith('vfx/')){continue};Assert-Preparing
   $name=$p.Name.Substring(4)
   if($name.StartsWith('licenses/')){$entries=@($zip.Entries|Where-Object{$_.FullName -like '*.dist-info/licenses/*' -and $_.Name -ceq [IO.Path]::GetFileName($name)});if($entries.Count -ne 1){throw 'Missing NVIDIA license.'};$e=$entries[0]}
   else{$e=$zip.GetEntry('nvvfx/libs/'+$name)}
   if(!$e -or $e.Length -ne $p.Value.size){throw 'Missing NVIDIA component.'}
   $dest=Child-Path $stage $p.Name;[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest));[IO.Compression.ZipFileExtensions]::ExtractToFile($e,$dest,$false)
  }
 }finally{$zip.Dispose()}
 Verify-Native $stage;Assert-Preparing
 [IO.Directory]::Move($stage,$target);$stage=$null;Report 'ready' 1 1
}finally{
 if($stage -and $stage.StartsWith($root+'\.relay-render-stage-',[StringComparison]::OrdinalIgnoreCase) -and [IO.Directory]::Exists($stage)){[IO.Directory]::Delete($stage,$true)}
 if($lock){$lock.Dispose()};if($cancelEvent){$cancelEvent.Dispose()}
}
