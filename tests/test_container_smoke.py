"""Local, mock-only unit tests; these do not validate a Docker image."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import container_smoke as smoke


class ContainerSmokeTests(unittest.TestCase):
    def arguments(self, report="unused-report.json"):
        return smoke.build_parser().parse_args([
            "--image", "local/wb2api:amd64", "--architecture", "amd64",
            "--report", str(report),
        ])

    def test_parser(self):
        args = self.arguments()
        self.assertEqual(args.image, "local/wb2api:amd64")
        self.assertEqual(args.architecture, "amd64")
        self.assertEqual(args.report, Path("unused-report.json"))
        self.assertGreater(args.startup_timeout, 0)
        with contextlib.redirect_stderr(io.StringIO()):
            for extra in (["--architecture", "x86_64"], ["--startup-timeout", "0"],
                          ["--shutdown-timeout", "nan"], ["--startup-timeout", "inf"],
                          ["--shutdown-timeout", "301"]):
                with self.subTest(extra=extra), self.assertRaises(SystemExit):
                    smoke.build_parser().parse_args(["--image", "local/test", "--report", "r", *extra])
            with self.assertRaises(SystemExit):
                smoke.build_parser().parse_args(["--image", "local/test"])

    def test_port_mapping_is_only_loopback(self):
        def container(bindings):
            return {"NetworkSettings": {"Ports": {"8788/tcp": bindings}}}
        binding = {"HostIp": "127.0.0.1", "HostPort": "49152"}
        self.assertEqual(smoke.published_port(container([binding])), 49152)
        for invalid in (None, [], [binding, binding],
                        [{"HostIp": "0.0.0.0", "HostPort": "49152"}],
                        [{"HostIp": "127.0.0.1", "HostPort": "0"}],
                        [{"HostIp": "127.0.0.1", "HostPort": "65536"}],
                        [{"HostIp": "127.0.0.1", "HostPort": "bad"}]):
            with self.subTest(invalid=invalid), self.assertRaises(smoke.SmokeFailure):
                smoke.published_port(container(invalid))

    def test_run_command_hardening_and_isolation(self):
        command = smoke.run_arguments("local/test", "own-name", "owner", "fake-key", "fake-password")
        self.assertEqual(command[:2], ["run", "--detach"])
        for flag in ("--read-only", "--offline"):
            self.assertIn(flag, command)
        for option, value in (("--user", "10001:10001"), ("--cap-drop", "ALL"),
                              ("--security-opt", "no-new-privileges:true"),
                              ("--publish", "127.0.0.1::8788"), ("--pull", "never")):
            self.assertEqual(command[command.index(option) + 1], value)
        for directory in ("/data/accounts", "/data/usage"):
            self.assertIn(directory + ":rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700", command)
        self.assertIn("/tmp:rw,nosuid,nodev,noexec,mode=1777", command)
        self.assertIn("API_KEY=fake-key", command)
        self.assertIn("WB_PANEL_PASSWORD=fake-password", command)
        self.assertNotIn("--volume", command)
        self.assertNotIn("--privileged", command)

    def test_docker_errors_do_not_expose_commands_or_output(self):
        errors = [subprocess.TimeoutExpired(["docker", "secret"], 1, output="secret", stderr="secret"),
                  OSError("secret")]
        for error in errors:
            with patch.object(smoke.subprocess, "run", side_effect=error):
                with self.assertRaises(smoke.SmokeFailure) as caught:
                    smoke.docker(["run", "secret"])
                self.assertNotIn("secret", str(caught.exception))
        with patch.object(smoke.subprocess, "run", return_value=subprocess.CompletedProcess(
                ["docker", "secret"], 125, "secret", "secret")) as run:
            with self.assertRaises(smoke.SmokeFailure) as caught:
                smoke.docker(["run", "secret"])
            self.assertNotIn("secret", str(caught.exception))
            self.assertTrue(run.call_args.kwargs["capture_output"])
            self.assertFalse(run.call_args.kwargs["check"])

    def test_failure_report_written_and_cleanup_attempted(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "nested" / "report.json"
            runner = smoke.ContainerSmoke(self.arguments(report))
            with patch.object(runner, "exercise", side_effect=ValueError("private response body")), \
                    patch.object(runner, "cleanup") as cleanup:
                self.assertFalse(runner.run())
            cleanup.assert_called_once()
            data = json.loads(report.read_text(encoding="utf-8"))
            self.assertFalse(data["ok"])
            self.assertEqual(data["error"], "ValueError")
            self.assertNotIn("private response body", report.read_text(encoding="utf-8"))

    def test_cleanup_refuses_foreign_container(self):
        runner = smoke.ContainerSmoke(self.arguments())
        runner.run_attempted = True
        with patch.object(smoke, "docker", return_value=subprocess.CompletedProcess([], 0, "id\n", "")) as docker, \
                patch.object(smoke, "inspect_one", return_value={
                    "Name": "/" + runner.name, "Config": {"Labels": {smoke.LABEL: "other-owner"}}}):
            with self.assertRaises(smoke.SmokeFailure):
                runner.cleanup()
            self.assertEqual(docker.call_count, 1)

    def test_cleanup_failure_fails_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            runner = smoke.ContainerSmoke(self.arguments(report))
            with patch.object(runner, "exercise"), patch.object(runner, "cleanup", side_effect=OSError("secret")):
                self.assertFalse(runner.run())
            data = json.loads(report.read_text(encoding="utf-8"))
            self.assertEqual(data["cleanup_error"], "OSError")
            self.assertFalse(data["ok"])

    def test_mocked_end_to_end_contract_and_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            runner = smoke.ContainerSmoke(self.arguments(report))
            image = {"Os": "linux", "Architecture": "amd64",
                     "Config": {"User": "10001:10001", "Entrypoint": smoke.ENTRYPOINT}}
            container = {
                "Name": "/" + runner.name,
                "Config": {"User": "10001:10001", "Labels": {smoke.LABEL: runner.owner}},
                "HostConfig": {"ReadonlyRootfs": True, "CapDrop": ["ALL"],
                               "SecurityOpt": ["no-new-privileges:true"]},
                "State": {"Running": True},
                "NetworkSettings": {"Ports": {"8788/tcp": [{"HostIp": "127.0.0.1", "HostPort": "49152"}]}},
            }
            stopped = {"State": {"Running": False, "ExitCode": 0, "OOMKilled": False}}
            seen = []

            def http(port, method, path, *, headers=None, payload=None, timeout=3):
                self.assertEqual(port, 49152)
                headers = headers or {}
                seen.append((method, path, dict(headers)))
                panel = headers.get("X-Panel-Token") == "mock-panel-session"
                api = headers.get("Authorization") == "Bearer " + runner.api_key
                status = 200
                if path == "/health":
                    value = {"ok": True, "api_key_required": True}
                elif path == "/v1/models":
                    status = 200 if api else 401
                    value = {"object": "list", "data": [{"id": "fixture", "object": "model"}]}
                elif path == "/panel/status":
                    value = {"panel_password_required": True, "panel_password_is_default": False,
                             "authenticated": panel, "api_key_set": True}
                elif path == "/panel/login":
                    status = 200 if payload == {"password": runner.password} else 401
                    value = {"ok": True, "using_default_password": False, "token": "mock-panel-session"}
                elif path in ("/accounts", "/settings", "/shutdown"):
                    status = 200 if panel else 401
                    value = {"auth_required": True, "auth_disabled": False, "panel_password_is_default": False,
                             "runtime": "rust", "ok": True, "shutting_down": True}
                elif path == "/dashboard":
                    return 200, "text/html; charset=utf-8", b"<!doctype html><html></html>"
                else:
                    self.fail("Unexpected request path")
                return status, "application/json", json.dumps(value).encode()

            def docker(arguments, **kwargs):
                output = ""
                if arguments[0] == "wait":
                    self.assertEqual(kwargs["timeout"], runner.args.shutdown_timeout)
                    output = "0\n"
                elif arguments[:2] == ["container", "ls"]:
                    output = "owned-container-id\n"
                return subprocess.CompletedProcess([], 0, output, "")

            with patch.object(smoke, "inspect_one", side_effect=[image, container, container, stopped, container]), \
                    patch.object(smoke, "docker", side_effect=docker) as commands, \
                    patch.object(smoke, "request", side_effect=http):
                self.assertTrue(runner.run())
            commands.assert_any_call(["exec", runner.name, "test", "-s", smoke.CA_FILE])
            commands.assert_any_call(["container", "rm", "--force", runner.name])
            content = report.read_text(encoding="utf-8")
            for secret in (runner.api_key, runner.password, "mock-panel-session"):
                self.assertNotIn(secret, content)
            data = json.loads(content)
            self.assertTrue(data["ok"])
            self.assertEqual(data["checks"][-2:], [{"name": "graceful_shutdown", "ok": True},
                                                   {"name": "cleanup", "ok": True}])
            self.assertIn(("GET", "/accounts", {"Authorization": "Bearer " + runner.api_key}), seen)


if __name__ == "__main__":
    unittest.main()
