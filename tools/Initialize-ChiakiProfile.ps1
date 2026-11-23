#requires -Version 5.1
<# Called AFTER tools/ensure-chiaki.ps1. Prepare a registry-isolated chiaki-ng GUI;
   never launch it, open personal settings, or place executable code in a profile.
   See docs/CHIAKI-PROFILE.md for the upstream source, exact call sites and launcher contract. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][ValidateNotNullOrEmpty()][string]$InstallRoot,
    [string]$ProfileRoot
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

$originalHash = 'c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec'
$imageSize = 6543375
$nameOffset = 0x221dbb
$bindingName = 'RelayPlayerProfileBindingV1'
$ascii = [Text.Encoding]::ASCII
$utf8 = [Text.UTF8Encoding]::new($false, $true)

function Hex([byte[]]$Bytes) { [BitConverter]::ToString($Bytes).Replace('-', '').ToLowerInvariant() }
function Absolute-Local([string]$Value) {
    # Rust can supply a verbatim DOS path. Canonicalize only a local drive path;
    # do not permit cwd-relative paths, alternate data streams or NT object names.
    if ($Value.StartsWith('\\?\') -and $Value.Substring(4) -match '^[A-Za-z]:[\\/]') { $Value = $Value.Substring(4) }
    if ($Value -notmatch '^[A-Za-z]:[\\/]' -or $Value.Substring(2).Contains(':')) { throw 'A fully qualified local path is required.' }
    $p = [IO.Path]::GetFullPath($Value.Replace('/', '\'))
    if ($p.Length -gt 3) { $p = $p.TrimEnd('\') }
    return $p
}
function Assert-Unlinked([string]$Path) {
    for ($p = $Path; $p; $p = [IO.Path]::GetDirectoryName($p)) {
        try { $attr = [IO.File]::GetAttributes($p) }
        catch [IO.FileNotFoundException] { continue }
        catch [IO.DirectoryNotFoundException] { continue }
        if ($attr -band [IO.FileAttributes]::ReparsePoint) { throw 'Profile runtime paths must not traverse links or junctions.' }
    }
}
function Read-Image([string]$Path) {
    Assert-Unlinked $Path
    $f = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($f.Length -ne $imageSize) { throw 'Unexpected chiaki-ng image length.' }
        $reader = [IO.BinaryReader]::new($f)
        try { $bytes = $reader.ReadBytes($imageSize); if ($bytes.Length -ne $imageSize) { throw 'Incomplete chiaki-ng image.' }; return ,$bytes }
        finally { $reader.Dispose() }
    } finally { $f.Dispose() }
}
function Assert-Text([byte[]]$Image, [int]$Offset, [string]$Expected) {
    $pattern = $ascii.GetBytes($Expected)
    if ($ascii.GetString($Image, $Offset, $pattern.Length) -cne $Expected) { throw 'Pinned upstream layout did not match.' }
}
function Assert-Setters([byte[]]$Image) {
    # SHA-256 also pins the untouched .text, import table, QML literal and PE headers.
    # Keep both rip-relative loads and both imported setters auditable at runtime.
    Assert-Text $Image ($nameOffset - 1) ("`0Chiaki`01.10.1`0chiaki-ng`0")
    Assert-Text $Image 0x22af6c ("`0Chiaki`0ChiakiSession`0")
    foreach ($i in @(0x1157c, 0x115c7)) {
        if ($Image[$i] -ne 0x48 -or $Image[$i+1] -ne 0x8d -or $Image[$i+2] -ne 0x05 -or
            $i + 0xc00 + 7 + [BitConverter]::ToInt32($Image,$i+3) -ne 0x223dbb) { throw 'Upstream Qt application-name references changed.' }
    }
    foreach ($pair in @(@(0x115ab,0x4da6a0),@(0x115ef,0x4da690))) {
        $i = $pair[0]
        if ($Image[$i] -ne 0xff -or $Image[$i+1] -ne 0x15 -or
            $i + 0xc00 + 6 + [BitConverter]::ToInt32($Image,$i+2) -ne $pair[1]) { throw 'Upstream Qt setter imports changed.' }
    }
}
function Lock-Mutex([Threading.Mutex]$Mutex) {
    try { if (!$Mutex.WaitOne(10000)) { throw 'Another player is preparing a backend. Retry shortly.' } }
    catch [Threading.AbandonedMutexException] { } # acquired; the registry and both files are reverified
}
function Claim-Registry([string]$Namespace, [string]$Binding) {
    # Use the full fingerprint to catch collisions in the fixed-length Qt literal.
    # Never enumerate third-party settings or adopt an existing unmarked namespace.
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $mutex = [Threading.Mutex]::new($false, "Global\RelayPlayer.ChiakiName.$sid.$Namespace")
    $owned = $false; $machine = $null; $current = $null; $key = $null; $systemKey = $null
    try {
        Lock-Mutex $mutex; $owned = $true
        $sub = 'Software\' + $Namespace
        $machine = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
        $systemKey = $machine.OpenSubKey($sub, $false)
        if ($null -ne $systemKey) { throw 'The profile namespace is already used by a system application.' }
        $current = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Registry64)
        $key = $current.OpenSubKey($sub, $false)
        if ($null -ne $key) {
            $stored = $key.GetValue($bindingName,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if ($stored -isnot [string] -or ![string]::Equals($stored, $Binding, [StringComparison]::Ordinal) -or
                $key.GetValueKind($bindingName) -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'The profile namespace is occupied. Existing settings were not changed.' }
        } else {
            $key = $current.CreateSubKey($sub)
            $key.SetValue($bindingName, $Binding, [Microsoft.Win32.RegistryValueKind]::String)
            $key.Flush()
        }
    } finally {
        if ($key) { $key.Dispose() }; if ($current) { $current.Dispose() }; if ($systemKey) { $systemKey.Dispose() }; if ($machine) { $machine.Dispose() }
        if ($owned) { $mutex.ReleaseMutex() }; $mutex.Dispose()
    }
}

$sha = [Security.Cryptography.SHA256]::Create()
$mutex = $null; $mutexOwned = $false; $tmpPath = $null
try {
    $root = Absolute-Local $InstallRoot
    $dir = [IO.Path]::Combine($root, 'chiaki-ng')
    $upstream = [IO.Path]::Combine($dir, 'chiaki.exe')
    $launch = [IO.Path]::Combine($dir, 'relay-profile.exe')
    Assert-Unlinked $launch
    $identity = 'default'
    if ($PSBoundParameters.ContainsKey('ProfileRoot')) {
        if ([string]::IsNullOrWhiteSpace($ProfileRoot)) { throw 'Explicit profile root is empty.' }
        # Input identifies the isolated user data. It never selects executable code.
        $profilePath = Absolute-Local $ProfileRoot
        Assert-Unlinked $profilePath
        $identity = 'root:' + $profilePath.ToUpperInvariant()
    }
    $finger = $sha.ComputeHash($utf8.GetBytes('RelayPlayer.Chiaki.Windows.profile.v1:' + $identity))
    $binding = Hex $finger
    $base32 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'; $slug = 'R'
    $packed = ([uint32]$finger[0] -shl 24) -bor ([uint32]$finger[1] -shl 16) -bor ([uint32]$finger[2] -shl 8) -bor [uint32]$finger[3]
    foreach ($shift in @(27,22,17,12,7)) { $slug += $base32[[int](($packed -shr $shift) -band 31)] }
    $rootId = Hex ($sha.ComputeHash($utf8.GetBytes($root.ToUpperInvariant())))
    $mutex = [Threading.Mutex]::new($false, 'Global\RelayPlayer.ChiakiImage.' + $rootId)
    Lock-Mutex $mutex; $mutexOwned = $true
    $bytes = Read-Image $upstream
    if ((Hex ($sha.ComputeHash($bytes))) -cne $originalHash) { throw 'Original chiaki-ng image failed SHA-256 validation.' }
    Assert-Setters $bytes
    [Array]::Copy($ascii.GetBytes($slug), 0, $bytes, $nameOffset, 6)
    $expected = Hex ($sha.ComputeHash($bytes))
    $exists = [IO.File]::Exists($launch); $write = $true
    if ($exists) {
        $before = Read-Image $launch
        if ((Hex ($sha.ComputeHash($before))) -ceq $expected) { $write = $false }
        else {
            # Refuse to reuse or overwrite a foreign or corrupt file, even if its name is ours.
            if ($ascii.GetString($before,$nameOffset,6) -cnotmatch '^R[A-Z2-7]{5}$') { throw 'Unexpected executable at the reserved profile path.' }
            [Array]::Copy($ascii.GetBytes('Chiaki'), 0, $before, $nameOffset, 6)
            if ((Hex ($sha.ComputeHash($before))) -cne $originalHash) { throw 'Stored profile executable failed verification.' }
        }
    }
    Claim-Registry $slug $binding
    if ($write) {
        # Never edit the vendor image or an existing file in place. The one installed
        # derived image lives beside its validated DLLs, even for an arbitrary profile root.
        $tmpPath = [IO.Path]::Combine($root, ([Guid]::NewGuid().ToString('N') + '.relay-profile-tmp'))
        $t = [IO.File]::Open($tmpPath,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try { $t.Write($bytes, 0, $bytes.Length); $t.Flush($true) } finally { $t.Dispose() }
        if ($exists) {
            try { [IO.File]::Replace($tmpPath, $launch, [System.Management.Automation.Language.NullString]::Value) }
            catch [IO.IOException] { throw 'Cannot switch the active profile image. Close the other player using this installation and retry.' }
        } else { [IO.File]::Move($tmpPath, $launch) }
        $tmpPath = $null
    }
    if ((Hex ($sha.ComputeHash((Read-Image $launch)))) -cne $expected) { throw 'Installed profile executable failed readback.' }
    [Console]::Out.WriteLine("RELAY_PROFILE $slug $expected")
} catch {
    $failure = $_.Exception
    if ($failure -is [Management.Automation.RuntimeException] -and $null -eq $failure.InnerException) {
        [Console]::Error.WriteLine('RelayPlayer profile setup: ' + $failure.Message)
    } else {
        # Do not send OS exception paths, registry details or other personal data to the host.
        [Console]::Error.WriteLine('RelayPlayer profile setup failed. Check folder permissions, close the other player, and retry.')
    }
    exit 1
} finally {
    if ($tmpPath -and [IO.File]::Exists($tmpPath)) { [IO.File]::Delete($tmpPath) }
    if ($mutexOwned) { $mutex.ReleaseMutex() }; if ($mutex) { $mutex.Dispose() }; $sha.Dispose()
}
