"""Windows-only isolated integration check; never use the caller's real Chiaki profile.

python tests/test_chiaki_profile.py --archive PATH/official.zip --work PATH/empty-qa-parent
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import math
import os
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import uuid
import winreg
import zipfile

ARCHIVE_HASH = '6ca3f5e6780608104d79345ab7755cfa77fefc33a41ae10651a874c43f54ea57'
IMAGE_HASH = 'c69a1e2dbf7406f74ae8a33c7c98fba226d79431a36aca5bc7143cedc4d19dec'
MARKER = 'RelayPlayerProfileBindingV1'
OFFSET = 0x221DBB
COMMUNITY_NR = False
HOST_FIXTURE = None
MOTION_PATCHES = [(0x4823EC, bytes.fromhex('3480b739'), b'\0'*4),
                  (0x4823F0, bytes.fromhex('3480373a'), b'\0'*4),
                  (0x4823FC, bytes.fromhex('3480b73a'), b'\0'*4),
                  (0x66CE7, b'\0', b'\1')]
ORIENTATION_RVA = 0xE9730
SCRIPT = Path(__file__).resolve().parents[1] / 'tools' / 'Initialize-ChiakiProfile.ps1'
POWERSHELL = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
REGBASE = winreg.HKEY_CURRENT_USER
REGVIEW = winreg.KEY_WOW64_64KEY


def digest(value):
    return hashlib.sha256(value).hexdigest()


def details(root):
    binding = hashlib.sha256(('RelayPlayer.Chiaki.Windows.profile.v1:root:' + str(root).upper()).encode())
    return 'R' + base64.b32encode(binding.digest()).decode()[:5], binding.hexdigest()


def org_path(slug):
    assert re.fullmatch(r'R[A-Z2-7]{5}', slug)
    return 'Software\\' + slug


def fresh_profiles(work, count):
    chosen = {}
    while len(chosen) != count:
        root = work / ('unique-profile-' + uuid.uuid4().hex)
        slug, binding = details(root)
        if slug in chosen:
            continue
        # Do not inspect any preexisting settings; choose an unused namespace.
        try:
            with winreg.OpenKey(REGBASE, org_path(slug), 0, winreg.KEY_READ | REGVIEW):
                continue
        except FileNotFoundError:
            pass
        root.mkdir()
        chosen[slug] = (root, binding)
    return [(root, slug, binding) for slug, (root, binding) in chosen.items()]


def clean_isolated_org(slug, binding):
    top = org_path(slug)
    try:
        with winreg.OpenKey(REGBASE, top, 0, winreg.KEY_READ | REGVIEW) as k:
            assert winreg.QueryValueEx(k, MARKER)[0] == binding, 'Will not remove an unowned registry subtree'
    except FileNotFoundError:
        return

    def remove(path):
        assert path == top or path.startswith(top + '\\')
        with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_READ | REGVIEW) as k:
            children = [winreg.EnumKey(k, i) for i in range(winreg.QueryInfoKey(k)[0])]
        for child in children:
            remove(path + '\\' + child)
        winreg.DeleteKeyEx(REGBASE, path, REGVIEW, 0)
    remove(top)


def prepare(install, profile, *, success=True, playback=None):
    root, slug, _ = profile
    args = [str(POWERSHELL), '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-File', str(SCRIPT), '-InstallRoot', str(install), '-ProfileRoot', str(root)]
    if COMMUNITY_NR:
        args.append('-CommunityNr')
    env = dict(os.environ)
    env.pop('RELAY_PLAYBACK_SETTINGS', None)
    if playback is not None: env['RELAY_PLAYBACK_SETTINGS'] = json.dumps(playback)
    p = subprocess.run(args, capture_output=True, check=False, timeout=25, env=env)
    line = p.stdout.decode('ascii', 'replace').strip()
    if not success:
        assert p.returncode != 0 and 'RELAY_PROFILE ' not in line, 'unexpected launch proof for invalid fixture'
        return None
    assert p.returncode == 0, 'Profile setup failed: ' + p.stderr.decode(errors='replace')
    m = re.fullmatch(r'RELAY_PROFILE (R[A-Z2-7]{5}) ([a-f0-9]{64})', line)
    assert m and m[1] == slug
    return m[2]


def list_known_fixture(engine, expected):
    # `list` loads the fresh QSettings and returns before opening a discovery service.
    env = dict(os.environ, QT_QPA_PLATFORM='windows', QT_COMMAND_LINE_PARSER_NO_GUI_MESSAGE_BOXES='1',
               SDL_AUDIODRIVER='dummy')
    p = subprocess.run([str(engine), 'list'], cwd=engine.parent, capture_output=True, timeout=15, env=env)
    assert p.returncode == 0 and p.stdout.decode('ascii').strip() == expected, 'Synthetic console visibility differs'


def bind_demo_host(profile):
    _, slug, binding = profile
    with winreg.OpenKey(REGBASE, org_path(slug), 0, winreg.KEY_READ | REGVIEW) as k:
        assert winreg.QueryValueEx(k, MARKER)[0] == binding
        with winreg.OpenKey(k, slug) as own:
            assert winreg.QueryValueEx(own, 'version')[0] == 2
        with winreg.OpenKey(k, 'pl_render_params'):
            pass
    # Only a label; no account id, address, console PIN or usable pairing key.
    with winreg.CreateKeyEx(REGBASE, org_path(slug) + '\\' + slug + '\\registered_hosts', 0, winreg.KEY_WRITE | REGVIEW) as a:
        winreg.SetValueEx(a, 'size', 0, winreg.REG_DWORD, 1)
        with winreg.CreateKeyEx(a, '1', 0, winreg.KEY_WRITE | REGVIEW) as one:
            winreg.SetValueEx(one, 'server_nickname', 0, winreg.REG_SZ, 'RelayPlayer-QA-only')


def edit_one(path):
    with path.open('r+b') as f:
        f.seek(0x500)
        byte = f.read(1)[0]
        f.seek(0x500)
        f.write(bytes([byte ^ 1]))


def check_motion_math(runner):
    # Map code without calling the program entry point, resolving DLLs, or starting discovery.
    # Exercise the actual reviewed pure quaternion function in the generated executable.
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    load = kernel.LoadLibraryExW
    load.argtypes = [wintypes.LPCWSTR, ctypes.c_void_p, wintypes.DWORD]
    load.restype = ctypes.c_void_p
    free = kernel.FreeLibrary; free.argtypes = [ctypes.c_void_p]
    class Quaternion(ctypes.Structure):
        _fields_ = [(s, ctypes.c_float) for s in ('x', 'y', 'z', 'w')]
    handle = load(str(runner), None, 1)  # DONT_RESOLVE_DLL_REFERENCES
    assert handle, ctypes.get_last_error()
    try:
        update = ctypes.CFUNCTYPE(None, ctypes.POINTER(Quaternion), *([ctypes.c_float]*8))(handle + ORIENTATION_RVA)
        q = Quaternion(math.sqrt(.5), 0, 0, math.sqrt(.5))
        for _ in range(1000): update(ctypes.byref(q), 0, 0, .01, 0, 0, 0, 0, .001)
        # Previously every small step was discarded: y/z remained exactly zero.
        assert abs(q.y + math.sin(.005)*math.sqrt(.5)) < 2e-6
        assert abs(q.z - math.sin(.005)*math.sqrt(.5)) < 2e-6
        assert abs(sum(getattr(q, s)**2 for s in ('x','y','z','w')) - 1) < 1e-5
        for _ in range(1000): update(ctypes.byref(q), 0, 0, -.01, 0, 0, 0, 0, .001)
        assert abs(q.y) < 2e-6 and abs(q.z) < 2e-6
    finally:
        assert free(handle)


def check_playback_settings(install, profile):
    _, slug, _ = profile
    path = org_path(slug) + '\\' + slug + '\\settings'
    with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_READ | REGVIEW) as key:
        assert winreg.QueryValueEx(key, 'allow_joystick_background_events')[0] == 'true'
        assert winreg.QueryValueEx(key, 'neural_render')[0] == ('true' if COMMUNITY_NR else 'false')
        assert winreg.QueryValueEx(key, 'audio_buffer_size')[0] == 16384
        if COMMUNITY_NR:
            assert winreg.QueryValueEx(key, 'render_backend')[0] == 'vulkan'
    for before, expected in [(8192,8192), (9600,16384), (19200,32768), (0,16384)]:
        with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_SET_VALUE | REGVIEW) as key:
            winreg.SetValueEx(key, 'audio_buffer_size', 0, winreg.REG_DWORD, before)
        prepare(install, profile)
        with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_READ | REGVIEW) as key:
            assert winreg.QueryValueEx(key, 'audio_buffer_size')[0] == expected
    with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_SET_VALUE | REGVIEW) as key:
        winreg.SetValueEx(key, 'current_profile', 0, winreg.REG_SZ, 'Synthetic-QA')
    prepare(install, profile)
    with winreg.OpenKey(REGBASE, org_path(slug) + '\\' + slug + '-Synthetic-QA\\settings', 0, winreg.KEY_READ | REGVIEW) as key:
        assert winreg.QueryValueEx(key, 'audio_buffer_size')[0] == 16384
        assert winreg.QueryValueEx(key, 'neural_render')[0] == ('true' if COMMUNITY_NR else 'false')
    prepare(install, profile, playback={'resolution':'720p','fps':30,'codec':'hevc','bitrateKbps':200000,'neuralRendering':COMMUNITY_NR})
    for app in [slug, slug + '-Synthetic-QA']:
        with winreg.OpenKey(REGBASE, org_path(slug) + '\\' + app + '\\settings', 0, winreg.KEY_READ | REGVIEW) as key:
            for location in ['local','remote']:
                for name, value in [('resolution','720p'),('fps',30),('codec','h265'),('bitrate',200000)]:
                    assert winreg.QueryValueEx(key, f'{name}_{location}_ps5')[0] == value
            assert winreg.QueryValueEx(key,'stream_menu_enabled')[0] == 'false'
    prepare(install, profile, playback={'bitrateKbps':200001},success=False)
    prepare(install, profile, playback={'upscaler':'invented'},success=False)
    with winreg.OpenKey(REGBASE, path, 0, winreg.KEY_SET_VALUE | REGVIEW) as key:
        winreg.DeleteValue(key, 'current_profile')


def check_fixture(work, archive):
    install = work / 'backends'; vendor = install / ('chiaki-nr' if COMMUNITY_NR else 'chiaki-ng'); vendor.mkdir(parents=True)
    with archive.open('rb') as f:
        assert hashlib.file_digest(f, 'sha256').hexdigest() == ARCHIVE_HASH
        f.seek(0)
        with zipfile.ZipFile(f) as z:
            for info in z.infolist():
                root, name = info.filename.replace('\\','/').split('/', 1)
                assert root == 'chiaki-ng-Win'
                out = (vendor / name).resolve(); assert out == vendor or out.is_relative_to(vendor)
                if info.is_dir():
                    out.mkdir(exist_ok=True, parents=True)
                else:
                    out.parent.mkdir(exist_ok=True, parents=True)
                    with z.open(info) as src, out.open('xb') as dst:
                        shutil.copyfileobj(src, dst)
    upstream = vendor / 'chiaki.exe'; runner = vendor / 'relay-profile.exe'; original = upstream.read_bytes()
    original_exes = {x.name for x in vendor.glob('*.exe')}
    assert digest(original) == IMAGE_HASH
    pa, pb = fresh_profiles(work, 2)
    try:
        a = prepare(install, pa)
        data = runner.read_bytes()
        patched = bytearray(original); patched[OFFSET:OFFSET+6] = pa[1].encode()
        for off, before, after in MOTION_PATCHES:
            assert patched[off:off+len(before)] == before
            patched[off:off+len(after)] = after
        assert digest(data) == a and data == patched
        assert upstream.read_bytes() == original
        if not COMMUNITY_NR: assert data[0x22AF6D:0x22AF74] == b'Chiaki\x00'
        check_motion_math(runner)
        check_playback_settings(install, pa)
        # Existing v0.1.8 has only the profile-name edit; upgrade it without changing settings.
        old = bytearray(original); old[OFFSET:OFFSET+6] = pa[1].encode()
        runner.write_bytes(old)
        assert prepare(install, pa) == a and runner.read_bytes() == patched
        # A partial patch must never be accepted as either the old or the current derivative.
        partial = bytearray(patched); off, before, after = MOTION_PATCHES[-1]
        partial[off:off+len(before)] = before
        runner.write_bytes(partial); prepare(install, pa, success=False)
        runner.write_bytes(patched)
        list_known_fixture(runner, '')
        if HOST_FIXTURE:
            subprocess.run([str(HOST_FIXTURE), '--native-pair-fixture', str(pa[0])], check=True, capture_output=True, timeout=15)
            list_known_fixture(runner, 'Host: Relay_020000000001')
            base = org_path(pa[1]) + '\\' + pa[1] + '\\registered_hosts'
            with winreg.OpenKey(REGBASE, base, 0, winreg.KEY_READ | REGVIEW) as key:
                assert winreg.QueryValueEx(key,'size')[0] == 1
            winreg.DeleteKeyEx(REGBASE, base + '\\1', REGVIEW)
            winreg.DeleteKeyEx(REGBASE, base, REGVIEW)
        bind_demo_host(pa); list_known_fixture(runner, 'Host: RelayPlayer-QA-only')
        stamp = runner.stat().st_mtime_ns
        assert prepare(install, pa) == a and runner.stat().st_mtime_ns == stamp
        # Simulate the Rust launcher's no-write/no-delete handle during a live session.
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        create = kernel.CreateFileW
        create.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                           wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        create.restype = wintypes.HANDLE
        close = kernel.CloseHandle; close.argtypes = [wintypes.HANDLE]; close.restype = wintypes.BOOL
        h = create(str(runner), 0x80000000, 1, None, 3, 0x80, None)
        assert h not in (None, ctypes.c_void_p(-1).value)
        try:
            assert prepare(install, pa) == a
            prepare(install, pb, success=False)
            assert digest(runner.read_bytes()) == a
        finally:
            assert close(h)
        b = prepare(install, pb); assert b != a
        list_known_fixture(runner, '')
        assert prepare(install, pa) == a; list_known_fixture(runner, 'Host: RelayPlayer-QA-only')
        # A six-character collision must not adopt another application's registry subtree.
        with winreg.OpenKey(REGBASE, org_path(pb[1]), 0, winreg.KEY_SET_VALUE | REGVIEW) as k:
            try:
                for kind, wrong in [(winreg.REG_SZ, 'unrelated-test-binding'), (winreg.REG_MULTI_SZ, [])]:
                    winreg.SetValueEx(k, MARKER, 0, kind, wrong)
                    prepare(install, pb, success=False)
            finally:
                winreg.SetValueEx(k, MARKER, 0, winreg.REG_SZ, pb[2])
        assert digest(runner.read_bytes()) == a
        edit_one(upstream); prepare(install, pa, success=False); edit_one(upstream)
        edit_one(runner); prepare(install, pa, success=False)
        # The roots that identify private settings remain data-only, including on failure.
        assert all(p.relative_to(pa[0]).parts[0]=='data' and (p.is_dir() or p.name=='public-connections.bin') for p in pa[0].rglob('*'))
        assert not any(pb[0].iterdir())
        assert {x.name for x in vendor.glob('*.exe')} == original_exes | {'relay-profile.exe'}
    finally:
        for _, slug, binding in (pa, pb): clean_isolated_org(slug, binding)


def main():
    global ARCHIVE_HASH, IMAGE_HASH, OFFSET, COMMUNITY_NR, MOTION_PATCHES, ORIENTATION_RVA, HOST_FIXTURE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path)
    parser.add_argument('--work', required=True, type=Path)
    parser.add_argument('--community-nr', action='store_true')
    parser.add_argument('--host-fixture-exe',type=Path)
    args = parser.parse_args(); base = args.work.resolve(strict=True)
    HOST_FIXTURE = args.host_fixture_exe.resolve(strict=True) if args.host_fixture_exe else None
    if args.community_nr:
        COMMUNITY_NR = True
        ARCHIVE_HASH = 'b07e58000ebf38545518c955ad9462ad8fac30dc8e0438ff44d5119d93c91ebe'
        IMAGE_HASH = '3488b8a2c6564f006b1447dc42b67e0fbafc5293963ee3af073a3f95e7db3e26'
        OFFSET = 0x45D124; ORIENTATION_RVA = 0x12A830
        MOTION_PATCHES = [(off,bytes.fromhex(before),b'\0'*4) for off,before in
                          [(0x44E6C4,'3480b739'),(0x44E6C8,'3480b7b9'),(0x44E6CC,'3480373a'),
                           (0x44E6D0,'348037ba'),(0x44E6D4,'3480b73a'),(0x44E6D8,'3480b7ba')]] + [(0x65CFA,b'\0',b'\1')]
    with tempfile.TemporaryDirectory(dir=base, prefix='chiaki-profile-test-') as tmp:
        work = Path(tmp).resolve(); assert work.parent == base and work.name.startswith('chiaki-profile-test-')
        check_fixture(work, args.archive.resolve(strict=True))
    print('PASS: actual motion math, playback settings/migration, genuine Qt list, synthetic host isolation/persistence, unchanged upstream, collision, lock, and corruption rejection.')


if __name__ == '__main__':
    main()
