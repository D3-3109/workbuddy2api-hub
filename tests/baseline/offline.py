"""Isolation shared by the original-suite runner and fixture exporter."""
import ipaddress
import os
from pathlib import Path
import sys
import tempfile

HERE = Path(__file__).resolve().parent
# Checkout of the original Python tree at the baseline commit (see BASELINE.json).
ROOT = Path(os.environ.get("WB_PY_ROOT") or sys.exit("set WB_PY_ROOT to a Python baseline checkout")).resolve()


def isolated_environment(work):
    # Do not inherit API keys, account locations, proxies, or WB_* switches.
    keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "SYSTEMDRIVE"}
    env = {k: v for k, v in os.environ.items() if k.upper() in keep}
    for key, sub in {
        "HOME": "home", "USERPROFILE": "home", "APPDATA": "appdata",
        "LOCALAPPDATA": "localappdata", "XDG_CONFIG_HOME": "config",
        "ACCOUNTS_DIR": "accounts", "WB_PROXY_USAGE_DIR": "usage",
        "WB_PROXY_CATALOG_DIR": "catalog", "WB_PRICING_DIR": "pricing",
        "TMP": "tmp", "TEMP": "tmp", "TMPDIR": "tmp",
    }.items():
        path = Path(work) / sub
        path.mkdir(parents=True, exist_ok=True)
        env[key] = str(path)
    env.update(PYTHONPATH=os.pathsep.join((str(ROOT), str(ROOT / "tests"))),
               PYTHONDONTWRITEBYTECODE="1", PYTHONIOENCODING="utf-8",
               PYTHONUTF8="1", PYTHONHASHSEED="0", NO_PROXY="*", no_proxy="*",
               TZ="UTC", WB_BASELINE_OFFLINE="1")
    return env


def install_guard():
    stats = {"allowed_loopback_events": 0, "blocked_events": []}
    accounts = os.path.normcase(os.path.abspath(ROOT / "accounts"))
    root = os.path.normcase(os.path.abspath(ROOT))
    baseline = os.path.normcase(os.path.abspath(HERE))

    def local(host):
        if isinstance(host, bytes):
            host = host.decode("ascii", "replace")
        if str(host).lower() == "localhost":
            return True
        try:
            return ipaddress.ip_address(host).is_loopback
        except ValueError:
            return False

    def deny(event, detail):
        stats["blocked_events"].append({"event": event, "detail": str(detail)})
        raise PermissionError("offline baseline guard denied %s: %s" % (event, detail))

    def audit(event, args):
        if event in ("socket.getaddrinfo", "socket.gethostbyname", "socket.gethostbyaddr"):
            if not local(args[0]):
                deny(event, args[0])
            stats["allowed_loopback_events"] += 1
        elif event in ("socket.connect", "socket.bind", "socket.sendto"):
            address = args[-1]
            if not isinstance(address, tuple) or not local(address[0]):
                deny(event, address)
            stats["allowed_loopback_events"] += 1
        elif event == "subprocess.Popen":
            deny(event, "child process inside reviewed suite")
        elif event == "open" and isinstance(args[0], (str, bytes, os.PathLike)):
            path = os.path.normcase(os.path.abspath(os.fsdecode(args[0])))
            if path == accounts or path.startswith(accounts + os.sep):
                deny(event, "original repository accounts directory")
            mode, flags = args[1:3]
            writing = (isinstance(mode, str) and any(c in mode for c in "wax+")) or (
                isinstance(flags, int) and bool(flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT)))
            if writing and path.startswith(root + os.sep) and not path.startswith(baseline + os.sep):
                deny(event, "repository write outside rust/tests/baseline")

    sys.addaudithook(audit)
    sys.dont_write_bytecode = True
    return stats


def activate(work):
    env = isolated_environment(work)
    os.environ.clear()
    os.environ.update(env)
    tempfile.tempdir = env["TMP"]
    for path in (str(ROOT), str(ROOT / "tests")):
        if path not in sys.path:
            sys.path.insert(0, path)
    return install_guard()
