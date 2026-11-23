"""Windows-only isolated integration check; never use the caller's real Chiaki profile.

python tests/test_chiaki_profile.py --archive PATH/official.zip --work PATH/empty-qa-parent
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import os
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


def prepare(install, profile, *, success=True):
    root, slug, _ = profile
    args = [str(POWERSHELL), '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-File', str(SCRIPT), '-InstallRoot', str(install), '-ProfileRoot', str(root)]
    p = subprocess.run(args, capture_output=True, check=False, timeout=25)
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


def check_fixture(work, archive):
    install = work / 'backends'; vendor = install / 'chiaki-ng'; vendor.mkdir(parents=True)
    with archive.open('rb') as f:
        assert hashlib.file_digest(f, 'sha256').hexdigest() == ARCHIVE_HASH
        f.seek(0)
        with zipfile.ZipFile(f) as z:
            for info in z.infolist():
                root, name = info.filename.split('/', 1)
                assert root == 'chiaki-ng-Win'
                out = (vendor / name).resolve(); assert out == vendor or out.is_relative_to(vendor)
                if info.is_dir():
                    out.mkdir(exist_ok=True, parents=True)
                else:
                    out.parent.mkdir(exist_ok=True, parents=True)
                    with z.open(info) as src, out.open('xb') as dst:
                        shutil.copyfileobj(src, dst)
    upstream = vendor / 'chiaki.exe'; runner = vendor / 'relay-profile.exe'; original = upstream.read_bytes()
    assert digest(original) == IMAGE_HASH
    pa, pb = fresh_profiles(work, 2)
    try:
        a = prepare(install, pa)
        data = runner.read_bytes()
        assert digest(data) == a and data == original[:OFFSET] + pa[1].encode() + original[OFFSET+6:]
        assert upstream.read_bytes() == original and data[0x22AF6D:0x22AF74] == b'Chiaki\x00'
        list_known_fixture(runner, '')
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
        assert not any(pa[0].iterdir()) and not any(pb[0].iterdir())
        assert sorted(x.name for x in vendor.glob('*.exe')) == ['chiaki.exe', 'relay-profile.exe']
    finally:
        for _, slug, binding in (pa, pb): clean_isolated_org(slug, binding)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path)
    parser.add_argument('--work', required=True, type=Path)
    args = parser.parse_args(); base = args.work.resolve(strict=True)
    with tempfile.TemporaryDirectory(dir=base, prefix='chiaki-profile-test-') as tmp:
        work = Path(tmp).resolve(); assert work.parent == base and work.name.startswith('chiaki-profile-test-')
        check_fixture(work, args.archive.resolve(strict=True))
    print('PASS: genuine Qt list, synthetic host isolation/persistence, unchanged upstream, collision, lock, and corruption rejection.')


if __name__ == '__main__':
    main()
