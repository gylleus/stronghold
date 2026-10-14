#!/usr/bin/env python3
"""Assembles Stronghold.app from a built binary.

    scripts/bundle.py [--binary target/release/stronghold] [--out target/release/Stronghold.app]
                      [--sign-identity -] [--no-sign]
    scripts/bundle.py --info-plist PATH     # only write Info.plist (for checks)

Contents/MacOS/stronghold is the binary, Contents/Resources/Stronghold.icns the icon, and
Contents/Frameworks every non-system dylib the binary loads (found with `otool -L`,
recursively), rewritten to load through `@rpath` from there so the app runs standalone. The
bundle is then signed ad hoc (`codesign --sign -`) unless `--sign-identity` or
`$STRONGHOLD_SIGN_IDENTITY` names a real identity.
"""

import argparse
import os
import plistlib
import re
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

BUNDLE_ID = "com.gylleus.stronghold"
APP_NAME = "Stronghold"
EXECUTABLE = "stronghold"
ICON = "Stronghold.icns"
MIN_MACOS = "11.0"
MIC_USAGE = "Stronghold listens while you hold the dictation or talk-to-the-King shortcut."
FRAMEWORKS_RPATH = "@executable_path/../Frameworks"


# ---------------------------------------------------------------------------
# Info.plist


def cargo_version(cargo_toml):
    """The [package] version from Cargo.toml's text."""
    in_package = False
    for line in cargo_toml.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
        elif in_package:
            m = re.match(r'version\s*=\s*"([^"]+)"', stripped)
            if m:
                return m.group(1)
    raise ValueError("no [package] version in Cargo.toml")


def info_plist(version):
    return {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": APP_NAME,
        "CFBundleExecutable": EXECUTABLE,
        "CFBundleIconFile": ICON,
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": APP_NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "LSMinimumSystemVersion": MIN_MACOS,
        "NSHighResolutionCapable": True,
        "NSMicrophoneUsageDescription": MIC_USAGE,
        "NSPrincipalClass": "NSApplication",
        "NSSupportsAutomaticGraphicsSwitching": True,
    }


def write_info_plist(path, version):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as f:
        plistlib.dump(info_plist(version), f)


# ---------------------------------------------------------------------------
# Dylibs (pure parts take otool output and a file-existence check, so they test with fakes)


def parse_otool_libs(output, own_id=None):
    """Install names from `otool -L` output, without the header line and the file's own id."""
    names = []
    for line in output.splitlines()[1:]:
        m = re.match(r"\s+(\S.*?) \(compatibility version", line)
        if m and m.group(1) != own_id:
            names.append(m.group(1))
    return names


def parse_otool_rpaths(output):
    """LC_RPATH entries from `otool -l` output."""
    rpaths = []
    lines = output.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH":
            for follow in lines[i + 1 : i + 4]:
                m = re.match(r"\s+path (.*) \(offset \d+\)", follow)
                if m:
                    rpaths.append(m.group(1))
                    break
    return rpaths


def parse_otool_id(output):
    """A dylib's install name from `otool -D` output, if it has one."""
    lines = output.splitlines()
    return lines[1].strip() if len(lines) > 1 else None


def is_system(name):
    """Libraries the OS provides (including the dyld shared cache, which has no files)."""
    return name.startswith(("/usr/lib/", "/System/"))


def expand(path, loader_dir, exe_dir):
    if path.startswith("@loader_path"):
        return loader_dir + path[len("@loader_path") :]
    if path.startswith("@executable_path"):
        return exe_dir + path[len("@executable_path") :]
    return path


def resolve(name, rpaths, loader_dir, exe_dir, exists):
    """The file an install name loads, or None if it cannot be found."""
    if name.startswith("@rpath/"):
        rest = name[len("@rpath/") :]
        candidates = [os.path.join(expand(r, loader_dir, exe_dir), rest) for r in rpaths]
    else:
        candidates = [expand(name, loader_dir, exe_dir)]
    for candidate in candidates:
        if exists(candidate):
            return os.path.normpath(candidate)
    return None


class Inspector:
    """Reads load commands with otool."""

    def libs(self, path):
        own = parse_otool_id(otool("-D", path))
        return parse_otool_libs(otool("-L", path), own)

    def rpaths(self, path):
        return parse_otool_rpaths(otool("-l", path))

    def exists(self, path):
        return os.path.isfile(path)


def otool(flag, path):
    return subprocess.run(["otool", flag, path], check=True, capture_output=True, text=True).stdout


def collect_dylibs(binary, inspector):
    """Non-system dylibs `binary` loads, directly or through other dylibs.

    Returns `{install name: resolved file}` in discovery order; raises if one cannot be found.
    Every dylib is resolved with the rpaths of the image that loads it (and, as dyld does for
    `@rpath`, of the main executable).
    """
    exe_dir = os.path.dirname(binary)
    exe_rpaths = inspector.rpaths(binary)
    found = {}
    queue = [(binary, exe_rpaths)]
    while queue:
        image, rpaths = queue.pop(0)
        loader_dir = os.path.dirname(image)
        for name in inspector.libs(image):
            if is_system(name) or name in found:
                continue
            path = resolve(name, rpaths + exe_rpaths, loader_dir, exe_dir, inspector.exists)
            if path is None:
                raise FileNotFoundError(f"{image} loads {name}, which was not found")
            found[name] = path
            queue.append((path, inspector.rpaths(path)))
    return found


def frameworks_names(found):
    """File name in Contents/Frameworks for each install name.

    Install names resolving to the same file share one copy, named after the first of them;
    different files with the same name are an error.
    """
    by_file = {}
    owner = {}
    names = {}
    for name, path in found.items():
        base = by_file.setdefault(path, os.path.basename(name))
        if owner.setdefault(base, path) != path:
            raise ValueError(f"two different dylibs are both named {base}: {owner[base]}, {path}")
        names[name] = base
    return names


def rpath_args(rpaths, keep):
    """install_name_tool arguments that leave `keep` as the only non-system rpath."""
    args = []
    for rpath in dict.fromkeys(rpaths):
        if rpath != keep and not is_system(rpath):
            args += ["-delete_rpath", rpath]
    if keep not in rpaths:
        args += ["-add_rpath", keep]
    return args


def relink_commands(binary, binary_rpaths, dylibs, names):
    """install_name_tool invocations that make `binary` and the copied dylibs load each other
    through @rpath from Contents/Frameworks, and drop rpaths into the build machine.

    `dylibs` maps each copied file to its rpaths; `names` maps original install names to
    their file names in Contents/Frameworks.
    """
    if not names:
        return []
    changes = []
    for name, base in names.items():
        if name != f"@rpath/{base}":
            changes += ["-change", name, f"@rpath/{base}"]
    commands = [["install_name_tool", *changes, *rpath_args(binary_rpaths, FRAMEWORKS_RPATH), binary]]
    for path, rpaths in dylibs.items():
        base = os.path.basename(path)
        commands.append(
            ["install_name_tool", "-id", f"@rpath/{base}", *changes, *rpath_args(rpaths, "@loader_path"), path]
        )
    return commands


# ---------------------------------------------------------------------------
# Assembly


def run(cmd):
    subprocess.run(cmd, check=True)


def bundle(binary, out, sign_identity):
    with open(os.path.join(ROOT, "Cargo.toml")) as f:
        version = cargo_version(f.read())
    icon = os.path.join(ROOT, "packaging", ICON)
    if not os.path.isfile(icon):
        run([sys.executable, os.path.join(ROOT, "scripts", "make_icon.py"), "--out", icon])

    if os.path.exists(out):
        shutil.rmtree(out)
    contents = os.path.join(out, "Contents")
    macos = os.path.join(contents, "MacOS")
    resources = os.path.join(contents, "Resources")
    frameworks = os.path.join(contents, "Frameworks")
    os.makedirs(macos)
    os.makedirs(resources)

    write_info_plist(os.path.join(contents, "Info.plist"), version)
    with open(os.path.join(contents, "PkgInfo"), "w") as f:
        f.write("APPL????")
    shutil.copy(icon, os.path.join(resources, ICON))
    exe = os.path.join(macos, EXECUTABLE)
    shutil.copy(binary, exe)

    inspector = Inspector()
    found = collect_dylibs(os.path.abspath(binary), inspector)
    names = frameworks_names(found)
    copied = {}
    for name, path in found.items():
        dest = os.path.join(frameworks, names[name])
        if dest not in copied:
            os.makedirs(frameworks, exist_ok=True)
            shutil.copyfile(path, dest)
            os.chmod(dest, 0o755)
            copied[dest] = inspector.rpaths(dest)
        print(f"bundled {name} <- {path}", file=sys.stderr)
    for cmd in relink_commands(exe, inspector.rpaths(exe), copied, names):
        run(cmd)

    if sign_identity is not None:
        for dest in copied:
            run(["codesign", "--force", "--sign", sign_identity, dest])
        run(["codesign", "--force", "--deep", "--sign", sign_identity, out])
    print(f"built {out} (version {version}, {len(copied)} bundled dylibs)", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", default=os.path.join(ROOT, "target", "release", EXECUTABLE))
    parser.add_argument("--out", default=os.path.join(ROOT, "target", "release", f"{APP_NAME}.app"))
    parser.add_argument(
        "--sign-identity",
        # MYRSTACK_SIGN_IDENTITY: the variable's name before the app was renamed.
        default=os.environ.get("STRONGHOLD_SIGN_IDENTITY")
        or os.environ.get("MYRSTACK_SIGN_IDENTITY")
        or "-",
        help="codesign identity; '-' (default) signs ad hoc",
    )
    parser.add_argument("--no-sign", action="store_true")
    parser.add_argument("--info-plist", metavar="PATH", help="only write Info.plist to PATH")
    args = parser.parse_args()

    if args.info_plist:
        with open(os.path.join(ROOT, "Cargo.toml")) as f:
            write_info_plist(args.info_plist, cargo_version(f.read()))
        return
    if not os.path.isfile(args.binary):
        sys.exit(f"{args.binary} not found; run `cargo build --release` first")
    bundle(args.binary, args.out, None if args.no_sign else args.sign_identity)


if __name__ == "__main__":
    main()
