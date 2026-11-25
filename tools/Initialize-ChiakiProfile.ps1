#requires -Version 5.1
<# Called AFTER tools/ensure-chiaki.ps1. Prepare a registry-isolated chiaki-ng GUI;
   never launch it, open personal settings, or place executable code in a profile.
   See docs/CHIAKI-PROFILE.md for the upstream source, exact call sites and launcher contract. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][ValidateNotNullOrEmpty()][string]$InstallRoot,
    [string]$ProfileRoot,
    [switch]$CommunityNr,
    [switch]$RelayRender
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

$runtimeName = 'chiaki-ng'
$imageSize = 6543375
$nameOffset = 0x221dbb
$originalHash = 'c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec'
if ($CommunityNr) { $runtimeName = 'chiaki-nr'; $imageSize = 5536256; $nameOffset = 0x45d124; $originalHash = '3488b8a2c6564f006b1447dc42b67e0fbafc5293963ee3af073a3f95e7db3e26' }
if ($RelayRender) { $runtimeName = 'relay-render'; $imageSize = 5422080; $originalHash = '615a26dd956fb37466792d396bdd6ad2906017b71b9b993a1fda4908a57855f6' }
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

function Configure-Playback([string]$Namespace, [string]$Binding) {
    # Operate only inside the namespace we have claimed. Never read registered_hosts,
    # tokens, PINs or account identifiers. QSettings stores slash-separated keys as subkeys.
    $playback = $null
    if ($env:RELAY_PLAYBACK_SETTINGS) {
        $playback = ConvertFrom-Json $env:RELAY_PLAYBACK_SETTINGS
        foreach ($property in $playback.PSObject.Properties) {
            $valid = switch ($property.Name) {
                'resolution' { $property.Value -is [string] -and $property.Value -cin @('360p','540p','720p','1080p') }
                'codec' { $property.Value -is [string] -and $property.Value -cin @('h264','hevc','h265','h265_hdr') }
                'fps' { ($property.Value -is [int] -or $property.Value -is [long]) -and $property.Value -in @(30,60) }
                'bitrateKbps' { ($property.Value -is [int] -or $property.Value -is [long]) -and $property.Value -ge 1000 -and $property.Value -le 200000 }
                'neuralRendering' { $property.Value -is [bool] }
                default { $false }
            }
            if (!$valid) { throw 'Unsupported playback setting.' }
        }
    }
    $current = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser,[Microsoft.Win32.RegistryView]::Registry64)
    $org = $null
    try {
        $org = $current.OpenSubKey(('Software\' + $Namespace),$true)
        if ($null -eq $org -or $org.GetValue($bindingName) -cne $Binding) { throw 'Playback profile binding changed.' }
        $apps = [Collections.Generic.List[string]]::new(); $apps.Add($Namespace)
        $base = $org.OpenSubKey(($Namespace + '\settings'),$false)
        try {
            if ($base) {
                $selected = $base.GetValue('current_profile')
                if ($selected -is [string] -and $selected.Length -gt 0) {
                    if ($selected.Length -gt 240 -or $selected.Contains('\') -or $selected.Contains('/')) { throw 'Unsupported selected profile name.' }
                    $apps.Add($Namespace + '-' + $selected)
                }
            }
        } finally { if ($base) { $base.Dispose() } }
        foreach ($app in $apps) {
            $settings = $org.CreateSubKey(($app + '\settings'))
            try {
                $settings.SetValue('allow_joystick_background_events','true',[Microsoft.Win32.RegistryValueKind]::String)
                # Relay supplies the connection panel and transport. Do not open
                # a second management menu from the controller's stream shortcut.
                if ($playback -or $RelayRender) {
                    $settings.SetValue('stream_menu_enabled','false',[Microsoft.Win32.RegistryValueKind]::String)
                    $settings.SetValue('show_stream_stats','false',[Microsoft.Win32.RegistryValueKind]::String)
                    $settings.SetValue('log_sanitize','true',[Microsoft.Win32.RegistryValueKind]::String)
                }
                if ($RelayRender) {
                    # No saved native menu value can enable FG before player settings arrive.
                    $settings.SetValue('frame_gen_backend',0,[Microsoft.Win32.RegistryValueKind]::DWord)
                    $settings.SetValue('frame_gen_multiplier',2,[Microsoft.Win32.RegistryValueKind]::DWord)
                }
                # SDL_AudioSpec.samples is sample frames and must be a power of two.
                # The upstream byte-based default (9600 = 2400 stereo S16 frames)
                # and slider steps violate that requirement. Keep valid custom sizes.
                $raw = $settings.GetValue('audio_buffer_size')
                $size = [long]0
                if ($null -eq $raw -or ![long]::TryParse([string]$raw,[ref]$size) -or $size -le 0) { $size = 16384 }
                $normalized = [long]65536
                foreach ($allowed in @(2048,4096,8192,16384,32768,65536)) { if ($size -le $allowed) { $normalized = $allowed; break } }
                # The Relay renderer pulls a single bounded PCM queue directly
                # from the device callback. Migrate the previous 16 KiB safety
                # default instead of preserving its 85 ms device period.
                if ($RelayRender) { $normalized = 2048 }
                $settings.SetValue('audio_buffer_size',[int]$normalized,[Microsoft.Win32.RegistryValueKind]::DWord)
                $settings.SetValue('neural_render',$(if ($playback -and $playback.neuralRendering) { 'true' } elseif (!$playback -and $CommunityNr) { 'true' } else { 'false' }),[Microsoft.Win32.RegistryValueKind]::String)
                if ($CommunityNr -or $RelayRender) {
                    $settings.SetValue('render_backend','vulkan',[Microsoft.Win32.RegistryValueKind]::String)
                    # Begin at the input resolution: avoids unnecessary upscaling cost.
                    if ($null -eq $settings.GetValue('neural_render_upscale')) { $settings.SetValue('neural_render_upscale','false',[Microsoft.Win32.RegistryValueKind]::String) }
                }
                if ($playback) {
                    foreach ($property in $playback.PSObject.Properties) {
                        $key = switch ($property.Name) {
                            'resolution' { 'resolution' }
                            'codec' { 'codec' }
                            'fps' { 'fps' }
                            'bitrateKbps' { 'bitrate' }
                            default { $null }
                        }
                        if ($key) {
                            $value = $property.Value
                            if ($property.Name -ceq 'codec' -and $value -ceq 'hevc') { $value = 'h265' }
                            $kind = if ($value -is [string]) { [Microsoft.Win32.RegistryValueKind]::String } else { [Microsoft.Win32.RegistryValueKind]::DWord }
                            foreach ($location in @('local','remote')) {
                                $name = $key + '_' + $location + '_ps5'
                                $settings.SetValue($name,$value,$kind)
                                if ([string]$settings.GetValue($name) -cne [string]$value) { throw 'Playback setting readback failed.' }
                            }
                        }
                    }
                }
                $settings.Flush()
            } finally { $settings.Dispose() }
        }
    } finally { if ($org) { $org.Dispose() }; $current.Dispose() }
}

$sha = [Security.Cryptography.SHA256]::Create()
$mutex = $null; $mutexOwned = $false; $tmpPath = $null
try {
    $root = Absolute-Local $InstallRoot
    $dir = [IO.Path]::Combine($root, $runtimeName)
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
    if (!$CommunityNr -and !$RelayRender) { Assert-Setters $bytes }
    if (!$RelayRender) { Update-MotionImage $bytes; [Array]::Copy($ascii.GetBytes($slug), 0, $bytes, $nameOffset, 6) }
    $expected = Hex ($sha.ComputeHash($bytes))
    $exists = [IO.File]::Exists($launch); $write = $true
    if ($exists) {
        $before = Read-Image $launch
        if ((Hex ($sha.ComputeHash($before))) -ceq $expected) { $write = $false }
        else {
            # Refuse to reuse or overwrite a foreign or corrupt file, even if its name is ours.
            if ($RelayRender) { throw 'Stored native renderer failed verification.' }
            if ($ascii.GetString($before,$nameOffset,6) -cnotmatch '^R[A-Z2-7]{5}$') { throw 'Unexpected executable at the reserved profile path.' }
            Update-MotionImage $before $true
            [Array]::Copy($ascii.GetBytes('Chiaki'), 0, $before, $nameOffset, 6)
            if ((Hex ($sha.ComputeHash($before))) -cne $originalHash) { throw 'Stored profile executable failed verification.' }
        }
    }
    Claim-Registry $slug $binding
    Configure-Playback $slug $binding
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
