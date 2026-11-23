"""Package the public RelayPlayer host from v0.1.7. Python 3.11+ and Git.

Build RelayPlayer.exe from that tag first. This tool checks its format and
public interface markers; it does not prove binary build provenance. Source and
document bytes come only from Git blobs. No data directory is opened or walked.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import tomllib
import zipfile

VERSION = "0.1.7"
TAG = "v" + VERSION
HOST = f"RelayPlayer-{VERSION}-windows-x64-host.zip"
SOURCE = f"RelayPlayer-{VERSION}-public-source.zip"
SUMS = "SHA256SUMS.txt"
DOCS = ("README.md", "docs/BACKEND.md", "docs/releases/v0.1.7.md",
        "notices/RUST-THIRD-PARTY.txt", "notices/RUST-STANDARD-LIBRARY.html")
# These three binary inputs are the reviewed output of tools/make_icon.py.
ICONS = {
    "assets/relay.ico": "70367948c24a0564c3d74ad35ffbbcb49bc789bbdd93f2a2f8461108af7db4f3",
    "assets/relay.png": "3c604cedf5d940510ba72c42cf0830d6a9234f9db0847759b0b8a88408e4f2de",
    "assets/icon.rgba": "75a7127c1a1890c6d28943d3be15ba5fd163815239c5add47a9fecf892c736d7",
}
FILES = frozenset((".gitattributes", ".gitignore", "Cargo.toml", "Cargo.lock", "build.rs",
                   "tools/make_icon.py", "tools/package_public.py", *DOCS, *ICONS,
                   *("src/" + s + ".rs" for s in ("backend", "backend_protocol_test", "design", "main", "migration",
                     "migration_settings_tests", "panels", "platform", "platform_tests"))))
DIRS = {p.rsplit("/", 1)[0] for p in FILES if "/" in p}
OID = re.compile(rb"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
MIB = 1024 * 1024


class Rejected(ValueError):
    pass


def require(ok, why):
    if not ok:
        raise Rejected(why)


# Look for materialized values, not API names, author emails or fixture filenames.
# Do not print matching text; it may be private.
RULES = {label: re.compile(expr, re.I) for label, expr in (
    ("private key header", rb"-----BEGIN (?:[A-Z0-9 ]{1,22} )?PRIVATE KEY-----"),
    ("service token", rb"(?:gh[pousr]_[0-9a-z]{32,}|github_pat_[0-9a-z_]{40,}|glpat-[0-9a-z_-]{20,}|xox[abprs]-[0-9a-z-]{24,}|sk-(?:proj-|svcacct-)?[0-9a-z_-]{32,}|AKIA[0-9A-Z]{16})"),
    ("signed token", rb"eyJ[A-Za-z0-9_-]{7,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{16,}"),
    ("credential value", rb"[\"']?(?:access[_-]?token|refresh[_-]?token|client[_-]?secret|password|rp[_-]?(?:regist[_-]?)?key|registration[_-]?key)[\"']?\s*[:=]\s*[\"'][A-Za-z0-9+_./=~-]{16,}[\"']"),
    ("authorization URL", rb"[?&](?:code|access_token|refresh_token)=[A-Za-z0-9+_./%~-]{16,}"),
    ("materialized sign-in cookie", rb"\bnpsso[\"']?\s*[:=]\s*[\"']?[a-z0-9._~+-]{48,}"),
    ("device identifier", rb"[\"']duid[\"']\s*:\s*[\"'][A-Fa-f0-9]{64}[\"']"),
    ("bearer credential", rb"\bBearer\s+[0-9a-z_./+~=-]{24,}"),
    ("encoded user vault", rb"AQAAANCMnd8BFdERjHoAwE/Cl\+s[A-Za-z0-9+/=]{20,}"),
    ("local profile path", rb"[A-Z]:[\\/]+Users[\\/]+"),
)}
WIDE_ASCII = re.compile(rb"(?:[\x20-\x7e]\x00){8,}")


def scan_values(data, label):
    for reason, pattern in RULES.items():
        require(not pattern.search(data), f"{label}: detected {reason}; matching value withheld")


def git(root, *args):
    # Read from this checkout, regardless of an ambient GIT_DIR/INDEX_FILE.
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("GIT_")}
    env.update(GIT_NO_REPLACE_OBJECTS="1", GIT_NO_LAZY_FETCH="1", GIT_OPTIONAL_LOCKS="0")
    proc = subprocess.run(["git", "-C", os.fspath(root), "-c", "core.fsmonitor=false", *args],
                          env=env, capture_output=True, check=False)
    require(proc.returncode == 0, "Git could not read " + args[0] + " for " + TAG)
    return proc.stdout


def tree_map(raw):
    found = {}
    for row in raw.split(b"\0"):
        if not row:
            continue
        fields, path = row.split(b"\t", 1)
        mode, kind, oid, size = fields.split()
        name = path.decode("utf-8")
        if kind == b"tree":
            require(mode == b"040000" and name in DIRS, "unreviewed directory in tagged tree")
            continue
        require(kind == b"blob" and mode in (b"100644", b"100755") and OID.fullmatch(oid)
                and name in FILES and name not in found, "unreviewed path or file type in tagged tree")
        require(size.isdigit() and 0 < int(size) <= 2*MIB, "unexpected source size: " + name)
        found[name] = (mode, oid, int(size))
    require(found.keys() == FILES, "tag is missing a required public source file")
    return found


def check_index(raw, tree):
    seen = set()
    for row in raw.split(b"\0"):
        if not row:
            continue
        fields, path = row.split(b"\t", 1)
        mode, oid, stage = fields.split()
        name = path.decode("utf-8")
        require(name in tree and name not in seen and stage == b"0" and (mode, oid) == tree[name][:2],
                "Git index differs from the reviewed tag; check staged files")
        seen.add(name)
    require(seen == FILES, "Git index is missing a tagged file")


def tagged_source(root):
    top = git(root, "rev-parse", "--show-toplevel").decode("utf-8").strip()
    require(Path(top).resolve() == root.resolve(), "run the tool from the public repository")
    tag = git(root, "rev-parse", "--verify", "refs/tags/" + TAG).strip()
    head = git(root, "rev-parse", "--verify", "HEAD").strip()
    require(OID.fullmatch(tag) and head == tag and git(root, "cat-file", "-t", tag.decode()).strip() == b"commit",
            "HEAD must be the exact commit of the lightweight tag " + TAG)
    tree = tree_map(git(root, "ls-tree", "-r", "-t", "-l", "-z", tag.decode()))
    check_index(git(root, "ls-files", "--stage", "-z"), tree)
    files = {}
    for name, (_, oid, size) in tree.items():
        data = git(root, "cat-file", "blob", oid.decode())
        framed = b"blob " + str(size).encode() + b"\0" + data
        algo = "sha1" if len(oid) == 40 else "sha256"
        require(len(data) == size and hashlib.new(algo, framed).hexdigest().encode() == oid,
                "Git blob did not verify: " + name)
        if name in ICONS:
            require(hashlib.sha256(data).hexdigest() == ICONS[name], "unreviewed icon: " + name)
        else:
            data.decode("utf-8")
            require(b"\0" not in data, "binary bytes in a public source text: " + name)
            scan_values(data, name)
        files[name] = data
    require(tomllib.loads(files["Cargo.toml"].decode())["package"]["version"] == VERSION, "wrong Cargo version")
    return files


def check_exe(data):
    require(len(data) >= 256 and data[:2] == b"MZ", "not a PE executable")
    pe, = struct.unpack_from("<I", data, 60)
    require(64 <= pe <= len(data)-24 and data[pe:pe+4] == b"PE\0\0", "invalid PE header")
    machine, sections, _, _, _, opt_size, flags = struct.unpack_from("<HHIIIHH", data, pe+4)
    opt = pe + 24
    start = opt + opt_size
    require(machine == 0x8664 and 1 <= sections <= 96 and flags & 2 and not flags & 0x2000
            and opt_size >= 112 and start+40*sections <= len(data)
            and data[opt:opt+2] == b"\x0b\x02" and data[opt+68:opt+70] == b"\x02\0",
            "expected a Windows x64 GUI executable")
    last, runnable = start + sections*40, False
    for i in range(sections):
        pos = start+40*i
        size, offset = struct.unpack_from("<II", data, pos+16)
        require(size == 0 or (offset >= start+sections*40 and offset+size <= len(data)), "invalid PE section")
        if size:
            last = max(last, offset+size)
        runnable |= size > 0 and data[pos:pos+8].rstrip(b"\0") == b".text" and bool(data[pos+39] & 0x20)
    require(runnable and not any(data[last:]), "expected a code section and no appended payload")
    for marker in ("relay-backend.exe", "RELAY_HOST_PID"):
        require(marker.encode() in data or marker.encode("utf-16le") in data,
                "executable is missing the public backend contract")
    scan_values(data, "RelayPlayer.exe")
    for match in WIDE_ASCII.finditer(data):
        scan_values(match.group()[::2], "RelayPlayer.exe (UTF-16)")


def check_path(path):
    for part in (path, *path.parents):
        try:
            info = part.lstat()
        except FileNotFoundError:
            continue
        require(not stat.S_ISLNK(info.st_mode) and not (getattr(info, "st_file_attributes", 0) & 0x400),
                "an input or output path passes through a symlink or reparse point")


def make_zip(path, files):
    with zipfile.ZipFile(path, "x") as z:
        for n in sorted(files):
            item = zipfile.ZipInfo(n, date_time=(1980, 1, 1, 0, 0, 0))
            item.create_system = 3
            item.external_attr = 0o100644 << 16
            z.writestr(item, files[n], compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
    with zipfile.ZipFile(path) as z:
        require(z.namelist() == sorted(files), "ZIP membership or order differs from the manifest")
        for item in z.infolist():
            require(not item.is_dir() and item.file_size == len(files[item.filename])
                    and z.read(item) == files[item.filename], "ZIP content failed readback: " + item.filename)


def assemble(exe, out):
    require(exe.name.casefold() == "relayplayer.exe", "supply the freshly built RelayPlayer.exe explicitly")
    check_path(exe)
    check_path(out)
    require(exe.is_file() and 0 < exe.stat().st_size <= 64*MIB, "missing or oversized host executable")
    require(not out.exists() or (out.is_dir() and next(out.iterdir(), None) is None),
            "output directory must be empty; refusing to overwrite files")
    source = tagged_source(Path(__file__).resolve().parents[1])
    binary = exe.read_bytes()
    check_exe(binary)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".public-", dir=out) as t:
        stage = Path(t)
        make_zip(stage/SOURCE, source)
        make_zip(stage/HOST, {"RelayPlayer.exe": binary, **{n: source[n] for n in DOCS}})
        digests = {n: hashlib.sha256((stage/n).read_bytes()).hexdigest() for n in (HOST, SOURCE)}
        (stage/SUMS).write_bytes("".join(f"{h}  {n}\n" for n, h in digests.items()).encode("ascii"))
        created = []
        try:
            for name in (HOST, SOURCE, SUMS):
                with (stage/name).open("rb") as src, (out/name).open("xb") as dst:
                    created.append(out/name)
                    shutil.copyfileobj(src, dst)
                require((out/name).read_bytes() == (stage/name).read_bytes(), "final write did not verify")
        except BaseException:
            for f in created:
                f.unlink(missing_ok=True)
            raise
    print("Verified:", HOST, SOURCE, SUMS, sep="\n  ")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", required=True, type=Path, help="new RelayPlayer.exe built from v0.1.7")
    parser.add_argument("--out-dir", required=True, type=Path, help="new or empty release output directory")
    args = parser.parse_args()
    try:
        assemble(Path(os.path.abspath(args.exe)), Path(os.path.abspath(args.out_dir)))
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as e:
        print(f"Packaging stopped: {e}", file=sys.stderr)
        raise SystemExit(1)
