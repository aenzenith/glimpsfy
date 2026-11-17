#!/usr/bin/env python3
"""Client of the dev-only driver (src-tauri/src/driver.rs).

Start the app with the driver and a private home:

    GLIMPSFY_HOME=/tmp/glimpsfy-dev-home GLIMPSFY_DRIVER=/tmp/glimpsfy-driver npm run tauri dev

then call it from Python (`from driver import Driver`) or from the shell:

    scripts/driver.py '{"op": "windows"}'
    scripts/driver.py eval overlay-1 'return document.title'

Every request is one JSON file in <folder>/in/, answered in <folder>/out/ under the same name.
Events the app sent are appended to <folder>/events.jsonl.
"""

import json
import os
import subprocess
import sys
import time

FOLDER = os.environ.get("GLIMPSFY_DRIVER", "/tmp/glimpsfy-driver")


class DriverError(Exception):
    pass


class Driver:
    def __init__(self, folder=FOLDER):
        self.folder = folder
        self.inbox = os.path.join(folder, "in")
        self.outbox = os.path.join(folder, "out")
        self.events_path = os.path.join(folder, "events.jsonl")
        self.seq = 0

    # ------------------------------------------------------------------ requests

    def call(self, op, timeout=30.0, **args):
        self.seq += 1
        name = f"{time.time_ns()}-{self.seq:05d}.json"
        request = {"op": op, **args}
        tmp = os.path.join(self.inbox, "." + name)
        with open(tmp, "w") as f:
            json.dump(request, f)
        os.rename(tmp, os.path.join(self.inbox, name))
        answer_path = os.path.join(self.outbox, name)
        deadline = time.time() + timeout + 5
        while not os.path.exists(answer_path):
            if time.time() > deadline:
                raise DriverError(f"no answer to {op} within {timeout} s")
            time.sleep(0.01)
        with open(answer_path) as f:
            answer = json.load(f)
        os.remove(answer_path)
        if not answer.get("ok"):
            raise DriverError(f"{op}: {answer.get('error')}")
        return answer.get("value")

    def eval(self, label, js, timeout=15.0):
        return self.call("eval", timeout=timeout, label=label, js=js, timeout_ms=timeout * 1000)

    def invoke(self, label, cmd, args=None, timeout=30.0):
        return self.call(
            "eval",
            timeout=timeout,
            label=label,
            js=f"return await D.invoke({json.dumps(cmd)}, {json.dumps(args or {})});",
        )

    def capture(self, mode="area"):
        return self.call("capture", mode=mode)

    def shortcut(self, action):
        return self.call("shortcut", action=action)

    def menu(self, item_id):
        return self.call("menu", id=item_id)

    def open(self, kind):
        return self.call("open", kind=kind)

    def windows(self):
        return self.call("windows")

    def window(self, label):
        for w in self.windows():
            if w["label"] == label:
                return w
        return None

    def ping(self):
        return self.call("ping", timeout=5)

    def quit(self):
        return self.call("quit")

    # -------------------------------------------------------------------- events

    def events(self, since=0.0, name=None):
        out = []
        if not os.path.exists(self.events_path):
            return out
        with open(self.events_path) as f:
            for line in f:
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(e, dict) or e.get("wall", 0) < since:
                    continue
                if name and e["event"] != name:
                    continue
                out.append(e)
        return out

    def wait_event(self, name, since, timeout=10.0, pred=None):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for e in self.events(since, name):
                if pred is None or pred(e["payload"]):
                    return e
            time.sleep(0.03)
        raise DriverError(f"no {name} within {timeout} s")

    # ------------------------------------------------------------------- helpers

    def wait_ready(self, timeout=120.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                self.call("ping", timeout=2)
                return True
            except (DriverError, OSError):
                time.sleep(0.5)
        raise DriverError("the app did not start")

    def overlays(self):
        return [w for w in self.windows() if w["label"].startswith("overlay-")]

    def shown_overlays(self):
        return [w for w in self.overlays() if w.get("ns", {}).get("alpha", 0) > 0.5 and w.get("ns", {}).get("onScreen")]


def screenshot(path, window_number=None, display=None):
    """Takes a silent screenshot of the screen, one display (1-based) or one window."""
    cmd = ["screencapture", "-x", "-t", "png"]
    if window_number is not None:
        cmd += ["-o", "-l", str(window_number)]
    if display is not None:
        cmd += ["-D", str(display)]
    subprocess.run(cmd + [path], check=True)
    return path


def main(argv):
    d = Driver()
    if not argv:
        print(__doc__)
        return 1
    if argv[0] == "eval":
        print(json.dumps(d.eval(argv[1], argv[2]), indent=2, ensure_ascii=False))
    elif argv[0] == "invoke":
        args = json.loads(argv[3]) if len(argv) > 3 else {}
        print(json.dumps(d.invoke(argv[1], argv[2], args), indent=2, ensure_ascii=False))
    else:
        request = json.loads(argv[0])
        op = request.pop("op")
        print(json.dumps(d.call(op, **request), indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
