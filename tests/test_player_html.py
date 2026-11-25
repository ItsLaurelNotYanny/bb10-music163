"""ES5 compliance and contract tests for the M3/M4 web/player.html UI."""

from pathlib import Path
import re
import unittest

ROOT_DIR = Path(__file__).resolve().parents[1]


class PlayerPageES5ComplianceTests(unittest.TestCase):
    """Verify that web/player.html stays strictly ES5-compatible for BB10 WebKit."""

    def setUp(self):
        self.html_path = ROOT_DIR / "web" / "player.html"
        self.assertTrue(self.html_path.is_file(), "web/player.html not found")
        self.html_content = self.html_path.read_text(encoding="utf-8")

        script_pattern = re.compile(
            r"<script\b[^>]*>(.*?)</script>", re.DOTALL | re.IGNORECASE
        )
        scripts = script_pattern.findall(self.html_content)
        self.assertTrue(len(scripts) > 0, "No <script> tag found in web/player.html")
        self.script_code = "\n".join(scripts)

    def test_no_forbidden_es6_declarations(self):
        """Ensure no 'const ' or 'let ' declarations exist in scripts."""
        for kw in ["const ", "let "]:
            pattern = re.compile(rf"\b{kw}")
            matches = pattern.findall(self.script_code)
            self.assertEqual(
                len(matches),
                0,
                f"Found prohibited ES6 keyword '{kw.strip()}' in player script: {matches}",
            )

    def test_no_arrow_functions_or_template_literals(self):
        """Ensure no arrow functions '=>' or backtick literals exist in scripts."""
        self.assertEqual(len(re.findall(r"=>", self.script_code)), 0)
        self.assertNotIn("`", self.script_code)

    def test_no_modern_js_syntax_or_fetch(self):
        """Ensure no ES6 classes, async/await, fetch, or Promise exist in scripts."""
        for kw in ["class ", "async ", "await ", "fetch(", "Promise"]:
            pattern = re.compile(rf"\b{re.escape(kw)}")
            matches = pattern.findall(self.script_code)
            self.assertEqual(
                len(matches),
                0,
                f"Found prohibited modern JS construct '{kw}' in player script",
            )

    def test_required_m3_m4_endpoints_referenced(self):
        """Ensure player.html references the M1/M3/M4 gateway endpoints."""
        required = [
            "/api/v1/search",
            "/api/v1/playlists",
            "/api/v1/recommend/songs",
            "/api/v1/personal-fm",
            "/api/v1/intelligence",
            "/api/v1/likelist",
            "/api/v1/playback/command",
            "/api/v1/playback/events",
            "/api/v1/playback/ack-stop",
            "/api/v1/auth/qr/start",
            "/api/v1/auth/status",
        ]
        for fragment in required:
            self.assertIn(fragment, self.script_code)

    def test_now_playing_turntable_elements_present(self):
        """Ensure player.html includes the Now Playing vinyl turntable dialog markers."""
        for fragment in [
            'id="now-playing-dialog"',
            'id="turntable-box"',
            'id="np-cover"',
            "vinyl-spin",
        ]:
            self.assertIn(fragment, self.html_content)

    def test_local_pause_and_sleep_timer_safeguards_present(self):
        """Verify local-immediate pause, server-confirmed resume, sleep timer, and 3h auto-stop."""
        for fragment in [
            'id="btn-sleep-timer"',
            "UNATTENDED_AUTO_STOP_MS",
            "markManualInteraction",
            "cycleSleepTimer",
            "checkSleepTimerExpiry",
        ]:
            self.assertIn(fragment, self.html_content)
        # Ensure togglePlayPause pauses locally before dispatching server sync
        m = re.search(
            r"function togglePlayPause\(\)\s*\{.*?\n      \}",
            self.script_code,
            re.DOTALL,
        )
        self.assertIsNotNone(m, "togglePlayPause function not found")
        body = m.group(0)
        self.assertIn("player.pause();", body)
        self.assertLess(body.index("player.pause();"), body.index('sendPlaybackCommand("pause", null);'))
        self.assertNotIn("player.play();", body)


if __name__ == "__main__":
    unittest.main()
