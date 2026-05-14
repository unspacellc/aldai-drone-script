#!/usr/bin/env python3
"""Upload one video using the existing dock-agent upload request.

Set API_KEY at the top of the file, then run:
    python upload_video.py /path/to/video.mp4
"""

from __future__ import annotations

import datetime
import http.client
import socket
import json
import mimetypes
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

import certifi

SSL_CTX = ssl.create_default_context(cafile=certifi.where())

API_BASE_URL = "https://api.demarimiller.com"
API_KEY = "ysk_j4elurfuswJ_gX7mud2u3qwiNrlfOLn1-NEy4mf3PRs"
UPLOAD_URL_PATH = "/api/v1/videos/upload-url"
UPLOAD_URL_TIMEOUT_SECONDS = 60
DEFAULT_CONTENT_TYPE = "application/octet-stream"
CHUNK_SIZE = 8 * 1024 * 1024  # 8 MB


def main() -> int:
    if len(sys.argv) != 2:
        print("No video path provided.", file=sys.stderr)
        print("Usage: python upload_video.py /path/to/video.mp4", file=sys.stderr)
        print("To test: python upload_video.py test.mp4", file=sys.stderr)
        return 1

    try:
        video_path = validate_video_path(sys.argv[1])
        upload = generate_upload_url(api_key=API_KEY, file_path=video_path)
        upload_file_to_s3(upload=upload, file_path=video_path)
    except urllib.error.HTTPError as exc:
        body = exc.read().decode(errors="replace")
        print(f"Upload failed: HTTP {exc.code} {exc.reason}: {body}", file=sys.stderr)
        return 1
    except Exception as exc:
        print(f"Upload failed: {exc}", file=sys.stderr)
        return 1

    print(json.dumps({"s3_key": upload["s3Key"]}))
    return 0


def validate_video_path(video_path: str) -> Path:
    resolved_video_path = Path(video_path.strip()).expanduser().resolve()
    if not resolved_video_path.exists():
        raise ValueError(f"video_path does not exist: {resolved_video_path}")
    if not resolved_video_path.is_file():
        raise ValueError(f"video_path is not a file: {resolved_video_path}")
    return resolved_video_path


def generate_upload_url(*, api_key: str, file_path: Path) -> dict:
    hostname = socket.gethostname()
    ts = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    stem = file_path.stem
    suffix = file_path.suffix
    filename = f"{stem}_{hostname}_{ts}{suffix}"

    body = json.dumps({
        "filename": filename,
        "file_size": file_path.stat().st_size,
        "yard_id": "bd38e14b-140c-438e-910e-5abc73311e15"
    }).encode()

    req = urllib.request.Request(
        f"{API_BASE_URL.rstrip('/')}{UPLOAD_URL_PATH}",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )

    with urllib.request.urlopen(req, timeout=UPLOAD_URL_TIMEOUT_SECONDS, context=SSL_CTX) as resp:
        return json.loads(resp.read())


def upload_file_to_s3(*, upload: dict, file_path: Path) -> None:
    content_type = mimetypes.guess_type(file_path.name)[0] or DEFAULT_CONTENT_TYPE
    boundary = uuid.uuid4().hex

    field_parts = b""
    for key, value in upload["fields"].items():
        field_parts += (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="{key}"\r\n\r\n'
            f"{value}\r\n"
        ).encode()
    field_parts += (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="Content-Type"\r\n\r\n'
        f"{content_type}\r\n"
    ).encode()

    file_header = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{file_path.name}"\r\n'
        f"Content-Type: {content_type}\r\n\r\n"
    ).encode()
    file_footer = f"\r\n--{boundary}--\r\n".encode()

    content_length = len(field_parts) + len(file_header) + file_path.stat().st_size + len(file_footer)

    parsed = urllib.parse.urlparse(str(upload["uploadUrl"]))
    path = parsed.path + (f"?{parsed.query}" if parsed.query else "")

    SSL_CTX = ssl.create_default_context(cafile=certifi.where())
    conn = http.client.HTTPSConnection(parsed.netloc, context=SSL_CTX)
    conn.putrequest("POST", path)
    conn.putheader("Content-Type", f"multipart/form-data; boundary={boundary}")
    conn.putheader("Content-Length", str(content_length))
    conn.endheaders()

    conn.send(field_parts)
    conn.send(file_header)
    with file_path.open("rb") as fh:
        while True:
            chunk = fh.read(CHUNK_SIZE)
            if not chunk:
                break
            conn.send(chunk)
    conn.send(file_footer)

    resp = conn.getresponse()
    if resp.status not in (200, 204):
        body = resp.read().decode(errors="replace")
        raise ValueError(f"S3 upload failed: HTTP {resp.status}: {body}")


if __name__ == "__main__":
    raise SystemExit(main())
