import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "osmo-wifi-run"

MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
p = pathlib.Path(os.environ["MOCK_STATE"])
s = json.loads(p.read_text())
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
s["calls"].append([name] + args)
if name == "networksetup":
    if args[0] == "-listallhardwareports":
        print("Hardware Port: Wi-Fi\nDevice: en0\n")
    elif args[0] == "-getairportnetwork":
        print("You are not associated with an AirPort network." if s["hidden"] else "Current Wi-Fi Network: " + s["ssid"])
    elif args[0] == "-setairportnetwork":
        if s.get("fail_join") and args[2] == "Camera WiFi":
            print("Failed to join network", file=sys.stderr)
            p.write_text(json.dumps(s)); sys.exit(1)
        if s.get("fail_restore") and args[2] == "Home WiFi":
            print("Failed to join network")
            p.write_text(json.dumps(s)); sys.exit(0)
        s["ssid"] = args[2]
elif name == "ipconfig":
    print("192.168.2.8" if s["ssid"] == "Camera WiFi" else "192.168.1.8")
elif name == "osmo":
    if args == ["pair"]:
        print("SSID: Camera WiFi\nPaired.")
    else:
        if s.get("slow_command"):
            p.write_text(json.dumps(s))
            import time
            time.sleep(2)
        p.write_text(json.dumps(s)); sys.exit(s.get("command_exit", 0))
p.write_text(json.dumps(s))
'''


class MacWifiTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.state = root / "state.json"
        self.state.write_text(json.dumps({"ssid": "Home WiFi", "hidden": False, "calls": []}))
        bindir = root / "bin"
        bindir.mkdir()
        for name in ("networksetup", "ipconfig", "osmo"):
            path = bindir / name
            path.write_text(MOCK)
            path.chmod(0o755)
        self.env = {**os.environ, "PATH": f"{bindir}:{os.environ['PATH']}",
                    "MOCK_STATE": str(self.state), "OSMO_BIN": str(bindir / "osmo")}

    def run_script(self, *args):
        return subprocess.run(["bash", str(SCRIPT), *args], env=self.env, text=True,
                              capture_output=True, timeout=15)

    def state_data(self):
        return json.loads(self.state.read_text())

    def test_runs_command_and_restores_original_network(self):
        result = self.run_script("--", "-v", "burst", "DCIM/shot with space.JPG")
        self.assertEqual(result.returncode, 0, result.stderr)
        state = self.state_data()
        self.assertEqual(state["ssid"], "Home WiFi")
        self.assertIn(["osmo", "-v", "burst", "DCIM/shot with space.JPG"], state["calls"])
        self.assertEqual([c[2] for c in state["calls"] if c[:2] == ["networksetup", "-setairportnetwork"]],
                         ["en0", "en0"])

    def test_failing_command_still_restores_network(self):
        state = self.state_data(); state["command_exit"] = 7; self.state.write_text(json.dumps(state))
        result = self.run_script("--", "ls")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.state_data()["ssid"], "Home WiFi")

    def test_hidden_network_requires_explicit_return_network(self):
        state = self.state_data(); state["hidden"] = True; self.state.write_text(json.dumps(state))
        result = self.run_script("--", "ls")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--return-to", result.stderr)
        self.assertFalse(any(c[:2] == ["networksetup", "-setairportnetwork"] for c in self.state_data()["calls"]))
        result = self.run_script("--return-to", "Home WiFi", "--", "ls")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.state_data()["ssid"], "Home WiFi")

    def test_join_failure_does_not_run_the_command_and_attempts_restore(self):
        state = self.state_data(); state["fail_join"] = True; self.state.write_text(json.dumps(state))
        result = self.run_script("--", "ls")
        self.assertNotEqual(result.returncode, 0)
        calls = self.state_data()["calls"]
        self.assertNotIn(["osmo", "ls"], calls)
        self.assertIn(["networksetup", "-setairportnetwork", "en0", "Home WiFi"], calls)

    def test_failed_restore_is_reported_even_if_networksetup_exits_zero(self):
        state = self.state_data(); state["fail_restore"] = True; self.state.write_text(json.dumps(state))
        result = self.run_script("--", "ls")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Reconnect manually", result.stderr)

    def test_termination_reconnects_the_original_network(self):
        state = self.state_data(); state["slow_command"] = True; self.state.write_text(json.dumps(state))
        proc = subprocess.Popen(["bash", str(SCRIPT), "--", "ls"], env=self.env,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            for _ in range(100):
                try:
                    if ["osmo", "ls"] in self.state_data()["calls"]:
                        break
                except json.JSONDecodeError:
                    pass
                time.sleep(0.05)
            else:
                self.fail("osmo command did not start")
            proc.send_signal(signal.SIGTERM)
            _, stderr = proc.communicate(timeout=10)
            self.assertEqual(proc.returncode, 143, stderr)
            self.assertEqual(self.state_data()["ssid"], "Home WiFi")
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate()


if __name__ == "__main__":
    unittest.main()
