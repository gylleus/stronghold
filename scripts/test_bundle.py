"""Tests for scripts/bundle.py's pure parts; run by `cargo test` (tests/packaging.rs) or with
`python3 -m unittest discover -s scripts -p 'test_*.py'`."""

import os
import plistlib
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import bundle  # noqa: E402

OTOOL_L_EXE = """/build/target/release/stronghold:
\t@rpath/libsherpa-onnx-c-api.dylib (compatibility version 0.0.0, current version 0.0.0)
\t/opt/homebrew/opt/foo/lib/libfoo.1.dylib (compatibility version 1.0.0, current version 1.2.0)
\t/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit (compatibility version 45.0.0, current version 2575.40.6)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)
"""

OTOOL_L_DYLIB = """/cache/lib/libsherpa-onnx-c-api.dylib:
\t@rpath/libsherpa-onnx-c-api.dylib (compatibility version 0.0.0, current version 0.0.0)
\t@rpath/libonnxruntime.1.17.1.dylib (compatibility version 0.0.0, current version 1.17.1)
\t/usr/lib/libc++.1.dylib (compatibility version 1.0.0, current version 1700.255.0)
"""

OTOOL_l_RPATHS = """Load command 20
          cmd LC_RPATH
      cmdsize 48
         path /cache/lib (offset 12)
Load command 21
          cmd LC_RPATH
      cmdsize 32
         path @loader_path (offset 12)
Load command 22
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name /usr/lib/libSystem.B.dylib (offset 24)
"""


class Parsing(unittest.TestCase):
    def test_libs_skip_header_and_own_id(self):
        self.assertEqual(
            bundle.parse_otool_libs(OTOOL_L_EXE),
            [
                "@rpath/libsherpa-onnx-c-api.dylib",
                "/opt/homebrew/opt/foo/lib/libfoo.1.dylib",
                "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit",
                "/usr/lib/libSystem.B.dylib",
            ],
        )
        self.assertEqual(
            bundle.parse_otool_libs(OTOOL_L_DYLIB, own_id="@rpath/libsherpa-onnx-c-api.dylib"),
            ["@rpath/libonnxruntime.1.17.1.dylib", "/usr/lib/libc++.1.dylib"],
        )

    def test_rpaths(self):
        self.assertEqual(bundle.parse_otool_rpaths(OTOOL_l_RPATHS), ["/cache/lib", "@loader_path"])
        self.assertEqual(bundle.parse_otool_rpaths(""), [])

    def test_id(self):
        self.assertEqual(
            bundle.parse_otool_id("/x/libfoo.dylib:\n@rpath/libfoo.dylib\n"), "@rpath/libfoo.dylib"
        )
        self.assertIsNone(bundle.parse_otool_id("/x/stronghold:\n"))

    def test_system_libraries(self):
        self.assertTrue(bundle.is_system("/usr/lib/libSystem.B.dylib"))
        self.assertTrue(bundle.is_system("/System/Library/Frameworks/AppKit.framework/AppKit"))
        self.assertFalse(bundle.is_system("/usr/local/lib/libfoo.dylib"))
        self.assertFalse(bundle.is_system("@rpath/libfoo.dylib"))

    def test_cargo_version(self):
        toml = '[package]\nname = "x"\nversion = "1.2.3"\n\n[dependencies]\nversion = "9"\n'
        self.assertEqual(bundle.cargo_version(toml), "1.2.3")
        with self.assertRaises(ValueError):
            bundle.cargo_version('[dependencies]\nversion = "9"\n')


class Resolve(unittest.TestCase):
    def test_rpath_tries_each_rpath_in_order(self):
        files = {"/b/lib/libx.dylib", "/c/libx.dylib"}
        got = bundle.resolve("@rpath/libx.dylib", ["/a", "@loader_path/lib", "/c"], "/b", "/e", files.__contains__)
        self.assertEqual(got, "/b/lib/libx.dylib")

    def test_executable_and_loader_path(self):
        files = {"/e/../Frameworks/liby.dylib", "/l/libz.dylib"}
        exists = files.__contains__
        self.assertEqual(
            bundle.resolve("@executable_path/../Frameworks/liby.dylib", [], "/l", "/e", exists),
            "/Frameworks/liby.dylib",
        )
        self.assertEqual(bundle.resolve("@loader_path/libz.dylib", [], "/l", "/e", exists), "/l/libz.dylib")
        self.assertIsNone(bundle.resolve("@rpath/nope.dylib", ["/a"], "/l", "/e", exists))


class FakeInspector:
    """Load commands of a made-up build: exe -> sherpa (@rpath) -> onnxruntime (@rpath via
    the dylib's own rpath), and exe -> libfoo (absolute) -> system only."""

    LIBS = {
        "/build/stronghold": [
            "@rpath/libsherpa.dylib",
            "/opt/foo/libfoo.dylib",
            "/usr/lib/libSystem.B.dylib",
        ],
        "/cache/lib/libsherpa.dylib": ["@rpath/libonnxruntime.dylib", "/usr/lib/libc++.1.dylib"],
        "/cache/lib/libonnxruntime.dylib": ["/usr/lib/libSystem.B.dylib"],
        "/opt/foo/libfoo.dylib": ["/usr/lib/libSystem.B.dylib", "@rpath/libsherpa.dylib"],
    }
    RPATHS = {"/build/stronghold": ["/cache/lib"], "/cache/lib/libsherpa.dylib": ["@loader_path"]}

    def libs(self, path):
        return self.LIBS[path]

    def rpaths(self, path):
        return self.RPATHS.get(path, [])

    def exists(self, path):
        return os.path.normpath(path) in self.LIBS


class Collect(unittest.TestCase):
    def test_collects_non_system_dylibs_recursively(self):
        found = bundle.collect_dylibs("/build/stronghold", FakeInspector())
        self.assertEqual(
            found,
            {
                "@rpath/libsherpa.dylib": "/cache/lib/libsherpa.dylib",
                "/opt/foo/libfoo.dylib": "/opt/foo/libfoo.dylib",
                "@rpath/libonnxruntime.dylib": "/cache/lib/libonnxruntime.dylib",
            },
        )

    def test_missing_dylib_is_an_error(self):
        inspector = FakeInspector()
        inspector.LIBS = dict(FakeInspector.LIBS, **{"/build/stronghold": ["@rpath/libgone.dylib"]})
        with self.assertRaises(FileNotFoundError):
            bundle.collect_dylibs("/build/stronghold", inspector)

    def test_no_dylibs(self):
        inspector = FakeInspector()
        inspector.LIBS = {"/build/stronghold": ["/usr/lib/libSystem.B.dylib"]}
        self.assertEqual(bundle.collect_dylibs("/build/stronghold", inspector), {})
        self.assertEqual(bundle.relink_commands("/app/stronghold", [], {}, {}), [])

    def test_clashing_file_names(self):
        with self.assertRaises(ValueError):
            bundle.frameworks_names({"/a/libx.dylib": "/a/libx.dylib", "/b/libx.dylib": "/b/libx.dylib"})
        same = {"@rpath/libx.1.dylib": "/a/libx.1.2.dylib", "/a/libx.dylib": "/a/libx.1.2.dylib"}
        self.assertEqual(set(bundle.frameworks_names(same).values()), {"libx.1.dylib"})

    def test_relink_commands(self):
        found = bundle.collect_dylibs("/build/stronghold", FakeInspector())
        names = bundle.frameworks_names(found)
        fw = "/app/Contents/Frameworks"
        copied = {f"{fw}/libsherpa.dylib": ["/cache/lib", "@loader_path"], f"{fw}/libfoo.dylib": []}
        cmds = bundle.relink_commands("/app/Contents/MacOS/stronghold", ["/cache/lib", "/cache/lib"], copied, names)
        changes = ["-change", "/opt/foo/libfoo.dylib", "@rpath/libfoo.dylib"]
        self.assertEqual(
            cmds[0],
            [
                "install_name_tool", *changes,
                "-delete_rpath", "/cache/lib",
                "-add_rpath", "@executable_path/../Frameworks",
                "/app/Contents/MacOS/stronghold",
            ],
        )  # fmt: skip
        self.assertEqual(
            cmds[1],
            [
                "install_name_tool", "-id", "@rpath/libsherpa.dylib", *changes,
                "-delete_rpath", "/cache/lib",
                f"{fw}/libsherpa.dylib",
            ],
        )  # fmt: skip
        self.assertEqual(cmds[2][-3:], ["-add_rpath", "@loader_path", f"{fw}/libfoo.dylib"])


class InfoPlist(unittest.TestCase):
    def test_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "Info.plist")
            bundle.write_info_plist(path, "1.2.3")
            with open(path, "rb") as f:
                plist = plistlib.load(f)
        self.assertEqual(plist["CFBundleIdentifier"], "ai.depict.stronghold")
        self.assertEqual(plist["CFBundleName"], "Stronghold")
        self.assertEqual(plist["CFBundleDisplayName"], "Stronghold")
        self.assertEqual(plist["CFBundleExecutable"], "stronghold")
        self.assertEqual(plist["CFBundleShortVersionString"], "1.2.3")
        self.assertEqual(plist["CFBundleVersion"], "1.2.3")
        self.assertIs(plist["NSHighResolutionCapable"], True)
        self.assertEqual(plist["LSApplicationCategoryType"], "public.app-category.developer-tools")
        self.assertIn("LSMinimumSystemVersion", plist)
        self.assertIn("hold the dictation", plist["NSMicrophoneUsageDescription"])


if __name__ == "__main__":
    unittest.main()
