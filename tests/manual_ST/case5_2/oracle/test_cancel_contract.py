from __future__ import annotations

import json
import os
import sys
import threading
from datetime import UTC, datetime
from pathlib import Path
from unittest.mock import patch

import pytest
from fastapi.testclient import TestClient


WORKSPACE = Path(os.environ["CASE5_2_WORKSPACE"]).resolve()
BACKEND = WORKSPACE / "backend"
if not BACKEND.is_dir():
    raise RuntimeError(f"CASE5_2_WORKSPACE has no backend directory: {WORKSPACE}")
sys.path.insert(0, str(BACKEND))

from app.api.deps import build_container  # noqa: E402
from app.core.config import Settings  # noqa: E402
from app.domain.entities.simulation import RunStatus, SimulationRun  # noqa: E402
from app.main import create_app  # noqa: E402


def _settings(root: Path) -> Settings:
    return Settings(
        project_root=root,
        database_url="sqlite:///./data/case5_2.db",
        memory_storage_path=Path("./memory"),
        run_artifact_root=Path("./runs"),
        report_output_dir=Path("./reports"),
        document_storage_path=Path("./documents"),
        scenario_template_dir=WORKSPACE / "examples" / "templates",
        sample_scenario_path=WORKSPACE / "examples" / "phase1-proposal-alignment.json",
        llm_provider="fixture",
        llm_model="fixture-model",
        llm_base_url="http://127.0.0.1:1/v1",
    )


def _new_app(root: Path):
    settings = _settings(root)
    container = build_container(settings)
    client = TestClient(create_app(settings=settings, container=container))
    return settings, container, client


def _seed_run(container, run_id: str, status: RunStatus, finished_at: datetime | None = None) -> None:
    run = SimulationRun(
        run_id=run_id,
        scenario_id="scenario-oracle",
        status=status,
        total_rounds=3,
        tick_minutes=15,
        finished_at=finished_at,
    )
    container.simulation_service.run_repository.create(run)
    container.simulation_service.artifact_store.write_run_state(run)


def _state_json(settings: Settings, run_id: str) -> dict[str, object]:
    path = settings.run_artifact_root / run_id / "state.json"
    return json.loads(path.read_text(encoding="utf-8"))


@pytest.mark.parametrize("initial_status", [RunStatus.PENDING, RunStatus.RUNNING])
def test_restart_without_controller_cancels_persisted_active_run(tmp_path: Path, initial_status: RunStatus) -> None:
    root = tmp_path / initial_status.value
    settings_a, container_a, _ = _new_app(root)
    run_id = f"run-restart-{initial_status.value}"
    _seed_run(container_a, run_id, initial_status)

    settings_b, container_b, client_b = _new_app(root)
    assert settings_a.database_url == settings_b.database_url
    assert not container_b.simulation_service.runner_registry.is_running(run_id)

    response = client_b.post(f"/api/v1/simulations/{run_id}/cancel")
    assert response.status_code == 200
    assert response.json() == {"cancelled": True}

    first = container_b.simulation_service.get(run_id)
    assert first is not None
    assert first.status is RunStatus.CANCELLED
    assert first.finished_at is not None
    first_finished_at = first.finished_at
    first_state = _state_json(settings_b, run_id)
    assert first_state["status"] == "cancelled"
    assert datetime.fromisoformat(str(first_state["finished_at"])) == first_finished_at

    with (
        patch.object(
            container_b.simulation_service.run_repository,
            "update",
            wraps=container_b.simulation_service.run_repository.update,
        ) as update_spy,
        patch.object(
            container_b.simulation_service.artifact_store,
            "write_run_state",
            wraps=container_b.simulation_service.artifact_store.write_run_state,
        ) as write_spy,
    ):
        repeated = client_b.post(f"/api/v1/simulations/{run_id}/cancel")

    assert repeated.status_code == 200
    assert repeated.json() == {"cancelled": True}
    assert update_spy.call_count == 0
    assert write_spy.call_count == 0
    second = container_b.simulation_service.get(run_id)
    assert second is not None
    assert second.status is RunStatus.CANCELLED
    assert second.finished_at == first_finished_at
    assert _state_json(settings_b, run_id) == first_state


def test_unknown_and_finished_runs_have_exact_errors_and_no_mutation(tmp_path: Path) -> None:
    settings, container, client = _new_app(tmp_path)
    fixed = datetime(2026, 7, 22, 1, 2, 3, tzinfo=UTC)
    for status in (RunStatus.COMPLETED, RunStatus.FAILED):
        _seed_run(container, f"run-{status.value}", status, fixed)

    missing = client.post("/api/v1/simulations/run-missing/cancel")
    assert missing.status_code == 404
    assert missing.json() == {"detail": "Run not found"}

    for status in (RunStatus.COMPLETED, RunStatus.FAILED):
        run_id = f"run-{status.value}"
        before = _state_json(settings, run_id)
        with (
            patch.object(
                container.simulation_service.run_repository,
                "update",
                wraps=container.simulation_service.run_repository.update,
            ) as update_spy,
            patch.object(
                container.simulation_service.artifact_store,
                "write_run_state",
                wraps=container.simulation_service.artifact_store.write_run_state,
            ) as write_spy,
        ):
            response = client.post(f"/api/v1/simulations/{run_id}/cancel")

        assert response.status_code == 409
        assert response.json() == {"detail": "Run already finished"}
        assert update_spy.call_count == 0
        assert write_spy.call_count == 0
        persisted = container.simulation_service.get(run_id)
        assert persisted is not None
        assert persisted.status is status
        assert persisted.finished_at == fixed
        assert _state_json(settings, run_id) == before


def test_live_controller_receives_cancel_signal(tmp_path: Path) -> None:
    _, container, client = _new_app(tmp_path)
    run_id = "run-live-controller"
    _seed_run(container, run_id, RunStatus.RUNNING)
    entered = threading.Event()
    observed = threading.Event()

    def worker(cancel_event: threading.Event) -> None:
        entered.set()
        if cancel_event.wait(timeout=5):
            observed.set()

    container.simulation_service.runner_registry.start(run_id, worker)
    assert entered.wait(timeout=5)
    response = client.post(f"/api/v1/simulations/{run_id}/cancel")
    assert response.status_code == 200
    assert response.json() == {"cancelled": True}
    assert observed.wait(timeout=5)
    persisted = container.simulation_service.get(run_id)
    assert persisted is not None
    assert persisted.status is RunStatus.CANCELLED


def test_cancel_response_is_explicit_in_openapi(tmp_path: Path) -> None:
    _, _, client = _new_app(tmp_path)
    document = client.get("/openapi.json").json()
    schema = document["paths"]["/api/v1/simulations/{run_id}/cancel"]["post"]["responses"]["200"]["content"][
        "application/json"
    ]["schema"]
    if "$ref" in schema:
        node: object = document
        for part in schema["$ref"].removeprefix("#/").split("/"):
            node = node[part]  # type: ignore[index]
        schema = node
    assert schema["type"] == "object"
    assert schema["properties"]["cancelled"]["type"] == "boolean"
    assert "cancelled" in schema.get("required", [])


def test_in_flight_generation_cannot_commit_after_cancel(tmp_path: Path) -> None:
    settings, container, client = _new_app(tmp_path)
    scenario_response = client.post(
        "/api/v1/scenarios",
        json={"title": "oracle race", "objective": "cancel race", "context": "deterministic fixture"},
    )
    assert scenario_response.status_code == 200
    scenario_id = scenario_response.json()["scenario_id"]

    entered = threading.Event()
    release = threading.Event()
    worker_finished = threading.Event()
    original_execute = container.simulation_service._execute
    original_generate = container.simulation_service._generate_event

    def observed_execute(*args, **kwargs):
        try:
            return original_execute(*args, **kwargs)
        finally:
            worker_finished.set()

    def blocked_generate(*args, **kwargs):
        entered.set()
        if not release.wait(timeout=5):
            raise RuntimeError("oracle did not release blocked generation")
        return original_generate(*args, **kwargs)

    container.simulation_service._execute = observed_execute
    container.simulation_service._generate_event = blocked_generate
    started = client.post(
        "/api/v1/simulations",
        json={"scenario_id": scenario_id, "total_rounds": 1, "tick_minutes": 15},
    )
    assert started.status_code == 200
    run_id = started.json()["run_id"]
    assert entered.wait(timeout=5)

    cancelled = client.post(f"/api/v1/simulations/{run_id}/cancel")
    assert cancelled.status_code == 200
    assert cancelled.json() == {"cancelled": True}
    at_cancel = container.simulation_service.get(run_id)
    assert at_cancel is not None
    assert at_cancel.status is RunStatus.CANCELLED
    assert at_cancel.finished_at is not None
    finished_at = at_cancel.finished_at
    completed_rounds = at_cancel.completed_rounds
    event_count = len(container.simulation_service.list_events(run_id))

    release.set()
    assert worker_finished.wait(timeout=5)
    assert not container.simulation_service.runner_registry.is_running(run_id)

    final = container.simulation_service.get(run_id)
    assert final is not None
    assert final.status is RunStatus.CANCELLED
    assert final.finished_at == finished_at
    assert final.completed_rounds == completed_rounds
    assert len(container.simulation_service.list_events(run_id)) == event_count
    state = _state_json(settings, run_id)
    assert state["status"] == "cancelled"
    assert datetime.fromisoformat(str(state["finished_at"])) == finished_at
