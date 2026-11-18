#!/usr/bin/env python3
"""M0 HTTP Audio Fixture and ES5 Web Player Server.

Implements RFC 7233 HTTP Range requests, bounded streaming,
token-based media authentication, and static asset serving for BB10 testing.
"""

from __future__ import annotations

import argparse
from http import HTTPStatus
from http.server import HTTPServer, SimpleHTTPRequestHandler
import mimetypes
import os
from pathlib import Path
import re
import sys
from urllib.parse import parse_qs, unquote, urlparse

RANGE_HEADER_PATTERN = re.compile(r"^bytes=(\d*)-(\d*)$")
DEFAULT_TICKET = "bb10-test-ticket"
CHUNK_SIZE = 64 * 1024  # 64 KB streaming buffer for bounded memory use


class RangeRequestHandler(SimpleHTTPRequestHandler):
    """HTTP Request Handler supporting RFC 7233 Range requests and auth tickets."""

    server_version = "BB10Music163-M0/1.0"

    def __init__(self, *args, web_dir: Path, audio_dir: Path, ticket: str | None = None, **kwargs):
        self.web_dir = Path(web_dir).resolve()
        self.audio_dir = Path(audio_dir).resolve()
        self.ticket = ticket
        super().__init__(*args, **kwargs)

    def log_message(self, format_str: str, *args) -> None:
        """Structured English logging."""
        sys.stderr.write(
            f"[{self.log_date_time_string()}] {self.client_address[0]} {format_str % args}\n"
        )

    def do_OPTIONS(self) -> None:
        """Handle CORS preflight requests."""
        self.send_response(HTTPStatus.NO_CONTENT)
        self.send_cors_headers()
        self.end_headers()

    def do_HEAD(self) -> None:
        """Handle HEAD requests without sending the body."""
        self._handle_request(send_body=False)

    def do_GET(self) -> None:
        """Handle GET requests with Range and ticket validation."""
        self._handle_request(send_body=True)

    def send_cors_headers(self) -> None:
        """Set standard CORS and Range response headers."""
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
        self.send_header(
            "Access-Control-Allow-Headers", "Range, Authorization, Content-Type, Accept"
        )
        self.send_header(
            "Access-Control-Expose-Headers",
            "Content-Range, Content-Length, Accept-Ranges, Content-Type",
        )
        self.send_header("Accept-Ranges", "bytes")

    def _validate_auth(self, parsed_url, query_params) -> bool:
        """Validate media ticket from query string or Authorization header."""
        if not self.ticket:
            return True

        # Query param ?ticket=...
        tickets_in_query = query_params.get("ticket", [])
        if any(t == self.ticket for t in tickets_in_query):
            return True

        # Authorization: Bearer ...
        auth_header = self.headers.get("Authorization", "")
        if auth_header.startswith("Bearer "):
            bearer_token = auth_header[7:].strip()
            if bearer_token == self.ticket:
                return True

        return False

    def _resolve_file_path(self, clean_path: str) -> tuple[Path | None, bool]:
        """Resolve requested URL path to local file.

        Returns (file_path, is_audio_route).
        """
        if clean_path in ("", "/"):
            index_path = self.web_dir / "index.html"
            return (index_path if index_path.is_file() else None, False)

        if clean_path.startswith("/audio/"):
            rel_audio = clean_path[len("/audio/") :].lstrip("/")
            target = (self.audio_dir / rel_audio).resolve()
            if target.is_file() and str(target).startswith(str(self.audio_dir)):
                return (target, True)
            return (None, True)

        # Static web assets
        rel_web = clean_path.lstrip("/")
        target = (self.web_dir / rel_web).resolve()
        if target.is_file() and str(target).startswith(str(self.web_dir)):
            return (target, False)

        return (None, False)

    def _parse_range_header(
        self, range_header: str, file_size: int
    ) -> tuple[int, int] | str | None:
        """Parse Range header into (start, end).

        Returns:
            (start, end) on valid range
            '416' if range is unsatisfiable
            None if no Range header or invalid syntax
        """
        if not range_header or not range_header.startswith("bytes="):
            return None

        match = RANGE_HEADER_PATTERN.match(range_header.strip())
        if not match:
            return "416"

        start_str, end_str = match.groups()

        if not start_str and not end_str:
            return "416"

        if not start_str:
            # Suffix range: bytes=-N (last N bytes)
            suffix_len = int(end_str)
            if suffix_len <= 0:
                return "416"
            if suffix_len > file_size:
                start = 0
            else:
                start = file_size - suffix_len
            end = file_size - 1
            return (start, end)

        start = int(start_str)
        if start >= file_size:
            return "416"

        if not end_str:
            # Open-ended range: bytes=N-
            end = file_size - 1
        else:
            end = int(end_str)
            if end < start:
                return "416"
            if end >= file_size:
                end = file_size - 1

        return (start, end)

    def _handle_request(self, send_body: bool) -> None:
        parsed_url = urlparse(self.path)
        clean_path = unquote(parsed_url.path)
        query_params = parse_qs(parsed_url.query)

        file_path, is_audio = self._resolve_file_path(clean_path)

        if file_path is None:
            self.send_error(HTTPStatus.NOT_FOUND, "File not found")
            return

        # Audio files require authentication when ticket is configured
        if is_audio and not self._validate_auth(parsed_url, query_params):
            self.send_response(HTTPStatus.UNAUTHORIZED)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_cors_headers()
            msg = b'{"error":"unauthorized","message":"Missing or invalid media ticket"}\n'
            self.send_header("Content-Length", str(len(msg)))
            self.end_headers()
            if send_body:
                self.wfile.write(msg)
            return

        try:
            file_size = file_path.stat().st_size
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND, "File inaccessible")
            return

        content_type, _ = mimetypes.guess_type(str(file_path))
        if not content_type:
            content_type = "application/octet-stream"
        if content_type.startswith("text/"):
            content_type += "; charset=utf-8"

        range_header = self.headers.get("Range")
        parsed_range = self._parse_range_header(range_header, file_size)

        if parsed_range == "416":
            self.send_response(HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE)
            self.send_header("Content-Range", f"bytes */{file_size}")
            self.send_header("Content-Length", "0")
            self.send_cors_headers()
            self.end_headers()
            return

        if parsed_range is None:
            # Full 200 OK
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(file_size))
            self.send_cors_headers()
            self.end_headers()

            if send_body:
                self._stream_file_range(file_path, 0, file_size - 1)
        else:
            # 206 Partial Content
            start, end = parsed_range
            content_len = end - start + 1

            self.send_response(HTTPStatus.PARTIAL_CONTENT)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Range", f"bytes {start}-{end}/{file_size}")
            self.send_header("Content-Length", str(content_len))
            self.send_cors_headers()
            self.end_headers()

            if send_body:
                self._stream_file_range(file_path, start, end)

    def _stream_file_range(self, file_path: Path, start: int, end: int) -> None:
        """Stream a byte range in 64 KB chunks to bound memory use."""
        bytes_to_send = end - start + 1
        try:
            with open(file_path, "rb") as f:
                f.seek(start)
                while bytes_to_send > 0:
                    read_len = min(bytes_to_send, CHUNK_SIZE)
                    chunk = f.read(read_len)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    bytes_to_send -= len(chunk)
        except (BrokenPipeError, ConnectionResetError):
            # Client disconnected or cancelled playback; clean exit
            pass


def create_handler_class(web_dir: Path, audio_dir: Path, ticket: str | None):
    """Factory creating handler with injected directories and ticket."""
    return lambda *args, **kwargs: RangeRequestHandler(
        *args, web_dir=web_dir, audio_dir=audio_dir, ticket=ticket, **kwargs
    )


def run_server(
    host: str = "0.0.0.0",
    port: int = 8080,
    web_dir: Path | str = "./web",
    audio_dir: Path | str = "./fixtures/audio",
    ticket: str | None = DEFAULT_TICKET,
) -> None:
    web_path = Path(web_dir).resolve()
    audio_path = Path(audio_dir).resolve()

    if not web_path.is_dir():
        print(f"ERROR: web directory not found: {web_path}", file=sys.stderr)
        sys.exit(1)
    if not audio_path.is_dir():
        print(f"ERROR: audio directory not found: {audio_path}", file=sys.stderr)
        sys.exit(1)

    handler = create_handler_class(web_path, audio_path, ticket)
    server_address = (host, port)
    httpd = HTTPServer(server_address, handler)

    print(f"Serving HTTP on {host}:{port}")
    print(f"  Web directory:   {web_path}")
    print(f"  Audio directory: {audio_path}")
    print(f"  Media ticket:    {ticket if ticket else '(disabled)'}")
    print(f"  Test URL:        http://{host}:{port}/?ticket={ticket or ''}")

    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nServer stopped.")
    finally:
        httpd.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="BB10 Music163 M0 Fixture Server")
    parser.add_argument("--host", default="0.0.0.0", help="Host address to bind (default: 0.0.0.0)")
    parser.add_argument(
        "--port", type=int, default=8080, help="Port to bind (default: 8080)"
    )
    parser.add_argument(
        "--web-dir",
        default=str(Path(__file__).resolve().parents[1] / "web"),
        help="Directory containing HTML player",
    )
    parser.add_argument(
        "--audio-dir",
        default=str(Path(__file__).resolve().parents[1] / "fixtures" / "audio"),
        help="Directory containing audio fixtures",
    )
    parser.add_argument(
        "--ticket",
        default=DEFAULT_TICKET,
        help=f"Expected authentication ticket (default: {DEFAULT_TICKET})",
    )
    parser.add_argument(
        "--no-auth", action="store_true", help="Disable ticket authentication check"
    )

    args = parser.parse_args()
    chosen_ticket = None if args.no_auth else args.ticket
    run_server(
        host=args.host,
        port=args.port,
        web_dir=args.web_dir,
        audio_dir=args.audio_dir,
        ticket=chosen_ticket,
    )
