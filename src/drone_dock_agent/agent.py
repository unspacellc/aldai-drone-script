"""Single-file drone dock agent for watching, uploading, and triggering inference."""

from __future__ import annotations

import json
import logging
import mimetypes
import os
import sys
import time
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass
from pathlib import Path
from queue import Queue
from threading import Thread
from typing import Any

import requests
from watchdog.events import FileSystemEvent, FileSystemEventHandler
from watchdog.observers import Observer

CONFIG_VERSION = 1
DEFAULT_CONFIG_PATH = Path("./drone-dock-agent.config.json")

UPLOAD_URL_PATH = "/api/v1/videos/upload-url"
INGEST_MISSION_PATH = "/api/v1/videos/ingest-mission"

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class AgentConfig:
    api_base_url: str
    api_key: str
    watch_directory: Path
    mission_name_prefix: str
    include_extensions: tuple[str, ...]
    stable_check_interval_seconds: float
    stable_required_checks: int
    model_type: str
    annotated_video: bool
    multi_track: bool
    heartbeat_file: Path
    heartbeat_interval_seconds: int
    heartbeat_max_age_seconds: int


@dataclass(frozen=True)
class UploadUrlResult:
    upload_url: str
    fields: dict[str, str]
    s3_key: str


class UnspaceAPIClient:
    def __init__(self, config: AgentConfig):
        self._config = config
        self._session = requests.Session()
        self._session.headers.update(
            {
                "Authorization": f"Bearer {config.api_key}",
                "Content-Type": "application/json",
            }
        )

    def generate_upload_url(self, file_path: Path) -> UploadUrlResult:
        response = self._request(
            "POST",
            UPLOAD_URL_PATH,
            json={
                "filename": file_path.name,
                "file_size": file_path.stat().st_size,
            },
            timeout=60,
        )
        return UploadUrlResult(
            upload_url=response["uploadUrl"],
            fields=response["fields"],
            s3_key=response["s3Key"],
        )

    def upload_file_to_s3(self, upload: UploadUrlResult, file_path: Path) -> None:
        content_type = mimetypes.guess_type(file_path.name)[0] or "application/octet-stream"
        with file_path.open("rb") as file_handle:
            response = requests.post(
                upload.upload_url,
                data=upload.fields,
                files={"file": (file_path.name, file_handle, content_type)},
                timeout=300,
            )
            response.raise_for_status()

    def ingest_uploaded_mission(
        self,
        *,
        s3_key: str,
        source_path: Path,
        mission_name: str,
        mission_metadata: dict[str, Any],
    ) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "s3_key": s3_key,
            "name": mission_name,
            "metadata": {
                "source": "hextronics-drone-dock",
                "source_filename": source_path.name,
                "mission_metadata": mission_metadata,
            },
            "model_type": self._config.model_type,
            "annotated_video": self._config.annotated_video,
            "multi_track": self._config.multi_track,
        }

        if track_group := mission_metadata.get("track_group_designator"):
            payload["track_group"] = track_group

        track_label = _build_track_label(mission_metadata)
        if track_label:
            payload["track_label"] = track_label

        return self._request(
            "POST",
            INGEST_MISSION_PATH,
            json=payload,
            timeout=60,
        )

    def _request(self, method: str, path: str, *, json: dict[str, Any], timeout: int) -> dict[str, Any]:
        response = self._session.request(
            method,
            f"{self._config.api_base_url}{path}",
            json=json,
            timeout=timeout,
        )
        response.raise_for_status()
        return response.json()


class FileProcessor:
    def __init__(self, config: AgentConfig, api_client: UnspaceAPIClient):
        self._config = config
        self._api_client = api_client

    def process(self, file_path: Path) -> None:
        if file_path.suffix.lower() not in self._config.include_extensions:
            logger.info("Skipping unsupported file extension: %s", file_path)
            return

        self._wait_until_file_stable(file_path)
        mission_metadata = _load_mission_metadata(file_path)
        mission_name = _build_mission_name(self._config.mission_name_prefix, file_path, mission_metadata)

        logger.info("Processing %s with mission metadata %s", file_path, mission_metadata)

        upload = self._api_client.generate_upload_url(file_path=file_path)
        self._api_client.upload_file_to_s3(upload=upload, file_path=file_path)
        ingest_result = self._api_client.ingest_uploaded_mission(
            s3_key=upload.s3_key,
            source_path=file_path,
            mission_name=mission_name,
            mission_metadata=mission_metadata,
        )

        mission = ingest_result.get("mission", {})
        ml_trigger = ingest_result.get("ml_trigger", {})
        logger.info(
            "Completed ingest flow for %s (mission=%s, s3_key=%s, ml_trigger=%s)",
            file_path.name,
            mission.get("id"),
            upload.s3_key,
            ml_trigger,
        )

    def _wait_until_file_stable(self, file_path: Path) -> None:
        stable_checks = 0
        previous_size = -1

        while stable_checks < self._config.stable_required_checks:
            current_size = file_path.stat().st_size
            if current_size == previous_size:
                stable_checks += 1
            else:
                stable_checks = 0
                previous_size = current_size

            time.sleep(self._config.stable_check_interval_seconds)


class _CreatedFileHandler(FileSystemEventHandler):
    def __init__(self, queue: Queue[Path]):
        self._queue = queue

    def on_created(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        self._queue.put(Path(event.src_path))


class DirectoryWatcher:
    def __init__(self, watch_directory: Path, processor: FileProcessor):
        self._watch_directory = watch_directory
        self._processor = processor
        self._queue: Queue[Path] = Queue()
        self._observer = Observer()

    def run_forever(self) -> None:
        self._enqueue_existing_files()
        Thread(target=self._consume, daemon=True).start()

        handler = _CreatedFileHandler(self._queue)
        self._observer.schedule(handler, str(self._watch_directory), recursive=False)
        self._observer.start()
        logger.info("Watching %s for drone dock outputs", self._watch_directory)

        try:
            self._observer.join()
        finally:
            self._observer.stop()
            self._observer.join()

    def _enqueue_existing_files(self) -> None:
        for path in sorted(self._watch_directory.iterdir()):
            if path.is_file():
                self._queue.put(path)

    def _consume(self) -> None:
        while True:
            file_path = self._queue.get()
            try:
                self._processor.process(file_path)
            except Exception:
                logger.exception("Failed processing %s", file_path)
            finally:
                self._queue.task_done()


def parse_kmz_filename(kmz_filename: str) -> dict[str, str]:
    """Extract mission metadata from Yard_TrackGroup_FirstTrack_LastTrack.kmz."""
    base_name = os.path.splitext(os.path.basename(kmz_filename))[0]
    parts = base_name.split("_")

    if len(parts) < 4:
        raise ValueError(
            f"KMZ filename '{kmz_filename}' does not match expected format: "
            "YardName_TrackGroup_FirstTrack_LastTrack.kmz"
        )

    return {
        "yard_name": parts[0],
        "track_group_designator": parts[1],
        "first_track_designator": parts[2],
        "last_track_designator": parts[3],
    }


def extract_heading_from_kmz(kmz_path: Path) -> str:
    """Read WPML embedded in KMZ and extract heading angle."""
    with zipfile.ZipFile(kmz_path, "r") as kmz:
        wpml_filename = next(
            (
                filename
                for filename in kmz.namelist()
                if filename.endswith("waylines.wpml") or filename.endswith(".wpml")
            ),
            None,
        )
        if not wpml_filename:
            raise FileNotFoundError(f"Could not locate .wpml inside KMZ: {kmz_path}")

        root = ET.fromstring(kmz.read(wpml_filename))
        for element in root.iter():
            if "waypointHeadingAngle" in element.tag or "executeWaypointHeading" in element.tag:
                return (element.text or "").strip() or "UNKNOWN"

    logger.warning("Could not find heading angle in WPML for %s", kmz_path)
    return "UNKNOWN"


def _resolve_kmz_path(video_path: Path) -> Path | None:
    same_stem_kmz = video_path.with_suffix(".kmz")
    if same_stem_kmz.exists():
        return same_stem_kmz

    candidates = sorted(video_path.parent.glob("*.kmz"))
    return candidates[0] if candidates else None


def _load_mission_metadata(video_path: Path) -> dict[str, Any]:
    kmz_path = _resolve_kmz_path(video_path)
    if not kmz_path:
        logger.warning("No KMZ found for %s; continuing with minimal metadata", video_path)
        return {"source": "hextronics-drone-dock", "kmz_found": False}

    metadata = parse_kmz_filename(kmz_path.name)
    metadata["drone_heading_angle"] = extract_heading_from_kmz(kmz_path)
    metadata["kmz_found"] = True
    metadata["kmz_filename"] = kmz_path.name
    return metadata


def _build_track_label(metadata: dict[str, Any]) -> str | None:
    first_track = metadata.get("first_track_designator")
    last_track = metadata.get("last_track_designator")
    if first_track and last_track:
        return f"{first_track}-{last_track}"
    return str(first_track or last_track) if first_track or last_track else None


def _build_mission_name(prefix: str, file_path: Path, metadata: dict[str, Any]) -> str:
    if metadata.get("kmz_found"):
        return (
            f"{prefix} - {metadata.get('yard_name', 'unknown')}_"
            f"{metadata.get('track_group_designator', 'unknown')}_"
            f"{metadata.get('first_track_designator', 'unknown')}_"
            f"{metadata.get('last_track_designator', 'unknown')}"
        )
    return f"{prefix} - {file_path.stem}"


def load_config() -> AgentConfig:
    config_path = Path(os.environ.get("DRONE_DOCK_CONFIG_PATH", str(DEFAULT_CONFIG_PATH)))
    with config_path.open("r", encoding="utf-8") as config_file:
        raw_config = json.load(config_file)

    if int(raw_config.get("config_version", -1)) != CONFIG_VERSION:
        raise ValueError(f"Unsupported config_version in {config_path}. Expected {CONFIG_VERSION}.")

    token = str(raw_config.get("api_key", "")).strip() or os.environ.get(
        "DRONE_DOCK_API_KEY", ""
    ).strip()
    if not token:
        raise ValueError("Missing API key in config or DRONE_DOCK_API_KEY")
    if not token.startswith("ysk_"):
        raise ValueError("Drone ingest requires a yard API key starting with ysk_")

    include_extensions = raw_config.get("include_extensions", [".mp4", ".mov", ".mkv", ".avi"])
    if isinstance(include_extensions, str):
        include_extensions = [value.strip() for value in include_extensions.split(",") if value.strip()]

    return AgentConfig(
        api_base_url=str(raw_config["api_base_url"]).rstrip("/"),
        api_key=token,
        watch_directory=Path(str(raw_config["watch_directory"])).expanduser().resolve(),
        mission_name_prefix=str(raw_config.get("mission_name_prefix", "Hextronics Dock")),
        include_extensions=tuple(f".{str(value).lower().lstrip('.')}" for value in include_extensions),
        stable_check_interval_seconds=float(raw_config.get("stable_check_interval_seconds", 1.0)),
        stable_required_checks=int(raw_config.get("stable_required_checks", 3)),
        model_type=str(raw_config.get("model_type", "coupler_genie")),
        annotated_video=_to_bool(raw_config.get("annotated_video", True)),
        multi_track=_to_bool(raw_config.get("multi_track", False)),
        heartbeat_file=Path(str(raw_config.get("heartbeat_file", "/tmp/drone-dock-agent.heartbeat"))),
        heartbeat_interval_seconds=int(raw_config.get("heartbeat_interval_seconds", 15)),
        heartbeat_max_age_seconds=int(raw_config.get("heartbeat_max_age_seconds", 120)),
    )


def _to_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "y", "on"}
    return bool(value)


class HeartbeatWriter:
    """Writes a heartbeat file so systemd health checks can verify liveness."""

    def __init__(self, heartbeat_file: Path, interval_seconds: int):
        self._heartbeat_file = heartbeat_file
        self._interval_seconds = interval_seconds

    def start(self) -> None:
        self._heartbeat_file.parent.mkdir(parents=True, exist_ok=True)
        Thread(target=self._run, daemon=True).start()

    def _run(self) -> None:
        while True:
            self._heartbeat_file.write_text(str(int(time.time())), encoding="utf-8")
            time.sleep(self._interval_seconds)


def run_healthcheck(config: AgentConfig) -> int:
    """Health check for systemd ExecStartPre/ExecStartPost probes."""
    if not config.watch_directory.exists():
        logger.error("Healthcheck failed: watch directory does not exist: %s", config.watch_directory)
        return 1

    if not config.watch_directory.is_dir():
        logger.error("Healthcheck failed: watch directory is not a directory: %s", config.watch_directory)
        return 1

    if not os.access(config.watch_directory, os.R_OK):
        logger.error("Healthcheck failed: watch directory is not readable: %s", config.watch_directory)
        return 1

    if config.heartbeat_file.exists():
        now = int(time.time())
        heartbeat_age = now - int(config.heartbeat_file.read_text(encoding="utf-8").strip())
        if heartbeat_age > config.heartbeat_max_age_seconds:
            logger.error(
                "Healthcheck failed: heartbeat is stale (%ss old, max %ss)",
                heartbeat_age,
                config.heartbeat_max_age_seconds,
            )
            return 1

    try:
        response = requests.get(f"{config.api_base_url}/health", timeout=5)
        response.raise_for_status()
    except Exception as exc:
        logger.error("Healthcheck failed: API /health is unreachable: %s", exc)
        return 1

    logger.info("Healthcheck passed")
    return 0


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s [%(name)s] %(message)s")
    config = load_config()
    if "--healthcheck" in sys.argv:
        raise SystemExit(run_healthcheck(config))

    heartbeat_writer = HeartbeatWriter(
        heartbeat_file=config.heartbeat_file,
        interval_seconds=config.heartbeat_interval_seconds,
    )
    heartbeat_writer.start()

    api_client = UnspaceAPIClient(config)
    processor = FileProcessor(config=config, api_client=api_client)
    watcher = DirectoryWatcher(watch_directory=config.watch_directory, processor=processor)
    watcher.run_forever()


if __name__ == "__main__":
    main()
