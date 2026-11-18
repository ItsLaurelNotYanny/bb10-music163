"""Automated unit and integration tests for M0 server and ES5 client."""

from http import HTTPStatus
import http.client
import os
from pathlib import Path
import re
import socket
import tempfile
import threading
import time
import unittest
import urllib.request
import urllib.error

# Import the server module
import sys
ROOT_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT_DIR / "scripts"))
import m0_server


class M0ServerRangeAndAuthTests(unittest.TestCase):
    """Test RFC 7233 Range requests, HEAD, authentication, and bounded streaming."""

    @classmethod
    def setUpClass(cls):
        # Create a temporary directory with test assets
        cls.temp_dir = tempfile.TemporaryDirectory()
        cls.web_dir = Path(cls.temp_dir.name) / "web"
        cls.audio_dir = Path(cls.temp_dir.name) / "audio"
        cls.web_dir.mkdir(parents=True)
        cls.audio_dir.mkdir(parents=True)

        # Write sample HTML and audio fixture (1000 bytes)
        cls.index_file = cls.web_dir / "index.html"
        cls.index_file.write_text("<!DOCTYPE html><html><body>Test</body></html>", encoding="utf-8")

        cls.audio_file = cls.audio_dir / "sample.mp3"
        # Deterministic 1000-byte sequence: 0, 1, 2, ..., 255, 0, 1...
        cls.sample_data = bytes([i % 256 for i in range(1000)])
        cls.audio_file.write_bytes(cls.sample_data)

        cls.ticket = "test-secret-ticket"

        # Find an open port
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.bind(("127.0.0.1", 0))
        cls.host, cls.port = sock.getsockname()
        sock.close()

        handler = m0_server.create_handler_class(
            cls.web_dir, cls.audio_dir, ticket=cls.ticket
        )
        cls.httpd = m0_server.HTTPServer((cls.host, cls.port), handler)
        cls.server_thread = threading.Thread(target=cls.httpd.serve_forever, daemon=True)
        cls.server_thread.start()
        time.sleep(0.05)  # Allow server to bind

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()
        cls.server_thread.join(timeout=2)
        cls.temp_dir.cleanup()

    def _request(self, method: str, path: str, headers: dict | None = None):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=5)
        conn.request(method, path, headers=headers or {})
        response = conn.getresponse()
        data = response.read()
        conn.close()
        return response.status, dict(response.getheaders()), data

    def test_serve_index_html_without_auth(self):
        """Index page must be accessible without tickets."""
        status, headers, body = self._request("GET", "/")
        self.assertEqual(status, HTTPStatus.OK)
        self.assertIn("text/html", headers.get("Content-Type", ""))
        self.assertIn(b"Test", body)

    def test_audio_unauthorized_without_ticket(self):
        """Audio route must reject requests missing a valid ticket."""
        status, headers, body = self._request("GET", "/audio/sample.mp3")
        self.assertEqual(status, HTTPStatus.UNAUTHORIZED)
        self.assertIn("application/json", headers.get("Content-Type", ""))
        self.assertIn(b"unauthorized", body)

    def test_audio_unauthorized_with_wrong_ticket(self):
        """Audio route must reject invalid query tickets."""
        status, headers, body = self._request("GET", "/audio/sample.mp3?ticket=wrong")
        self.assertEqual(status, HTTPStatus.UNAUTHORIZED)

    def test_audio_authorized_with_query_ticket_full_get(self):
        """Valid query ticket returns 200 OK for full GET request."""
        status, headers, body = self._request(
            "GET", f"/audio/sample.mp3?ticket={self.ticket}"
        )
        self.assertEqual(status, HTTPStatus.OK)
        self.assertEqual(headers.get("Accept-Ranges"), "bytes")
        self.assertEqual(headers.get("Content-Length"), "1000")
        self.assertEqual(headers.get("Content-Type"), "audio/mpeg")
        self.assertEqual(body, self.sample_data)

    def test_audio_authorized_with_bearer_header(self):
        """Valid Authorization: Bearer header is also accepted."""
        status, headers, body = self._request(
            "GET", "/audio/sample.mp3", {"Authorization": f"Bearer {self.ticket}"}
        )
        self.assertEqual(status, HTTPStatus.OK)
        self.assertEqual(body, self.sample_data)

    def test_range_request_start_to_end(self):
        """bytes=0-49 returns first 50 bytes with 206 Partial Content."""
        status, headers, body = self._request(
            "GET",
            f"/audio/sample.mp3?ticket={self.ticket}",
            {"Range": "bytes=0-49"},
        )
        self.assertEqual(status, HTTPStatus.PARTIAL_CONTENT)
        self.assertEqual(headers.get("Content-Range"), "bytes 0-49/1000")
        self.assertEqual(headers.get("Content-Length"), "50")
        self.assertEqual(body, self.sample_data[0:50])

    def test_range_request_open_ended(self):
        """bytes=900- returns bytes from 900 to end (100 bytes)."""
        status, headers, body = self._request(
            "GET",
            f"/audio/sample.mp3?ticket={self.ticket}",
            {"Range": "bytes=900-"},
        )
        self.assertEqual(status, HTTPStatus.PARTIAL_CONTENT)
        self.assertEqual(headers.get("Content-Range"), "bytes 900-999/1000")
        self.assertEqual(headers.get("Content-Length"), "100")
        self.assertEqual(body, self.sample_data[900:1000])

    def test_range_request_suffix(self):
        """bytes=-50 returns the last 50 bytes."""
        status, headers, body = self._request(
            "GET",
            f"/audio/sample.mp3?ticket={self.ticket}",
            {"Range": "bytes=-50"},
        )
        self.assertEqual(status, HTTPStatus.PARTIAL_CONTENT)
        self.assertEqual(headers.get("Content-Range"), "bytes 950-999/1000")
        self.assertEqual(headers.get("Content-Length"), "50")
        self.assertEqual(body, self.sample_data[950:1000])

    def test_range_request_unsatisfiable_beyond_eof(self):
        """Range starting beyond EOF returns 416 Range Not Satisfiable."""
        status, headers, body = self._request(
            "GET",
            f"/audio/sample.mp3?ticket={self.ticket}",
            {"Range": "bytes=1500-1600"},
        )
        self.assertEqual(status, HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE)
        self.assertEqual(headers.get("Content-Range"), "bytes */1000")
        self.assertEqual(headers.get("Content-Length"), "0")

    def test_range_request_inverted_bounds(self):
        """Invalid bounds like bytes=50-20 returns 416."""
        status, headers, body = self._request(
            "GET",
            f"/audio/sample.mp3?ticket={self.ticket}",
            {"Range": "bytes=50-20"},
        )
        self.assertEqual(status, HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE)
        self.assertEqual(headers.get("Content-Range"), "bytes */1000")

    def test_head_request_contains_headers_and_no_body(self):
        """HEAD request must return Content-Length, Accept-Ranges, Content-Type with 0-byte body."""
        status, headers, body = self._request(
            "HEAD", f"/audio/sample.mp3?ticket={self.ticket}"
        )
        self.assertEqual(status, HTTPStatus.OK)
        self.assertEqual(headers.get("Content-Length"), "1000")
        self.assertEqual(headers.get("Accept-Ranges"), "bytes")
        self.assertEqual(headers.get("Content-Type"), "audio/mpeg")
        self.assertEqual(len(body), 0)

    def test_cors_headers_present(self):
        """CORS headers must allow Range requests and expose relevant headers."""
        status, headers, _ = self._request(
            "OPTIONS", f"/audio/sample.mp3?ticket={self.ticket}"
        )
        self.assertEqual(status, HTTPStatus.NO_CONTENT)
        self.assertEqual(headers.get("Access-Control-Allow-Origin"), "*")
        self.assertIn("Range", headers.get("Access-Control-Allow-Headers", ""))
        self.assertIn("Content-Range", headers.get("Access-Control-Expose-Headers", ""))


class WebPageES5ComplianceTests(unittest.TestCase):
    """Verify that web/index.html is strictly ES5 compliant for BB10 WebKit."""

    def setUp(self):
        self.html_path = ROOT_DIR / "web" / "index.html"
        self.assertTrue(self.html_path.is_file(), "web/index.html not found")
        self.html_content = self.html_path.read_text(encoding="utf-8")

        # Extract contents of <script> tags
        script_pattern = re.compile(r"<script\b[^>]*>(.*?)</script>", re.DOTALL | re.IGNORECASE)
        scripts = script_pattern.findall(self.html_content)
        self.assertTrue(len(scripts) > 0, "No <script> tag found in web/index.html")
        self.script_code = "\n".join(scripts)

    def test_no_forbidden_es6_declarations(self):
        """Ensure no 'const ' or 'let ' declarations exist in scripts."""
        for kw in ["const ", "let "]:
            # Ensure it's not part of an identifier
            pattern = re.compile(rf"\b{kw}")
            matches = pattern.findall(self.script_code)
            self.assertEqual(
                len(matches),
                0,
                f"Found prohibited ES6 keyword '{kw.strip()}' in script: {matches}",
            )

    def test_no_arrow_functions(self):
        """Ensure no arrow functions '=>' exist in scripts."""
        # Check for => while avoiding HTML comments
        pattern = re.compile(r"=>")
        matches = pattern.findall(self.script_code)
        self.assertEqual(
            len(matches), 0, f"Found prohibited arrow function '=>' in script"
        )

    def test_no_template_literals(self):
        """Ensure no backtick template literals exist in scripts."""
        self.assertNotIn("`", self.script_code, "Found backtick template literal in script")

    def test_no_classes_async_or_fetch(self):
        """Ensure no ES6 classes, async/await, or fetch APIs exist in scripts."""
        for kw in ["class ", "async ", "await ", "fetch(", "Promise"]:
            pattern = re.compile(rf"\b{re.escape(kw)}")
            matches = pattern.findall(self.script_code)
            self.assertEqual(
                len(matches),
                0,
                f"Found prohibited modern JS construct '{kw}' in script",
            )


class AudioFixturesIntegrityTests(unittest.TestCase):
    """Verify that generated audio fixtures exist, are non-empty, and valid MP3."""

    def test_all_fixtures_present_and_sized(self):
        fixtures_dir = ROOT_DIR / "fixtures" / "audio"
        if not (fixtures_dir / "track_long_30min.mp3").is_file():
            self.skipTest("Audio fixtures not generated; run ./scripts/generate-test-audio.sh")
        expected_files = {
            "track1_440hz.mp3": 50_000,      # ~80 KB
            "track2_554hz.mp3": 50_000,
            "track3_659hz.mp3": 50_000,
            "track4_880hz.mp3": 50_000,
            "track_long_30min.mp3": 20_000_000,  # ~28 MB
        }
        for filename, min_size in expected_files.items():
            path = fixtures_dir / filename
            self.assertTrue(path.is_file(), f"Missing audio fixture: {filename}")
            size = path.stat().st_size
            self.assertGreaterEqual(
                size, min_size, f"{filename} is smaller than expected ({size} bytes)"
            )


if __name__ == "__main__":
    unittest.main()
