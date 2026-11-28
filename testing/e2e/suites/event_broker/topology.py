"""Own the Event Broker worker and its authenticated platform host."""

from __future__ import annotations

import ctypes
import json
import os
import re
import secrets
import signal
import subprocess
import sys
import time
import uuid
from collections.abc import Callable, Iterable
from contextlib import contextmanager
from pathlib import Path

import httpx

_ROOT = Path(__file__).resolve().parents[4]
BROKER_CONFIG = _ROOT / "config" / "event-broker-standalone.yaml"
_CORE_CONFIG = _ROOT / "config" / "event-broker-test-flight-control.yaml"
_DEMO_ENTITIES = Path(__file__).with_name("fixtures") / "demo_entities.json"
_BASE_SCHEMAS = _ROOT / "gears/system/event-broker/docs/schemas"

OWNER_TOKEN = "eb-e2e-principal-a"
OWNER_SUBJECT = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
OWNER_TENANT = "00000000-0000-0000-0000-000000000002"

TOPIC_SCHEMA = "gts.cf.core.events.topic.v1~"
EVENT_SCHEMA = "gts.cf.core.events.event.v1~"
_PLUGIN_BASE = "gts.cf.toolkit.plugins.plugin.v1~cf.core."
_REQUIRED_ENTITIES = {
    _PLUGIN_BASE + "authz_resolver.plugin.v1~cf.e2e.eb_test_policy.plugin.v1": "instance",
    _PLUGIN_BASE + "authz_resolver.plugin.v1~cf.builtin.tr_authz_resolver.plugin.v1": "instance",
    _PLUGIN_BASE + "authn_resolver.plugin.v1~cf.builtin.static_authn_resolver.plugin.v1": (
        "instance"
    ),
    _PLUGIN_BASE + "tenant_resolver.plugin.v1~cf.builtin.static_tenant_resolver.plugin.v1": (
        "instance"
    ),
}
REGISTRY_PATH = "/types-registry/v2/platform/entities"
STARTUP_TIMEOUT = 30.0
PEER_CALL_TIMEOUT = 3.0
SHUTDOWN_TIMEOUT = 5.0
KILL_TIMEOUT = 3.0
RECHECK = 0.05


def _write_config(path: Path, config: str) -> None:
    with path.open("x", encoding="utf-8") as stream:
        stream.write(config)


def _artifact(variable: str) -> Path:
    value = os.environ.get(variable)
    if not value:
        raise RuntimeError(f"{variable} is required")
    path = Path(value)
    if not path.is_absolute():
        raise RuntimeError(f"{variable} must select an absolute executable path")
    try:
        path = path.resolve(strict=True)
    except OSError:
        raise RuntimeError(f"{variable} must select an existing executable") from None
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RuntimeError(f"{variable} must select an executable file")
    return path


def replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        raise ValueError(f"expected one configuration anchor: {old.splitlines()[0]}")
    return text.replace(old, new, 1)


def entity_id(document: dict) -> str:
    if "id" in document:
        return document["id"]
    return document["$id"].removeprefix("gts://")


def registry_entity(document: dict) -> dict:
    gts_id = entity_id(document)
    return {
        "gts_id": gts_id,
        "gts_uuid": str(uuid.uuid5(uuid.uuid5(uuid.NAMESPACE_URL, "gts"), gts_id)),
        "kind": "instance" if "id" in document else "type_schema",
        "lifecycle_status": "active",
        "ownership": "global",
        "content": document,
    }


def _process_executable(pid: int) -> Path:
    if sys.platform == "darwin":
        buffer = ctypes.create_string_buffer(4096)
        if ctypes.CDLL("/usr/lib/libproc.dylib").proc_pidpath(pid, buffer, len(buffer)) <= 0:
            raise ProcessLookupError(f"cannot read executable for child {pid}")
        return Path(os.fsdecode(buffer.value))
    return Path(os.readlink(f"/proc/{pid}/exe"))


class _Child:
    def __init__(self, binary: Path, config: Path, log: Path, args: list[str], env: dict[str, str]):
        self.binary = binary
        self.log = log
        self.started = time.monotonic()
        argv = [str(binary), "--config", str(config), *args]
        child_env = {
            name: os.environ[name]
            for name in ("PATH", "SystemRoot", "TMPDIR", "TEMP", "TMP")
            if name in os.environ
        }
        with log.open("xb") as stream:
            self.proc = subprocess.Popen(
                argv, cwd=config.parent, env={**child_env, **env}, stdout=stream,
                stderr=subprocess.STDOUT, start_new_session=os.name == "posix",
            )
        print(f"started pid={self.proc.pid} argv={argv!r} log={log}", flush=True)

    @property
    def pid(self) -> int:
        return self.proc.pid

    def owns_process(self) -> bool:
        if self.proc.poll() is not None:
            return False
        if os.name != "posix":
            return True
        try:
            group = os.getpgid(self.pid)
            executable = _process_executable(self.pid)
        except (FileNotFoundError, ProcessLookupError):
            if self.proc.poll() is not None:
                return False
            raise RuntimeError(f"cannot verify child identity for pid={self.pid}") from None
        if group != self.pid or executable != self.binary:
            raise RuntimeError(f"unexpected process group or executable for child pid={self.pid}")
        return True

    def require_live(self) -> None:
        if not self.owns_process():
            raise RuntimeError(f"{self.binary.name} pid={self.pid} exited {self.proc.returncode}")

    def _signal(self, signum: int) -> None:
        if not self.owns_process():
            return
        try:
            if os.name == "posix":
                os.killpg(self.pid, signum)
            elif signum == signal.SIGTERM:
                self.proc.terminate()
            else:
                self.proc.kill()
        except ProcessLookupError:
            pass

    def wait(self, timeout: float) -> int:
        return self.proc.wait(timeout=timeout)

    def stop(self, force: bool = False) -> None:
        started = time.monotonic()
        running = self.proc.poll() is None
        kill = getattr(signal, "SIGKILL", signal.SIGTERM)
        self._signal(kill if force else signal.SIGTERM)
        try:
            status = self.wait(KILL_TIMEOUT if force else SHUTDOWN_TIMEOUT)
        except subprocess.TimeoutExpired:
            self._signal(kill)
            status = self.wait(KILL_TIMEOUT)
            raise RuntimeError(
                f"child pid={self.pid} needed forced shutdown (exit={status})"
            ) from None
        finally:
            print(
                f"cleanup pid={self.pid} exe={self.binary} status={self.proc.returncode} "
                f"elapsed={time.monotonic() - started:.3f}s",
                flush=True,
            )
        if os.name == "posix" and running and not force and status != 0:
            raise RuntimeError(f"child pid={self.pid} exited {status} during graceful shutdown")


def _loopback(url: str) -> str:
    if not re.fullmatch(r"http://127\.0\.0\.1:[0-9]{1,5}/?", url):
        raise RuntimeError("expected a literal loopback HTTP listener")
    if not 0 < httpx.URL(url).port <= 65535:
        raise RuntimeError("expected a nonzero loopback listener port")
    return url.rstrip("/")


def _event_fields(log: str) -> dict[str, dict]:
    events = {}
    for line in log.splitlines():
        try:
            fields = json.loads(line)["fields"]
        except (ValueError, KeyError, TypeError):
            continue
        if isinstance(fields, dict) and isinstance(fields.get("message"), str):
            events[fields["message"]] = fields
    return events


def _pause_until(deadline: float, message: str) -> None:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError(message)
    time.sleep(min(RECHECK, remaining))


def _call_timeout(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("fixture startup deadline expired")
    return min(PEER_CALL_TIMEOUT, remaining)


def _release(resources: Iterable[Callable[[], None]], primary, redact: Callable[[str], str]):
    errors = []
    for release in resources:
        try:
            release()
        except BaseException as error:
            errors.append(RuntimeError(redact(str(error))))
    if errors:
        failure = ExceptionGroup("fixture cleanup failed", errors)
        if primary is None:
            raise failure
        for error in errors:
            primary.add_note(f"fixture cleanup failed: {error}")


@contextmanager
def own_sidecars(sidecars, *, redact=lambda message: message):
    acquired = []
    primary = None
    try:
        for sidecar in sidecars:
            acquired.append(sidecar)
            sidecar.start()
        yield {sidecar.name: sidecar for sidecar in acquired}
    except BaseException as error:
        primary = error
        raise
    finally:
        _release((sidecar.stop for sidecar in reversed(acquired)), primary, redact)


class FlightControlBrokerEnv:
    """Own two process lifetimes and expose only their loopback listeners."""

    def __init__(
        self,
        root: Path,
        *,
        broker_config: Callable[[str, Path, dict], str],
        documents: Iterable[dict] = (),
        narrowing_policy: bool = False,
        additional_event_types: Iterable[str] = (),
        additional_grants: Iterable[dict] = (),
        sidecars: dict | None = None,
        health_path: str = "/readyz",
        refusal: bool = False,
    ):
        self._binaries = (_artifact("E2E_BINARY"), _artifact("E2E_FLIGHT_CONTROL_BINARY"))
        if self._binaries[0].samefile(self._binaries[1]):
            raise RuntimeError("Event Broker and Flight Control require distinct executables")
        self._mode = os.environ.get("E2E_CLUSTER_MODE", "memory")
        if self._mode not in ("memory", "remote"):
            raise RuntimeError("E2E_CLUSTER_MODE must be memory or remote")

        self.root = root
        self.broker_home = root / "event-broker"
        self.core_home = root / "flight-control"
        self.sidecars = sidecars if sidecars is not None else {}
        self._broker_config = broker_config
        self._documents = list(documents)
        self._items = json.loads(_DEMO_ENTITIES.read_text(encoding="utf-8"))["items"]
        self._items.extend(
            {"gts_id": entity_id(document), "content": document}
            for document in self._documents
        )
        self._narrowing_policy = narrowing_policy
        self._additional_event_types = list(additional_event_types)
        self._additional_grants = list(additional_grants)
        self._health_path = health_path
        self._refusal = refusal
        self._workload = secrets.token_hex(32)
        self._secrets = (self._workload,)
        self._broker: _Child | None = None
        self._core: _Child | None = None
        self._attempt = 0

    @property
    def platform_headers(self) -> dict[str, str]:
        return {"x-toolkit-internal-token": self._workload}

    @property
    def broker_pid(self) -> int:
        return self._broker.pid

    def broker_running(self) -> bool:
        return self._broker.owns_process()

    @property
    def core_pid(self) -> int:
        return self._core.pid

    @property
    def logs(self) -> dict[str, Path]:
        return {"broker": self._broker.log, "flight-control": self._core.log}

    def redact(self, message: str) -> str:
        for secret in self._secrets:
            message = message.replace(secret, "[redacted]")
        return re.sub(r"(://[^\s/:]+:)[^\s@]+@", r"\1[redacted]@", message)

    def log(self, role: str = "broker") -> str:
        return self.redact(self.logs[role].read_text(encoding="utf-8", errors="replace"))

    def _auth(self) -> dict:
        return {"provider": "shared_secret", "secret": self._workload, "peer_name": "e2e-platform"}

    def _start_core(self) -> None:
        config = json.loads(_CORE_CONFIG.read_text(encoding="utf-8"))
        config["server"]["home_dir"] = self.core_home.as_posix()
        gears = config["gears"]
        for name in ("api-gateway", "grpc-hub"):
            gears[name]["config"]["internal_auth"] = self._auth()
        gears["api-gateway"]["config"]["bind_addr"] = "127.0.0.1:0"
        gears["grpc-hub"]["config"]["listen_addr"] = "127.0.0.1:0"
        self._secrets += tuple(
            entry["token"] for entry in gears["static-authn-plugin"]["config"]["tokens"]
        )
        if self._narrowing_policy:
            gears["authz-resolver"]["config"]["vendor"] = "e2e"
            grants = gears["eb-test-policy"]["config"]["grants"]
            for document in self._documents:
                gts_id = entity_id(document)
                grants.append({
                    "subject_id": OWNER_SUBJECT,
                    "action": "consume" if gts_id.startswith(TOPIC_SCHEMA) else "produce",
                    "resource": gts_id,
                })
            for gts_id in self._additional_event_types:
                grants.append(
                    {"subject_id": OWNER_SUBJECT, "action": "produce", "resource": gts_id}
                )
            grants.extend(self._additional_grants)
        if self._mode == "remote":
            gears["cluster"] = {"config": {"profiles": {"event-broker": {
                "cache": {"provider": "standalone"},
            }}}}
        path = self.core_home / "config.yaml"
        _write_config(path, json.dumps(config, indent=2))
        self._core = _Child(
            self._binaries[1], path, self.core_home / "core.log", ["run"], {},
        )
        self._wait_for_core()

    def _wait_for_core(self) -> None:
        deadline = self._core.started + STARTUP_TIMEOUT
        while True:
            self._core.require_live()
            events = _event_fields(self.redact(
                self._core.log.read_text(encoding="utf-8", errors="replace"),
            ))
            directory = events.get("gRPC hub listening", {}).get("bound_addr")
            gateway = events.get("REST host endpoint published for directory registration", {})
            if directory and gateway.get("endpoint") and (
                "late gRPC services installed on the running listener" in events
            ):
                self.directory_endpoint = _loopback(f"http://{directory}")
                self.flight_control_base_url = _loopback(gateway["endpoint"])
                if self.directory_endpoint == self.flight_control_base_url:
                    raise RuntimeError("Flight Control REST and gRPC listeners coincide")
                break
            _pause_until(deadline, "Flight Control did not publish its listeners and plugins")

        with httpx.Client(
            base_url=self.flight_control_base_url, headers=self.platform_headers, trust_env=False,
        ) as client:
            for gts_id, kind in _REQUIRED_ENTITIES.items():
                self._read_active(client, gts_id, kind, deadline)
        print(
            f"Core admitted pid={self.core_pid} exe={self._core.binary} "
            f"http={self.flight_control_base_url} grpc={self.directory_endpoint} "
            f"elapsed={time.monotonic() - self._core.started:.3f}s",
            flush=True,
        )

    def _read_active(self, client, gts_id: str, kind: str, deadline: float) -> dict:
        while True:
            self._core.require_live()
            if self._broker is not None:
                self._broker.require_live()
            try:
                response = client.get(
                    f"{REGISTRY_PATH}/{gts_id}", params={"$select": "content"},
                    timeout=_call_timeout(deadline),
                )
            except httpx.TransportError:
                pass
            else:
                if response.status_code == 200:
                    self._core.require_live()
                    assert response.headers.get("content-type") == "application/json"
                    body = response.json()
                    if (
                        isinstance(body, dict) and body.get("gts_id") == gts_id
                        and body.get("kind") == kind and body.get("lifecycle_status") == "active"
                    ):
                        return body
                elif response.status_code not in (404, 503):
                    raise RuntimeError(
                        f"Flight Control registry read failed (HTTP {response.status_code})"
                    )
            _pause_until(deadline, f"Flight Control did not admit {gts_id}")

    def _admit_documents(self, deadline: float) -> None:
        with httpx.Client(
            base_url=self.flight_control_base_url, headers=self.platform_headers, trust_env=False,
        ) as client:
            for gts_id in (EVENT_SCHEMA, TOPIC_SCHEMA):
                schema = json.loads(
                    (_BASE_SCHEMAS / f"{gts_id}.schema.json").read_text(encoding="utf-8"),
                )
                assert self._read_active(
                    client, gts_id, "type_schema", deadline,
                ) == registry_entity(schema)
            if self._attempt == 1:
                self._submit_documents(client, deadline)
            for item in self._items:
                gts_id = item["gts_id"]
                expected = registry_entity(item["content"])
                assert self._read_active(client, gts_id, expected["kind"], deadline) == expected
        print(
            f"Core pid={self.core_pid} serves {len(self._items)} admitted documents "
            f"after broker pid={self.broker_pid} published its base schemas "
            f"in {time.monotonic() - self._broker.started:.3f}s",
            flush=True,
        )

    def _submit_documents(self, client: httpx.Client, deadline: float) -> None:
        response = client.post(
            REGISTRY_PATH, headers={"Idempotency-Key": "eb-e2e-fixture-registration"},
            json={"items": self._items}, timeout=_call_timeout(deadline),
        )
        assert response.status_code == 202, self.redact(response.text)
        location = response.url.join(response.headers["location"])
        assert (location.scheme, location.host, location.port) == (
            response.url.scheme, response.url.host, response.url.port,
        )
        assert not location.userinfo, "credentials in registry Location"
        while True:
            self._core.require_live()
            self._broker.require_live()
            polled = client.get(location, timeout=_call_timeout(deadline))
            assert (polled.status_code, polled.headers.get("content-type")) == (
                200, "application/json",
            ), self.redact(polled.text)
            operation = polled.json()
            if operation["status"] == "completed":
                break
            _pause_until(deadline, "Flight Control fixture registration did not complete")
        assert operation["items"] == [
            {"gts_id": item["gts_id"], "status": "succeeded", "resource_version": 1, "error": None}
            for item in self._items
        ], self.redact(polled.text)

    def _wait_for_catalogue(self, deadline: float) -> None:
        with httpx.Client(
            base_url=f"{self.base_url}/event-broker/v1",
            headers={"Authorization": f"Bearer {OWNER_TOKEN}"},
            trust_env=False,
        ) as client:
            for item in self._items:
                gts_id = item["gts_id"]
                path = "topics" if gts_id.startswith(TOPIC_SCHEMA) else "event-types"
                while True:
                    self._core.require_live()
                    self._broker.require_live()
                    response = client.get(
                        path, params={"$filter": f"id eq '{gts_id}'"},
                        timeout=_call_timeout(deadline),
                    )
                    if response.status_code == 200:
                        body = response.json()
                        if body["items"]:
                            assert [entry["id"] for entry in body["items"]] == [gts_id]
                            assert body["page_info"] == {
                                "next_cursor": None, "prev_cursor": None, "limit": 25,
                            }
                            break
                    elif response.status_code != 503:
                        raise RuntimeError(self.redact(response.text))
                    _pause_until(deadline, f"Event Broker catalogue did not include {gts_id}")

    def _write_broker_config(self) -> None:
        text = BROKER_CONFIG.read_text(encoding="utf-8")
        text = replace_once(
            text, 'home_dir: "~/.cf-gears-event-broker"',
            f"home_dir: {json.dumps(self.broker_home.as_posix())}",
        )
        text = replace_once(text, 'listen_addr: "127.0.0.1:8080"', 'listen_addr: "127.0.0.1:0"')
        text = replace_once(
            text, 'advertise_uri: "http://127.0.0.1:8080"', 'advertise_uri: "http://127.0.0.1:0"',
        )
        text = replace_once(
            text, 'secret: "eb-local-demo-workload-credential"',
            f"secret: {json.dumps(self._workload)}",
        )
        endpoint = json.dumps(self.flight_control_base_url)
        clients = (
            "        types-registry:\n"
            f"          endpoint: {endpoint}\n"
            f"        authz-resolver:\n"
            f"          endpoint: {endpoint}\n"
        )
        text = replace_once(
            text,
            '        types-registry:\n'
            '          endpoint: "http://127.0.0.1:8081"\n'
            '        authz-resolver:\n'
            '          endpoint: "http://127.0.0.1:8081"\n',
            clients,
        )
        if self._mode == "remote":
            text = replace_once(text, "mode: memory", "mode: remote")
            directory = json.dumps(self.directory_endpoint)
            text = replace_once(
                text,
                "gears:\n",
                "gears:\n"
                "  cluster-sdk:\n"
                "    clients:\n"
                "      consumes:\n"
                f"        cluster: {{ grpc_endpoint: {directory} }}\n",
            )
        text = self._broker_config(text, self.broker_home, self.sidecars)
        _write_config(self.broker_home / "config.yaml", text)

    def _start_broker(self) -> None:
        self._core.require_live()
        self._attempt += 1
        self._broker = _Child(
            self._binaries[0], self.broker_home / "config.yaml",
            self.broker_home / f"broker-{self._attempt}.log", ["--e2e-flight-control", "run"],
            {
                # The Core gRPC hub serves the directory and private bridge.
                "TOOLKIT_DIRECTORY_ENDPOINT": self.directory_endpoint,
                "E2E_FLIGHT_CONTROL_ENDPOINT": self.directory_endpoint,
            },
        )
        if not self._refusal:
            deadline = self._broker.started + STARTUP_TIMEOUT
            self._admit_documents(deadline)
            self._wait_for_broker(deadline)
            # Outage tests enter on the early health probe to watch preparation
            # through route attachment with the backend unavailable.
            if self._health_path == "/readyz":
                self._wait_for_ingest_outbox(deadline)
                self._wait_for_catalogue(deadline)

    def _wait_for_ingest_outbox(self, deadline: float) -> None:
        # Specification refresh can provision topics while ingest still awaits
        # the outbox. The broker logs this event after installing the handle.
        while True:
            self._core.require_live()
            self._broker.require_live()
            event = _event_fields(self.log()).get("ingest outbox pipeline started", {})
            if event.get("module") == "event-broker":
                self._broker.require_live()
                return
            _pause_until(deadline, "Event Broker did not install its ingest outbox")

    def _wait_for_broker(self, deadline: float) -> None:
        while True:
            self._broker.require_live()
            events = _event_fields(self.log())
            bound = events.get(
                "OoP HTTP server bound (probes live; gear routes attach after start)"
            )
            if bound:
                self.base_url = _loopback(f"http://{bound['addr']}")
                if self.base_url in (self.flight_control_base_url, self.directory_endpoint):
                    raise RuntimeError("Event Broker listener coincides with Flight Control")
                break
            _pause_until(deadline, "Event Broker did not bind its HTTP listener")
        self.wait_ready(self._health_path, deadline=deadline)
        print(
            f"broker ready pid={self.broker_pid} exe={self._broker.binary} "
            f"http={self.base_url} probe={self._health_path} home={self.broker_home} "
            f"elapsed={time.monotonic() - self._broker.started:.3f}s",
            flush=True,
        )

    def wait_ready(self, path: str = "/readyz", *, deadline: float | None = None) -> None:
        if deadline is None:
            deadline = time.monotonic() + STARTUP_TIMEOUT
        with httpx.Client(base_url=self.base_url, trust_env=False) as client:
            while True:
                self._core.require_live()
                self._broker.require_live()
                try:
                    response = client.get(path, timeout=_call_timeout(deadline))
                    if response.status_code == 200:
                        self._broker.require_live()
                        return
                except httpx.TransportError:
                    pass
                _pause_until(deadline, f"Event Broker did not answer 200 at {path}")

    def wait_for_broker_exit(self, timeout: float) -> int:
        return self._broker.wait(timeout)

    def restart_broker(self) -> None:
        self._core.require_live()
        old_pid = self.broker_pid
        self._broker.stop(force=True)
        self._broker = None
        self._start_broker()
        if self.broker_pid == old_pid:
            raise RuntimeError("Event Broker restart reused the retired process identity")
        self._core.require_live()
        print(
            f"broker restarted old_pid={old_pid} new_pid={self.broker_pid} "
            f"core_pid={self.core_pid} core_home={self.core_home} broker_home={self.broker_home}",
            flush=True,
        )

    def __enter__(self):
        try:
            self.broker_home.mkdir(mode=0o700)
            self.core_home.mkdir(mode=0o700)
            self._start_core()
            self._write_broker_config()
            self._start_broker()
            return self
        except BaseException as error:
            self.__exit__(type(error), error, error.__traceback__)
            for child in (self._core, self._broker):
                if child is not None:
                    try:
                        log = child.log.read_text(encoding="utf-8", errors="replace")
                    except OSError as log_error:
                        error.add_note(self.redact(str(log_error)))
                    else:
                        error.add_note(self.redact(log)[-2000:])
            raise

    def __exit__(self, exc_type, error, traceback):
        children = (child for child in (self._broker, self._core) if child is not None)
        _release((child.stop for child in children), error, self.redact)
