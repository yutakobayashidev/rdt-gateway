"""Exercise the updater with deterministic HTTP responses; no network access."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "update_oauth_resources.sh"
FAKE_CURL = r'''
import os, sys
url = sys.argv[-1]
mode = os.environ.get("FIXTURE_MODE", "ok")
if mode == "http_error" and "phone-2026.2.0" in url:
    sys.exit(22)
if "old-versions" in url:
    if mode == "empty":
        print("<html>unexpected markup</html>")
    else:
        for version in ["2026.2.0", "2026.1.0", "2026.2.0"]:
            print(f'<a href="/reddit/com.reddit.frontpage/download/phone-{version}-apk">download</a>')
else:
    version = url.split("phone-")[1].split("-apk")[0]
    if mode == "mismatch":
        version = "2026.99.0"
    print(f'<span class="vername">Reddit {version}</span>')
    if mode != "missing_build":
        print('<span class="vercode">(12345)</span>')
        print('<span class="vercode">(12346)</span>')
    print('<span class="vername">Reddit 2026.99.1</span>')
'''


class UpdaterTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        bindir = self.root / "bin"
        bindir.mkdir()
        curl = bindir / "curl"
        curl.write_text(f"#!{sys.executable}" + chr(10) + FAKE_CURL)
        curl.chmod(0o755)
        self.env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"])
        self.output = self.root / "oauth_resources.rs"

    def run_updater(self, *args, script=SCRIPT, mode="ok"):
        return subprocess.run(
            ["bash", str(script), *map(str, args)],
            cwd="/", env=dict(self.env, FIXTURE_MODE=mode),
            text=True, capture_output=True, timeout=15,
        )

    def test_success_deduplicates_and_generates_matching_array_length(self):
        result = self.run_updater("--output", self.output)
        self.assertEqual(result.returncode, 0, result.stderr)
        text = self.output.read_text()
        self.assertIn("ANDROID_APP_VERSION_LIST: &[&str; 2]", text)
        self.assertEqual(text.count('"Version '), 2)
        self.assertLess(text.index("2026.1.0"), text.index("2026.2.0"))
        self.assertNotIn("IOS_", text)
        self.assertNotIn("12346", text)
        self.assertEqual(list(self.root.glob(".oauth_resources.*")), [])

    def test_failures_preserve_existing_file_and_clean_temporary_output(self):
        for mode in ["http_error", "empty", "missing_build", "mismatch"]:
            with self.subTest(mode=mode):
                self.output.write_text("previous valid data")
                result = self.run_updater("--output", self.output, mode=mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.output.read_text(), "previous valid data")
                self.assertEqual(list(self.root.glob(".oauth_resources.*")), [])

    def test_default_path_is_relative_to_script_not_working_directory(self):
        script = self.root / "scripts" / SCRIPT.name
        script.parent.mkdir()
        shutil.copyfile(SCRIPT, script)
        target = self.root / "crates/rdt-request/src/oauth_resources.rs"
        target.parent.mkdir(parents=True)
        result = self.run_updater(script=script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ANDROID_APP_VERSION_LIST", target.read_text())

    def test_invalid_arguments_do_not_write_output(self):
        result = self.run_updater("--output")
        self.assertEqual(result.returncode, 2)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
